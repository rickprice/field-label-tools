use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::{SecondsFormat, Utc};
use clap::Parser;
use eframe::egui;
use qrcode::{Color as QrColor, EcLevel, QrCode};
use serde::Deserialize;
use serde_json::{Map, Value, json};

#[derive(Parser)]
#[command(
    version,
    about = "Fullscreen appliance: enter field-label metadata on the left, read it back as a large QR code on the right"
)]
struct Args {
    /// TOML file defining extra project-specific metadata fields to collect,
    /// in addition to the built-in Project/Location/ID/Name fields
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,
}

#[derive(Deserialize, Default)]
struct FieldsConfig {
    #[serde(default)]
    field: Vec<ExtraFieldDef>,
}

#[derive(Deserialize, Clone)]
struct ExtraFieldDef {
    name: String,
    /// Clear this field's value on "Next" (like Name), instead of keeping it
    /// (like Project/Location). Defaults to keeping it.
    #[serde(default)]
    reset_on_next: bool,
}

fn load_fields_config(path: &Path) -> anyhow::Result<Vec<ExtraFieldDef>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading fields config {}", path.display()))?;
    let config: FieldsConfig = toml::from_str(&text)
        .with_context(|| format!("parsing fields config {}", path.display()))?;
    Ok(config.field)
}

struct LabelApp {
    client: String,
    project: String,
    location: String,
    id: u64,
    name: String,
    extra: Vec<(ExtraFieldDef, String)>,
}

impl LabelApp {
    fn new(extra_fields: Vec<ExtraFieldDef>) -> Self {
        Self {
            client: String::new(),
            project: String::new(),
            location: String::new(),
            id: 1,
            name: String::new(),
            extra: extra_fields
                .into_iter()
                .map(|field| (field, String::new()))
                .collect(),
        }
    }

    fn payload(&self) -> String {
        let mut obj = Map::new();
        obj.insert("client".into(), json!(self.client));
        obj.insert("project".into(), json!(self.project));
        obj.insert("location".into(), json!(self.location));
        obj.insert("id".into(), json!(self.id));
        obj.insert("name".into(), json!(self.name));
        obj.insert(
            "timestamp_utc".into(),
            json!(Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)),
        );

        let mut extra_obj = Map::new();
        for (field, value) in &self.extra {
            if !value.is_empty() {
                extra_obj.insert(field.name.clone(), json!(value));
            }
        }
        if !extra_obj.is_empty() {
            obj.insert("extra".into(), Value::Object(extra_obj));
        }

        serde_json::to_string(&Value::Object(obj)).unwrap_or_default()
    }

    fn folder_name(&self) -> String {
        format!(
            "{}-{}-{}-{}-{}",
            self.client, self.project, self.location, self.id, self.name
        )
    }
}

impl eframe::App for LabelApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // The QR's embedded timestamp should track "now" even if the form
        // sits idle with no input between filling it out and photographing
        // the screen, so keep repainting instead of waiting for an event.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs(1));

        egui::Panel::left("metadata")
            .resizable(false)
            .default_size(420.0)
            .show(ui, |ui| {
                ui.add_space(16.0);
                ui.heading("Field Label");
                ui.add_space(12.0);

                ui.label("Client");
                ui.text_edit_singleline(&mut self.client);
                ui.add_space(8.0);

                ui.label("Project");
                ui.text_edit_singleline(&mut self.project);
                ui.add_space(8.0);

                ui.label("Location");
                ui.text_edit_singleline(&mut self.location);
                ui.add_space(8.0);

                ui.label("ID");
                ui.add(egui::DragValue::new(&mut self.id));
                ui.add_space(8.0);

                ui.label("Name");
                let name_resp = ui.text_edit_singleline(&mut self.name);
                ui.add_space(8.0);

                for (field, value) in &mut self.extra {
                    ui.label(&field.name);
                    ui.text_edit_singleline(value);
                    ui.add_space(8.0);
                }

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(12.0);
                ui.monospace(self.folder_name());
                ui.add_space(20.0);

                let next_clicked = ui
                    .add_sized([120.0, 40.0], egui::Button::new("Next"))
                    .clicked();
                let enter_pressed =
                    name_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

                if next_clicked || enter_pressed {
                    self.id += 1;
                    self.name.clear();
                    for (field, value) in &mut self.extra {
                        if field.reset_on_next {
                            value.clear();
                        }
                    }
                    name_resp.request_focus();
                }
            });

        egui::CentralPanel::default().show(ui, |ui| {
            draw_qr(ui, &self.payload());
        });
    }
}

fn draw_qr(ui: &mut egui::Ui, payload: &str) {
    let rect = ui.available_rect_before_wrap();
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, egui::Color32::WHITE);

    let code = match QrCode::with_error_correction_level(payload, EcLevel::H) {
        Ok(code) => code,
        Err(err) => {
            log::warn!(
                "could not encode QR payload ({} bytes long): {err}",
                payload.len()
            );
            return;
        }
    };
    let modules = code.width();
    let colors = code.to_colors();

    const QUIET_ZONE_MODULES: f32 = 2.0;
    let side = rect.width().min(rect.height());
    let module_size = side / (modules as f32 + QUIET_ZONE_MODULES * 2.0);
    let qr_pixel_side = module_size * modules as f32;
    let origin = rect.center() - egui::vec2(qr_pixel_side / 2.0, qr_pixel_side / 2.0);

    for y in 0..modules {
        for x in 0..modules {
            if colors[y * modules + x] == QrColor::Dark {
                let min = origin + egui::vec2(x as f32 * module_size, y as f32 * module_size);
                let max = min + egui::vec2(module_size, module_size);
                painter.rect_filled(
                    egui::Rect::from_min_max(min, max),
                    0.0,
                    egui::Color32::BLACK,
                );
            }
        }
    }
}

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = Args::parse();

    let extra_fields = match &args.config {
        Some(path) => match load_fields_config(path) {
            Ok(fields) => fields,
            Err(err) => {
                log::error!("{err:#}");
                std::process::exit(1);
            }
        },
        None => Vec::new(),
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1600.0, 900.0])
            .with_title("Field Label"),
        ..Default::default()
    };

    eframe::run_native(
        "Field Label",
        options,
        Box::new(move |_cc| Ok(Box::new(LabelApp::new(extra_fields)))),
    )
}

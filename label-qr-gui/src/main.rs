use clap::Parser;
use eframe::egui;
use qrcode::{Color as QrColor, EcLevel, QrCode};
use serde::Serialize;

#[derive(Parser)]
#[command(
    version,
    about = "Fullscreen appliance: enter field-label metadata on the left, read it back as a large QR code on the right"
)]
struct Args {}

#[derive(Serialize)]
struct Label<'a> {
    project: &'a str,
    location: &'a str,
    id: u64,
    name: &'a str,
}

struct LabelApp {
    project: String,
    location: String,
    id: u64,
    name: String,
}

impl Default for LabelApp {
    fn default() -> Self {
        Self {
            project: String::new(),
            location: String::new(),
            id: 1,
            name: String::new(),
        }
    }
}

impl LabelApp {
    fn payload(&self) -> String {
        let label = Label {
            project: &self.project,
            location: &self.location,
            id: self.id,
            name: &self.name,
        };
        serde_json::to_string(&label).unwrap_or_default()
    }

    fn folder_name(&self) -> String {
        format!(
            "{}-{}-{}-{}",
            self.project, self.location, self.id, self.name
        )
    }
}

impl eframe::App for LabelApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::left("metadata")
            .resizable(false)
            .default_size(420.0)
            .show(ui, |ui| {
                ui.add_space(16.0);
                ui.heading("Field Label");
                ui.add_space(12.0);

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
                ui.add_space(20.0);

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
    let _args = Args::parse();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1600.0, 900.0])
            .with_title("Field Label"),
        ..Default::default()
    };

    eframe::run_native(
        "Field Label",
        options,
        Box::new(|_cc| Ok(Box::new(LabelApp::default()))),
    )
}

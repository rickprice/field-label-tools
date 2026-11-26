use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Context;
use chrono::{Local, SecondsFormat, Timelike, Utc};
use clap::Parser;
use eframe::egui;
use qrcode::{Color as QrColor, EcLevel, QrCode};
use serde::Deserialize;
use serde_json::{Map, Value, json};

mod ltc;

/// `(label, fps, drop_frame)` -- mirrors web/index.html's `#f-ltc-fps`
/// options exactly, including the two separate 29.97 entries.
const LTC_FPS_OPTIONS: [(&str, f64, bool); 5] = [
    ("24 fps", 24.0, false),
    ("25 fps", 25.0, false),
    ("29.97 fps (Non-Drop)", 29.97, false),
    ("29.97 fps (Drop-Frame)", 29.97, true),
    ("30 fps", 30.0, false),
];

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

    /// Write out the browser-based alternative front end (web/index.html
    /// and web/fields.example.json from the source repo) into DIR instead
    /// of launching the GUI -- useful if you installed via `cargo install`
    /// / crates.io and don't have a GitHub checkout to copy it from.
    #[arg(long, value_name = "DIR")]
    export_web: Option<PathBuf>,
}

const WEB_INDEX_HTML: &str = include_str!("../web/index.html");
const WEB_FIELDS_EXAMPLE_JSON: &str = include_str!("../web/fields.example.json");

/// Writes the embedded copies of web/index.html and web/fields.example.json
/// into `dir` (created if missing). Kept separate from `main` so it's
/// testable without going through Args/eframe at all.
fn export_web(dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;

    let index_path = dir.join("index.html");
    std::fs::write(&index_path, WEB_INDEX_HTML)
        .with_context(|| format!("writing {}", index_path.display()))?;

    let fields_path = dir.join("fields.example.json");
    std::fs::write(&fields_path, WEB_FIELDS_EXAMPLE_JSON)
        .with_context(|| format!("writing {}", fields_path.display()))?;

    log::info!(
        "wrote {} and {}",
        index_path.display(),
        fields_path.display()
    );
    Ok(())
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
    /// Index into LTC_FPS_OPTIONS for the currently selected rate.
    ltc_fps_index: usize,
    /// `None` until a tick has actually been generated (updated from the
    /// cpal audio callback thread, read from the UI thread each repaint).
    ltc_readout: Arc<Mutex<Option<ltc::Timecode>>>,
    /// Kept alive only while the timecode is playing -- dropping this
    /// stops the audio immediately; `Some` doubles as the "is running"
    /// flag, so there's no separate boolean to keep in sync with it.
    ltc_stream: Option<cpal::Stream>,
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
            ltc_fps_index: 1, // 25 fps, matching web/index.html's default
            ltc_readout: Arc::new(Mutex::new(None)),
            ltc_stream: None,
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
        // While the timecode is playing, repaint much faster so its
        // readout doesn't look stuck between frame updates (it ticks at
        // 24-30Hz; a human would notice anything slower than that).
        let repaint_interval = if self.ltc_stream.is_some() {
            std::time::Duration::from_millis(50)
        } else {
            std::time::Duration::from_secs(1)
        };
        ui.ctx().request_repaint_after(repaint_interval);

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

                ui.add_space(24.0);
                ui.separator();
                ui.add_space(12.0);
                ui.label("Timecode Warble");
                let running = self.ltc_stream.is_some();

                ui.add_enabled_ui(!running, |ui| {
                    egui::ComboBox::from_id_salt("ltc_fps")
                        .selected_text(LTC_FPS_OPTIONS[self.ltc_fps_index].0)
                        .show_ui(ui, |ui| {
                            for (i, (label, _, _)) in LTC_FPS_OPTIONS.iter().enumerate() {
                                ui.selectable_value(&mut self.ltc_fps_index, i, *label);
                            }
                        });
                });
                ui.add_space(8.0);

                let toggle_label = if running {
                    "Stop Timecode"
                } else {
                    "Start Timecode"
                };
                if ui
                    .add_sized([160.0, 36.0], egui::Button::new(toggle_label))
                    .clicked()
                {
                    if running {
                        self.ltc_stream = None;
                        *self.ltc_readout.lock().unwrap() = None;
                    } else {
                        let (_, fps, drop_frame) = LTC_FPS_OPTIONS[self.ltc_fps_index];
                        match start_ltc_stream(fps, drop_frame, Arc::clone(&self.ltc_readout)) {
                            Ok(stream) => self.ltc_stream = Some(stream),
                            Err(err) => log::error!("could not start timecode audio: {err:#}"),
                        }
                    }
                }
                ui.add_space(10.0);

                let (_, _, drop_frame) = LTC_FPS_OPTIONS[self.ltc_fps_index];
                let readout_text = match *self.ltc_readout.lock().unwrap() {
                    Some(tc) => format!(
                        "{:02}:{:02}:{:02}{}{:02}",
                        tc.h,
                        tc.m,
                        tc.s,
                        if drop_frame { ';' } else { ':' },
                        tc.f
                    ),
                    None => "--:--:--:--".to_string(),
                };
                ui.monospace(readout_text);
                ui.add_space(8.0);
                ui.weak(
                    "Plays a continuous SMPTE LTC audio tone (jam-synced to \
                     this device's clock when started) through the default \
                     audio output -- feed it into a camera/recorder's mic \
                     input to help auto-sync tools align multiple cameras/ \
                     audio afterward. Independent of the QR code above.",
                );
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

/// Opens the system default audio output and starts an [`ltc::LtcGenerator`]
/// jam-synced to the current wall-clock time of day, writing its readout
/// into `readout` once per callback buffer for the UI thread to display.
/// Dropping the returned `Stream` stops the audio immediately.
fn start_ltc_stream(
    fps: f64,
    drop_frame: bool,
    readout: Arc<Mutex<Option<ltc::Timecode>>>,
) -> anyhow::Result<cpal::Stream> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let device = cpal::default_host()
        .default_output_device()
        .context("no default audio output device")?;
    let supported_config = device
        .default_output_config()
        .context("querying default audio output config")?;
    let sample_format = supported_config.sample_format();
    let channels = supported_config.channels() as usize;
    let sample_rate = supported_config.sample_rate().0 as f64;
    let config: cpal::StreamConfig = supported_config.into();

    let now = Local::now();
    let frac_secs = now.timestamp_subsec_millis() as f64 / 1000.0;
    let raw_frame = (frac_secs * fps.round()) as u8;
    let initial_frame = ltc::first_valid_frame(raw_frame, now.minute() as u8, drop_frame);
    let generator = ltc::LtcGenerator::new(
        sample_rate,
        fps,
        drop_frame,
        now.hour() as u8,
        now.minute() as u8,
        now.second() as u8,
        initial_frame,
    );

    let err_fn = |err| log::error!("timecode audio stream error: {err}");
    let stream = match sample_format {
        cpal::SampleFormat::F32 => {
            let mut generator = generator;
            device.build_output_stream(
                &config,
                move |data: &mut [f32], _| {
                    write_ltc_samples(data, channels, &mut generator, &readout, |s| s)
                },
                err_fn,
                None,
            )?
        }
        cpal::SampleFormat::I16 => {
            let mut generator = generator;
            device.build_output_stream(
                &config,
                move |data: &mut [i16], _| {
                    write_ltc_samples(data, channels, &mut generator, &readout, |s| {
                        (s * i16::MAX as f32) as i16
                    })
                },
                err_fn,
                None,
            )?
        }
        cpal::SampleFormat::U16 => {
            let mut generator = generator;
            device.build_output_stream(
                &config,
                move |data: &mut [u16], _| {
                    write_ltc_samples(data, channels, &mut generator, &readout, |s| {
                        ((s * 0.5 + 0.5) * u16::MAX as f32) as u16
                    })
                },
                err_fn,
                None,
            )?
        }
        other => anyhow::bail!("unsupported audio output sample format: {other:?}"),
    };
    stream.play().context("starting timecode audio stream")?;
    Ok(stream)
}

/// Fills one callback buffer with [`ltc::LtcGenerator`] output (the same
/// sample repeated across every channel -- LTC is carried mono, feeding
/// the same signal to all outputs is harmless) and publishes the
/// generator's current timecode for the UI thread to read.
fn write_ltc_samples<T: Copy>(
    data: &mut [T],
    channels: usize,
    generator: &mut ltc::LtcGenerator,
    readout: &Arc<Mutex<Option<ltc::Timecode>>>,
    convert: impl Fn(f32) -> T,
) {
    for frame in data.chunks_mut(channels.max(1)) {
        let sample = convert(generator.next_sample());
        for out in frame.iter_mut() {
            *out = sample;
        }
    }
    if let Ok(mut guard) = readout.lock() {
        *guard = Some(generator.timecode());
    }
}

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = Args::parse();

    if let Some(dir) = &args.export_web {
        if let Err(err) = export_web(dir) {
            log::error!("{err:#}");
            std::process::exit(1);
        }
        return Ok(());
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_web_writes_both_files_with_the_expected_content() {
        let dir = tempfile::tempdir().unwrap();
        export_web(dir.path()).unwrap();

        let index = std::fs::read_to_string(dir.path().join("index.html")).unwrap();
        assert!(index.contains("<title>Field Label</title>"));
        assert!(index.contains("DEFAULT_FIELD_SETS"));

        let fields = std::fs::read_to_string(dir.path().join("fields.example.json")).unwrap();
        assert!(fields.contains("Surveyor"));
    }

    #[test]
    fn export_web_creates_the_target_directory_if_missing() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/b/c");
        export_web(&nested).unwrap();
        assert!(nested.join("index.html").is_file());
    }
}

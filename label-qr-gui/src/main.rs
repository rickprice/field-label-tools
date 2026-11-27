use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Context;
use chrono::{Local, SecondsFormat, Timelike, Utc};
use clap::Parser;
use eframe::egui;
use qrcode::render::svg;
use qrcode::{Color as QrColor, EcLevel, QrCode};
use serde::Deserialize;
use serde::de::{Deserializer, MapAccess, Visitor};
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
    /// JSON file defining named Metadata Sets of extra project-specific
    /// fields, selectable from a dropdown, in addition to the built-in
    /// Client/Project/Location/ID/Name fields -- the exact same file
    /// format (and, if you point both tools at the same path, the exact
    /// same file) web/index.html's fields.json sidecar uses: {"Set
    /// Name": [{"name": ..., "reset_on_next": bool, "multiline": bool},
    /// ...], ...} (see web/fields.example.json)
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

/// One named "Metadata Set" selectable from the GUI's dropdown. Loaded
/// from a JSON file shaped `{"Set Name": [field, ...], ...}` -- the exact
/// same shape (and, if pointed at the same path, the exact same file)
/// web/index.html's `fields.json` sidecar uses. `name` comes from the
/// JSON object's key, not a field inside the value, so there's no
/// `Deserialize` derive on this struct -- see `FieldSetsFile` below,
/// which builds these from a JSON object's entries directly.
#[derive(Clone)]
struct FieldSet {
    name: String,
    field: Vec<ExtraFieldDef>,
}

#[derive(Deserialize, Clone)]
struct ExtraFieldDef {
    name: String,
    /// Clear this field's value on "Next" (like Name), instead of keeping it
    /// (like Project/Location). Defaults to keeping it.
    #[serde(default)]
    reset_on_next: bool,
    /// Use a multi-line text area instead of a single-line box -- for a
    /// field that commonly holds more than one line (a list of names, a
    /// multi-line address, etc.).
    #[serde(default)]
    multiline: bool,
}

/// Deserializes a `{"Set Name": [field, ...], ...}` JSON object into
/// `Vec<FieldSet>`, preserving the object's own key order (matching JS
/// object property order, which is what web/index.html's dropdown
/// construction relies on) -- a plain `BTreeMap`/`serde_json::Map` would
/// silently re-sort the sets alphabetically instead, without
/// `serde_json`'s `preserve_order` feature (not worth enabling just for
/// this one call site when a short manual `Visitor` does the same thing
/// with no new dependency, by collecting map entries in the order
/// `MapAccess` yields them -- which is the JSON text's own order,
/// regardless of what Rust container `Deserialize` builds them into).
struct FieldSetsFile(Vec<FieldSet>);

impl<'de> Deserialize<'de> for FieldSetsFile {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct FieldSetsVisitor;

        impl<'de> Visitor<'de> for FieldSetsVisitor {
            type Value = Vec<FieldSet>;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a JSON object mapping metadata-set names to field lists")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut sets = Vec::new();
                while let Some((name, field)) = map.next_entry::<String, Vec<ExtraFieldDef>>()? {
                    sets.push(FieldSet { name, field });
                }
                Ok(sets)
            }
        }

        deserializer
            .deserialize_map(FieldSetsVisitor)
            .map(FieldSetsFile)
    }
}

/// A no-extra-fields "Plain" set is always available, even if the config
/// file doesn't define one itself -- checked case-insensitively against
/// "Plain"/"Default", or any set that's already empty -- so there's
/// always a quick way to skip extra fields entirely, the same guarantee
/// web/index.html's `ensurePlainFieldSet()` makes.
fn ensure_plain_field_set(mut sets: Vec<FieldSet>) -> Vec<FieldSet> {
    let has_plain_equivalent = sets.iter().any(|set| {
        set.name.eq_ignore_ascii_case("plain")
            || set.name.eq_ignore_ascii_case("default")
            || set.field.is_empty()
    });
    if !has_plain_equivalent {
        sets.insert(
            0,
            FieldSet {
                name: "Plain".to_string(),
                field: Vec::new(),
            },
        );
    }
    sets
}

fn default_field_sets() -> Vec<FieldSet> {
    vec![FieldSet {
        name: "Plain".to_string(),
        field: Vec::new(),
    }]
}

fn load_fields_config(path: &Path) -> anyhow::Result<Vec<FieldSet>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading fields config {}", path.display()))?;
    let FieldSetsFile(sets) = serde_json::from_str(&text)
        .with_context(|| format!("parsing fields config {}", path.display()))?;
    Ok(ensure_plain_field_set(sets))
}

struct LabelApp {
    client: String,
    project: String,
    location: String,
    id: u64,
    name: String,
    field_sets: Vec<FieldSet>,
    /// Index into `field_sets` for the currently selected Metadata Set.
    selected_set_index: usize,
    /// Rebuilt from `field_sets[selected_set_index]` whenever the
    /// selection changes -- see `ui()`'s ComboBox handling.
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
    fn new(field_sets: Vec<FieldSet>) -> Self {
        let extra = Self::build_extra(&field_sets[0]);
        Self {
            client: String::new(),
            project: String::new(),
            location: String::new(),
            id: 1,
            name: String::new(),
            field_sets,
            selected_set_index: 0,
            extra,
            ltc_fps_index: 1, // 25 fps, matching web/index.html's default
            ltc_readout: Arc::new(Mutex::new(None)),
            ltc_stream: None,
        }
    }

    fn build_extra(set: &FieldSet) -> Vec<(ExtraFieldDef, String)> {
        set.field
            .iter()
            .cloned()
            .map(|field| (field, String::new()))
            .collect()
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
                ui.heading("QR Field Data");
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

                ui.label("Metadata Set");
                let previous_set_index = self.selected_set_index;
                egui::ComboBox::from_id_salt("field_set")
                    .selected_text(&self.field_sets[self.selected_set_index].name)
                    .show_ui(ui, |ui| {
                        for (i, set) in self.field_sets.iter().enumerate() {
                            ui.selectable_value(&mut self.selected_set_index, i, &set.name);
                        }
                    });
                if self.selected_set_index != previous_set_index {
                    self.extra = Self::build_extra(&self.field_sets[self.selected_set_index]);
                }
                ui.add_space(8.0);

                for (field, value) in &mut self.extra {
                    ui.label(&field.name);
                    if field.multiline {
                        ui.add(egui::TextEdit::multiline(value).desired_rows(3));
                    } else {
                        ui.text_edit_singleline(value);
                    }
                    ui.add_space(8.0);
                }

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(12.0);
                ui.monospace(self.folder_name());
                ui.add_space(20.0);

                let (next_clicked, print_clicked) = ui
                    .horizontal(|ui| {
                        let next = ui
                            .add_sized([120.0, 40.0], egui::Button::new("Next"))
                            .clicked();
                        let print = ui
                            .add_sized([120.0, 40.0], egui::Button::new("Print"))
                            .clicked();
                        (next, print)
                    })
                    .inner;
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

                if print_clicked {
                    if let Err(err) = print_label(&self.payload(), &self.folder_name()) {
                        log::error!("could not open print preview: {err:#}");
                    }
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

/// Builds a minimal, self-contained HTML page with the QR code (as inline
/// SVG, so it prints crisp at any size/DPI rather than a raster image) and
/// `caption` below it, writes it to a fixed path in the system temp
/// directory, and opens it with the default web browser via `xdg-open` --
/// reusing the browser's own print dialog/PDF export rather than
/// reimplementing print layout or PDF generation in Rust, the same
/// reasoning web/index.html's own "just open it in a browser" approach
/// uses. Linux-only (`xdg-open`), matching this crate's `platforms.linux`
/// in flake.nix.
fn build_print_html(payload: &str, caption: &str) -> anyhow::Result<String> {
    let code = QrCode::with_error_correction_level(payload, EcLevel::H)
        .map_err(|err| anyhow::anyhow!("encoding QR payload: {err}"))?;
    let svg_markup = code.render::<svg::Color>().min_dimensions(600, 600).build();

    Ok(format!(
        r#"<!doctype html>
<html>
<head>
<meta charset="utf-8">
<title>Print QR Field Data</title>
<style>
  body {{ font-family: sans-serif; text-align: center; margin: 40px; }}
  svg {{ width: 80%; max-width: 500px; height: auto; }}
  .caption {{ margin-top: 16px; font-size: 1.1rem; }}
</style>
</head>
<body>
{svg_markup}
<div class="caption">{escaped_caption}</div>
</body>
</html>
"#,
        escaped_caption = caption
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;"),
    ))
}

fn print_label(payload: &str, caption: &str) -> anyhow::Result<()> {
    let html = build_print_html(payload, caption)?;

    let path = std::env::temp_dir().join("label-qr-gui-print.html");
    std::fs::write(&path, html).with_context(|| format!("writing {}", path.display()))?;

    std::process::Command::new("xdg-open")
        .arg(&path)
        .spawn()
        .with_context(|| format!("opening {} with xdg-open", path.display()))?;
    Ok(())
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

    let field_sets = match &args.config {
        Some(path) => match load_fields_config(path) {
            Ok(sets) => sets,
            Err(err) => {
                log::error!("{err:#}");
                std::process::exit(1);
            }
        },
        None => default_field_sets(),
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1600.0, 900.0])
            .with_title("QR Field Data"),
        ..Default::default()
    };

    eframe::run_native(
        "QR Field Data",
        options,
        Box::new(move |_cc| Ok(Box::new(LabelApp::new(field_sets)))),
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
        assert!(index.contains("<title>QR Field Data</title>"));
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

    fn set_names(sets: &[FieldSet]) -> Vec<&str> {
        sets.iter().map(|s| s.name.as_str()).collect()
    }

    #[test]
    fn ensure_plain_field_set_injects_plain_first_when_none_qualifies() {
        let sets = vec![FieldSet {
            name: "Survey".to_string(),
            field: vec![ExtraFieldDef {
                name: "Surveyor".to_string(),
                reset_on_next: false,
                multiline: false,
            }],
        }];
        let resolved = ensure_plain_field_set(sets);
        assert_eq!(set_names(&resolved), vec!["Plain", "Survey"]);
        assert!(resolved[0].field.is_empty());
    }

    #[test]
    fn ensure_plain_field_set_does_not_duplicate_an_explicit_default_set() {
        let sets = vec![
            FieldSet {
                name: "Default".to_string(),
                field: Vec::new(),
            },
            FieldSet {
                name: "Survey".to_string(),
                field: vec![ExtraFieldDef {
                    name: "Surveyor".to_string(),
                    reset_on_next: false,
                    multiline: false,
                }],
            },
        ];
        let resolved = ensure_plain_field_set(sets);
        assert_eq!(set_names(&resolved), vec!["Default", "Survey"]);
    }

    #[test]
    fn ensure_plain_field_set_counts_any_already_empty_set_as_satisfying() {
        let sets = vec![FieldSet {
            name: "Whatever".to_string(),
            field: Vec::new(),
        }];
        let resolved = ensure_plain_field_set(sets);
        assert_eq!(set_names(&resolved), vec!["Whatever"]);
    }

    #[test]
    fn load_fields_config_parses_sets_with_multiline_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fields.json");
        std::fs::write(
            &path,
            r#"{
                "Church Photos": [
                    { "name": "People", "multiline": true },
                    { "name": "Notes", "reset_on_next": true }
                ]
            }"#,
        )
        .unwrap();

        let sets = load_fields_config(&path).unwrap();
        assert_eq!(set_names(&sets), vec!["Plain", "Church Photos"]);
        let church = &sets[1];
        assert_eq!(church.field[0].name, "People");
        assert!(church.field[0].multiline);
        assert!(!church.field[0].reset_on_next);
        assert_eq!(church.field[1].name, "Notes");
        assert!(church.field[1].reset_on_next);
    }

    #[test]
    fn load_fields_config_preserves_the_json_files_own_key_order() {
        // Deliberately not alphabetical -- a plain BTreeMap/serde_json::Map
        // would silently re-sort these, which is exactly the bug
        // FieldSetsFile's custom Visitor exists to avoid. Each set has a
        // real field so none of them trivially count as a "Plain
        // equivalent" (an empty set would, by design -- see
        // ensure_plain_field_set -- which would make this test
        // accidentally pass/fail for the wrong reason).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fields.json");
        std::fs::write(
            &path,
            r#"{
                "Zebra Shoots": [{"name": "Handler"}],
                "Apple Orchard": [{"name": "Variety"}],
                "Mango Farm": [{"name": "Variety"}]
            }"#,
        )
        .unwrap();

        let sets = load_fields_config(&path).unwrap();
        assert_eq!(
            set_names(&sets),
            vec!["Plain", "Zebra Shoots", "Apple Orchard", "Mango Farm"]
        );
    }

    #[test]
    fn load_fields_config_can_read_the_webpages_own_fields_json_sidecar() {
        // The whole point: both front ends accept the exact same file.
        let path = PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/web/fields.example.json"
        ));
        let sets = load_fields_config(&path).unwrap();
        assert_eq!(
            set_names(&sets),
            vec!["Plain", "Survey", "Inspection", "Church Photos"]
        );
    }

    #[test]
    fn build_print_html_embeds_a_scannable_qr_and_the_caption() {
        let payload = r#"{"client":"Acme Co","project":"P1","location":"North Ridge","id":3,"name":"overview","timestamp_utc":"2026-11-26T00:00:00Z"}"#;
        let html = build_print_html(payload, "Acme Co-P1-North Ridge-3-overview").unwrap();
        assert!(html.contains("<svg"));
        assert!(html.contains("Acme Co-P1-North Ridge-3-overview"));
        assert!(html.contains("<title>Print QR Field Data</title>"));
    }

    #[test]
    fn build_print_html_escapes_the_caption() {
        let html = build_print_html("payload", "A & B <script>").unwrap();
        assert!(html.contains("A &amp; B &lt;script&gt;"));
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn default_field_sets_is_a_single_empty_plain_set() {
        let sets = default_field_sets();
        assert_eq!(set_names(&sets), vec!["Plain"]);
        assert!(sets[0].field.is_empty());
    }
}

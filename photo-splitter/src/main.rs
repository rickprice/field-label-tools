use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use clap::Parser;
use exif::{In, Tag, Value};
use serde::{Deserialize, Serialize};

mod libraw_sys;
mod raw_decode;

/// RAW extensions matching darktable-import-xmp's supported format list.
const RAW_EXTENSIONS: &[&str] = &["cr2", "nef", "arw", "dng", "orf", "pef"];
const RASTER_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "tif", "tiff", "bmp", "webp"];

#[derive(Parser)]
#[command(
    version,
    about = "Split a directory of time-ordered photos into per-shoot folders using QR-code marker frames"
)]
struct Args {
    /// Directory containing the photos to split. Each shoot's photos are
    /// moved out of here into a new `<Project>-<Location>-<ID>-<Name>`
    /// subdirectory once the next marker (or the end of the directory) is
    /// found, so partially-processed shoots are never left half-moved.
    dir: PathBuf,

    /// Show what would be grouped without moving any files or writing metadata
    #[arg(long, short = 'n')]
    dry_run: bool,

    /// Scan RAW files' embedded EXIF thumbnail for QR codes instead of
    /// running LibRaw's full demosaic pipeline. Much faster, but the
    /// thumbnail is often small and some cameras omit it entirely, so a
    /// marker frame can be missed and silently merged into the previous
    /// group. Prefer the default unless a full run is too slow.
    #[arg(long)]
    fast: bool,
}

/// The JSON payload label-qr-gui encodes into its QR code.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
struct LabelPayload {
    project: String,
    location: String,
    id: u64,
    name: String,
    timestamp_utc: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    extra: BTreeMap<String, String>,
}

struct Group {
    label: LabelPayload,
    /// All member files, including the marker photo itself (always first).
    members: Vec<PathBuf>,
}

impl Group {
    fn new(label: LabelPayload, marker_path: PathBuf) -> Self {
        Self {
            label,
            members: vec![marker_path],
        }
    }
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = Args::parse();
    run(&args)
}

fn run(args: &Args) -> Result<()> {
    let mut entries: Vec<PathBuf> = fs::read_dir(&args.dir)
        .with_context(|| format!("reading directory {}", args.dir.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && is_supported(path))
        .collect();

    if entries.is_empty() {
        log::warn!("no supported photos found in {}", args.dir.display());
        return Ok(());
    }

    entries.sort_by_key(|path| (captured_at(path), path.clone()));

    let mut current: Option<Group> = None;
    let mut unmatched = 0usize;
    let mut groups_written = 0usize;

    for path in entries {
        match decode_qr(&path, args.fast) {
            Some(label) => {
                if let Some(group) = current.take() {
                    finish_group(group, &args.dir, args.dry_run)?;
                    groups_written += 1;
                }
                current = Some(Group::new(label, path));
            }
            None => match current.as_mut() {
                Some(group) => group.members.push(path),
                None => {
                    log::warn!(
                        "{}: no QR marker seen yet, leaving in place",
                        path.display()
                    );
                    unmatched += 1;
                }
            },
        }
    }

    if let Some(group) = current.take() {
        finish_group(group, &args.dir, args.dry_run)?;
        groups_written += 1;
    }

    log::info!(
        "{}{groups_written} group(s) written; {unmatched} file(s) left in place (no marker seen yet)",
        if args.dry_run { "[dry run] " } else { "" }
    );
    Ok(())
}

// ── File classification & ordering ──────────────────────────────────────────

fn extension_lower(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
}

fn is_raw(path: &Path) -> bool {
    extension_lower(path).is_some_and(|ext| RAW_EXTENSIONS.contains(&ext.as_str()))
}

fn is_supported(path: &Path) -> bool {
    extension_lower(path).is_some_and(|ext| {
        RAW_EXTENSIONS.contains(&ext.as_str()) || RASTER_EXTENSIONS.contains(&ext.as_str())
    })
}

/// EXIF `DateTimeOriginal` when present, otherwise the file's mtime.
fn captured_at(path: &Path) -> SystemTime {
    exif_datetime_original(path).unwrap_or_else(|| {
        fs::metadata(path)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH)
    })
}

fn exif_datetime_original(path: &Path) -> Option<SystemTime> {
    let exif = read_exif(path)?;
    let field = exif.get_field(Tag::DateTimeOriginal, In::PRIMARY)?;
    let Value::Ascii(ref v) = field.value else {
        return None;
    };
    let text = String::from_utf8_lossy(v.first()?)
        .trim_end_matches('\0')
        .to_string();
    let naive = chrono::NaiveDateTime::parse_from_str(&text, "%Y:%m:%d %H:%M:%S").ok()?;
    Some(naive.and_utc().into())
}

fn read_exif(path: &Path) -> Option<exif::Exif> {
    let file = File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    exif::Reader::new().read_from_container(&mut reader).ok()
}

// ── QR decoding ──────────────────────────────────────────────────────────────

fn decode_qr(path: &Path, fast: bool) -> Option<LabelPayload> {
    let gray = load_grayscale_for_qr(path, fast)?;
    let mut prepared = rqrr::PreparedImage::prepare(gray);
    for grid in prepared.detect_grids() {
        let Ok((_, content)) = grid.decode() else {
            continue;
        };
        match serde_json::from_str::<LabelPayload>(&content) {
            Ok(label) => return Some(label),
            Err(err) => log::debug!(
                "{}: found a QR code but it isn't a label payload ({err})",
                path.display()
            ),
        }
    }
    None
}

fn load_grayscale_for_qr(path: &Path, fast: bool) -> Option<image::GrayImage> {
    if is_raw(path) {
        let image = if fast {
            raw_decode::decode_thumbnail(path)
        } else {
            raw_decode::decode_default(path)
        };
        if image.is_none() {
            log::warn!(
                "{}: could not get an image to scan for a QR code ({})",
                path.display(),
                if fast {
                    "no usable EXIF thumbnail"
                } else {
                    "libraw decode failed"
                }
            );
        }
        image
    } else {
        match image::open(path) {
            Ok(img) => Some(img.to_luma8()),
            Err(err) => {
                log::debug!("{}: could not decode image: {err}", path.display());
                None
            }
        }
    }
}

// ── Grouping & moving ────────────────────────────────────────────────────────

fn sanitize(s: &str) -> String {
    let cleaned: String = s
        .trim()
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    if cleaned.is_empty() {
        "_".to_string()
    } else {
        cleaned
    }
}

fn folder_name(label: &LabelPayload) -> String {
    format!(
        "{}-{}-{}-{}",
        sanitize(&label.project),
        sanitize(&label.location),
        label.id,
        sanitize(&label.name)
    )
}

fn finish_group(group: Group, dir: &Path, dry_run: bool) -> Result<()> {
    let dest = dir.join(folder_name(&group.label));

    if dry_run {
        log::info!(
            "[dry run] would create {} with {} file(s)",
            dest.display(),
            group.members.len()
        );
        return Ok(());
    }

    if dest.exists() {
        bail!(
            "destination {} already exists, refusing to merge/overwrite",
            dest.display()
        );
    }

    fs::create_dir(&dest).with_context(|| format!("creating {}", dest.display()))?;

    for member in &group.members {
        let file_name = member
            .file_name()
            .with_context(|| format!("{} has no file name", member.display()))?;
        let target = dest.join(file_name);
        fs::rename(member, &target)
            .with_context(|| format!("moving {} to {}", member.display(), target.display()))?;
    }

    let yaml = serde_yaml::to_string(&group.label)?;
    fs::write(dest.join("metadata.yaml"), yaml)
        .with_context(|| format!("writing metadata.yaml in {}", dest.display()))?;

    log::info!("{} ({} file(s))", dest.display(), group.members.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // ── sanitize / folder_name ───────────────────────────────────────────

    #[test]
    fn sanitize_leaves_plain_text_alone() {
        assert_eq!(sanitize("North Ridge"), "North Ridge");
    }

    #[test]
    fn sanitize_replaces_path_separators() {
        assert_eq!(sanitize("a/b\\c"), "a_b_c");
    }

    #[test]
    fn sanitize_empty_becomes_underscore() {
        assert_eq!(sanitize("   "), "_");
    }

    #[test]
    fn folder_name_matches_expected_format() {
        let label = LabelPayload {
            project: "P1".into(),
            location: "North Ridge".into(),
            id: 3,
            name: "overview".into(),
            timestamp_utc: "2026-11-26T00:00:00Z".into(),
            extra: BTreeMap::new(),
        };
        assert_eq!(folder_name(&label), "P1-North Ridge-3-overview");
    }

    // ── decode_qr end-to-end against a real generated QR+JPEG ────────────

    fn make_qr_jpeg(payload: &LabelPayload) -> Vec<u8> {
        let json = serde_json::to_string(payload).unwrap();
        let code = qrcode::QrCode::with_error_correction_level(&json, qrcode::EcLevel::H).unwrap();
        let image = code
            .render::<image::Luma<u8>>()
            .min_dimensions(400, 400)
            .build();
        let mut buf = Vec::new();
        image::DynamicImage::ImageLuma8(image)
            .write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Jpeg)
            .unwrap();
        buf
    }

    #[test]
    fn decode_qr_reads_back_a_real_label_payload() {
        let label = LabelPayload {
            project: "P1".into(),
            location: "North Ridge".into(),
            id: 7,
            name: "overview".into(),
            timestamp_utc: "2026-11-26T00:00:00Z".into(),
            extra: BTreeMap::from([("Surveyor".to_string(), "J. Price".to_string())]),
        };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("marker.jpg");
        fs::write(&path, make_qr_jpeg(&label)).unwrap();

        assert_eq!(decode_qr(&path, false), Some(label));
    }

    #[test]
    fn decode_qr_returns_none_for_a_photo_without_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plain.jpg");
        let image = image::RgbImage::new(64, 64);
        image.save(&path).unwrap();

        assert_eq!(decode_qr(&path, false), None);
    }

    // ── grouping/moving ────────────────────────────────────────────────────

    #[test]
    fn finish_group_moves_members_and_writes_metadata_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker.jpg");
        let member = dir.path().join("member.jpg");
        fs::write(&marker, b"marker").unwrap();
        fs::write(&member, b"member").unwrap();

        let label = LabelPayload {
            project: "P1".into(),
            location: "North Ridge".into(),
            id: 3,
            name: "overview".into(),
            timestamp_utc: "2026-11-26T00:00:00Z".into(),
            extra: BTreeMap::new(),
        };
        let mut group = Group::new(label.clone(), marker.clone());
        group.members.push(member.clone());

        finish_group(group, dir.path(), false).unwrap();

        let dest = dir.path().join("P1-North Ridge-3-overview");
        assert!(dest.join("marker.jpg").is_file());
        assert!(dest.join("member.jpg").is_file());
        assert!(!marker.exists());
        assert!(!member.exists());

        let yaml = fs::read_to_string(dest.join("metadata.yaml")).unwrap();
        let roundtripped: LabelPayload = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(roundtripped, label);
    }

    #[test]
    fn finish_group_dry_run_does_not_touch_the_filesystem() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker.jpg");
        fs::write(&marker, b"marker").unwrap();

        let label = LabelPayload {
            project: "P1".into(),
            location: "North Ridge".into(),
            id: 3,
            name: "overview".into(),
            timestamp_utc: "2026-11-26T00:00:00Z".into(),
            extra: BTreeMap::new(),
        };
        let group = Group::new(label, marker.clone());

        finish_group(group, dir.path(), true).unwrap();

        assert!(marker.exists());
        assert!(!dir.path().join("P1-North Ridge-3-overview").exists());
    }

    #[test]
    fn finish_group_refuses_to_overwrite_an_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker.jpg");
        fs::write(&marker, b"marker").unwrap();
        fs::create_dir(dir.path().join("P1-North Ridge-3-overview")).unwrap();

        let label = LabelPayload {
            project: "P1".into(),
            location: "North Ridge".into(),
            id: 3,
            name: "overview".into(),
            timestamp_utc: "2026-11-26T00:00:00Z".into(),
            extra: BTreeMap::new(),
        };
        let group = Group::new(label, marker.clone());

        assert!(finish_group(group, dir.path(), false).is_err());
        assert!(marker.exists(), "marker must not be moved on failure");
    }

    // ── full pipeline ────────────────────────────────────────────────────

    fn write_with_mtime(path: &Path, bytes: &[u8], seconds_from_epoch: u64) {
        fs::write(path, bytes).unwrap();
        let time = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds_from_epoch);
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(time)
            .unwrap();
    }

    fn label(id: u64, name: &str) -> LabelPayload {
        LabelPayload {
            project: "P1".into(),
            location: "North Ridge".into(),
            id,
            name: name.into(),
            timestamp_utc: "2026-11-26T00:00:00Z".into(),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn run_splits_a_directory_into_multiple_time_ordered_groups() {
        let dir = tempfile::tempdir().unwrap();

        // Deliberately out-of-alphabetical-order filenames, with explicit
        // mtimes, so the test actually proves time-based ordering rather
        // than coincidentally matching a lexical sort.
        write_with_mtime(&dir.path().join("z_stray.jpg"), b"stray", 0);
        write_with_mtime(
            &dir.path().join("a_marker1.jpg"),
            &make_qr_jpeg(&label(1, "first")),
            10,
        );
        write_with_mtime(&dir.path().join("y_member1a.jpg"), b"member", 20);
        write_with_mtime(&dir.path().join("x_member1b.jpg"), b"member", 30);
        write_with_mtime(
            &dir.path().join("b_marker2.jpg"),
            &make_qr_jpeg(&label(2, "second")),
            40,
        );
        write_with_mtime(&dir.path().join("w_member2a.jpg"), b"member", 50);

        run(&Args {
            dir: dir.path().to_path_buf(),
            dry_run: false,
            fast: false,
        })
        .unwrap();

        // Stray file before the first marker is left untouched.
        assert!(dir.path().join("z_stray.jpg").is_file());

        let group1 = dir.path().join("P1-North Ridge-1-first");
        assert!(group1.join("a_marker1.jpg").is_file());
        assert!(group1.join("y_member1a.jpg").is_file());
        assert!(group1.join("x_member1b.jpg").is_file());
        assert!(!group1.join("w_member2a.jpg").exists());
        let yaml1: LabelPayload =
            serde_yaml::from_str(&fs::read_to_string(group1.join("metadata.yaml")).unwrap())
                .unwrap();
        assert_eq!(yaml1, label(1, "first"));

        let group2 = dir.path().join("P1-North Ridge-2-second");
        assert!(group2.join("b_marker2.jpg").is_file());
        assert!(group2.join("w_member2a.jpg").is_file());
        let yaml2: LabelPayload =
            serde_yaml::from_str(&fs::read_to_string(group2.join("metadata.yaml")).unwrap())
                .unwrap();
        assert_eq!(yaml2, label(2, "second"));
    }
}

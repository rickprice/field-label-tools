use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use clap::Parser;
use exif::{In, Tag, Value};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

mod libraw_sys;
mod raw_decode;
mod xmp;

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
    /// moved out of here into a new
    /// `<Project>/<Location>/<Name>/<yyyy-mm-dd>/` directory (the date
    /// comes from the marker's embedded timestamp; `_<ID>` is appended to
    /// the date only if two shoots collide on the same day) once the next
    /// marker (or the end of the directory) is found, so partially-processed
    /// shoots are never left half-moved.
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

    /// Write a <file>.xmp sidecar for every photo with Project, Location,
    /// Name, and any extra fields as Darktable hierarchical tags under this
    /// prefix (e.g. `--tag-prefix PricePixel` tags a photo
    /// `PricePixel|Project|...`). Overrides --config and the default
    /// config file.
    #[arg(long)]
    tag_prefix: Option<String>,

    /// TOML config file (just `tag_prefix = "..."` for now) to read the tag
    /// prefix from instead of the default config file at
    /// ~/.config/photo-splitter/config.toml. Unlike the default path, a
    /// --config path that doesn't exist or doesn't parse is a hard error,
    /// since it was asked for explicitly.
    #[arg(long)]
    config: Option<PathBuf>,
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
    let tag_prefix = xmp::resolve_tag_prefix(args.tag_prefix.as_deref(), args.config.as_deref())?;
    if tag_prefix.is_none() {
        log::debug!(
            "no tag prefix configured (--tag-prefix / --config / default config file); not writing XMP sidecars"
        );
    }

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

    // Decoding (the expensive step -- LibRaw's dcraw_process per RAW file)
    // is independent per file, so it's done in parallel across all of
    // them first. Grouping, right below, depends on seeing markers in
    // time order and stays strictly sequential.
    let decoded: Vec<Option<LabelPayload>> = entries
        .par_iter()
        .map(|path| decode_qr(path, args.fast))
        .collect();

    let mut current: Option<Group> = None;
    let mut unmatched = 0usize;
    let mut groups_written = 0usize;

    for (path, label) in entries.into_iter().zip(decoded) {
        match label {
            Some(label) => {
                if let Some(group) = current.take() {
                    finish_group(group, &args.dir, args.dry_run, tag_prefix.as_deref())?;
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
        finish_group(group, &args.dir, args.dry_run, tag_prefix.as_deref())?;
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
    dump_qr_debug_image(path, &gray);

    // A light blur before detection dramatically cuts rqrr's candidate-match
    // cost on noisy/high-ISO photos: fine-grained sensor noise (especially
    // in dark, near-black frames) creates huge numbers of spurious
    // finder-pattern-like transitions for rqrr to individually check and
    // reject, while a real QR's large, high-contrast, structured pattern
    // survives a mild blur without issue. Confirmed against a real
    // pathological file (a near-black, high-ISO 12800 shot): detection went
    // from 90+ seconds to ~150ms, with no change in detection results
    // across 21 real test photos (same groups found, same markers).
    let gray = image::imageops::blur(&gray, 1.0);

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

/// With `RUST_LOG=trace`, save the exact image handed to the QR detector
/// for each photo into a `_qr_debug/` folder next to it -- useful for
/// seeing *why* a marker frame wasn't found (e.g. a RAW thumbnail too dark
/// or small to read, or a libraw decode that came out looking wrong).
/// Gated behind trace rather than debug since, unlike a log line, this
/// writes a file per photo.
fn dump_qr_debug_image(path: &Path, image: &image::GrayImage) {
    if !log::log_enabled!(log::Level::Trace) {
        return;
    }
    match write_qr_debug_image(path, image) {
        Ok(dump_path) => log::trace!(
            "{}: wrote the image scanned for a QR code to {}",
            path.display(),
            dump_path.display()
        ),
        Err(err) => log::debug!("{}: could not write QR debug dump: {err}", path.display()),
    }
}

fn write_qr_debug_image(path: &Path, image: &image::GrayImage) -> Result<PathBuf> {
    let parent = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    let file_name = path
        .file_name()
        .with_context(|| format!("{} has no file name", path.display()))?;

    let debug_dir = parent.join("_qr_debug");
    fs::create_dir_all(&debug_dir).with_context(|| format!("creating {}", debug_dir.display()))?;

    let dump_path = debug_dir.join(format!("{}.jpg", file_name.to_string_lossy()));
    image
        .save(&dump_path)
        .with_context(|| format!("writing {}", dump_path.display()))?;
    Ok(dump_path)
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

/// The date portion of `timestamp_utc` (the embedded QR timestamp, not the
/// photo's own EXIF capture time), formatted `yyyy-mm-dd`. Falls back to a
/// clearly-labeled placeholder rather than panicking if a payload somehow
/// carries a malformed timestamp.
fn group_date(label: &LabelPayload) -> String {
    chrono::DateTime::parse_from_rfc3339(&label.timestamp_utc)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|_| "unknown-date".to_string())
}

/// `<dir>/<Project>/<Location>/<Name>/<yyyy-mm-dd>`, optionally with the
/// group's ID appended to the date component (`<yyyy-mm-dd>_<ID>`) to
/// disambiguate two shoots that share Project/Location/Name and fall on the
/// same day -- see `finish_group`, which only adds the suffix when the
/// plain date path is already taken. Underscore rather than hyphen so it
/// doesn't read as part of the date itself.
fn group_dir(dir: &Path, label: &LabelPayload, disambiguate_with_id: bool) -> PathBuf {
    let date = group_date(label);
    let date_component = if disambiguate_with_id {
        format!("{date}_{}", label.id)
    } else {
        date
    };
    dir.join(sanitize(&label.project))
        .join(sanitize(&label.location))
        .join(sanitize(&label.name))
        .join(date_component)
}

fn finish_group(group: Group, dir: &Path, dry_run: bool, tag_prefix: Option<&str>) -> Result<()> {
    let plain_dest = group_dir(dir, &group.label, false);
    let dest = if plain_dest.exists() {
        group_dir(dir, &group.label, true)
    } else {
        plain_dest
    };

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

    fs::create_dir_all(&dest).with_context(|| format!("creating {}", dest.display()))?;

    for member in &group.members {
        let file_name = member
            .file_name()
            .with_context(|| format!("{} has no file name", member.display()))?;
        let target = dest.join(file_name);
        fs::rename(member, &target)
            .with_context(|| format!("moving {} to {}", member.display(), target.display()))?;

        if let Some(prefix) = tag_prefix {
            xmp::write_xmp_sidecar(&target, &group.label, prefix)
                .with_context(|| format!("writing XMP sidecar for {}", target.display()))?;
        }
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

    // ── sanitize / group_dir ──────────────────────────────────────────────

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

    fn sample_label() -> LabelPayload {
        LabelPayload {
            project: "P1".into(),
            location: "North Ridge".into(),
            id: 3,
            name: "overview".into(),
            timestamp_utc: "2026-11-26T14:30:00Z".into(),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn group_date_takes_just_the_date_portion_of_timestamp_utc() {
        assert_eq!(group_date(&sample_label()), "2026-11-26");
    }

    #[test]
    fn group_date_falls_back_for_a_malformed_timestamp() {
        let mut label = sample_label();
        label.timestamp_utc = "not a timestamp".into();
        assert_eq!(group_date(&label), "unknown-date");
    }

    #[test]
    fn group_dir_nests_by_project_location_name_then_date() {
        let dir = Path::new("/photos");
        assert_eq!(
            group_dir(dir, &sample_label(), false),
            dir.join("P1")
                .join("North Ridge")
                .join("overview")
                .join("2026-11-26")
        );
    }

    #[test]
    fn group_dir_appends_id_to_the_date_only_when_disambiguating() {
        let dir = Path::new("/photos");
        assert_eq!(
            group_dir(dir, &sample_label(), true),
            dir.join("P1")
                .join("North Ridge")
                .join("overview")
                .join("2026-11-26_3")
        );
    }

    // ── real-camera fixtures (tests/fixtures/) ────────────────────────────
    //
    // Two files from an actual Canon EOS 5D Mark IV, kept deliberately
    // minimal and privacy-reviewed before committing:
    //   - near-black-no-marker.CR2: a genuine RAW file, confirmed to have no
    //     identifiable content (a near-black closet shot), used to regression
    //     -test both correctness (no marker) and the half_size+blur speed fix
    //     -- this exact file took 90+ seconds to decode before the blur was
    //     added (see decode_qr's comment), because its heavy ISO-12800 sensor
    //     noise produced huge numbers of spurious QR-candidate matches.
    //   - real-camera-marker.jpg: a genuine label-qr-gui QR code, as actually
    //     photographed and demosaiced, cropped down to just the QR + monitor
    //     bezel (desk/screen content removed) -- gives real-world lens
    //     distortion/lighting/JPEG-artifact coverage a synthetically
    //     rendered QR (see make_qr_jpeg below) doesn't.

    fn fixture_path(name: &str) -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures")).join(name)
    }

    #[test]
    fn decode_qr_on_real_near_black_raw_finds_no_marker_quickly() {
        let path = fixture_path("near-black-no-marker.CR2");
        let start = std::time::Instant::now();
        let result = decode_qr(&path, false);
        let elapsed = start.elapsed();

        assert_eq!(result, None);
        assert!(
            elapsed < std::time::Duration::from_secs(15),
            "decode took {elapsed:?}, expected well under 15s -- the whole point of \
             this fixture is to catch a regression back to the 90+s this used to take"
        );
    }

    #[test]
    fn decode_qr_reads_a_real_camera_captured_marker() {
        let path = fixture_path("real-camera-marker.jpg");
        let label = decode_qr(&path, false).expect("real camera marker must decode");

        assert_eq!(label.project, "Test QR Code generation");
        assert_eq!(label.location, "Home Office");
        assert_eq!(label.id, 1);
        assert_eq!(label.name, "Testing");
        assert_eq!(label.timestamp_utc, "2026-11-26T02:09:58Z");
        assert!(label.extra.is_empty());
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

    // ── QR debug dump ────────────────────────────────────────────────────

    #[test]
    fn write_qr_debug_image_writes_into_a_sibling_qr_debug_dir() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("IMG_0001.CR2");
        fs::write(&path, b"not a real raw file").unwrap();
        let image = image::GrayImage::new(8, 8);

        let dump_path = write_qr_debug_image(&path, &image).unwrap();

        assert_eq!(
            dump_path,
            dir.path().join("_qr_debug").join("IMG_0001.CR2.jpg")
        );
        assert!(dump_path.is_file());
    }

    #[test]
    fn write_qr_debug_image_reuses_an_existing_qr_debug_dir() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("_qr_debug")).unwrap();
        let path = dir.path().join("IMG_0002.CR2");
        fs::write(&path, b"not a real raw file").unwrap();
        let image = image::GrayImage::new(8, 8);

        assert!(write_qr_debug_image(&path, &image).is_ok());
    }

    // ── grouping/moving ────────────────────────────────────────────────────

    #[test]
    fn finish_group_moves_members_and_writes_metadata_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker.jpg");
        let member = dir.path().join("member.jpg");
        fs::write(&marker, b"marker").unwrap();
        fs::write(&member, b"member").unwrap();

        let label = sample_label();
        let mut group = Group::new(label.clone(), marker.clone());
        group.members.push(member.clone());

        finish_group(group, dir.path(), false, None).unwrap();

        let dest = group_dir(dir.path(), &label, false);
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

        let label = sample_label();
        let group = Group::new(label.clone(), marker.clone());

        finish_group(group, dir.path(), true, None).unwrap();

        assert!(marker.exists());
        assert!(!group_dir(dir.path(), &label, false).exists());
    }

    #[test]
    fn finish_group_disambiguates_with_id_when_the_plain_date_is_taken() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker.jpg");
        fs::write(&marker, b"marker").unwrap();

        let label = sample_label();
        // Simulate an earlier, unrelated shoot already occupying the plain
        // <project>/<location>/<name>/<yyyy-mm-dd> path.
        fs::create_dir_all(group_dir(dir.path(), &label, false)).unwrap();

        let group = Group::new(label.clone(), marker.clone());
        finish_group(group, dir.path(), false, None).unwrap();

        let dest = group_dir(dir.path(), &label, true);
        assert!(dest.join("marker.jpg").is_file());
    }

    #[test]
    fn finish_group_refuses_to_overwrite_when_both_plain_and_id_suffixed_exist() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker.jpg");
        fs::write(&marker, b"marker").unwrap();

        let label = sample_label();
        fs::create_dir_all(group_dir(dir.path(), &label, false)).unwrap();
        fs::create_dir_all(group_dir(dir.path(), &label, true)).unwrap();

        let group = Group::new(label, marker.clone());

        assert!(finish_group(group, dir.path(), false, None).is_err());
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
            tag_prefix: None,
            config: None,
        })
        .unwrap();

        // Stray file before the first marker is left untouched.
        assert!(dir.path().join("z_stray.jpg").is_file());

        let group1 = group_dir(dir.path(), &label(1, "first"), false);
        assert!(group1.join("a_marker1.jpg").is_file());
        assert!(group1.join("y_member1a.jpg").is_file());
        assert!(group1.join("x_member1b.jpg").is_file());
        assert!(!group1.join("w_member2a.jpg").exists());
        let yaml1: LabelPayload =
            serde_yaml::from_str(&fs::read_to_string(group1.join("metadata.yaml")).unwrap())
                .unwrap();
        assert_eq!(yaml1, label(1, "first"));

        let group2 = group_dir(dir.path(), &label(2, "second"), false);
        assert!(group2.join("b_marker2.jpg").is_file());
        assert!(group2.join("w_member2a.jpg").is_file());
        let yaml2: LabelPayload =
            serde_yaml::from_str(&fs::read_to_string(group2.join("metadata.yaml")).unwrap())
                .unwrap();
        assert_eq!(yaml2, label(2, "second"));
    }

    #[test]
    fn run_disambiguates_same_day_same_name_groups_with_id() {
        let dir = tempfile::tempdir().unwrap();

        // Two markers sharing Project/Location/Name and the same day --
        // exactly the real-world case (two "Test2" shoots minutes apart)
        // that motivated appending the ID only when it's actually needed.
        write_with_mtime(
            &dir.path().join("a_marker1.jpg"),
            &make_qr_jpeg(&label(1, "same-name")),
            10,
        );
        write_with_mtime(
            &dir.path().join("b_marker2.jpg"),
            &make_qr_jpeg(&label(2, "same-name")),
            20,
        );

        run(&Args {
            dir: dir.path().to_path_buf(),
            dry_run: false,
            fast: false,
            tag_prefix: None,
            config: None,
        })
        .unwrap();

        let group1 = group_dir(dir.path(), &label(1, "same-name"), false);
        let group2 = group_dir(dir.path(), &label(2, "same-name"), true);
        assert!(group1.join("a_marker1.jpg").is_file());
        assert!(group2.join("b_marker2.jpg").is_file());
    }
}

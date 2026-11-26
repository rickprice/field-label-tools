//! Config loading (tag prefix) and XMP sidecar generation.
//!
//! Darktable represents hierarchical tags across two XMP fields together,
//! confirmed against a real Darktable-written sidecar (a tag named
//! "PricePixel - 5D MKIV Default" under `darktable|style|...`):
//!   - `dc:subject`: a flat, deduplicated bag of every individual path
//!     *segment* appearing anywhere in the tag tree (not full paths) --
//!     e.g. `darktable|style|Foo` contributes "darktable", "style", and
//!     "Foo" as three separate flat entries, each only once even if the
//!     same segment is shared by other hierarchical tags too.
//!   - `lr:hierarchicalSubject`: the actual pipe-joined full paths
//!     (`darktable|style|Foo`) that reconstruct the tree.
//!
//! This writer only emits those two fields (plus the standard XMP/RDF
//! wrapper) -- no darktable:xmp_version/history_end/etc., since these
//! sidecars are written for images that have never been processed by
//! Darktable at all. This relies on Darktable's import reading foreign/
//! minimal XMP sidecars for metadata the same way it reads e.g.
//! Lightroom-originated ones (no edit history, since there isn't any) --
//! verify this against a real Darktable import before trusting it broadly;
//! it isn't something that could be tested without a running Darktable.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::LabelPayload;
use crate::video;

#[derive(Deserialize, Default)]
struct ConfigFile {
    tag_prefix: Option<String>,
    default_client: Option<String>,
    default_project: Option<String>,
    default_location: Option<String>,
    default_name: Option<String>,
    video_scan_window_secs: Option<u64>,
    video_scan_interval_secs: Option<u64>,
}

impl From<ConfigFile> for video::ScanConfig {
    fn from(config: ConfigFile) -> Self {
        let defaults = video::ScanConfig::default();
        Self {
            window: config
                .video_scan_window_secs
                .map(Duration::from_secs)
                .unwrap_or(defaults.window),
            interval: config
                .video_scan_interval_secs
                .map(Duration::from_secs)
                .unwrap_or(defaults.interval),
        }
    }
}

/// Configured fallback values for blank Client/Project/Location/Name
/// fields, read from the same config file as `tag_prefix`. There's no
/// CLI-flag override for these, unlike `tag_prefix` -- they're meant to be
/// set once per install (e.g. a single client/project that's always the
/// same), not vary per invocation.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct FieldDefaults {
    pub client: Option<String>,
    pub project: Option<String>,
    pub location: Option<String>,
    pub name: Option<String>,
}

impl From<ConfigFile> for FieldDefaults {
    fn from(config: ConfigFile) -> Self {
        Self {
            client: config.default_client,
            project: config.default_project,
            location: config.default_location,
            name: config.default_name,
        }
    }
}

impl FieldDefaults {
    /// Fill in any blank (empty or whitespace-only) built-in field on
    /// `label` with its configured default, leaving it blank if none is
    /// configured. `hierarchical_tags` already omits still-blank fields
    /// from the XMP sidecar, so "no default configured" and "omitted from
    /// the sidecar" fall out of the same blank check for free.
    pub fn apply(&self, label: &mut LabelPayload) {
        Self::apply_one(&mut label.client, self.client.as_deref());
        Self::apply_one(&mut label.project, self.project.as_deref());
        Self::apply_one(&mut label.location, self.location.as_deref());
        Self::apply_one(&mut label.name, self.name.as_deref());
    }

    fn apply_one(field: &mut String, default: Option<&str>) {
        if field.trim().is_empty() {
            if let Some(default) = default {
                *field = default.to_string();
            }
        }
    }
}

fn default_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("photo-splitter").join("config.toml"))
}

/// Loads the config file per the shared precedence every `resolve_*`
/// function below uses: an explicit `--config <path>` (hard error if
/// missing/invalid, since it was asked for by name) or else the default
/// `~/.config/photo-splitter/config.toml` (silently all-defaults if
/// absent -- most users haven't set one up).
fn resolve_config_file(
    config_arg: Option<&Path>,
    default_path: Option<PathBuf>,
) -> Result<ConfigFile> {
    if let Some(path) = config_arg {
        return load_config_file(path)
            .with_context(|| format!("loading config file {}", path.display()));
    }

    let Some(path) = default_path else {
        return Ok(ConfigFile::default());
    };
    if !path.is_file() {
        return Ok(ConfigFile::default());
    }
    load_config_file(&path).with_context(|| format!("loading config file {}", path.display()))
}

fn load_config_file(path: &Path) -> Result<ConfigFile> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Resolve the tag prefix to use, in order: `--tag-prefix`, then
/// `--config <path>` (hard error if that explicit path is missing/invalid --
/// the user asked for it specifically), then the default
/// `~/.config/photo-splitter/config.toml` (silently absent if not found --
/// most users haven't set one up). Returns `None` if nothing resolves a
/// prefix, in which case no XMP sidecars are written at all.
pub fn resolve_tag_prefix(
    tag_prefix_arg: Option<&str>,
    config_arg: Option<&Path>,
) -> Result<Option<String>> {
    resolve_tag_prefix_from(tag_prefix_arg, config_arg, default_config_path())
}

fn resolve_tag_prefix_from(
    tag_prefix_arg: Option<&str>,
    config_arg: Option<&Path>,
    default_path: Option<PathBuf>,
) -> Result<Option<String>> {
    if let Some(prefix) = tag_prefix_arg {
        return Ok(Some(prefix.to_string()));
    }
    Ok(resolve_config_file(config_arg, default_path)?.tag_prefix)
}

/// Resolve configured per-field defaults, from the same config file and
/// precedence as `resolve_tag_prefix`: an explicit `--config <path>` (hard
/// error if missing/invalid) or else the default
/// `~/.config/photo-splitter/config.toml` (silently empty if absent).
pub fn resolve_field_defaults(config_arg: Option<&Path>) -> Result<FieldDefaults> {
    resolve_field_defaults_from(config_arg, default_config_path())
}

fn resolve_field_defaults_from(
    config_arg: Option<&Path>,
    default_path: Option<PathBuf>,
) -> Result<FieldDefaults> {
    Ok(resolve_config_file(config_arg, default_path)?.into())
}

/// Resolve the video QR-marker scan window/interval, from the same config
/// file and precedence as `resolve_tag_prefix`. There's no CLI-flag
/// override, same reasoning as `resolve_field_defaults` -- this is a
/// once-per-install setting, not a per-run one.
pub fn resolve_video_scan_config(config_arg: Option<&Path>) -> Result<video::ScanConfig> {
    resolve_video_scan_config_from(config_arg, default_config_path())
}

fn resolve_video_scan_config_from(
    config_arg: Option<&Path>,
    default_path: Option<PathBuf>,
) -> Result<video::ScanConfig> {
    Ok(resolve_config_file(config_arg, default_path)?.into())
}

/// Hierarchical tags (`{prefix}|Client|...`, `{prefix}|Project|...`,
/// `{prefix}|Location|...`, `{prefix}|Name|...`, plus one per `extra`
/// field) for one label. A field whose value is blank (empty or
/// whitespace-only) is skipped entirely -- e.g. `client` on a QR payload
/// captured before that field existed -- rather than emitting a bare
/// `{prefix}|Client|` tag that would just clutter Darktable's tag tree
/// with an empty leaf. ID and timestamp_utc are deliberately not tagged at
/// all -- they're bookkeeping/disambiguation data (ID especially: it's
/// otherwise invisible in the output path), not the kind of thing you'd
/// browse/filter photos by. Both are still in metadata.yaml for anyone who
/// needs them.
fn hierarchical_tags(label: &LabelPayload, tag_prefix: &str) -> Vec<String> {
    let built_in = [
        ("Client", label.client.as_str()),
        ("Project", label.project.as_str()),
        ("Location", label.location.as_str()),
        ("Name", label.name.as_str()),
    ];

    let mut tags = Vec::new();
    for (field, value) in built_in {
        if !value.trim().is_empty() {
            tags.push(format!("{tag_prefix}|{field}|{value}"));
        }
    }
    for (key, value) in &label.extra {
        if !value.trim().is_empty() {
            tags.push(format!("{tag_prefix}|{key}|{value}"));
        }
    }
    tags
}

/// The flat `dc:subject` set: every unique path segment across all of
/// `hierarchical_tags`, in first-seen order.
fn flat_tags(hierarchical: &[String]) -> Vec<String> {
    let mut flat: Vec<String> = Vec::new();
    for tag in hierarchical {
        for segment in tag.split('|') {
            if !flat.iter().any(|s| s == segment) {
                flat.push(segment.to_string());
            }
        }
    }
    flat
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn render_xmp(flat: &[String], hierarchical: &[String]) -> String {
    let flat_items: String = flat
        .iter()
        .map(|t| format!("     <rdf:li>{}</rdf:li>\n", xml_escape(t)))
        .collect();
    let hierarchical_items: String = hierarchical
        .iter()
        .map(|t| format!("     <rdf:li>{}</rdf:li>\n", xml_escape(t)))
        .collect();

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/" x:xmptk="field-label-tools photo-splitter">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:dc="http://purl.org/dc/elements/1.1/"
    xmlns:lr="http://ns.adobe.com/lightroom/1.0/">
   <dc:subject>
    <rdf:Bag>
{flat_items}    </rdf:Bag>
   </dc:subject>
   <lr:hierarchicalSubject>
    <rdf:Bag>
{hierarchical_items}    </rdf:Bag>
   </lr:hierarchicalSubject>
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
"#
    )
}

/// Darktable's sidecar convention: `<filename>.xmp` next to the image,
/// keeping the original extension (e.g. `IMG_0001.CR2` ->
/// `IMG_0001.CR2.xmp`), matching the sibling darktable-import-xmp tool.
fn sidecar_path(image_path: &Path) -> PathBuf {
    let mut os = image_path.as_os_str().to_os_string();
    os.push(".xmp");
    PathBuf::from(os)
}

/// Write `<image_path>.xmp` with `label`'s metadata as hierarchical tags
/// under `tag_prefix`. Overwrites any existing sidecar at that path.
pub fn write_xmp_sidecar(image_path: &Path, label: &LabelPayload, tag_prefix: &str) -> Result<()> {
    let hierarchical = hierarchical_tags(label, tag_prefix);
    let flat = flat_tags(&hierarchical);
    let xmp = render_xmp(&flat, &hierarchical);

    let path = sidecar_path(image_path);
    fs::write(&path, xmp).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn label() -> LabelPayload {
        LabelPayload {
            client: "Acme Co".into(),
            project: "P1".into(),
            location: "North Ridge".into(),
            id: 3,
            name: "overview".into(),
            timestamp_utc: "2026-11-26T00:00:00Z".into(),
            extra: BTreeMap::from([("Surveyor".to_string(), "J. Price".to_string())]),
        }
    }

    #[test]
    fn hierarchical_tags_cover_client_project_location_name_and_extra() {
        let tags = hierarchical_tags(&label(), "PricePixel");
        assert_eq!(
            tags,
            vec![
                "PricePixel|Client|Acme Co",
                "PricePixel|Project|P1",
                "PricePixel|Location|North Ridge",
                "PricePixel|Name|overview",
                "PricePixel|Surveyor|J. Price",
            ]
        );
    }

    #[test]
    fn hierarchical_tags_skips_blank_built_in_and_extra_fields() {
        let mut label = label();
        label.client = String::new();
        label.name = "   ".into(); // whitespace-only counts as blank too
        label.extra = BTreeMap::from([
            ("Surveyor".to_string(), "J. Price".to_string()),
            ("Notes".to_string(), String::new()),
        ]);

        let tags = hierarchical_tags(&label, "PricePixel");
        assert_eq!(
            tags,
            vec![
                "PricePixel|Project|P1",
                "PricePixel|Location|North Ridge",
                "PricePixel|Surveyor|J. Price",
            ]
        );
    }

    #[test]
    fn flat_tags_are_deduplicated_segments_in_first_seen_order() {
        let hierarchical = vec![
            "PricePixel|Project|P1".to_string(),
            "PricePixel|Location|North Ridge".to_string(),
        ];
        assert_eq!(
            flat_tags(&hierarchical),
            vec!["PricePixel", "Project", "P1", "Location", "North Ridge"]
        );
    }

    #[test]
    fn xml_escape_handles_the_three_special_characters() {
        assert_eq!(xml_escape("A & B <C> D"), "A &amp; B &lt;C&gt; D");
    }

    #[test]
    fn sidecar_path_appends_xmp_keeping_the_original_extension() {
        assert_eq!(
            sidecar_path(Path::new("/x/IMG_0001.CR2")),
            PathBuf::from("/x/IMG_0001.CR2.xmp")
        );
    }

    #[test]
    fn write_xmp_sidecar_produces_both_tag_fields() {
        let dir = tempfile::tempdir().unwrap();
        let image_path = dir.path().join("marker.jpg");
        fs::write(&image_path, b"fake image").unwrap();

        write_xmp_sidecar(&image_path, &label(), "PricePixel").unwrap();

        let xmp = fs::read_to_string(dir.path().join("marker.jpg.xmp")).unwrap();
        assert!(xmp.contains("<dc:subject>"));
        assert!(xmp.contains("<lr:hierarchicalSubject>"));
        assert!(xmp.contains("<rdf:li>PricePixel|Client|Acme Co</rdf:li>"));
        assert!(xmp.contains("<rdf:li>PricePixel|Project|P1</rdf:li>"));
        assert!(xmp.contains("<rdf:li>PricePixel</rdf:li>"));
        assert!(xmp.contains("<rdf:li>P1</rdf:li>"));
        // label()'s extra field (a stand-in for a label-qr-gui --config
        // custom field, e.g. "Surveyor") must make it into both XMP fields
        // too, not just the four built-in ones.
        assert!(xmp.contains("<rdf:li>PricePixel|Surveyor|J. Price</rdf:li>"));
        assert!(xmp.contains("<rdf:li>Surveyor</rdf:li>"));
        assert!(xmp.contains("<rdf:li>J. Price</rdf:li>"));
    }

    #[test]
    fn resolve_tag_prefix_cli_flag_wins_over_everything() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(&config_path, "tag_prefix = \"FromFile\"\n").unwrap();

        let resolved = resolve_tag_prefix(Some("FromCli"), Some(&config_path)).unwrap();
        assert_eq!(resolved, Some("FromCli".to_string()));
    }

    #[test]
    fn resolve_tag_prefix_reads_an_explicit_config_path() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(&config_path, "tag_prefix = \"FromFile\"\n").unwrap();

        let resolved = resolve_tag_prefix(None, Some(&config_path)).unwrap();
        assert_eq!(resolved, Some("FromFile".to_string()));
    }

    #[test]
    fn resolve_tag_prefix_errors_on_a_missing_explicit_config_path() {
        let missing = Path::new("/nonexistent/config.toml");
        assert!(resolve_tag_prefix(None, Some(missing)).is_err());
    }

    #[test]
    fn resolve_tag_prefix_from_is_none_when_the_default_path_does_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        let default_path = dir.path().join("does-not-exist.toml");

        let resolved = resolve_tag_prefix_from(None, None, Some(default_path)).unwrap();
        assert_eq!(resolved, None);
    }

    #[test]
    fn resolve_tag_prefix_from_reads_the_default_path_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let default_path = dir.path().join("config.toml");
        fs::write(&default_path, "tag_prefix = \"FromDefault\"\n").unwrap();

        let resolved = resolve_tag_prefix_from(None, None, Some(default_path)).unwrap();
        assert_eq!(resolved, Some("FromDefault".to_string()));
    }

    #[test]
    fn field_defaults_apply_fills_only_blank_fields() {
        let defaults = FieldDefaults {
            client: Some("Acme Co".to_string()),
            project: Some("Default Project".to_string()),
            location: None,
            name: None,
        };
        let mut label = label();
        label.client = String::new();
        label.project = "Explicit Project".into(); // already set, must not be overwritten
        label.location = "   ".into(); // whitespace-only counts as blank, but no default configured

        defaults.apply(&mut label);

        assert_eq!(label.client, "Acme Co");
        assert_eq!(label.project, "Explicit Project");
        assert_eq!(label.location, "   "); // left as-is: no default_location configured
    }

    #[test]
    fn resolve_field_defaults_reads_an_explicit_config_path() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(
            &config_path,
            "default_client = \"Acme Co\"\ndefault_project = \"Default Project\"\n",
        )
        .unwrap();

        let resolved = resolve_field_defaults(Some(&config_path)).unwrap();
        assert_eq!(
            resolved,
            FieldDefaults {
                client: Some("Acme Co".to_string()),
                project: Some("Default Project".to_string()),
                location: None,
                name: None,
            }
        );
    }

    #[test]
    fn resolve_field_defaults_errors_on_a_missing_explicit_config_path() {
        let missing = Path::new("/nonexistent/config.toml");
        assert!(resolve_field_defaults(Some(missing)).is_err());
    }

    #[test]
    fn resolve_field_defaults_from_is_empty_when_the_default_path_does_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        let default_path = dir.path().join("does-not-exist.toml");

        let resolved = resolve_field_defaults_from(None, Some(default_path)).unwrap();
        assert_eq!(resolved, FieldDefaults::default());
    }

    #[test]
    fn resolve_field_defaults_from_reads_the_default_path_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let default_path = dir.path().join("config.toml");
        fs::write(&default_path, "default_location = \"North Ridge\"\n").unwrap();

        let resolved = resolve_field_defaults_from(None, Some(default_path)).unwrap();
        assert_eq!(
            resolved,
            FieldDefaults {
                client: None,
                project: None,
                location: Some("North Ridge".to_string()),
                name: None,
            }
        );
    }

    #[test]
    fn resolve_video_scan_config_reads_configured_values() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(
            &config_path,
            "video_scan_window_secs = 60\nvideo_scan_interval_secs = 2\n",
        )
        .unwrap();

        let resolved = resolve_video_scan_config(Some(&config_path)).unwrap();
        assert_eq!(
            resolved,
            video::ScanConfig {
                window: Duration::from_secs(60),
                interval: Duration::from_secs(2),
            }
        );
    }

    #[test]
    fn resolve_video_scan_config_from_falls_back_to_defaults_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let default_path = dir.path().join("does-not-exist.toml");

        let resolved = resolve_video_scan_config_from(None, Some(default_path)).unwrap();
        assert_eq!(resolved, video::ScanConfig::default());
    }

    #[test]
    fn resolve_video_scan_config_falls_back_to_default_interval_when_only_window_is_set() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        fs::write(&config_path, "video_scan_window_secs = 60\n").unwrap();

        let resolved = resolve_video_scan_config(Some(&config_path)).unwrap();
        assert_eq!(resolved.window, Duration::from_secs(60));
        assert_eq!(resolved.interval, video::ScanConfig::default().interval);
    }
}

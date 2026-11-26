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

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::LabelPayload;

#[derive(Deserialize, Default)]
struct ConfigFile {
    tag_prefix: Option<String>,
}

fn default_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("photo-splitter").join("config.toml"))
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

    if let Some(path) = config_arg {
        let config = load_config_file(path)
            .with_context(|| format!("loading config file {}", path.display()))?;
        return Ok(config.tag_prefix);
    }

    let Some(path) = default_path else {
        return Ok(None);
    };
    if !path.is_file() {
        return Ok(None);
    }
    let config = load_config_file(&path)
        .with_context(|| format!("loading config file {}", path.display()))?;
    Ok(config.tag_prefix)
}

fn load_config_file(path: &Path) -> Result<ConfigFile> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Hierarchical tags (`{prefix}|Project|...`, `{prefix}|Location|...`,
/// `{prefix}|Name|...`, plus one per `extra` field) for one label. ID and
/// timestamp_utc are deliberately not tagged -- they're bookkeeping/
/// disambiguation data (ID especially: it's otherwise invisible in the
/// output path), not the kind of thing you'd browse/filter photos by. Both
/// are still in metadata.yaml for anyone who needs them.
fn hierarchical_tags(label: &LabelPayload, tag_prefix: &str) -> Vec<String> {
    let mut tags = vec![
        format!("{tag_prefix}|Project|{}", label.project),
        format!("{tag_prefix}|Location|{}", label.location),
        format!("{tag_prefix}|Name|{}", label.name),
    ];
    for (key, value) in &label.extra {
        tags.push(format!("{tag_prefix}|{key}|{value}"));
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
            project: "P1".into(),
            location: "North Ridge".into(),
            id: 3,
            name: "overview".into(),
            timestamp_utc: "2026-11-26T00:00:00Z".into(),
            extra: BTreeMap::from([("Surveyor".to_string(), "J. Price".to_string())]),
        }
    }

    #[test]
    fn hierarchical_tags_cover_project_location_name_and_extra() {
        let tags = hierarchical_tags(&label(), "PricePixel");
        assert_eq!(
            tags,
            vec![
                "PricePixel|Project|P1",
                "PricePixel|Location|North Ridge",
                "PricePixel|Name|overview",
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
        assert!(xmp.contains("<rdf:li>PricePixel|Project|P1</rdf:li>"));
        assert!(xmp.contains("<rdf:li>PricePixel</rdf:li>"));
        assert!(xmp.contains("<rdf:li>P1</rdf:li>"));
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
}

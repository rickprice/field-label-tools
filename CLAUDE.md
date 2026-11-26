# CLAUDE.md

Guidance for Claude Code (or any AI coding assistant) working in this
repository.

## Architecture

```
label-qr-gui/      egui/eframe appliance. LabelApp holds the four
                    built-in fields plus `extra: Vec<(ExtraFieldDef,
                    String)>` loaded from an optional `--config` TOML
                    file (see fields.example.toml). `payload()`
                    serializes everything as JSON for the QR code
                    (always including a `timestamp_utc` captured fresh
                    on each draw), `folder_name()` previews the eventual
                    `<Project>-<Location>-<ID>-<Name>` directory name
                    from the four built-in fields only -- extra fields
                    never affect the folder name, only the QR's "extra"
                    map. draw_qr() renders QR modules directly with
                    egui::Painter (no image/raster step) so the code
                    stays crisp and purely black/white at any window
                    size.
photo-splitter/     scans a directory of time-ordered photos (EXIF
                    DateTimeOriginal, falling back to mtime), decodes
                    each QR-marker frame via rqrr, and moves each
                    group's files into a `<Project>-<Location>-<ID>-
                    <Name>/` folder with a metadata.yaml (the decoded
                    LabelPayload, serde_yaml). Each group is buffered
                    in `Group` and only moved (`finish_group`) once the
                    *next* marker or end-of-directory is reached --
                    never move a file before you know the group is
                    complete. RAW files are not decoded directly; their
                    EXIF IFD1 thumbnail (JPEGInterchangeFormat /
                    JPEGInterchangeFormatLength tags, read the same way
                    darktable-import-xmp's exif_meta.rs reads other EXIF
                    fields) is extracted and scanned instead -- see the
                    doc comment on load_grayscale_for_qr for why this is
                    a known weak point worth watching in practice.
darktable-export/   not yet implemented. Will convert those split
                    directories into XMP sidecars for the sibling
                    `darktable-import-xmp` repo to pick up — this crate
                    should not talk to Darktable's SQLite database
                    directly, that's already handled there.
```

## The QR payload is JSON, not the dash-joined string

`label-qr-gui` encodes the QR as a JSON object (`{"project":...,
"location":..., "id":..., "name":..., "timestamp_utc":...,
"extra":{...}}`), not as the
`<Project>-<Location>-<ID>-<Name>` string shown in the UI preview. JSON
round-trips safely even if a field itself contains a dash, and the
`"extra"` map is open-ended (field names come from whatever config file
was used at capture time). Keep this split (JSON on the wire,
dash-joined string only as a human-readable folder-name preview).
`photo-splitter`'s `LabelPayload.extra` is a plain
`BTreeMap<String, String>` deliberately -- it must treat keys as
unknown/arbitrary, never assume a fixed schema -- and the field is
absent entirely when no extra fields were configured, not just empty
(`#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]`
mirrors that on both sides).

`timestamp_utc` is not stored as app state -- it's computed fresh with
`Utc::now()` every time `payload()` runs. `ui()` calls
`ctx.request_repaint_after(Duration::from_secs(1))` unconditionally so
egui keeps redrawing (and thus recomputing the timestamp) even when the
form is left idle with no input, since the point is for the embedded
time to reflect whenever the screen is actually photographed, not
whenever it was last edited.

## Workflow notes

- `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D
  warnings`, and `cargo test` must all pass before committing. CI
  enforces the same three.
- `nix build` / `nix develop` should keep working for `label-qr-gui`;
  if you add a dependency, make sure `Cargo.lock` is committed so the
  Nix build stays reproducible.
- `photo-splitter`'s `metadata.yaml` mirrors the decoded QR payload
  verbatim (same `LabelPayload` struct, serialized with `serde_yaml`)
  rather than inventing a second schema for the same data.
- `photo-splitter` moves files with `fs::rename`, which only works
  within a single filesystem -- it does not fall back to copy+delete
  across filesystem boundaries. The destination folders are always
  created as siblings inside the same directory being scanned, so this
  is a non-issue unless that assumption changes.
- Before trusting `photo-splitter` on a real shoot with a new camera,
  run it with `--dry-run` and `RUST_LOG=debug` first, specifically to
  check whether that camera's RAW thumbnails are actually being found
  and decoded (see the `load_grayscale_for_qr` doc comment). If they
  turn out too small/absent in practice, the fix is a heavier
  RAW-preview crate (e.g. LibRaw bindings), not a workaround in this
  crate's existing EXIF-thumbnail path.

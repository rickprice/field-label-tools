# CLAUDE.md

Guidance for Claude Code (or any AI coding assistant) working in this
repository.

## Architecture

```
label-qr-gui/      egui/eframe appliance. LabelApp holds the four
                    built-in fields plus `extra: Vec<(ExtraFieldDef,
                    String)>` loaded from an optional `--config` TOML
                    file (see fields.example.toml). `payload()`
                    serializes everything as JSON for the QR code,
                    `folder_name()` previews the eventual
                    `<Project>-<Location>-<ID>-<Name>` directory name
                    from the four built-in fields only -- extra fields
                    never affect the folder name, only the QR's "extra"
                    map. draw_qr() renders QR modules directly with
                    egui::Painter (no image/raster step) so the code
                    stays crisp and purely black/white at any window
                    size.
photo-splitter/     not yet implemented. Will scan a directory of
                    time-ordered photos, decode each QR-marker frame
                    (the JSON payload label-qr-gui encodes), and split
                    subsequent photos into
                    `<Project>-<Location>-<ID>-<Name>/` folders with a
                    YAML metadata file.
darktable-export/   not yet implemented. Will convert those split
                    directories into XMP sidecars for the sibling
                    `darktable-import-xmp` repo to pick up — this crate
                    should not talk to Darktable's SQLite database
                    directly, that's already handled there.
```

## The QR payload is JSON, not the dash-joined string

`label-qr-gui` encodes the QR as a JSON object (`{"project":...,
"location":..., "id":..., "name":..., "extra":{...}}`), not as the
`<Project>-<Location>-<ID>-<Name>` string shown in the UI preview. JSON
round-trips safely even if a field itself contains a dash, and the
`"extra"` map is open-ended (field names come from whatever config file
was used at capture time). Keep this split (JSON on the wire,
dash-joined string only as a human-readable folder-name preview) when
`photo-splitter` is built. `photo-splitter` must treat `"extra"`'s keys
as unknown/arbitrary -- never assume a fixed schema -- and the key is
absent entirely when no extra fields were configured, not just empty.

## Workflow notes

- `cargo fmt --all`, `cargo clippy --all-targets --all-features -- -D
  warnings`, and `cargo test` must all pass before committing. CI
  enforces the same three.
- `nix build` / `nix develop` should keep working for `label-qr-gui`;
  if you add a dependency, make sure `Cargo.lock` is committed so the
  Nix build stays reproducible.
- `photo-splitter`'s own YAML metadata file (once implemented) should
  mirror the QR's `extra` map verbatim rather than inventing a second
  schema for the same data.

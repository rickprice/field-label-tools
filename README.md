# Field Label Tools

A small toolchain for labeling field photo shoots with a QR code at
capture time, then splitting and organizing the resulting photos
afterward.

## Workflow

1. **`label-qr-gui`** — a fullscreen appliance (meant to run under a
   tiling WM like XMonad) with a metadata form on the left and a large,
   high-contrast QR code on the right. Fill in Project / Location / ID /
   Name, photograph the screen as the first frame of a shoot, then press
   **Next** (or hit Enter in the Name field) to bump the ID and clear the
   Name for the next shoot while keeping Project/Location. Extra,
   project-specific fields can be added with `--config fields.toml` (see
   [`fields.example.toml`](label-qr-gui/fields.example.toml)); their
   values are embedded in the QR's JSON payload alongside the built-in
   fields, under `"extra"`. ✅ implemented.
2. **`photo-splitter`** — a CLI that walks a directory of time-ordered
   photos, treats each QR-code frame as the start of a new group, and
   moves everything up to the next QR frame into
   `<Project>-<Location>-<ID>-<Name>/`, writing a `metadata.yaml` file
   (the decoded QR payload verbatim) inside each folder. Photos are
   ordered by EXIF `DateTimeOriginal` (falling back to file mtime), and
   each group is only moved once its boundary is known — i.e. once the
   next marker (or the end of the directory) is found — so an
   interrupted run never leaves a group half-moved. Any files before
   the first marker are left in place with a warning rather than
   guessed at. ✅ implemented.

   RAW files (CR2/NEF/ARW/DNG/ORF/PEF, matching `darktable-import-xmp`'s
   supported list) are the primary target; JPEG/PNG/TIFF/BMP/WebP are
   also supported. For RAW files, the QR is scanned from the
   EXIF-embedded thumbnail rather than the full RAW data (there's no
   cheap way to decode that directly) — label-qr-gui's QR is
   deliberately huge and high-contrast so it should survive being
   shrunk to thumbnail size, but if your camera's thumbnail turns out
   too small (or absent), that marker frame will be missed and merged
   into the previous group. Run with `--dry-run` first and check the
   log output (`RUST_LOG=debug` for more detail) before trusting it on
   a real shoot, especially the first time with a new camera.
3. **`darktable-export`** — a CLI that converts those split, tagged
   directories into XMP sidecars. 🚧 planned. This is expected to feed
   into the existing
   [`darktable-import-xmp`](../darktable-import-xmp) tool, which already
   imports loose XMP sidecars into Darktable's library database — so
   this crate's job is just to *produce* sidecars carrying the
   Project/Location/ID/Name (and any configured extra fields) as
   Darktable-visible metadata (e.g. IPTC keywords/title), not to talk to
   the Darktable database directly.

## Configurable metadata

Project / Location / ID / Name are built in and always present.
Additional fields are configured per project with a TOML file:

```toml
[[field]]
name = "Surveyor"

[[field]]
name = "Notes"
reset_on_next = true  # cleared on "Next", like Name; default is to persist, like Project/Location
```

Pass it to the GUI with `label-qr-gui --config fields.toml`. Each
configured field's value ends up in the QR's JSON payload under
`"extra"` (empty values are omitted), e.g.:

```json
{"project":"P1","location":"North Ridge","id":3,"name":"overview","timestamp_utc":"2026-11-26T00:10:00Z","extra":{"Surveyor":"J. Price"}}
```

The QR also always embeds `timestamp_utc` (RFC 3339, UTC, second
precision) — the time the QR was last redrawn, not shown on the form
itself. The app forces a repaint at least once a second so this stays
current even if the form sits idle before the screen is photographed.

`photo-splitter` treats `"extra"` as an open map (field names it's
never seen are still valid) rather than a fixed schema, since the set
of extra fields varies per project/config file, and writes it into
`metadata.yaml` verbatim.

## Project layout

```
label-qr-gui/      GUI appliance (egui/eframe) — crate implemented
photo-splitter/    CLI — crate implemented
darktable-export/  CLI — stub only, not yet implemented
```

This is a Cargo workspace; shared package metadata (version, edition,
license, repository) lives in the top-level `Cargo.toml`'s
`[workspace.package]`.

## Building

```sh
cargo build --release
```

Binaries land in `target/release/`.

### NixOS

```sh
nix build        # builds label-qr-gui
nix run           # runs label-qr-gui
nix develop        # dev shell with rustc/cargo/clippy/rustfmt and the
                    # GUI's runtime libraries on LD_LIBRARY_PATH
```

`photo-splitter` and `darktable-export` aren't exposed as Nix packages
yet since they're not implemented; `nix build` only builds
`label-qr-gui` for now.

## License

MIT — see [LICENSE](LICENSE).

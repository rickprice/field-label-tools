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
   Name for the next shoot while keeping Project/Location. ✅ implemented.
2. **`photo-splitter`** — a CLI that walks a directory of time-ordered
   photos, treats each QR-code frame as the start of a new group, and
   splits everything up to the next QR frame into
   `<Project>-<Location>-<ID>-<Name>/`, writing a YAML metadata file
   inside each folder. 🚧 planned.
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

The initial field set is Project / Location / ID / Name, but more
fields may be needed later. `photo-splitter`'s metadata schema is meant
to be configurable (e.g. via a config file listing additional fields)
once it's clear what those fields are — see that crate's issues/TODOs
once started.

## Project layout

```
label-qr-gui/      GUI appliance (egui/eframe) — crate implemented
photo-splitter/    CLI — stub only, not yet implemented
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

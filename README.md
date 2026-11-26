# Field Label Tools

A small toolchain for labeling field photo shoots with a QR code at
capture time, then splitting and organizing the resulting photos
afterward.

## Workflow

1. **`label-qr-gui`** — a fullscreen appliance (meant to run under a
   tiling WM like XMonad) with a metadata form on the left and a large,
   high-contrast QR code on the right. Fill in Client / Project /
   Location / ID / Name, photograph the screen as the first frame of a
   shoot, then press **Next** (or hit Enter in the Name field) to bump
   the ID and clear the Name for the next shoot while keeping
   Client/Project/Location. Extra, project-specific fields can be added
   with `--config fields.toml` (see
   [`fields.example.toml`](label-qr-gui/fields.example.toml)); their
   values are embedded in the QR's JSON payload alongside the built-in
   fields, under `"extra"`. ✅ implemented.
2. **`photo-splitter`** — a CLI that walks a directory of time-ordered
   photos, treats each QR-code frame as the start of a new group, and
   moves everything up to the next QR frame into
   `<Client>/<Project>/<Location>/<Name>/<yyyy-mm-dd>/`, writing a
   `metadata.yaml` file (the decoded QR payload verbatim) inside each
   folder. The date comes from the marker's embedded `timestamp_utc`,
   not the photo's own EXIF capture time. If two shoots share
   Client/Project/Location/Name *and* land on the same day, the second
   one's ID is appended to the date (`<yyyy-mm-dd>_<ID>/`) to
   disambiguate — ID stays invisible in the common case, only showing up
   when it's actually needed. Photos themselves are ordered by EXIF
   `DateTimeOriginal` (falling back to file mtime), and each group is
   only moved once its boundary is known — i.e. once the next marker (or
   the end of the directory) is found — so an interrupted run never
   leaves a group half-moved. Any files before the first marker are left
   in place with a warning rather than guessed at. A blank built-in
   field (e.g. a QR payload captured before `client` existed) can be
   filled in with a configured default — see "Configurable field
   defaults" below — and if no default is configured, it's left blank:
   `sanitize`d to `_` in the folder path, and omitted entirely from the
   XMP sidecar rather than appearing as an empty tag. ✅ implemented.

   RAW files (CR2/NEF/ARW/DNG/ORF/PEF, matching `darktable-import-xmp`'s
   supported list) are the primary target; JPEG/PNG/TIFF/BMP/WebP are
   also supported. By default, RAW files are decoded with LibRaw's
   `dcraw_process` pipeline, run in `half_size` mode (block-average
   demosaic — quarter resolution, much faster than full AHD/VNG
   interpolation, and QR detection doesn't need photo-quality output)
   with default white balance and LibRaw's default gamma/brightness
   curve — the same kind of default rendering most RAW viewers,
   including Darktable itself, produce. This is slower per file than
   reading the embedded EXIF thumbnail, but much more reliably
   bright/contrasty enough to find the QR regardless of the as-shot
   exposure. Files are decoded in parallel across all available CPU
   cores (`rayon`); real-world testing against 21 Canon CR2 files
   (~40MB each) found the whole batch decodes in well under 15 seconds
   on a 12-core machine. Before detection, the image is given a light
   blur — a dark, high-ISO photo's sensor noise can otherwise produce
   huge numbers of spurious QR-candidate matches (one real test file
   went from 90+ seconds to ~150ms once this was added, with no change
   in which markers were found). Pass `--fast` to scan
   the EXIF thumbnail instead (much faster, but thumbnails are often
   small — commonly ~160×120, sometimes framing far too little of the
   scene to resolve the QR even at that resolution — and some cameras
   omit one entirely, so a marker frame scanned this way can be missed
   and silently merged into the previous group; confirmed in practice,
   not just in theory). Run with `--dry-run` first and check the log
   output (`RUST_LOG=debug` for more detail) before trusting either
   mode on a real shoot, especially the first time with a new camera.

   **If you build/run outside the `flake.nix` package or dev shell**,
   set `OMP_NUM_THREADS=1` yourself. `libraw_r` links `libgomp`
   (OpenMP), and without this, each of rayon's per-file worker threads
   spawns its own OpenMP thread team internally — e.g. 12 rayon workers
   × ~12 OpenMP threads each can oversubscribe a 12-core machine ~12x,
   turning the intended parallel speedup into thread-contention
   thrashing instead. The Nix package and dev shell both set this for
   you already.

   To see *why* a particular marker frame wasn't recognized, run with
   `RUST_LOG=trace`: every photo's exact QR-scan image (the LibRaw
   output, EXIF thumbnail, or plain decoded raster, whichever applies)
   is saved into a `_qr_debug/` folder next to it, named
   `<original filename>.jpg`. This writes one JPEG per photo, so it's
   gated behind `trace` rather than `debug`.

   **Darktable XMP sidecars.** If a tag prefix is configured (see
   below), every photo also gets a `<filename>.xmp` sidecar with
   Client/Project/Location/Name (and any `extra` fields) written as
   Darktable hierarchical tags under that prefix — e.g.
   `PricePixel|Project|...`, `PricePixel|Location|...`. This mirrors the
   two XMP fields Darktable itself writes for hierarchical tags
   (confirmed against a real Darktable-generated sidecar): `dc:subject`
   (a flat, deduplicated set of every tag-tree segment) and
   `lr:hierarchicalSubject` (the actual pipe-joined paths). A blank
   field (no value, and no configured default — see below) is omitted
   from both entirely, rather than appearing as an empty tag. ID and
   `timestamp_utc` are deliberately *not* tagged — they're bookkeeping,
   not something you'd browse photos by — but both are still in
   `metadata.yaml`. No `darktable:*` processing-history fields are
   written, since these sidecars are for photos Darktable has never
   seen; this relies on Darktable's import reading foreign/minimal XMP
   for metadata the same way it does for e.g. Lightroom-originated
   sidecars (no edit history, since there isn't any) — **verify this
   against a real Darktable import** before relying on it, since it
   isn't something testable without a running Darktable instance.

   The tag prefix resolves, in order: `--tag-prefix <value>` >
   `--config <path>` (a path given explicitly this way is a hard error
   if missing/invalid) > the default config file at
   `~/.config/photo-splitter/config.toml` (silently skipped if absent —
   no prefix configured means no XMP sidecars are written at all,
   everything else works the same).

   **Configurable field defaults.** The same config file can also set
   `default_client`, `default_project`, `default_location`, and/or
   `default_name` — fallback values filled into a photo's Client /
   Project / Location / Name whenever that field comes out blank from
   the QR payload (e.g. an older payload from before `client` existed,
   or simply an unset field). There's no `--default-client`-style CLI
   flag for these, unlike `--tag-prefix`; they're meant to be set once
   per install (e.g. "I'm always shooting for Acme Co") rather than
   vary per run. A field with no configured default is left blank:
   `sanitize`d to `_` in the folder path (as always), and omitted
   entirely from the XMP sidecar — see above.

   ```toml
   tag_prefix = "PricePixel"
   default_client = "Acme Co"
   ```
3. **`darktable-export`** — still a stub; its originally-planned job
   (produce XMP sidecars) is now handled by `photo-splitter` directly
   (see above). Whether a separate import step is still needed at all
   depends on whether Darktable's plain import (reading those sidecars'
   tags, with no special handling) is sufficient — if so, this crate
   may turn out to be unnecessary. The one thing a bare import can't do
   is grouping (Darktable's `group_id` is a `library.db`-only concept,
   not something storable in a portable sidecar — see
   [`darktable-import-xmp`](../darktable-import-xmp), which already
   writes `group_id` directly, for how that would be done if it turns
   out to matter). 🚧 undecided.

## Configurable metadata

Client / Project / Location / ID / Name are built in and always present.
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
{"client":"Acme Co","project":"P1","location":"North Ridge","id":3,"name":"overview","timestamp_utc":"2026-11-26T00:10:00Z","extra":{"Surveyor":"J. Price"}}
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
nix build .#label-qr-gui
nix build .#photo-splitter
nix run .#label-qr-gui
nix develop        # dev shell with rustc/cargo/clippy/rustfmt, the
                    # GUI's runtime libraries on LD_LIBRARY_PATH, and
                    # libraw + bindgenHook for photo-splitter's build.rs
```

`photo-splitter` links against the system `libraw_r` library via
`bindgen` (see `photo-splitter/build.rs`); on NixOS this is handled by
`pkgs.libraw` + `pkgs.rustPlatform.bindgenHook` in `flake.nix`. On other
platforms, install LibRaw's dev package (e.g. `libraw-dev` on
Debian/Ubuntu) and a libclang (e.g. `libclang-dev`) before building.

`darktable-export` isn't exposed as a Nix package yet since it's not
implemented.

## License

MIT — see [LICENSE](LICENSE).

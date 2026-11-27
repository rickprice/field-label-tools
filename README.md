# Field Label Tools

A small toolchain for labeling field photo shoots with a QR code at
capture time, then splitting and organizing the resulting photos
afterward.

## Workflow

1. **`label-qr-gui`** ("QR Field Data") — a fullscreen appliance (meant to
   run under a tiling WM like XMonad) with a metadata form on the left
   and a large, high-contrast QR code on the right. Fill in Client /
   Project / Location / ID / Name, photograph the screen as the first
   frame of a shoot, then press **Next** (or hit Enter in the Name
   field) to bump the ID and clear the Name for the next shoot while
   keeping Client/Project/Location. **Print** opens the current QR code
   (as an inline SVG, so it stays crisp at any size) in your default web
   browser via `xdg-open`, for printing a physical card on paper that can
   be held up to a camera instead of (or alongside) the on-screen
   appliance — handy for handing the same label to several cameras in
   turn. Extra, project-specific fields are grouped into named
   **Metadata Sets**, picked from a dropdown, configured with `--config
   fields.json` — **the exact same JSON sidecar file format** (and, if
   you point both tools at the same path, the exact same file) the web
   page below uses; see
   [`fields.example.json`](label-qr-gui/web/fields.example.json), which
   includes a "Church Photos" example with multiline People/Address/
   Phone Numbers fields. Their values are embedded in the QR's JSON
   payload alongside the built-in fields, under `"extra"`. A "Plain" set
   (no extra fields) is always available even if you don't define one
   yourself. ✅ implemented.

   **[`web/index.html`](label-qr-gui/web/index.html)** is a browser-based
   alternative to the native GUI — a single self-contained HTML file (no
   build step, no server, no network access required) with the same five
   built-in fields, the same JSON payload shape, and the same "Next"
   semantics, so `photo-splitter` can't tell which one produced a given
   QR code. Open it directly from a phone's local storage (`file://`,
   fully offline) or host it anywhere static (e.g. GitHub Pages) and "Add
   to Home Screen." Extra fields are grouped into named **metadata sets**
   picked from a "Metadata Set" dropdown (a project with more than one
   kind of shoot — survey vs. inspection, say — can define one field set
   per kind instead of a single fixed list) via the same JSON sidecar
   file format the native GUI's `--config` takes: copy
   [`fields.example.json`](label-qr-gui/web/fields.example.json) to
   `fields.json` next to `index.html` (only takes effect when the page is
   fetched over http(s) — some browsers block `fetch()` of local files
   opened via `file://`) or edit the `DEFAULT_FIELD_SETS` object near
   the bottom of `index.html` directly, which works either way. A "Plain"
   set (no extra fields) is always available even if it isn't explicitly
   listed. Any field can set `"multiline": true` for a bigger text area
   instead of a single-line box (see fields.example.json's "Church
   Photos" set: People/Address/Phone Numbers). QR
   encoding is handled by a vendored, unmodified copy of Kazuhiko Arase's
   MIT-licensed [qrcode-generator](https://github.com/kazuhikoarase/qrcode-generator)
   — see the license comment at the top of its embedded `<script>` block.
   Installed via `cargo install`/crates.io rather than a GitHub checkout?
   `label-qr-gui --export-web <DIR>` writes `index.html` and
   `fields.example.json` into `DIR` (both are embedded in the binary via
   `include_str!`) instead of launching the GUI. ✅ implemented.

   Both the web page and the native GUI have a **Timecode Warble**
   section, independent of the QR code — a "Start Timecode" button plays
   a continuous SMPTE LTC audio signal (24/25/29.97-Non-Drop/29.97-Drop-
   Frame/30fps) through the device's audio output (the Web Audio API on
   the page, [`cpal`](https://crates.io/crates/cpal) in the native GUI —
   same bit-generation logic ported between the two, see `ltc.rs`). Feed
   that into a camera or audio recorder's mic input to help auto-sync
   tools (DaVinci Resolve, PluralEyes, etc.) align multiple cameras/audio
   recordings afterward. It jam-syncs to the device's own clock when
   started and then free-runs — there's no external time reference, so
   how well multiple devices line up depends on how well their clocks
   already agree. ✅ implemented.
2. **`photo-splitter`** — a CLI that walks a directory of time-ordered
   photos, treats each QR-code frame as the start of a new group, and
   moves everything up to the next QR frame into
   `<Client>/<Project>/<Location>/<Name>/<yyyy-mm-dd>/`, writing a
   `metadata.yaml` file (the decoded QR payload verbatim) inside each
   folder. The date comes from the marker's embedded `timestamp_utc`,
   not the photo's own EXIF capture time. The exact same QR code
   photographed more than once (e.g. a printed copy shown to several
   cameras in turn, or an on-screen one re-photographed by mistake) just
   merges all of that marker's files into the one directory, rather than
   erroring or creating duplicates. If two *different* shoots share
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

   A separate import step for grouping isn't part of this repo — see
   [`darktable-import-xmp`](../darktable-import-xmp), a sibling tool that
   sets Darktable's `group_id` directly (not something storable in a
   portable sidecar), for how that would be done if it turns out to
   matter.

## Configurable metadata

Client / Project / Location / ID / Name are built in and always present.
Additional fields are grouped into named Metadata Sets (selectable from a
dropdown) and configured per project with a JSON file — **the same one
file works for both the native GUI and the web page** (see
[`fields.example.json`](label-qr-gui/web/fields.example.json)):

```json
{
  "Survey": [
    { "name": "Surveyor" },
    { "name": "Notes", "reset_on_next": true }
  ],
  "Church Photos": [
    { "name": "People", "multiline": true },
    { "name": "Address", "multiline": true }
  ]
}
```

`reset_on_next` clears a field on "Next", like Name; default is to
persist, like Project/Location. `multiline` gets a bigger text area
instead of a single-line box.

Pass it to the GUI with `label-qr-gui --config fields.json`. A "Plain"
set (no extra fields) is always available even if you don't define one
yourself — and a project's Metadata Sets only need to be written once,
in one file, for both front ends. Each configured field's value ends up
in the QR's JSON payload under `"extra"` (empty values are omitted),
e.g.:

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
  web/             browser-based alternative, no build/install needed
photo-splitter/    CLI — crate implemented
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

`label-qr-gui`'s Timecode Warble feature links ALSA via `cpal` on Linux
(see `flake.nix`'s `alsa-lib`); on other platforms, install ALSA's dev
package (e.g. `libasound2-dev` on Debian/Ubuntu) before building.

## License

MIT — see [LICENSE](LICENSE).

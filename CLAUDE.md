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
                    complete. With RUST_LOG=trace, dump_qr_debug_image()
                    saves the exact image handed to the QR detector for
                    every photo into a `_qr_debug/` folder next to it.
                    Decoding all files (the expensive part) happens in
                    one `entries.par_iter().map(decode_qr).collect()`
                    BEFORE the grouping loop -- grouping itself must stay
                    strictly sequential (it depends on seeing markers in
                    time order), so it's cheap pure bookkeeping once
                    `decoded` already holds every file's result.
  src/raw_decode.rs  Two ways to get a grayscale image out of a RAW file
                    to scan for a QR code. `decode_default` (the
                    default) runs LibRaw's dcraw_process pipeline with
                    `params.half_size = 1` (block-average demosaic --
                    quarter resolution, skips the expensive AHD/VNG
                    interpolation entirely, since QR detection doesn't
                    need photo-quality output) via FFI bindings
                    generated in src/libraw_sys.rs -- this is the
                    "default light leveling and contrast" path, chosen
                    over the cheap EXIF-thumbnail route specifically
                    because thumbnails are often too small/absent (or
                    just frame too little of the QR within the shot),
                    and getting QR detection wrong silently merges two
                    shoots together. Confirmed against 21 real Canon CR2
                    files (2026-11-26): the EXIF-thumbnail path (--fast)
                    found zero markers; decode_default found them all.
                    `decode_thumbnail` (`--fast`) is the old cheap
                    fallback, kept for when a full LibRaw decode per
                    photo is too slow. See src/libraw_sys.rs's module
                    doc for why this is hand-bound via bindgen rather
                    than an existing crate (both `rawler` and the
                    published LibRaw Rust bindings were unsuitable --
                    checked 2026-11-26, worth re-checking if revisiting
                    this).
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
- `nix build .#label-qr-gui` / `.#photo-splitter` / `nix develop`
  should keep working; if you add a Cargo dependency, make sure
  `Cargo.lock` is committed so the Nix build stays reproducible. A new
  source file is invisible to a flake-from-git-checkout build until
  it's at least `git add`ed (staged, not necessarily committed) --
  this bit twice while building `raw_decode.rs`/`libraw_sys.rs`.
- `photo-splitter`'s `metadata.yaml` mirrors the decoded QR payload
  verbatim (same `LabelPayload` struct, serialized with `serde_yaml`)
  rather than inventing a second schema for the same data.
- `photo-splitter` moves files with `fs::rename`, which only works
  within a single filesystem -- it does not fall back to copy+delete
  across filesystem boundaries. The destination folders are always
  created as siblings inside the same directory being scanned, so this
  is a non-issue unless that assumption changes.
- `photo-splitter/tests/fixtures/` holds two real-camera files (Canon
  EOS 5D Mark IV, privacy-reviewed before committing -- see the comment
  above `fixture_path()` in main.rs's test module for what each one is
  and why): `near-black-no-marker.CR2` (a genuine RAW decode + the
  half_size+blur speed fix, regression-tested with a 15s time bound)
  and `real-camera-marker.jpg` (a real photographed-and-cropped QR,
  giving lens/lighting/JPEG-artifact coverage a synthetically rendered
  QR doesn't). Only these two, not the full 21-file set used to find
  and fix the issues below -- keep it that way; more fixtures means
  more repo weight (no Git LFS here) and more real-photo content
  permanently in git history, both deliberately kept minimal. Before
  trusting `photo-splitter` on a real shoot with a *new* camera,
  though, still repeat that kind of manual check against a handful of
  its actual RAW files -- these two fixtures don't cover every
  camera/sensor's quirks.
- **libraw_r links libgomp (OpenMP).** Combining rayon's per-file
  parallelism with LibRaw's own internal OpenMP parallelism
  oversubscribes badly -- confirmed in practice: 12 rayon workers each
  spawning their own ~12-thread OpenMP team produced 145 OS threads on
  a 12-core machine, all contending rather than working (CPU usage
  stuck around 150%, i.e. ~1.2 cores' worth of real work, the entire
  time). `OMP_NUM_THREADS=1` fixes this by leaving rayon as the only
  parallelism layer; both the Nix package (`postFixup` wrapProgram
  `--set-default`) and the dev shell set this already, so it only
  bites if you build/run outside of Nix entirely. Verify empirically,
  don't guess, if you ever revisit this: check `ldd` on the actual
  `libraw_r.so` being linked (there can be multiple outputs/paths for
  the same nixpkgs attribute -- `nix build nixpkgs#libraw` alone
  pointed at a `bin`-only output with no `.so` in it at all; the
  shared libraries were in `nixpkgs#libraw.lib`), and watch
  `/proc/<pid>/task/*/wchan` for `futex_do_wait` plus total CPU-time
  (sum of utime+stime across threads) vs wall-clock elapsed as the
  simplest way to confirm whether real parallelism is happening at
  all, before assuming code is at fault.
- A single file can make the whole batch look broken even when
  parallelism is working correctly -- observed: one 36MB file took
  10+ minutes alone while twenty 36-44MB files from the same camera,
  same session, each took 5-22 seconds, dominating a parallel batch's
  tail. **Root cause, confirmed by per-stage timing (2026-11-26): not
  LibRaw.** All four libraw_* calls (open_file/unpack/dcraw_process/
  make_mem_image) finished in ~1.3s combined even on the slow file --
  the cost was entirely in `rqrr::detect_grids()` afterward. The file
  was a near-black, ISO-12800 shot (ended up being this repo's
  `near-black-no-marker.CR2` fixture); heavy sensor noise in a dark
  frame creates huge numbers of spurious finder-pattern-like
  candidates for rqrr to individually check and reject, where a
  cleaner or brighter frame doesn't. **Fix:** `decode_qr` now runs
  `image::imageops::blur(&gray, 1.0)` immediately before
  `rqrr::PreparedImage::prepare` -- real QR patterns (large,
  high-contrast, structured) survive a mild blur fine, fine-grained
  noise doesn't. This took that file from 90+s to ~150ms with no
  change in detection results across all 21 real test photos.
  Tried first and rejected: `half_size` downscaling further (real risk
  -- these photos frame the whole desk, not a tight crop of the QR, so
  the QR's pixel margin isn't huge to begin with; shrinking it further
  risks reintroducing the exact too-small-to-read failure the
  EXIF-thumbnail path already has) and LibRaw's own `fbdd_noiserd`
  pre-demosaic denoise (tested at the max setting, zero measurable
  effect on this file -- whatever rqrr is reacting to isn't fixed by
  denoising the Bayer data pre-demosaic). If a future pathological file
  isn't fixed by the existing blur, re-run the same per-stage timing
  method before guessing: it's what found the real bottleneck here
  after an initial wrong guess (busy/detailed scene content) that
  didn't hold up.
- `RUST_LOG=trace` makes `decode_qr` dump the exact grayscale image it
  fed to `rqrr` into a `_qr_debug/` folder next to each source photo
  (`dump_qr_debug_image`/`write_qr_debug_image`), for diagnosing why a
  marker frame wasn't recognized. This is deliberately gated behind
  `trace`, not `debug`, since it writes a file per photo rather than
  just a log line -- don't lower that threshold without good reason.
  Note for tests: `log::log_enabled!` consults the *installed logger's*
  `enabled()`, not just `log::set_max_level` -- the default no-op
  logger (when nothing has called `set_logger`/`env_logger::init`)
  always reports disabled regardless of max level, so a test
  exercising this path needs a real logger installed (e.g.
  `env_logger::Builder::new().filter_level(..).is_test(true).try_init()`),
  not just `log::set_max_level`.
- `libraw_sys.rs`'s bindings are regenerated by `build.rs` on every
  build (not checked in), via `bindgen` against whatever `libraw_r`
  pkg-config resolves to. If a field/function name changes (e.g. after
  bumping the LibRaw version in `flake.nix`), the fix is almost always
  in `raw_decode.rs`'s call sites, not in the generated bindings.

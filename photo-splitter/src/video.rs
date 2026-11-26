//! Video marker support.
//!
//! A video clip can be a QR marker the same way a photo can, but the QR
//! code might not be in its very first frame -- the person recording may
//! walk into position before holding the code up to the camera. Rather
//! than decoding every frame (expensive, and most video files in a shoot
//! aren't markers at all), `frames_in_window` samples one frame every
//! [`ScanConfig::interval`] across the first [`ScanConfig::window`] of the
//! clip, so the caller can stop at the first one that decodes. Both are
//! configurable (xmp::ConfigFile's `video_scan_window_secs`/
//! `video_scan_interval_secs`) rather than fixed, since the right window
//! depends on how long it typically takes to walk into frame and hold the
//! code up, which varies by shoot.
//!
//! Video is handled by shelling out to `ffmpeg`/`ffprobe` rather than an
//! FFI binding -- unlike LibRaw (see raw_decode.rs/libraw_sys.rs), a full
//! video-codec FFI binding would be a much bigger lift than this project
//! needs just to pull a handful of still frames and a metadata tag, and
//! `ffmpeg` is about as standard an external dependency as a video tool
//! gets. This means, unlike RAW decoding, video QR scanning is a *runtime*
//! dependency (ffmpeg/ffprobe must be on PATH when photo-splitter runs),
//! not just a build-time one.
//!
//! A non-marker video file still pays the full scan cost (every sample
//! point fails, so the whole window gets scanned before concluding
//! there's no marker here) -- there's no way to know in advance that a
//! given video isn't the one holding the QR code. This is parallelized
//! across files the same way photo decoding is (see decode_qr's caller in
//! main.rs), but a shoot with many non-marker video clips will still cost
//! meaningfully more wall-clock time than the same shoot with none.

use std::path::Path;
use std::process::Command;
use std::sync::Once;
use std::time::{Duration, SystemTime};

const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mov", "m4v", "avi", "mts"];

static FFMPEG_MISSING_WARNED: Once = Once::new();
static FFPROBE_MISSING_WARNED: Once = Once::new();

/// How much of a video clip to scan for a QR marker, and how finely --
/// configurable (see xmp::ConfigFile's `video_scan_window_secs`/
/// `video_scan_interval_secs`) rather than fixed, since the right window
/// depends on how long it typically takes to walk into frame and hold the
/// code up, which varies by shoot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScanConfig {
    pub window: Duration,
    pub interval: Duration,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            window: Duration::from_secs(5 * 60),
            interval: Duration::from_secs(5),
        }
    }
}

pub fn is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|ext| VIDEO_EXTENSIONS.contains(&ext.as_str()))
}

/// One sampled frame every `config.interval`, up to `config.window`, as a
/// lazy iterator (so a caller that stops at the first successful QR
/// decode doesn't pay for samples past that point).
pub fn frames_in_window(path: &Path, config: ScanConfig) -> FrameSampler<'_> {
    FrameSampler {
        path,
        next_offset: Duration::ZERO,
        window: config.window,
        interval: config.interval,
    }
}

pub struct FrameSampler<'a> {
    path: &'a Path,
    next_offset: Duration,
    window: Duration,
    interval: Duration,
}

impl Iterator for FrameSampler<'_> {
    /// `(offset into the clip, the sampled frame)`, so a caller can
    /// attribute a decode (or a `_qr_debug/` dump) to a specific point in
    /// the video rather than just "somewhere in this file."
    type Item = (Duration, image::GrayImage);

    fn next(&mut self) -> Option<Self::Item> {
        if self.next_offset >= self.window {
            return None;
        }
        let offset = self.next_offset;
        self.next_offset += self.interval;

        // Stop entirely on the first failed sample rather than trying the
        // next offset: either ffmpeg/ffprobe aren't on PATH (every later
        // sample will fail identically) or this offset is already past
        // the end of a clip shorter than `window` (so is every later
        // one). A single transient glitch at one sample point isn't worth
        // specifically retrying for -- there's another sample `interval`
        // later regardless.
        extract_frame(self.path, offset).map(|gray| (offset, gray))
    }
}

fn extract_frame(path: &Path, offset: Duration) -> Option<image::GrayImage> {
    let output = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error"])
        .args(["-ss", &offset.as_secs().to_string()])
        .arg("-i")
        .arg(path)
        .args(["-frames:v", "1", "-f", "image2", "-vcodec", "mjpeg", "-"])
        .stdin(std::process::Stdio::null())
        .output();

    let output = match output {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            FFMPEG_MISSING_WARNED.call_once(|| {
                log::error!(
                    "ffmpeg not found on PATH -- video files cannot be scanned for QR markers \
                     (install ffmpeg to enable this)"
                );
            });
            return None;
        }
        Err(err) => {
            log::debug!(
                "{}: failed to run ffmpeg at {offset:?}: {err}",
                path.display()
            );
            return None;
        }
    };

    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }

    image::load_from_memory_with_format(&output.stdout, image::ImageFormat::Jpeg)
        .ok()
        .map(|img| img.to_luma8())
}

/// The container's `creation_time` metadata tag (QuickTime/MP4's
/// equivalent of EXIF `DateTimeOriginal`), for ordering video files
/// alongside photos the same way `exif_datetime_original` does -- falls
/// back to the file's mtime just like the photo path if this isn't
/// present or ffprobe isn't available.
pub fn creation_time(path: &Path) -> Option<SystemTime> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format_tags=creation_time",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .output();

    let output = match output {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            FFPROBE_MISSING_WARNED.call_once(|| {
                log::error!(
                    "ffprobe not found on PATH -- video files will be ordered by file \
                     modification time instead of their embedded creation time"
                );
            });
            return None;
        }
        Err(err) => {
            log::debug!("{}: failed to run ffprobe: {err}", path.display());
            return None;
        }
    };

    if !output.status.success() {
        return None;
    }
    parse_creation_time(String::from_utf8_lossy(&output.stdout).trim())
}

fn parse_creation_time(text: &str) -> Option<SystemTime> {
    if text.is_empty() {
        return None;
    }
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|dt| dt.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_video_matches_common_camera_and_phone_extensions() {
        for ext in ["mp4", "MOV", "m4v", "avi", "mts"] {
            assert!(
                is_video(Path::new(&format!("clip.{ext}"))),
                "{ext} should be recognized as video"
            );
        }
    }

    #[test]
    fn is_video_rejects_non_video_extensions() {
        assert!(!is_video(Path::new("photo.jpg")));
        assert!(!is_video(Path::new("raw.CR2")));
        assert!(!is_video(Path::new("no_extension")));
    }

    #[test]
    fn parse_creation_time_handles_ffprobes_actual_output_format() {
        // ffprobe's default=noprint_wrappers=1:nokey=1 prints just the
        // bare value, e.g. "2026-11-26T14:30:00.000000Z" for a typical
        // QuickTime/MP4 creation_time tag.
        let parsed = parse_creation_time("2026-11-26T14:30:00.000000Z").unwrap();
        let expected: SystemTime = chrono::DateTime::parse_from_rfc3339("2026-11-26T14:30:00Z")
            .unwrap()
            .into();
        assert_eq!(parsed, expected);
    }

    #[test]
    fn parse_creation_time_is_none_for_blank_or_malformed_input() {
        assert_eq!(parse_creation_time(""), None);
        assert_eq!(parse_creation_time("not a timestamp"), None);
    }
}

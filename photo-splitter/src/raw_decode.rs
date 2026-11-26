//! Turning a RAW file into a grayscale image to scan for a QR code.
//!
//! [`decode_default`] runs LibRaw's full `dcraw_process` pipeline --
//! demosaic, default white balance, LibRaw's default gamma/brightness curve
//! -- the same kind of default rendering most RAW viewers (including
//! Darktable, which also uses LibRaw) produce for a quick preview. Slower
//! than reading the embedded thumbnail, but far more likely to be bright
//! and contrasty enough to reliably find a QR code in, regardless of the
//! as-shot exposure.
//!
//! [`decode_thumbnail`] is the fast fallback (`--fast-raw-preview`): read
//! the EXIF-embedded thumbnail instead. Standard EXIF thumbnails are
//! commonly only ~160x120 and some cameras omit one entirely -- a marker
//! frame missed this way gets silently merged into the previous group.

use std::ffi::{CStr, CString};
use std::os::raw::c_int;
use std::path::Path;

use exif::{In, Tag, Value};

use crate::libraw_sys as sys;

struct Processor(*mut sys::libraw_data_t);

impl Processor {
    fn new() -> Option<Self> {
        let ptr = unsafe { sys::libraw_init(0) };
        if ptr.is_null() { None } else { Some(Self(ptr)) }
    }
}

impl Drop for Processor {
    fn drop(&mut self) {
        unsafe { sys::libraw_close(self.0) };
    }
}

fn strerror(code: c_int) -> String {
    let ptr = unsafe { sys::libraw_strerror(code) };
    if ptr.is_null() {
        return format!("unknown error {code}");
    }
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

pub fn decode_default(path: &Path) -> Option<image::GrayImage> {
    let c_path = CString::new(path.to_str()?).ok()?;
    let processor = Processor::new()?;

    // Skip full demosaic interpolation (AHD/VNG/etc.) and just average each
    // 2x2 Bayer block into one pixel -- quarter resolution, but an order of
    // magnitude faster, and QR detection doesn't need photo-quality output.
    unsafe { (*processor.0).params.half_size = 1 };

    let rc = unsafe { sys::libraw_open_file(processor.0, c_path.as_ptr()) };
    if rc != 0 {
        log::debug!(
            "{}: libraw_open_file failed: {}",
            path.display(),
            strerror(rc)
        );
        return None;
    }

    let rc = unsafe { sys::libraw_unpack(processor.0) };
    if rc != 0 {
        log::debug!("{}: libraw_unpack failed: {}", path.display(), strerror(rc));
        return None;
    }

    let rc = unsafe { sys::libraw_dcraw_process(processor.0) };
    if rc != 0 {
        log::debug!(
            "{}: libraw_dcraw_process failed: {}",
            path.display(),
            strerror(rc)
        );
        return None;
    }

    let mut errc: c_int = 0;
    let image_ptr = unsafe { sys::libraw_dcraw_make_mem_image(processor.0, &mut errc) };
    if image_ptr.is_null() {
        log::debug!(
            "{}: libraw_dcraw_make_mem_image failed: {}",
            path.display(),
            strerror(errc)
        );
        return None;
    }

    // SAFETY: image_ptr is non-null (checked above) and was just returned by
    // libraw_dcraw_make_mem_image, which guarantees a valid
    // libraw_processed_image_t until libraw_dcraw_clear_mem is called on it.
    let gray = unsafe { gray_image_from_processed(path, &*image_ptr) };
    unsafe { sys::libraw_dcraw_clear_mem(image_ptr) };
    gray
}

/// # Safety
/// `img` must be a fully-initialized `libraw_processed_image_t` as returned
/// by `libraw_dcraw_make_mem_image` (specifically: `data_size` bytes must
/// actually be allocated starting at `img.data`'s address, even though the
/// struct declares `data` as a 1-element array -- that's a C flexible array
/// member, which bindgen has no way to express as a real slice).
unsafe fn gray_image_from_processed(
    path: &Path,
    img: &sys::libraw_processed_image_t,
) -> Option<image::GrayImage> {
    if img.type_ != sys::LibRaw_image_formats_LIBRAW_IMAGE_BITMAP {
        log::debug!(
            "{}: unexpected libraw output type {}",
            path.display(),
            img.type_
        );
        return None;
    }
    if img.bits != 8 {
        log::debug!(
            "{}: libraw produced {}-bit output, only 8-bit is handled",
            path.display(),
            img.bits
        );
        return None;
    }

    let width = u32::from(img.width);
    let height = u32::from(img.height);
    let data = unsafe { std::slice::from_raw_parts(img.data.as_ptr(), img.data_size as usize) };

    match img.colors {
        3 => {
            let rgb = image::RgbImage::from_raw(width, height, data.to_vec())?;
            Some(image::DynamicImage::ImageRgb8(rgb).to_luma8())
        }
        1 => image::GrayImage::from_raw(width, height, data.to_vec()),
        n => {
            log::debug!("{}: unexpected color count {n} from libraw", path.display());
            None
        }
    }
}

pub fn decode_thumbnail(path: &Path) -> Option<image::GrayImage> {
    let bytes = extract_thumbnail_bytes(path)?;
    image::load_from_memory(&bytes)
        .ok()
        .map(|img| img.to_luma8())
}

fn extract_thumbnail_bytes(path: &Path) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file);
    let exif = exif::Reader::new().read_from_container(&mut reader).ok()?;

    let get_u32 = |tag: Tag| -> Option<u32> {
        let field = exif.get_field(tag, In::THUMBNAIL)?;
        match &field.value {
            Value::Long(v) => v.first().copied(),
            Value::Short(v) => v.first().map(|&n| u32::from(n)),
            _ => None,
        }
    };

    let offset = get_u32(Tag::JPEGInterchangeFormat)? as usize;
    let length = get_u32(Tag::JPEGInterchangeFormatLength)? as usize;
    let end = offset.checked_add(length)?;
    exif.buf().get(offset..end).map(<[u8]>::to_vec)
}

#[cfg(test)]
mod tests {
    use super::*;

    // There's no real camera RAW fixture available to exercise the full
    // dcraw_process pipeline against, so these only prove the FFI plumbing
    // itself is sound: the library loads, libraw_init/libraw_close don't
    // crash, and failure paths (bad path, garbage file) return None instead
    // of panicking or leaking the LibRaw processor.

    #[test]
    fn processor_init_and_drop_does_not_crash() {
        let processor = Processor::new().expect("libraw_init should succeed");
        drop(processor);
    }

    #[test]
    fn decode_default_returns_none_for_a_nonexistent_path() {
        assert!(decode_default(Path::new("/nonexistent/does-not-exist.cr2")).is_none());
    }

    #[test]
    fn decode_default_returns_none_for_a_non_raw_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not_actually_raw.cr2");
        std::fs::write(&path, b"this is not a raw file").unwrap();
        assert!(decode_default(&path).is_none());
    }

    #[test]
    fn strerror_returns_a_nonempty_message_for_an_error_code() {
        assert!(!strerror(-1).is_empty());
    }
}

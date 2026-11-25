//! Avatars: a picked picture made into the square JPEG the core stores, stored paths resolved, and
//! the picker that does both off the UI thread.
use crate::media::stage::{flatten, open_oriented};
use eframe::egui::{ColorImage, Context};
use image::{codecs::jpeg::JpegEncoder, imageops::FilterType};
use std::{path::{Path, PathBuf}, sync::mpsc::{self, Receiver}};

/// Account avatar cap: contacts drop one over 50 KiB.
pub const ACCOUNT_MAX: usize = 48 << 10;
/// Group avatar cap (Android MAX_AVATAR_SIZE).
pub const GROUP_MAX: usize = 200 << 10;
/// Side of a prepared avatar (Android loadSquareAvatar).
const SIDE: u32 = 256;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AvatarError {
    /// Missing, or not a picture we can decode.
    Unreadable,
    /// Over the cap even at the lowest quality.
    TooLarge,
}

/// Decodes `src` (EXIF orientation applied), flattens transparency onto white, centre-crops it square at 256 px
/// (an already-square smaller picture is kept, as on Android) and encodes a JPEG, quality 95 down to 35 in steps of 5,
/// the first that fits `max_bytes`.
pub fn prepare(src: &Path, max_bytes: usize) -> Result<Vec<u8>, AvatarError> {
    let img = open_oriented(src).ok_or(AvatarError::Unreadable)?;
    let img = if img.width() == img.height() && img.width() <= SIDE { img } else { img.resize_to_fill(SIDE, SIDE, FilterType::CatmullRom) };
    let rgb = flatten(img);
    for quality in (35..=95).rev().step_by(5) {
        let mut out = vec![];
        JpegEncoder::new_with_quality(&mut out, quality).encode_image(&rgb).map_err(|_| AvatarError::Unreadable)?;
        if out.len() <= max_bytes {
            return Ok(out);
        }
    }
    Err(AvatarError::TooLarge)
}

/// A stored avatar string: relative to the core's `data_dir`, or a legacy absolute path; empty means none.
pub fn resolve(data_dir: &Path, stored: &str) -> Option<PathBuf> {
    let p = Path::new(stored);
    (!stored.is_empty()).then(|| if p.is_absolute() { p.to_path_buf() } else { data_dir.join(p) })
}

/// A prepared avatar and its decoded picture (the dialog's preview before it is saved).
pub struct Picked {
    pub bytes: Vec<u8>,
    pub image: ColorImage,
}

/// The picker's answer; the sender is dropped without a message when the dialog is cancelled.
pub type Picking = Receiver<Result<Picked, AvatarError>>;

#[cfg(test)]
thread_local! {
    static PICKED: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Test seam: what the next avatar picker "returns" on this thread (tests never open a dialog).
#[cfg(test)]
pub fn set_picked(path: PathBuf) {
    PICKED.with(|p| *p.borrow_mut() = Some(path));
}

#[cfg(not(test))]
fn picker(filter: String) -> impl FnOnce() -> Option<PathBuf> + Send {
    move || crate::platform::file_dialog().add_filter(filter, &["png", "jpg", "jpeg", "webp", "bmp", "gif"]).pick_file()
}

#[cfg(test)]
fn picker(_filter: String) -> impl FnOnce() -> Option<PathBuf> + Send {
    let picked = PICKED.with(|p| p.borrow_mut().take());
    move || picked
}

/// Asks for a picture (`filter` names the file type) and prepares it for `max_bytes`, all on a thread of its own:
/// the dialog blocks until answered, and decoding a big photo takes a while.
pub fn pick(ctx: &Context, filter: &str, max_bytes: usize) -> Picking {
    let pick = picker(filter.to_string());
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let Some(path) = pick() else { return };
        let picked = prepare(&path, max_bytes).and_then(|bytes| {
            let rgba = image::load_from_memory(&bytes).map_err(|_| AvatarError::Unreadable)?.to_rgba8();
            let image = ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw());
            Ok(Picked { bytes, image })
        });
        let _ = tx.send(picked);
        ctx.request_repaint();
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A photo-like picture: a smooth gradient with a little noise.
    fn photo(path: &Path, w: u32, h: u32) {
        let mut seed = 0x9e37_79b9_u32;
        image::RgbImage::from_fn(w, h, |x, y| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let n = (seed % 24) as u8;
            image::Rgb([(x * 200 / w) as u8 + n, (y * 200 / h) as u8 + n, 120 + n])
        })
        .save(path)
        .unwrap();
    }

    #[test]
    fn prepare_square_256_under_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("wide.png");
        photo(&src, 3000, 1000);
        let bytes = prepare(&src, ACCOUNT_MAX).unwrap();
        assert!(bytes.len() <= ACCOUNT_MAX, "{} bytes", bytes.len());
        assert_eq!(image::guess_format(&bytes).unwrap(), image::ImageFormat::Jpeg);
        let img = image::load_from_memory(&bytes).unwrap();
        assert_eq!((img.width(), img.height()), (256, 256), "centre-cropped square");
        // A cap no quality can meet.
        assert_eq!(prepare(&src, 500), Err(AvatarError::TooLarge));
        // A small square is kept at its size; garbage and missing files are unreadable.
        let small = tmp.path().join("small.png");
        photo(&small, 100, 100);
        assert_eq!(image::load_from_memory(&prepare(&small, GROUP_MAX).unwrap()).unwrap().width(), 100);
        let bad = tmp.path().join("bad.jpg");
        std::fs::write(&bad, b"not a picture").unwrap();
        assert_eq!(prepare(&bad, GROUP_MAX), Err(AvatarError::Unreadable));
        assert_eq!(prepare(&tmp.path().join("gone.png"), GROUP_MAX), Err(AvatarError::Unreadable));
    }

    #[test]
    fn transparent_avatar_flattened_on_white() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("clear.png");
        image::RgbaImage::from_pixel(300, 300, image::Rgba([0, 0, 0, 0])).save(&src).unwrap();
        let img = image::load_from_memory(&prepare(&src, ACCOUNT_MAX).unwrap()).unwrap().to_rgb8();
        let [r, g, b] = img.get_pixel(0, 0).0;
        assert!(r > 245 && g > 245 && b > 245, "white, not black: {:?}", [r, g, b]);
    }

    #[test]
    fn resolve_paths() {
        let data = Path::new("D:/m/mimir");
        assert_eq!(resolve(data, "avatars/ab.avatar"), Some(data.join("avatars/ab.avatar")));
        let abs = std::env::temp_dir().join("old.avatar");
        assert_eq!(resolve(data, &abs.to_string_lossy()), Some(abs), "a legacy absolute path is kept");
        assert_eq!(resolve(data, ""), None);
    }
}

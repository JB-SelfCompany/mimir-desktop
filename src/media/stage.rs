//! Staging a picked, dropped or pasted file for sending (AM §1-3, P3-R3): size limits, MIME by extension (LM §10)
//! and the image shrink rule. Runs off the UI thread (it may decode and re-encode a big picture).
use crate::{i18n::{tr, Key, Lang}, settings::MediaSettings};
use image::{codecs::jpeg::JpegEncoder, imageops::FilterType, DynamicImage, RgbImage};
use std::{fs::File, io::{BufWriter, Write}, path::{Path, PathBuf}};

/// 1:1 send limit (Android MAX_FILE_SIZE).
pub const MAX_FILE: u64 = 1 << 30;
/// Group send limit (Android MAX_GROUP_FILE_SIZE).
pub const MAX_GROUP_FILE: u64 = 500 << 20;
/// Pictures up to this size go untouched; bigger ones are shrunk (Android PICTURE_MAX_SIZE).
const PICTURE_MAX: u64 = 5 << 20;
/// Long edge per `images-format` index (AM §2); 0 = «Не уменьшать», never re-encoded (fixes AM §12.F).
const MAX_EDGES: [u32; 4] = [0, 3840, 1920, 1280];

/// A file ready to hand to the core as an AttachmentSpec.
#[derive(Debug, Clone, PartialEq)]
pub struct Staged {
    pub path: PathBuf,
    pub original_name: String,
    pub mime: String,
    pub msg_type: i32,
    pub size: u64,
    /// `path` is ours (a re-encoded or pasted image): deleted once sent or dropped.
    // ponytail: a crash between staging and send leaves it in the system temp dir; nothing sweeps it.
    pub temp: bool,
    /// A big picture was shrunk (the panel says so, AM `too_big_picture_resizing`).
    pub resized: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StageError {
    /// Over the limit it carries (`MAX_FILE` or `MAX_GROUP_FILE`).
    TooLarge(u64),
    /// Missing, locked, unreadable or empty.
    Unreadable,
    /// A folder (or other non-file).
    NotAFile,
}

/// The inline text for a staging failure (AM §3 texts, the ru limits corrected per AM §12.D).
pub fn error_text(e: &StageError, lang: Lang) -> String {
    tr(lang, match e {
        StageError::TooLarge(MAX_GROUP_FILE) => Key::GroupFileTooLarge,
        StageError::TooLarge(_) => Key::FileTooLargeMax,
        StageError::Unreadable => Key::CannotReadFile,
        StageError::NotAFile => Key::NotAFile,
    })
    .to_string()
}

/// MIME and message type by extension (LM §10): 1 for pictures, 3 for everything else.
pub fn mime_for(path: &Path) -> (&'static str, i32) {
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "mp3" => "audio/mpeg",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "7z" => "application/x-7z-compressed",
        "txt" | "md" | "log" => "text/plain",
        _ => "application/octet-stream",
    };
    (mime, if mime.starts_with("image/") { 1 } else { 3 })
}

/// Stages `src` for a 1:1 chat (or Saved Messages).
pub fn stage_path(src: &Path, media: &MediaSettings) -> Result<Staged, StageError> {
    stage(src, media, MAX_FILE)
}

/// Stages `src` for a chat whose limit is `limit` bytes.
pub fn stage(src: &Path, media: &MediaSettings, limit: u64) -> Result<Staged, StageError> {
    let meta = std::fs::metadata(src).map_err(|_| StageError::Unreadable)?;
    if !meta.is_file() {
        return Err(StageError::NotAFile);
    }
    if meta.len() > limit {
        return Err(StageError::TooLarge(limit));
    }
    // Locked or forbidden: said now in the panel, not later by the core's copy.
    if meta.len() == 0 || File::open(src).is_err() {
        return Err(StageError::Unreadable);
    }
    let original_name = src.file_name().map_or("file".into(), |n| n.to_string_lossy().into_owned());
    let (mime, msg_type) = mime_for(src);
    let s = Staged { path: src.to_path_buf(), original_name, mime: mime.into(), msg_type, size: meta.len(), temp: false, resized: false };
    Ok(if msg_type == 1 { shrink(s, media) } else { s })
}

/// P3-R3: a picture over 5 MiB is resized to the configured long edge and re-encoded as JPEG; a GIF never is
/// (it would lose its animation), nor is any picture with «Не уменьшать». Whatever comes out (also a picture that
/// will not decode, or is still over 5 MiB) is sent as a picture message: never dropped, never downgraded.
// ponytail: a picture at or under 5 MiB keeps its bytes and its EXIF orientation tag (ruling I2); receivers that
// honour EXIF (Android Glide, this app) show it upright, an EXIF-blind one shows it sideways.
fn shrink(s: Staged, media: &MediaSettings) -> Staged {
    let edge = MAX_EDGES.get(media.image_format as usize).copied().unwrap_or(0);
    if s.size <= PICTURE_MAX || edge == 0 || s.mime == "image/gif" {
        return s;
    }
    reencode(&s, edge, media.image_quality).unwrap_or(s)
}

fn reencode(s: &Staged, edge: u32, quality: u8) -> Option<Staged> {
    let img = open_oriented(&s.path)?;
    let img = if img.width().max(img.height()) > edge { img.resize(edge, edge, FilterType::CatmullRom) } else { img };
    let path = temp_path("jpg");
    let written = (|| {
        let mut w = BufWriter::new(File::create(&path).ok()?);
        JpegEncoder::new_with_quality(&mut w, quality.clamp(1, 100)).encode_image(&flatten(img)).ok()?;
        w.flush().ok()?;
        std::fs::metadata(&path).ok()
    })();
    let Some(meta) = written else {
        let _ = std::fs::remove_file(&path);
        return None;
    };
    let stem = Path::new(&s.original_name).file_stem().map_or("image".into(), |n| n.to_string_lossy().into_owned());
    Some(Staged { path, original_name: format!("{stem}.jpg"), mime: "image/jpeg".into(), msg_type: 1, size: meta.len(), temp: true, resized: true })
}

/// Decodes `path` by content with its EXIF orientation applied, as Android does (the JPEGs we write carry no EXIF).
pub(crate) fn open_oriented(path: &Path) -> Option<DynamicImage> {
    let mut decoder = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.into_decoder().ok()?;
    let orientation = image::ImageDecoder::orientation(&mut decoder).unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder).ok()?;
    img.apply_orientation(orientation);
    Some(img)
}

/// JPEG has no alpha: transparent pixels become white, not black (LM §6 to_rgb8 bug).
pub(crate) fn flatten(img: DynamicImage) -> RgbImage {
    if !img.color().has_alpha() {
        return img.to_rgb8();
    }
    let rgba = img.to_rgba8();
    RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let [r, g, b, a] = rgba.get_pixel(x, y).0;
        let over = |c: u8| ((c as u32 * a as u32 + 255 * (255 - a as u32)) / 255) as u8;
        image::Rgb([over(r), over(g), over(b)])
    })
}

/// A pasted clipboard picture (egui's `Event::PasteImage`), written as a PNG and staged like a picked one.
pub fn stage_pasted(img: &eframe::egui::ColorImage, media: &MediaSettings, limit: u64) -> Result<Staged, StageError> {
    let [w, h] = img.size;
    let bytes = img.pixels.iter().flat_map(|c| c.to_srgba_unmultiplied()).collect();
    let png = temp_path("png");
    let saved = image::RgbaImage::from_raw(w as u32, h as u32, bytes).is_some_and(|b| b.save_with_format(&png, image::ImageFormat::Png).is_ok());
    let staged = if saved { stage(&png, media, limit) } else { Err(StageError::Unreadable) };
    if staged.as_ref().map_or(true, |s| s.path != png) {
        let _ = std::fs::remove_file(&png); // re-encoded into another temp file, or failed
    }
    staged.map(|s| Staged { original_name: if s.mime == "image/jpeg" { "image.jpg" } else { "image.png" }.into(), temp: true, ..s })
}

/// Drops a staged file that will not be sent; a temp file of ours is deleted.
pub fn discard(s: Staged) {
    if s.temp {
        let _ = std::fs::remove_file(&s.path);
    }
}

/// A fresh path in the system temp dir.
pub(crate) fn temp_path(ext: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join(format!("mimir_staged_{}_{t}_{n}.{ext}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;
    use image::{ImageEncoder, codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder}};

    fn png(path: &Path, w: u32, h: u32, noise_rows: u32) {
        // Noise in the first rows (incompressible), flat grey below: a big file that still decodes fast.
        let mut seed = 0x2545_f491_u32;
        let px: Vec<u8> = (0..w * h * 3)
            .map(|i| if i / (w * 3) < noise_rows { seed ^= seed << 13; seed ^= seed >> 17; seed ^= seed << 5; seed as u8 } else { 128 })
            .collect();
        let f = std::io::BufWriter::new(File::create(path).unwrap());
        PngEncoder::new_with_quality(f, CompressionType::Fast, PngFilter::NoFilter).write_image(&px, w, h, image::ExtendedColorType::Rgb8).unwrap();
    }

    #[test]
    fn mime_table() {
        // LM §10 by extension (lowercased); only images are type 1.
        for (name, mime, t) in [
            ("a.PNG", "image/png", 1), ("a.jpeg", "image/jpeg", 1), ("a.gif", "image/gif", 1), ("a.webp", "image/webp", 1), ("a.bmp", "image/bmp", 1),
            ("a.mp3", "audio/mpeg", 3), ("a.opus", "audio/ogg", 3), ("a.m4a", "audio/mp4", 3), ("a.wav", "audio/wav", 3),
            ("a.mp4", "video/mp4", 3), ("a.mkv", "video/x-matroska", 3), ("a.pdf", "application/pdf", 3), ("a.log", "text/plain", 3),
            ("a.zip", "application/zip", 3), ("noext", "application/octet-stream", 3), ("a.exe", "application/octet-stream", 3),
        ] {
            assert_eq!(mime_for(Path::new(name)), (mime, t), "{name}");
        }
    }

    #[test]
    fn stage_small_image_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("x.png");
        png(&src, 40, 30, 30);
        let s = stage_path(&src, &MediaSettings { image_format: 2, ..Default::default() }).unwrap();
        let size = std::fs::metadata(&src).unwrap().len();
        assert_eq!(s, Staged { path: src, original_name: "x.png".into(), mime: "image/png".into(), msg_type: 1, size, temp: false, resized: false });
    }

    #[test]
    fn stage_big_image_resized_to_edge() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("x.png");
        png(&src, 4000, 3000, 600);
        let raw = std::fs::metadata(&src).unwrap().len();
        assert!(raw > 6 << 20, "a 6 MiB source, got {raw}");
        let s = stage_path(&src, &MediaSettings { image_format: 2, image_quality: 80, ..Default::default() }).unwrap();
        assert_eq!((s.original_name.as_str(), s.mime.as_str(), s.msg_type, s.temp, s.resized), ("x.jpg", "image/jpeg", 1, true, true));
        assert_ne!(s.path, src);
        assert_eq!(image::image_dimensions(&s.path).unwrap(), (1920, 1440), "long edge exactly FullHD");
        assert_eq!(s.size, std::fs::metadata(&s.path).unwrap().len());
        assert!(s.size <= PICTURE_MAX);
        assert_eq!(image::ImageReader::open(&s.path).unwrap().with_guessed_format().unwrap().format(), Some(image::ImageFormat::Jpeg));
        std::fs::remove_file(&s.path).unwrap();
        // «Не уменьшать» (format 0) sends it unchanged, still as a picture (P3-R3 as ruled).
        let s = stage_path(&src, &MediaSettings::default()).unwrap();
        assert_eq!((&s.path, s.msg_type, s.mime.as_str(), s.temp, s.resized), (&src, 1, "image/png", false, false));
    }

    #[test]
    fn transparent_pixels_flatten_onto_white() {
        let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 0])));
        assert_eq!(flatten(img).get_pixel(0, 0).0, [255, 255, 255]);
    }

    #[test]
    fn reencode_applies_exif_orientation() {
        // A 40×20 JPEG tagged Orientation=6 (rotate 90° clockwise to display) comes out upright: 20×40.
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("phone.jpg");
        let exif = [0x49, 0x49, 0x2A, 0, 8, 0, 0, 0, 1, 0, 0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0];
        let mut enc = JpegEncoder::new_with_quality(BufWriter::new(File::create(&src).unwrap()), 90);
        image::ImageEncoder::set_exif_metadata(&mut enc, exif.to_vec()).unwrap();
        enc.encode_image(&RgbImage::from_pixel(40, 20, image::Rgb([200, 10, 10]))).unwrap();
        drop(enc);
        let staged = Staged { path: src.clone(), original_name: "phone.jpg".into(), mime: "image/jpeg".into(), msg_type: 1, size: 1, temp: false, resized: false };
        let out = reencode(&staged, 1920, 90).unwrap();
        assert_eq!(image::image_dimensions(&out.path).unwrap(), (20, 40));
        discard(out);
    }

    #[test]
    fn gif_never_reencoded() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("anim.gif");
        let f = File::create(&src).unwrap();
        f.set_len(6 << 20).unwrap();
        let s = stage_path(&src, &MediaSettings { image_format: 3, ..Default::default() }).unwrap();
        assert_eq!((&s.path, s.mime.as_str(), s.temp, s.size), (&src, "image/gif", false, 6 << 20), "bytes untouched, animation kept");
        assert_eq!((s.msg_type, s.resized), (1, false), "still a picture message");
    }

    #[test]
    fn oversize_file_rejected_with_limit_text() {
        let tmp = tempfile::tempdir().unwrap();
        let big = tmp.path().join("big.bin");
        File::create(&big).unwrap().set_len(MAX_FILE + 1).unwrap();
        let e = stage_path(&big, &MediaSettings::default()).unwrap_err();
        assert_eq!(e, StageError::TooLarge(MAX_FILE));
        assert_eq!(error_text(&e, Lang::Ru), "Файл слишком большой. Максимальный размер 1024 МБ.");
        assert_eq!(error_text(&e, Lang::En), "File is too large. Maximum size is 1024 MB.");
        let mid = tmp.path().join("mid.bin");
        File::create(&mid).unwrap().set_len(MAX_GROUP_FILE + 1).unwrap();
        assert!(stage_path(&mid, &MediaSettings::default()).is_ok(), "fits a 1:1 chat");
        let e = stage(&mid, &MediaSettings::default(), MAX_GROUP_FILE).unwrap_err();
        assert_eq!(e, StageError::TooLarge(MAX_GROUP_FILE));
        assert_eq!(error_text(&e, Lang::Ru), "Файл слишком велик для группового чата. Максимальный размер 500 МБ.");
        assert_eq!(error_text(&e, Lang::En), "File is too large for group chat. Maximum size is 500 MB.");
    }

    #[test]
    fn unreadable_file_reports_error() {
        let tmp = tempfile::tempdir().unwrap();
        let m = MediaSettings::default();
        assert_eq!(stage_path(&tmp.path().join("gone.txt"), &m), Err(StageError::Unreadable));
        assert_eq!(stage_path(tmp.path(), &m), Err(StageError::NotAFile));
        let empty = tmp.path().join("empty.txt");
        File::create(&empty).unwrap();
        assert_eq!(stage_path(&empty, &m), Err(StageError::Unreadable), "zero bytes: nothing to send");
        assert_eq!(error_text(&StageError::Unreadable, Lang::Ru), "Не удалось прочитать файл");
        assert_eq!(error_text(&StageError::NotAFile, Lang::En), "Only files can be sent, not folders");
    }

    #[test]
    fn pasted_image_is_a_temp_png() {
        let img = eframe::egui::ColorImage::new([4, 3], vec![eframe::egui::Color32::RED; 12]);
        let s = stage_pasted(&img, &MediaSettings::default(), MAX_FILE).unwrap();
        assert_eq!((s.original_name.as_str(), s.mime.as_str(), s.msg_type, s.temp), ("image.png", "image/png", 1, true));
        let back = image::open(&s.path).unwrap().to_rgba8();
        assert_eq!((back.dimensions(), back.get_pixel(0, 0).0), ((4, 3), [255, 0, 0, 255]));
        discard(s.clone());
        assert!(!s.path.exists(), "a temp file goes with it");
    }
}

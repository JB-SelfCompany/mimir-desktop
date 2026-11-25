//! Off-thread image decoding into textures (AM §12.G): chat thumbnails, and a larger copy for the viewer.
use eframe::egui::{ColorImage, Context, TextureHandle, TextureOptions};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    time::SystemTime,
};

/// Long edge of a chat thumbnail (Android createImagePreview).
pub const THUMB_PX: u32 = 512;

#[derive(Clone)]
pub enum Thumb {
    Loading,
    /// The texture and the image's original size.
    Ready(TextureHandle, [u32; 2]),
    Failed,
}

/// (file, size cap, centre-cropped square).
type Key = (PathBuf, u32, bool);
type Decoded = (Key, Option<SystemTime>, Result<(ColorImage, [u32; 2]), String>);

/// Textures by path and size cap, each tagged with the file mtime it was decoded from.
// ponytail: chat thumbnails (≤ 1 MiB of texture each) are never evicted; every picture seen this session stays resident.
#[derive(Default)]
pub struct ThumbCache {
    map: HashMap<Key, (Option<SystemTime>, Thumb)>,
    /// The decoder thread's job queue and results, started on first use.
    lane: Option<(Sender<(Key, Option<SystemTime>)>, Receiver<Decoded>)>,
}

impl ThumbCache {
    /// A chat thumbnail of `path` (≤ 512 px long edge).
    pub fn get(&mut self, ctx: &Context, path: &Path) -> Thumb {
        self.get_sized(ctx, path, THUMB_PX)
    }

    /// `path` scaled down to at most `max` px on the long edge; decoded again when the file's mtime changes.
    pub fn get_sized(&mut self, ctx: &Context, path: &Path, max: u32) -> Thumb {
        self.get_key(ctx, (path.to_path_buf(), max, false))
    }

    /// `path` centre-cropped to a square of at most `px` px (avatars); never upscaled.
    pub fn get_square(&mut self, ctx: &Context, path: &Path, px: u32) -> Thumb {
        self.get_key(ctx, (path.to_path_buf(), px, true))
    }

    fn get_key(&mut self, ctx: &Context, key: Key) -> Thumb {
        self.collect(ctx);
        let mtime = std::fs::metadata(&key.0).and_then(|m| m.modified()).ok();
        if let Some((_, thumb)) = self.map.get(&key).filter(|(t, _)| *t == mtime) {
            return thumb.clone();
        }
        let thumb = if mtime.is_none() {
            Thumb::Failed // no such file
        } else {
            let (jobs, _) = self.lane.get_or_insert_with(|| spawn(ctx.clone()));
            let _ = jobs.send((key.clone(), mtime));
            Thumb::Loading
        };
        self.map.insert(key, (mtime, thumb.clone()));
        thumb
    }

    /// Some image is still being decoded.
    pub fn loading(&self) -> bool {
        self.map.values().any(|(_, t)| matches!(t, Thumb::Loading))
    }

    /// Releases the `max`-capped texture of `path` (the viewer's large copy when it closes).
    pub fn forget(&mut self, path: &Path, max: u32) {
        self.map.remove(&(path.to_path_buf(), max, false));
    }

    #[cfg(test)]
    pub fn cached(&self, path: &Path, max: u32) -> bool {
        self.map.contains_key(&(path.to_path_buf(), max, false))
    }

    /// The decoded square texture of `path` at `px`, if any.
    #[cfg(test)]
    pub fn square_texture(&self, path: &Path, px: u32) -> Option<eframe::egui::TextureId> {
        match self.map.get(&(path.to_path_buf(), px, true)) {
            Some((_, Thumb::Ready(t, _))) => Some(t.id()),
            _ => None,
        }
    }

    /// Forgets every texture of `path` (its file was deleted or replaced).
    pub fn invalidate(&mut self, path: &Path) {
        self.map.retain(|(p, ..), _| p != path);
    }

    fn collect(&mut self, ctx: &Context) {
        let Some((_, done)) = &self.lane else { return };
        while let Ok((key, mtime, result)) = done.try_recv() {
            // A result for a file that changed (or was invalidated) since is dropped; a newer job follows.
            if self.map.get(&key).is_some_and(|(t, thumb)| *t == mtime && matches!(thumb, Thumb::Loading)) {
                let thumb = match result {
                    Ok((img, dims)) => Thumb::Ready(ctx.load_texture(key.0.to_string_lossy(), img, TextureOptions::LINEAR), dims),
                    Err(_) => Thumb::Failed,
                };
                self.map.insert(key, (mtime, thumb));
            }
        }
    }
}

/// One decoder thread for the whole cache; it ends when the cache (its job sender) is dropped.
// ponytail: one thread, first come first served; a page of big photos delays the viewer's decode behind them.
fn spawn(ctx: Context) -> (Sender<(Key, Option<SystemTime>)>, Receiver<Decoded>) {
    let (jobs, queue) = mpsc::channel::<(Key, Option<SystemTime>)>();
    let (results, done) = mpsc::channel();
    std::thread::spawn(move || {
        for (key, mtime) in queue {
            let decoded = decode(&key.0, key.1, key.2);
            if results.send((key, mtime, decoded)).is_err() {
                break;
            }
            ctx.request_repaint();
        }
    });
    (jobs, done)
}

/// Decodes by content (not the extension), EXIF orientation applied, and scales down to `max` px on the long
/// edge, aspect kept; or, for `square`, crops the centre square and scales it down to `max` px.
fn decode(path: &Path, max: u32, square: bool) -> Result<(ColorImage, [u32; 2]), String> {
    let img = crate::media::stage::open_oriented(path).ok_or("not a picture")?;
    let dims = [img.width(), img.height()];
    let side = max.min(dims[0]).min(dims[1]);
    let img = match square {
        true if dims != [side, side] => img.resize_to_fill(side, side, image::imageops::FilterType::Triangle),
        false if dims[0].max(dims[1]) > max => img.thumbnail(max, max),
        _ => img,
    };
    let rgba = img.to_rgba8();
    Ok((ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw()), dims))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    pub fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = std::io::Cursor::new(vec![]);
        image::RgbImage::from_pixel(w, h, image::Rgb([200, 80, 40])).write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    fn wait(c: &mut ThumbCache, ctx: &Context, p: &Path) -> Thumb {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            match c.get(ctx, p) {
                Thumb::Loading if Instant::now() < end => std::thread::sleep(Duration::from_millis(10)),
                t => return t,
            }
        }
    }

    #[test]
    fn decodes_off_thread_downscaled_and_follows_mtime() {
        let tmp = tempfile::tempdir().unwrap();
        let (ctx, p) = (Context::default(), tmp.path().join("a.png"));
        std::fs::write(&p, png(1200, 800)).unwrap();
        let mut c = ThumbCache::default();
        assert!(matches!(c.get(&ctx, &p), Thumb::Loading), "never decoded on the caller's thread");
        assert!(c.loading());
        let Thumb::Ready(tex, dims) = wait(&mut c, &ctx, &p) else { panic!("not decoded") };
        assert_eq!((dims, tex.size()), ([1200, 800], [512, 341]), "original size kept, texture capped at 512 px");
        assert!(!c.loading());
        // A changed file (new mtime) is decoded again.
        std::fs::write(&p, png(100, 50)).unwrap();
        let later = std::time::SystemTime::now() + Duration::from_secs(5);
        std::fs::File::options().write(true).open(&p).unwrap().set_modified(later).unwrap();
        let Thumb::Ready(tex, dims) = wait(&mut c, &ctx, &p) else { panic!() };
        assert_eq!((dims, tex.size()), ([100, 50], [100, 50]), "small images are not upscaled");
        c.invalidate(&p);
        assert!(matches!(c.get(&ctx, &p), Thumb::Loading), "invalidate forgets the texture");
        // Garbage and missing files fail instead of panicking.
        let bad = tmp.path().join("bad.jpg");
        std::fs::write(&bad, b"not a jpeg at all").unwrap();
        assert!(matches!(wait(&mut c, &ctx, &bad), Thumb::Failed));
        assert!(matches!(wait(&mut c, &ctx, &tmp.path().join("gone.png")), Thumb::Failed));
    }

    #[test]
    fn exif_orientation_is_applied() {
        // I2: a 40×20 JPEG tagged Orientation=6 (a phone photo) shows upright, 20×40, in bubbles and the viewer.
        let tmp = tempfile::tempdir().unwrap();
        let (ctx, p) = (Context::default(), tmp.path().join("phone.jpg"));
        let exif = [0x49, 0x49, 0x2A, 0, 8, 0, 0, 0, 1, 0, 0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0];
        let mut enc = image::codecs::jpeg::JpegEncoder::new(std::fs::File::create(&p).unwrap());
        image::ImageEncoder::set_exif_metadata(&mut enc, exif.to_vec()).unwrap();
        enc.encode_image(&image::RgbImage::from_pixel(40, 20, image::Rgb([200, 10, 10]))).unwrap();
        drop(enc);
        let mut c = ThumbCache::default();
        let Thumb::Ready(tex, dims) = wait(&mut c, &ctx, &p) else { panic!("not decoded") };
        assert_eq!((dims, tex.size()), ([20, 40], [20, 40]));
    }

    #[test]
    fn square_is_centre_cropped_and_never_upscaled() {
        let tmp = tempfile::tempdir().unwrap();
        let (ctx, p) = (Context::default(), tmp.path().join("wide.png"));
        std::fs::write(&p, png(300, 100)).unwrap();
        let mut c = ThumbCache::default();
        let end = Instant::now() + Duration::from_secs(10);
        let mut get = |px| loop {
            match c.get_square(&ctx, &p, px) {
                Thumb::Loading if Instant::now() < end => std::thread::sleep(Duration::from_millis(10)),
                t => break t,
            }
        };
        let Thumb::Ready(tex, dims) = get(48) else { panic!("not decoded") };
        assert_eq!((dims, tex.size()), ([300, 100], [48, 48]));
        let Thumb::Ready(tex, _) = get(128) else { panic!() };
        assert_eq!(tex.size(), [100, 100], "short edge kept, not upscaled");
        assert!(!c.cached(&p, 48), "squares do not collide with long-edge thumbnails");
        c.invalidate(&p);
        assert!(matches!(c.get_square(&ctx, &p, 48), Thumb::Loading), "invalidate forgets squares too");
    }
}

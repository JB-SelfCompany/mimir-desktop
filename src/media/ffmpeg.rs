//! The external ffmpeg: looked for next to the executable (the Windows release bundles one there), then on PATH. It encodes voice
//! messages and makes video posters. Every call blocks: run them off the UI thread.
use eframe::egui::Context;
use std::{
    collections::HashMap,
    ffi::OsString,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{mpsc::{self, Receiver, Sender}, OnceLock},
};

const EXE: &str = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };

/// ffmpeg next to our executable, else the first one on PATH.
// ponytail: looked up once per session; ffmpeg installed while the app runs is found after a restart.
pub fn locate() -> Option<PathBuf> {
    static FOUND: OnceLock<Option<PathBuf>> = OnceLock::new();
    FOUND
        .get_or_init(|| {
            let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
            locate_in(exe_dir.as_deref(), std::env::var_os("PATH"))
        })
        .clone()
}

fn locate_in(exe_dir: Option<&Path>, path: Option<OsString>) -> Option<PathBuf> {
    let dirs: Vec<PathBuf> = path.map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    exe_dir.map(Path::to_path_buf).into_iter().chain(dirs).map(|d| d.join(EXE)).find(|p| p.is_file())
}

/// \`bin\` with no console window (Windows CREATE_NO_WINDOW), no stdin or stdout, stderr captured.
pub fn command(bin: &Path) -> Command {
    let mut cmd = Command::new(bin);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

/// Raw f32 PCM on stdin to AAC-LC 44.1 kHz mono in MP4 (the voice wire format).
fn voice_args(rate: u32, channels: u16, bitrate: u32, out: &Path) -> Vec<OsString> {
    let (rate, channels, bitrate) = (rate.to_string(), channels.to_string(), bitrate.to_string());
    let mut args: Vec<OsString> = ["-hide_banner", "-loglevel", "error", "-f", "f32le", "-ar", &rate, "-ac", &channels, "-i", "pipe:0"]
        .into_iter()
        .chain(["-ar", "44100", "-ac", "1", "-c:a", "aac", "-b:a", &bitrate, "-movflags", "+faststart"])
        .map(OsString::from)
        .collect();
    args.push(out.into());
    args
}

/// Encodes interleaved \`pcm\` into \`out\` (a new path: no -y, so ffmpeg never asks on stdin). On failure \`out\` is
/// removed and the error is ffmpeg's last stderr line.
pub fn encode_voice(bin: &Path, pcm: &[f32], rate: u32, channels: u16, bitrate: u32, out: &Path) -> Result<(), String> {
    let run = || {
        let mut child = command(bin).args(voice_args(rate, channels, bitrate, out)).stdin(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        // Written from a thread of its own while stderr is read here, so neither pipe can stall the other.
        // A failed write means ffmpeg quit early, which its exit status reports; dropping stdin is the end of input.
        let writer = std::thread::spawn(move || drop(stdin.write_all(&bytes)));
        let output = child.wait_with_output().map_err(|e| e.to_string())?;
        let _ = writer.join();
        if output.status.success() { Ok(()) } else { Err(failure(&output)) }
    };
    run().inspect_err(|_| drop(std::fs::remove_file(out)))
}

/// The first frame of \`video\` as \`out_jpg\`, and the clip's duration in ms when ffmpeg reports one.
pub fn video_poster(bin: &Path, video: &Path, out_jpg: &Path) -> Result<Option<u64>, String> {
    let output = command(bin)
        .args(["-hide_banner", "-y", "-i"])
        .arg(video)
        .args(["-frames:v", "1", "-q:v", "4"])
        .arg(out_jpg)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() || !out_jpg.is_file() {
        let _ = std::fs::remove_file(out_jpg);
        return Err(failure(&output));
    }
    Ok(parse_duration(&String::from_utf8_lossy(&output.stderr)))
}

fn failure(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    stderr.lines().rev().map(str::trim).find(|l| !l.is_empty()).map_or_else(|| output.status.to_string(), str::to_string)
}

/// \`Duration: HH:MM:SS.xx\` from ffmpeg's input summary.
fn parse_duration(stderr: &str) -> Option<u64> {
    let t = stderr.split("Duration: ").nth(1)?.split(',').next()?;
    let mut parts = t.trim().split(':');
    let (h, m, s): (u64, u64, f64) = (parts.next()?.parse().ok()?, parts.next()?.parse().ok()?, parts.next()?.parse().ok()?);
    Some((h * 3600 + m * 60) * 1000 + (s * 1000.).round() as u64)
}

#[derive(Debug, Clone, PartialEq)]
pub enum Poster {
    Pending,
    /// The poster JPEG and the clip's duration in ms, if known.
    Ready(PathBuf, Option<u64>),
    /// No ffmpeg and nothing cached, or ffmpeg could not read the clip: the neutral tile.
    Failed,
}

/// (video, cache dir, ffmpeg) in; (video, poster) out.
type Job = (PathBuf, PathBuf, Option<PathBuf>);
type Lane = (Sender<Job>, Receiver<(PathBuf, Poster)>);

/// Video posters, cached as \`<dir>/<video file name>.jpg\` with the duration in a \`.dur\` file beside it.
// ponytail: one ffmpeg at a time on one thread; a chat full of new videos fills in one tile after another.
#[derive(Default)]
pub struct Posters {
    map: HashMap<PathBuf, Poster>,
    lane: Option<Lane>,
}

impl Posters {
    /// The poster of \`video\`: from the cache in \`dir\`, else made with \`bin\` off the UI thread.
    pub fn get(&mut self, ctx: &Context, bin: Option<&Path>, video: &Path, dir: &Path) -> Poster {
        if let Some((_, done)) = &self.lane {
            while let Ok((v, p)) = done.try_recv() {
                self.map.insert(v, p);
            }
        }
        if let Some(p) = self.map.get(video) {
            return p.clone();
        }
        let (jobs, _) = self.lane.get_or_insert_with(|| spawn(ctx.clone()));
        let _ = jobs.send((video.to_path_buf(), dir.to_path_buf(), bin.map(Path::to_path_buf)));
        self.map.insert(video.to_path_buf(), Poster::Pending);
        Poster::Pending
    }
}

fn spawn(ctx: Context) -> Lane {
    let (jobs, queue) = mpsc::channel::<Job>();
    let (results, done) = mpsc::channel();
    std::thread::spawn(move || {
        for (video, dir, bin) in queue {
            let poster = make(&video, &dir, bin.as_deref());
            if results.send((video, poster)).is_err() {
                break;
            }
            ctx.request_repaint();
        }
    });
    (jobs, done)
}

fn make(video: &Path, dir: &Path, bin: Option<&Path>) -> Poster {
    let Some(name) = video.file_name() else { return Poster::Failed };
    let jpg = dir.join(format!("{}.jpg", name.to_string_lossy()));
    let dur = jpg.with_extension("dur");
    if jpg.is_file() {
        return Poster::Ready(jpg, std::fs::read_to_string(&dur).ok().and_then(|s| s.trim().parse().ok()));
    }
    let Some(bin) = bin else { return Poster::Failed };
    let _ = std::fs::create_dir_all(dir);
    // Made under a temp name and renamed: a crash mid-write never leaves a broken poster that looks cached.
    let part = dir.join(format!("{}.part.jpg", name.to_string_lossy()));
    match video_poster(bin, video, &part) {
        Ok(ms) => {
            if let Some(ms) = ms {
                let _ = std::fs::write(&dur, ms.to_string());
            }
            if std::fs::rename(&part, &jpg).is_err() {
                let _ = std::fs::remove_file(&part);
                return Poster::Failed;
            }
            Poster::Ready(jpg, ms)
        }
        Err(_) => Poster::Failed,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A stand-in ffmpeg (a .cmd on Windows, a shell script elsewhere): prints \`stderr\` to stderr, puts a file at its
    /// last argument (a copy of \`copy\`, or a few bytes) and exits with \`code\`. It never reads stdin.
    pub fn fake_ffmpeg(dir: &Path, code: i32, stderr: &str, copy: Option<&Path>) -> PathBuf {
        #[cfg(windows)]
        {
            let put = match copy {
                Some(src) => format!("copy /y \"{}\" \"%last%\" >nul", src.display()),
                None => "echo fake>\"%last%\"".into(),
            };
            let echo = if stderr.is_empty() { String::new() } else { format!("echo {stderr} 1>&2\r\n") };
            let bin = dir.join("ffmpeg.cmd");
            std::fs::write(&bin, format!("@echo off\r\n{echo}for %%a in (%*) do set \"last=%%~a\"\r\n{put}\r\nexit /b {code}\r\n")).unwrap();
            bin
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::PermissionsExt;
            let put = match copy {
                Some(src) => format!("cp '{}' \"$last\"", src.display()),
                None => "echo fake > \"$last\"".into(),
            };
            let bin = dir.join("ffmpeg");
            std::fs::write(&bin, format!("#!/bin/sh\necho '{stderr}' >&2\nfor a; do last=$a; done\n{put}\nexit {code}\n")).unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
            bin
        }
    }

    #[test]
    fn ffmpeg_locate_prefers_exe_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let (exe_dir, on_path, other) = (tmp.path().join("app"), tmp.path().join("bin"), tmp.path().join("empty"));
        for d in [&exe_dir, &on_path, &other] {
            std::fs::create_dir(d).unwrap();
        }
        std::fs::write(exe_dir.join(EXE), b"").unwrap();
        std::fs::write(on_path.join(EXE), b"").unwrap();
        let path = std::env::join_paths([&other, &on_path]).unwrap();
        assert_eq!(locate_in(Some(&exe_dir), Some(path.clone())), Some(exe_dir.join(EXE)), "next to the executable first");
        std::fs::remove_file(exe_dir.join(EXE)).unwrap();
        assert_eq!(locate_in(Some(&exe_dir), Some(path)), Some(on_path.join(EXE)), "then PATH, in order");
        assert_eq!(locate_in(Some(&exe_dir), Some(other.into_os_string())), None);
        assert_eq!(locate_in(None, None), None);
    }

    #[test]
    fn encode_voice_command_line() {
        let out = Path::new("v.m4a");
        let args: Vec<String> = voice_args(48000, 2, 64000, out).iter().map(|a| a.to_string_lossy().into_owned()).collect();
        let want = "-hide_banner -loglevel error -f f32le -ar 48000 -ac 2 -i pipe:0 -ar 44100 -ac 1 -c:a aac -b:a 64000 -movflags +faststart v.m4a";
        assert_eq!(args.join(" "), want);
    }

    #[test]
    fn encode_writes_output_and_failure_leaves_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("v.m4a");
        let ok = fake_ffmpeg(tmp.path(), 0, "", None);
        // A whole second of stereo 48 kHz (384 KB): more than any pipe buffer, written while ffmpeg runs.
        encode_voice(&ok, &vec![0.25; 96_000], 48000, 2, 32000, &out).unwrap();
        assert!(out.is_file());
        std::fs::remove_file(&out).unwrap();
        let bad_dir = tmp.path().join("bad");
        std::fs::create_dir(&bad_dir).unwrap();
        let bad = fake_ffmpeg(&bad_dir, 1, "Unknown encoder aac", None);
        let err = encode_voice(&bad, &vec![0.; 96_000], 48000, 2, 32000, &out).unwrap_err();
        assert!(err.contains("Unknown encoder aac"), "ffmpeg's own words: {err}");
        assert!(!out.exists(), "a half-written file is removed");
        let err = encode_voice(&tmp.path().join("missing"), &[0.], 8000, 1, 32000, &out).unwrap_err();
        assert!(!err.is_empty() && !out.exists());
    }

    #[test]
    fn poster_and_duration() {
        let tmp = tempfile::tempdir().unwrap();
        let jpg = tmp.path().join("src.jpg");
        image::RgbImage::from_pixel(8, 6, image::Rgb([10, 200, 10])).save(&jpg).unwrap();
        let bin = fake_ffmpeg(tmp.path(), 1, "  Duration: 00:01:05.20, start: 0.000000, bitrate: 900 kb/s", Some(&jpg));
        let out = tmp.path().join("poster.jpg");
        // ffmpeg -i without an output exits 1; here it is a failed poster: no file kept.
        assert!(video_poster(&bin, Path::new("v.mp4"), &out).is_err());
        assert!(!out.exists());
        let bin = fake_ffmpeg(tmp.path(), 0, "  Duration: 00:01:05.20, start: 0.000000, bitrate: 900 kb/s", Some(&jpg));
        assert_eq!(video_poster(&bin, Path::new("v.mp4"), &out), Ok(Some(65_200)));
        assert_eq!(image::image_dimensions(&out).unwrap(), (8, 6));
        assert_eq!(parse_duration("Duration: N/A, bitrate: N/A"), None);
        assert_eq!(parse_duration("  Duration: 01:00:00.04, start"), Some(3_600_040));
    }

    #[test]
    fn posters_are_cached_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let jpg = tmp.path().join("src.jpg");
        image::RgbImage::from_pixel(8, 6, image::Rgb([10, 200, 10])).save(&jpg).unwrap();
        let bin = fake_ffmpeg(tmp.path(), 0, "  Duration: 00:00:07.00, start: 0", Some(&jpg));
        let (ctx, dir, video) = (eframe::egui::Context::default(), tmp.path().join(".thumbs"), tmp.path().join("Vv1.mp4"));
        std::fs::write(&video, b"mp4").unwrap();
        let mut posters = Posters::default();
        let wait = |posters: &mut Posters, bin: Option<&Path>| {
            for _ in 0..500 {
                match posters.get(&ctx, bin, &video, &dir) {
                    Poster::Pending => std::thread::sleep(std::time::Duration::from_millis(10)),
                    p => return p,
                }
            }
            panic!("no poster");
        };
        assert_eq!(wait(&mut posters, Some(&bin)), Poster::Ready(dir.join("Vv1.mp4.jpg"), Some(7000)));
        assert!(dir.join("Vv1.mp4.jpg").is_file());
        assert!(!dir.join("Vv1.mp4.part.jpg").exists(), "written under a temp name, then renamed");
        // Next session, without ffmpeg: the cached poster and duration are still there.
        let mut later = Posters::default();
        assert_eq!(wait(&mut later, None), Poster::Ready(dir.join("Vv1.mp4.jpg"), Some(7000)));
        std::fs::remove_dir_all(&dir).unwrap();
        let mut none = Posters::default();
        assert_eq!(wait(&mut none, None), Poster::Failed, "no ffmpeg, no cache: the neutral tile");
    }

    /// The real ffmpeg on this machine: encodes a tone and makes a poster from a generated clip.
    #[test]
    #[ignore = "needs ffmpeg"]
    fn real_ffmpeg_encodes_and_posters() {
        let bin = locate().expect("ffmpeg");
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("v.m4a");
        let pcm: Vec<f32> = (0..48_000 * 2).map(|i| (i as f32 * 0.05).sin() * 0.3).collect();
        encode_voice(&bin, &pcm, 48000, 1, 32000, &out).unwrap();
        assert!(std::fs::metadata(&out).unwrap().len() > 1000);
        let clip = tmp.path().join("c.mp4");
        let st = command(&bin).args(["-f", "lavfi", "-i", "testsrc=duration=3:size=320x240:rate=10"]).arg(&clip).status().unwrap();
        assert!(st.success());
        let poster = tmp.path().join("p.jpg");
        let ms = video_poster(&bin, &clip, &poster).unwrap().unwrap();
        assert!((2900..=3100).contains(&ms), "{ms}");
        assert_eq!(image::image_dimensions(&poster).unwrap(), (320, 240));
    }
}

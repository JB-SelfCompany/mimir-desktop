//! Video messages from a camera: one ffmpeg captures the camera (dshow on Windows, v4l2 on Linux) and the
//! microphone, writes Android's video-message mp4 (VideoMessageTranscoder: a centred 384² square, H.264 Baseline
//! ~350 kb/s at 24 fps, AAC 64 kb/s) and streams a small RGBA preview of the same frames on stdout.
//! `MIMIR_TEST_CAMERA=testsrc` swaps the devices for ffmpeg's synthetic testsrc + sine (machines with no camera).
use crate::media::{ffmpeg, recorder};
use eframe::egui::{ColorImage, Context};
use std::{
    cell::RefCell,
    ffi::OsString,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Stdio},
    sync::{atomic::{AtomicBool, AtomicU64, Ordering}, mpsc::{self, Receiver}, Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

/// Android VideoMessageRecorder.CAPTURE_FPS and VideoMessageTranscoder.SIZE / VIDEO_BITRATE / AUDIO_BITRATE.
pub const FPS: u64 = 24;
const SIZE: u32 = 384;
const VIDEO_BITRATE: &str = "350k";
const AUDIO_BITRATE: &str = "64k";
/// Android BaseChatActivity VIDEO_MAX_DURATION_MS / VIDEO_MIN_DURATION_MS.
pub const MAX_MS: u64 = 60_000;
pub const MIN_MS: u64 = 3_000;
/// Side of the square preview frames on stdout.
pub const PREVIEW: usize = 192;
pub const NO_CAMERA: &str = "No camera found";
/// `finish`'s error for a take under 3 s.
pub const TOO_SHORT: &str = recorder::TOO_SHORT;
const CANCELLED: &str = "cancelled";
/// How long `finish` waits for ffmpeg to close the mp4 after `q`, then the take is given up.
const STOP_WAIT: Duration = Duration::from_secs(if cfg!(test) { 3 } else { 10 });
/// A take with no preview frame for this long is given up: the camera is not responding.
pub const STALL: Duration = Duration::from_secs(5);
/// `STALL` before the first frame: a cold camera can take a while to open.
pub const FIRST_FRAME: Duration = Duration::from_secs(10);
const NOT_STOPPED: &str = "ffmpeg did not stop";

/// Where the picture and sound come from.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    /// A DirectShow camera and, when there is one, a DirectShow microphone.
    Dshow { camera: String, mic: Option<String> },
    /// A V4L2 device node and ALSA's default input.
    V4l2 { device: String },
    /// ffmpeg's testsrc picture and a 440 Hz tone, in real time.
    Test,
}

/// The ffmpeg command line of a take into `out` (a new path), with `encoder` from `h264_encoder`. Both outputs stop
/// at the 60 s cap by themselves. `vga` asks a DirectShow camera for 640x480 at 30 fps (other sources ignore it).
pub fn capture_args(src: &Source, encoder: &str, out: &Path, vga: bool) -> Vec<OsString> {
    let mut a: Vec<String> = ["-hide_banner", "-loglevel", "error"].map(String::from).to_vec();
    let audio = match src {
        Source::Dshow { camera, mic } => {
            let input = match mic {
                Some(m) => format!("video={camera}:audio={m}"),
                None => format!("video={camera}"),
            };
            a.extend(["-f", "dshow", "-rtbufsize", "64M"].map(String::from));
            if vga {
                a.extend(["-video_size", "640x480", "-framerate", "30"].map(String::from));
            }
            a.push("-i".into());
            a.push(input);
            mic.as_ref().map(|_| "0:a")
        }
        Source::V4l2 { device } => {
            // ponytail: ALSA's default input is required; a machine without one gets ffmpeg's error, not a silent take.
            a.extend(["-f", "v4l2", "-i"].map(String::from));
            a.push(device.clone());
            a.extend(["-f", "alsa", "-i", "default"].map(String::from));
            Some("1:a")
        }
        Source::Test => {
            a.extend(["-re", "-f", "lavfi", "-i", "testsrc=size=640x480:rate=24", "-re", "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100"].map(String::from));
            Some("1:a")
        }
    };
    // Android's Presentation LAYOUT_SCALE_TO_FIT_WITH_CROP: the centred square, scaled.
    a.push("-filter_complex".into());
    a.push(format!("[0:v]crop='min(iw,ih)':'min(iw,ih)',scale={SIZE}:{SIZE},fps={FPS},setsar=1,split[enc][pv];[pv]scale={PREVIEW}:{PREVIEW},format=rgba[preview]"));
    a.extend(["-map", "[enc]"].map(String::from));
    if let Some(audio) = audio {
        a.extend(["-map", audio].map(String::from));
    }
    a.extend(["-c:v", encoder].map(String::from));
    if encoder == "libx264" {
        // Android pins Baseline@L4 (VideoMessageTranscoder); h264_mf and openh264 make Baseline on their own.
        a.extend(["-preset", "veryfast", "-profile:v", "baseline", "-level", "4.0"].map(String::from));
    }
    let max = (MAX_MS / 1000).to_string();
    a.extend(["-b:v", VIDEO_BITRATE, "-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", AUDIO_BITRATE, "-ac", "1", "-ar", "44100", "-t", &max, "-movflags", "+faststart"].map(String::from));
    let mut args: Vec<OsString> = a.into_iter().map(OsString::from).collect();
    args.push(out.into());
    args.extend(["-map", "[preview]", "-f", "rawvideo", "-t", &max, "pipe:1"].map(OsString::from));
    args
}

/// The first H.264 encoder `ffmpeg -encoders` lists: libx264 (GPL builds, Linux distros), else openh264 or Media
/// Foundation (the LGPL Windows build).
fn h264_encoder(list: &str) -> &'static str {
    let has = |name: &str| list.lines().any(|l| l.split_whitespace().nth(1) == Some(name));
    ["libx264", "libopenh264", "h264_mf"].into_iter().find(|e| has(e)).unwrap_or("libx264")
}

fn encoder(bin: &Path) -> &'static str {
    // ponytail: asked once per session; a different ffmpeg picked up after a restart is asked again.
    static FOUND: OnceLock<&'static str> = OnceLock::new();
    cached(&FOUND, || {
        let out = ffmpeg::command(bin).args(["-hide_banner", "-encoders"]).stdout(Stdio::piped()).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    })
}

/// The encoder kept in `found`, else the one `probe`'s `-encoders` list names. A failed probe is not kept: the
/// next take asks again.
fn cached(found: &OnceLock<&'static str>, probe: impl FnOnce() -> Option<String>) -> &'static str {
    if let Some(e) = found.get() {
        return e;
    }
    match probe() {
        Some(list) => found.get_or_init(|| h264_encoder(&list)),
        None => h264_encoder(""),
    }
}

/// Camera and microphone names from `ffmpeg -list_devices true -f dshow -i dummy` (ffmpeg 5+: `"Name" (video)`).
fn parse_dshow(stderr: &str) -> (Vec<String>, Option<String>) {
    let (mut cams, mut mics) = (Vec::new(), Vec::new());
    for l in stderr.lines() {
        let Some((name, kind)) = l.split_once("] \"").and_then(|(_, rest)| rest.split_once("\" (")) else { continue };
        if kind.contains("video") {
            cams.push(name.to_string());
        } else if kind.contains("audio") {
            mics.push(name.to_string());
        }
    }
    (cams, mics.into_iter().find(|m| real_mic(m)))
}

/// A microphone, not a loopback of the speakers, with a name `video=…:audio=…` can carry (ffmpeg splits it at ':'
/// and has no escape).
fn real_mic(name: &str) -> bool {
    let n = name.to_lowercase();
    !name.contains(':') && !["stereo mix", "стерео микшер", "loopback"].iter().any(|l| n.contains(l))
}

/// Video4Linux capture nodes under `sys` (/sys/class/video4linux; index 0: the other nodes of a webcam carry
/// metadata): their name (else the /dev path) and /dev path.
// ponytail: two identical webcams show the same name twice and the picker can only reach the first; label them
// apart when someone has two.
fn v4l2_devices(sys: &Path) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = std::fs::read_dir(sys)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| std::fs::read_to_string(e.path().join("index")).is_ok_and(|i| i.trim() == "0"))
        .map(|e| {
            let dev = format!("/dev/{}", e.file_name().to_string_lossy());
            let name = std::fs::read_to_string(e.path().join("name")).map(|n| n.trim().to_string()).unwrap_or_default();
            (if name.is_empty() { dev.clone() } else { name }, dev)
        })
        .collect();
    v.sort_by(|a, b| a.1.cmp(&b.1));
    v
}

/// The cameras there are (as the picker names them), the one for `want` (else the first camera) and its source.
// ponytail: the microphone is the system's first real (dshow) or default (ALSA) one; no picker for it.
fn resolve(bin: &Path, want: Option<&str>) -> Result<(Vec<String>, String, Source), String> {
    if std::env::var("MIMIR_TEST_CAMERA").is_ok_and(|v| v == "testsrc") {
        return Ok((vec!["testsrc".into()], "testsrc".into(), Source::Test));
    }
    // (name, device): one and the same for DirectShow.
    let (cams, mic): (Vec<(String, String)>, _) = if cfg!(windows) {
        let out = ffmpeg::command(bin).args(["-hide_banner", "-list_devices", "true", "-f", "dshow", "-i", "dummy"]).output().map_err(|e| e.to_string())?;
        let (cams, mic) = parse_dshow(&String::from_utf8_lossy(&out.stderr));
        (cams.into_iter().map(|c| (c.clone(), c)).collect(), mic)
    } else {
        (v4l2_devices(Path::new("/sys/class/video4linux")), None)
    };
    let (name, device) = want.and_then(|w| cams.iter().find(|(n, _)| n == w)).or(cams.first()).cloned().ok_or(NO_CAMERA)?;
    let src = if cfg!(windows) { Source::Dshow { camera: device, mic } } else { Source::V4l2 { device } };
    Ok((cams.into_iter().map(|(n, _)| n).collect(), name, src))
}

#[derive(Default)]
struct Shared {
    /// The running ffmpeg; `None` before it starts and once it is waited for.
    child: Mutex<Option<Child>>,
    cancelled: AtomicBool,
    /// `finish` was called: ffmpeg was told to stop, or must not start.
    stopping: AtomicBool,
    frames: AtomicU64,
    /// When the last preview frame came (or ffmpeg started); `None` while the device is looked up.
    last_frame: Mutex<Option<Instant>>,
    /// The newest preview frame not yet shown.
    frame: Mutex<Option<Vec<u8>>>,
    cameras: Mutex<Vec<String>>,
    camera: Mutex<Option<String>>,
}

/// A take in progress. Opening the device happens on its own thread: a failure shows up in `failed`.
pub struct Camera {
    shared: Arc<Shared>,
    done: Receiver<Result<(), String>>,
    /// The capture's outcome once it ended (an error, or the cap reached).
    outcome: RefCell<Option<Result<(), String>>>,
    /// The take's file; emptied once `finish` hands it over.
    out: PathBuf,
}

impl Camera {
    /// Starts recording `want` (else the first camera) into `out`.
    pub fn start(bin: PathBuf, want: Option<String>, out: PathBuf, ctx: Context) -> Camera {
        let path = out.clone();
        Self::spawn(out, ctx.clone(), move |sh| run(&bin, &path, sh, &ctx, || resolve(&bin, want.as_deref())))
    }

    /// Runs `job` on the capture thread; on failure or cancel its file goes.
    fn spawn(out: PathBuf, ctx: Context, job: impl FnOnce(&Shared) -> Result<(), String> + Send + 'static) -> Camera {
        let shared = Arc::new(Shared::default());
        let (tx, done) = mpsc::channel();
        let (sh, path) = (shared.clone(), out.clone());
        std::thread::Builder::new()
            .name("camera".into())
            .spawn(move || {
                let r = job(&sh);
                if r.is_err() || sh.cancelled.load(Ordering::SeqCst) {
                    let _ = std::fs::remove_file(&path);
                }
                let _ = tx.send(r);
                ctx.request_repaint();
            })
            .expect("camera thread");
        Camera { shared, done, outcome: RefCell::new(None), out }
    }

    /// Recorded time, counted in encoded frames (not wall-clock: the camera takes a while to start).
    pub fn elapsed_ms(&self) -> u64 {
        self.shared.frames.load(Ordering::SeqCst) * 1000 / FPS
    }

    /// The newest preview frame since the last call.
    pub fn take_frame(&self) -> Option<ColorImage> {
        let px = self.shared.frame.lock().unwrap().take()?;
        Some(ColorImage::from_rgba_unmultiplied([PREVIEW, PREVIEW], &px))
    }

    /// The cameras found, and the one recording; empty until the device lookup is done.
    pub fn cameras(&self) -> (Vec<String>, Option<String>) {
        (self.shared.cameras.lock().unwrap().clone(), self.shared.camera.lock().unwrap().clone())
    }

    fn check(&self) {
        let mut o = self.outcome.borrow_mut();
        if o.is_none() {
            *o = self.done.try_recv().ok();
        }
    }

    /// Why the capture stopped on its own (no camera, device or encoder error), if it did.
    pub fn failed(&self) -> Option<String> {
        self.check();
        self.outcome.borrow().clone().and_then(Result::err)
    }

    /// Whether ffmpeg stopped by itself at the 60 s cap.
    pub fn ended(&self) -> bool {
        self.check();
        matches!(*self.outcome.borrow(), Some(Ok(())))
    }

    /// Whether the camera has sent no frame for `STALL` (`FIRST_FRAME` for the first one) by `now` while the take
    /// runs: it is not responding.
    pub fn stalled(&self, now: Instant) -> bool {
        self.check();
        let limit = if self.shared.frames.load(Ordering::SeqCst) == 0 { FIRST_FRAME } else { STALL };
        self.outcome.borrow().is_none() && self.shared.last_frame.lock().unwrap().is_some_and(|t| now.saturating_duration_since(t) >= limit)
    }

    /// Stops and throws the take away (its file is deleted): `Drop` does it.
    pub fn cancel(self) {}

    /// Stops ffmpeg (`q`: it finishes the mp4) and hands over the file and its length; under 3 s is `TOO_SHORT`.
    /// Waits for ffmpeg: call it off the UI thread.
    pub fn finish(mut self) -> Result<(PathBuf, u64), String> {
        {
            // Still looking for the device: ffmpeg never starts, and the take is too short.
            let mut child = self.shared.child.lock().unwrap();
            self.shared.stopping.store(true, Ordering::SeqCst);
            if let Some(stdin) = child.as_mut().and_then(|c| c.stdin.as_mut()) {
                let _ = stdin.write_all(b"q").and_then(|_| stdin.flush());
            }
        }
        self.check();
        let r = match self.outcome.take() {
            Some(r) => r,
            None => self.done.recv_timeout(STOP_WAIT).unwrap_or_else(|_| Err(NOT_STOPPED.into())), // `Drop` kills it
        };
        let ms = self.elapsed_ms().min(MAX_MS);
        // An error leaves the file to `Drop`.
        match r {
            Err(e) if e != CANCELLED => Err(e),
            _ if ms < MIN_MS => Err(TOO_SHORT.into()),
            Err(e) => Err(e),
            Ok(()) => Ok((std::mem::take(&mut self.out), ms)),
        }
    }

    /// A camera that "recorded" `ms` (a few bytes in `out`) on `cameras`, the first of them current; it ends when told.
    #[cfg(test)]
    pub fn fake(ms: u64, cameras: &[&str], out: PathBuf) -> Camera {
        Self::fake_frozen(ms, cameras, out, Duration::ZERO)
    }

    /// `fake`, its last frame `ago` old (and no more coming).
    #[cfg(test)]
    pub fn fake_frozen(ms: u64, cameras: &[&str], out: PathBuf, ago: Duration) -> Camera {
        let cameras: Vec<String> = cameras.iter().map(|c| c.to_string()).collect();
        let path = out.clone();
        Self::spawn(out, Context::default(), move |sh| {
            std::fs::write(&path, b"fake mp4").map_err(|e| e.to_string())?;
            *sh.camera.lock().unwrap() = cameras.first().cloned();
            *sh.cameras.lock().unwrap() = cameras;
            *sh.frame.lock().unwrap() = Some(vec![128; PREVIEW * PREVIEW * 4]);
            sh.frames.store(ms * FPS / 1000, Ordering::SeqCst);
            *sh.last_frame.lock().unwrap() = Some(Instant::now() - ago);
            while !sh.stopping.load(Ordering::SeqCst) {
                if sh.cancelled.load(Ordering::SeqCst) {
                    return Err(CANCELLED.into());
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Ok(())
        })
    }

    /// A camera whose device failed with `err`.
    #[cfg(test)]
    pub fn failing(err: &'static str) -> Camera {
        Self::failing_after(0, err)
    }

    /// A camera that recorded `ms`, then failed with `err` (unplugged, say).
    #[cfg(test)]
    pub fn failing_after(ms: u64, err: &'static str) -> Camera {
        Self::spawn(PathBuf::new(), Context::default(), move |sh| {
            sh.frames.store(ms * FPS / 1000, Ordering::SeqCst);
            Err(err.into())
        })
    }
}

/// A take dropped unsent (✕, a chat switch, the composer gone, the app closing): ffmpeg stops and the file goes,
/// even after the 60 s cap ended the capture. A file ffmpeg still holds goes from the capture thread, once it exits.
impl Drop for Camera {
    fn drop(&mut self) {
        self.shared.cancelled.store(true, Ordering::SeqCst);
        if let Some(c) = self.shared.child.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            let _ = c.kill();
        }
        if !self.out.as_os_str().is_empty() {
            let _ = std::fs::remove_file(&self.out);
        }
    }
}

/// The capture thread: finds the device, runs ffmpeg and passes its preview frames on until it exits. A DirectShow
/// camera is asked for 640x480 at 30 fps first; one that refuses it before a frame gets ffmpeg's own choice.
fn run(bin: &Path, out: &Path, sh: &Shared, ctx: &Context, resolve: impl FnOnce() -> Result<(Vec<String>, String, Source), String>) -> Result<(), String> {
    let (cams, current, src) = resolve()?;
    *sh.cameras.lock().unwrap() = cams;
    *sh.camera.lock().unwrap() = Some(current);
    let encoder = encoder(bin);
    vga_then_any(&src, sh, |vga| {
        if !vga {
            let _ = std::fs::remove_file(out); // no -y: ffmpeg would ask before overwriting
        }
        capture(bin, capture_args(&src, encoder, out, vga), sh, ctx)
    })
}

/// `attempt(true)` (640x480 at 30 fps), then `attempt(false)` once for a DirectShow camera that failed before its
/// first frame.
fn vga_then_any(src: &Source, sh: &Shared, mut attempt: impl FnMut(bool) -> Result<(), String>) -> Result<(), String> {
    let r = attempt(true);
    if matches!(src, Source::Dshow { .. }) && sh.frames.load(Ordering::SeqCst) == 0 && r.as_ref().is_err_and(|e| e != CANCELLED) {
        *sh.last_frame.lock().unwrap() = None; // the retry's own spawn restarts the clock: a slow first try must not trip the watchdog
        return attempt(false);
    }
    r
}

/// One ffmpeg run with `args`, its preview frames passed on until it exits.
fn capture(bin: &Path, args: Vec<OsString>, sh: &Shared, ctx: &Context) -> Result<(), String> {
    let (mut stdout, mut stderr) = {
        let mut child = sh.child.lock().unwrap();
        if sh.cancelled.load(Ordering::SeqCst) || sh.stopping.load(Ordering::SeqCst) {
            return Err(CANCELLED.into());
        }
        let mut c = ffmpeg::command(bin).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
        let io = (c.stdout.take().expect("piped stdout"), c.stderr.take().expect("piped stderr"));
        *child = Some(c);
        *sh.last_frame.lock().unwrap() = Some(Instant::now()); // the camera has `FIRST_FRAME` to send its first frame
        io
    };
    // stderr is read beside stdout so neither pipe can fill up and stall ffmpeg.
    let errors = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let mut px = vec![0u8; PREVIEW * PREVIEW * 4];
    while stdout.read_exact(&mut px).is_ok() {
        *sh.frame.lock().unwrap() = Some(px.clone());
        *sh.last_frame.lock().unwrap() = Some(Instant::now());
        sh.frames.fetch_add(1, Ordering::SeqCst);
        ctx.request_repaint();
    }
    let errors = errors.join().unwrap_or_default();
    let status = sh.child.lock().unwrap().take().map(|mut c| c.wait());
    if sh.cancelled.load(Ordering::SeqCst) {
        return Err(CANCELLED.into());
    }
    match status {
        Some(Ok(s)) if s.success() => Ok(()),
        Some(Ok(s)) => Err(errors.lines().rev().map(str::trim).find(|l| !l.is_empty()).map_or_else(|| s.to_string(), str::to_string)),
        Some(Err(e)) => Err(e.to_string()),
        None => Err(CANCELLED.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn joined(args: Vec<OsString>) -> String {
        args.iter().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" ")
    }

    const FILTER: &str = "-filter_complex [0:v]crop='min(iw,ih)':'min(iw,ih)',scale=384:384,fps=24,setsar=1,split[enc][pv];[pv]scale=192:192,format=rgba[preview]";
    const TAIL: &str = "-b:v 350k -pix_fmt yuv420p -c:a aac -b:a 64k -ac 1 -ar 44100 -t 60 -movflags +faststart v.mp4 -map [preview] -f rawvideo -t 60 pipe:1";

    #[test]
    fn capture_command_line_per_os() {
        let out = Path::new("v.mp4");
        let x264 = "-c:v libx264 -preset veryfast -profile:v baseline -level 4.0";
        let windows = Source::Dshow { camera: "HD Webcam".into(), mic: Some("Microphone (Realtek)".into()) };
        let want = format!("-hide_banner -loglevel error -f dshow -rtbufsize 64M -i video=HD Webcam:audio=Microphone (Realtek) {FILTER} -map [enc] -map 0:a {x264} {TAIL}");
        assert_eq!(joined(capture_args(&windows, "libx264", out, false)), want);
        // No DirectShow microphone: a silent take, and the LGPL build's Media Foundation encoder takes no profile.
        let silent = Source::Dshow { camera: "HD Webcam".into(), mic: None };
        let want = format!("-hide_banner -loglevel error -f dshow -rtbufsize 64M -i video=HD Webcam {FILTER} -map [enc] -c:v h264_mf {TAIL}");
        assert_eq!(joined(capture_args(&silent, "h264_mf", out, false)), want);
        let linux = Source::V4l2 { device: "/dev/video0".into() };
        let want = format!("-hide_banner -loglevel error -f v4l2 -i /dev/video0 -f alsa -i default {FILTER} -map [enc] -map 1:a {x264} {TAIL}");
        assert_eq!(joined(capture_args(&linux, "libx264", out, false)), want);
        let test = format!(
            "-hide_banner -loglevel error -re -f lavfi -i testsrc=size=640x480:rate=24 -re -f lavfi -i sine=frequency=440:sample_rate=44100 {FILTER} -map [enc] -map 1:a {x264} {TAIL}"
        );
        assert_eq!(joined(capture_args(&Source::Test, "libx264", out, false)), test);
        // DirectShow is asked for 640x480 at 30 fps first (`run` retries without); the others ignore it.
        let want = format!("-hide_banner -loglevel error -f dshow -rtbufsize 64M -video_size 640x480 -framerate 30 -i video=HD Webcam {FILTER} -map [enc] -c:v h264_mf {TAIL}");
        assert_eq!(joined(capture_args(&silent, "h264_mf", out, true)), want);
        assert_eq!(capture_args(&linux, "libx264", out, true), capture_args(&linux, "libx264", out, false));
        assert_eq!(capture_args(&Source::Test, "libx264", out, true), capture_args(&Source::Test, "libx264", out, false));
    }

    #[test]
    fn dshow_retries_once_without_vga() {
        let cam = Source::Dshow { camera: "c".into(), mic: None };
        let run = |src: &Source, frames: u64, first: Result<(), String>| {
            let sh = Shared { frames: AtomicU64::new(frames), ..Default::default() };
            *sh.last_frame.lock().unwrap() = Some(Instant::now() - FIRST_FRAME);
            let mut asked = vec![];
            let r = vga_then_any(src, &sh, |vga| {
                asked.push(vga);
                if !vga {
                    assert!(sh.last_frame.lock().unwrap().is_none(), "the first try's time is dropped before the retry");
                }
                if vga { first.clone() } else { Err("second".into()) }
            });
            (asked, r)
        };
        assert_eq!(run(&cam, 0, Err("Could not set video options".into())), (vec![true, false], Err("second".into())), "refused: ffmpeg picks");
        assert_eq!(run(&cam, 0, Ok(())), (vec![true], Ok(())));
        assert_eq!(run(&cam, 12, Err("Device lost".into())), (vec![true], Err("Device lost".into())), "failed mid-take: no retry");
        assert_eq!(run(&cam, 0, Err(CANCELLED.into())), (vec![true], Err(CANCELLED.into())));
        let v4l2 = Source::V4l2 { device: "/dev/video0".into() };
        assert_eq!(run(&v4l2, 0, Err("x".into())), (vec![true], Err("x".into())), "only DirectShow retries");
    }

    #[test]
    fn v4l2_capture_nodes_by_name() {
        let sys = tempfile::tempdir().unwrap();
        let node = |n: &str, index: &str, name: Option<&str>| {
            let d = sys.path().join(n);
            std::fs::create_dir(&d).unwrap();
            std::fs::write(d.join("index"), index).unwrap();
            if let Some(name) = name {
                std::fs::write(d.join("name"), name).unwrap();
            }
        };
        node("video2", "0\n", None);
        node("video1", "1\n", Some("Integrated Camera: Integrated C\n"));
        node("video0", "0\n", Some("Integrated Camera: Integrated C\n"));
        node("video3", "0\n", Some("\n"));
        let found = v4l2_devices(sys.path());
        let want = [("Integrated Camera: Integrated C", "/dev/video0"), ("/dev/video2", "/dev/video2"), ("/dev/video3", "/dev/video3")];
        assert_eq!(found, want.map(|(n, d)| (n.to_string(), d.to_string())), "the metadata node is skipped; no name: the path");
        assert_eq!(v4l2_devices(&sys.path().join("missing")), vec![], "no video4linux at all");
    }

    /// The fake's own last-frame time, once it is set.
    fn last_frame(cam: &Camera) -> Instant {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(t) = *cam.shared.last_frame.lock().unwrap() {
                return t;
            }
            assert!(Instant::now() < end, "the fake never set its frame time");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn stall_watchdog() {
        let tmp = tempfile::tempdir().unwrap();
        let cam = Camera::fake(5_000, &["cam"], tmp.path().join("a.mp4"));
        let t = last_frame(&cam);
        assert!(!cam.stalled(t), "a frame just came");
        assert!(!cam.stalled(t + STALL - Duration::from_millis(100)));
        assert!(cam.stalled(t + STALL), "{STALL:?} without a frame");
        // No frame yet (ffmpeg just started): a cold camera gets longer to open.
        let cold = Camera::fake(0, &["cam"], tmp.path().join("b.mp4"));
        let t = last_frame(&cold);
        assert!(!cold.stalled(t + STALL), "{STALL:?} is not enough for the first frame");
        assert!(!cold.stalled(t + FIRST_FRAME - Duration::from_millis(100)));
        assert!(cold.stalled(t + FIRST_FRAME), "{FIRST_FRAME:?} without a first frame");
        assert!(!Camera::failing(NO_CAMERA).stalled(t + FIRST_FRAME * 2), "no device, no frames to wait for");
        // As `run` at the 60 s cap: over, so no longer waiting for frames.
        let ended = Camera::spawn(PathBuf::new(), Context::default(), |sh| {
            *sh.last_frame.lock().unwrap() = Some(Instant::now());
            Ok(())
        });
        let end = Instant::now() + Duration::from_secs(5);
        while !ended.ended() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!ended.stalled(Instant::now() + FIRST_FRAME * 2), "an ended take is not stalled");
    }

    #[test]
    fn encoder_choice() {
        let gpl = " V....D libx264              libx264 H.264\n V....D h264_mf              H264 via MediaFoundation";
        let lgpl = " V....D libx264rgb           libx264 H.264 RGB\n V....D h264_mf              H264 via MediaFoundation";
        assert_eq!(h264_encoder(gpl), "libx264");
        assert_eq!(h264_encoder(lgpl), "h264_mf", "libx264rgb is not libx264");
        assert_eq!(h264_encoder(" V....D libopenh264          OpenH264\n V....D h264_mf  MF"), "libopenh264");
        assert_eq!(h264_encoder(""), "libx264", "unknown: ffmpeg's own error says what is missing");
        let found = OnceLock::new();
        assert_eq!(cached(&found, || None), "libx264", "the probe failed");
        assert_eq!(cached(&found, || Some(lgpl.into())), "h264_mf", "a failed probe is not kept");
        assert_eq!(cached(&found, || unreachable!()), "h264_mf", "a good one is");
    }

    #[test]
    fn dshow_device_list() {
        let stderr = r#"[in#0 @ 000001] "Integrated Camera" (video)
[in#0 @ 000001]   Alternative name "@device_pnp_\\?\usb#vid_04f2"
[in#0 @ 000001] "OBS Virtual Camera" (video)
[in#0 @ 000001] "Hy-UXGA(8101)-Camera" (none)
[in#0 @ 000001] "Stereo Mix (Realtek(R) Audio)" (audio)
[in#0 @ 000001] "Стерео микшер (Realtek(R) Audio)" (audio)
[in#0 @ 000001] "Loopback Audio" (audio)
[in#0 @ 000001] "Mic: USB" (audio)
[in#0 @ 000001] "Microphone Array (Realtek(R) Audio)" (audio)
Error opening input file dummy."#;
        let (cams, mic) = parse_dshow(stderr);
        assert_eq!(cams, ["Integrated Camera", "OBS Virtual Camera"], "a device of no kind is not a camera");
        assert_eq!(mic.as_deref(), Some("Microphone Array (Realtek(R) Audio)"), "the first real microphone");
        assert_eq!(parse_dshow("Could not enumerate video devices (or none found)."), (vec![], None));
    }

    #[test]
    fn duration_cap_and_minimum() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("v.mp4");
        let (path, ms) = Camera::fake(61_500, &["cam"], out.clone()).finish().unwrap();
        assert_eq!((path.as_path(), ms), (out.as_path(), MAX_MS), "60 s at most");
        assert_eq!(Camera::fake(3_000, &["cam"], out.clone()).finish().unwrap().1, 3_000);
        assert_eq!(Camera::fake(2_990, &["cam"], out.clone()).finish().err().as_deref(), Some(TOO_SHORT), "2.958 s in whole frames");
        assert!(!out.exists(), "a too-short take leaves no file");
        let args = joined(capture_args(&Source::Test, "libx264", &out, false));
        assert_eq!(args.matches("-t 60 ").count(), 2, "ffmpeg itself stops both outputs at the cap: {args}");
    }

    #[test]
    fn cancel_and_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("v.mp4");
        let cam = Camera::fake(5_000, &["a", "b"], out.clone());
        let end = Instant::now() + Duration::from_secs(5);
        while cam.cameras().0.is_empty() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(cam.cameras(), (vec!["a".to_string(), "b".to_string()], Some("a".to_string())));
        assert!(cam.take_frame().is_some_and(|f| f.size == [PREVIEW, PREVIEW]));
        assert!(cam.take_frame().is_none(), "a frame is handed over once");
        assert_eq!((cam.failed(), cam.ended()), (None, false));
        cam.cancel();
        let end = Instant::now() + Duration::from_secs(5);
        while out.exists() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!out.exists(), "a cancelled take is deleted");
        let broken = Camera::failing(NO_CAMERA);
        let end = Instant::now() + Duration::from_secs(5);
        while broken.failed().is_none() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(broken.failed().as_deref(), Some(NO_CAMERA), "kept until the dialog shows it");
        assert_eq!(broken.finish().err().as_deref(), Some(NO_CAMERA));
    }

    #[test]
    fn finish_gives_up_on_an_ffmpeg_that_ignores_q() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("v.mp4");
        let path = out.clone();
        // Deaf to `q` (`stopping`); only the kill (`cancelled`) ends it.
        let cam = Camera::spawn(out.clone(), Context::default(), move |sh| {
            std::fs::write(&path, b"mp4").map_err(|e| e.to_string())?;
            sh.frames.store(5 * FPS, Ordering::SeqCst);
            while !sh.cancelled.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(CANCELLED.into())
        });
        while !out.exists() {
            std::thread::sleep(Duration::from_millis(5));
        }
        let t = Instant::now();
        assert_eq!(cam.finish().err().as_deref(), Some(NOT_STOPPED));
        assert!(t.elapsed() < STOP_WAIT + Duration::from_secs(2), "bounded");
        let end = Instant::now() + Duration::from_secs(5);
        while out.exists() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!out.exists(), "the take is given up");
    }

    #[test]
    fn a_capped_take_dropped_unsent_is_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("v.mp4");
        let path = out.clone();
        // As `run` at the 60 s cap: the file is written and the capture thread is over.
        let cam = Camera::spawn(out.clone(), Context::default(), move |_| std::fs::write(&path, b"mp4").map_err(|e| e.to_string()));
        let end = Instant::now() + Duration::from_secs(5);
        while !cam.ended() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(cam.ended() && out.exists());
        cam.cancel();
        assert!(!out.exists(), "cancel removes it itself");
        let path = out.clone();
        drop(Camera::spawn(out.clone(), Context::default(), move |_| std::fs::write(&path, b"mp4").map_err(|e| e.to_string())));
        let end = Instant::now() + Duration::from_secs(5);
        while out.exists() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!out.exists(), "so does a plain drop");
    }

    /// The real ffmpeg with its synthetic testsrc + sine (no device): a 4 s take, previewed and finished with `q`.
    #[test]
    #[ignore = "needs ffmpeg"]
    fn real_testsrc_capture() {
        let bin = ffmpeg::locate().expect("ffmpeg");
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("v.mp4");
        let (path, ctx) = (out.clone(), Context::default());
        let cam = Camera::spawn(out.clone(), ctx.clone(), move |sh| run(&bin, &path, sh, &ctx, || Ok((vec!["testsrc".into()], "testsrc".into(), Source::Test))));
        let end = Instant::now() + Duration::from_secs(20);
        while cam.elapsed_ms() < 4_000 && Instant::now() < end {
            assert_eq!(cam.failed(), None);
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(cam.take_frame().is_some(), "preview frames arrive");
        let (file, ms) = cam.finish().unwrap();
        assert!((4_000..5_000).contains(&ms), "{ms}");
        let poster = tmp.path().join("p.jpg");
        let dur = ffmpeg::video_poster(&ffmpeg::locate().unwrap(), &file, &poster).unwrap().unwrap();
        assert!(dur.abs_diff(ms) < 500, "the file is as long as the frames counted: {dur} vs {ms}");
        assert_eq!(image::image_dimensions(&poster).unwrap(), (SIZE, SIZE), "Android's square");
    }

    /// A real camera and microphone. It films whoever sits in front of it: run it only on purpose.
    #[test]
    #[ignore = "needs a camera; records a real person"]
    fn real_camera_capture() {
        let tmp = tempfile::tempdir().unwrap();
        let cam = Camera::start(ffmpeg::locate().expect("ffmpeg"), None, tmp.path().join("v.mp4"), Context::default());
        std::thread::sleep(Duration::from_secs(5));
        assert_eq!(cam.failed(), None);
        let (file, ms) = cam.finish().unwrap();
        assert!(ms >= MIN_MS && std::fs::metadata(file).unwrap().len() > 1000);
    }
}

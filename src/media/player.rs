//! In-process audio playback (P3-R4, spec §9b: no ffmpeg). One thread owns the rodio output stream, which is `!Send`,
//! and at most one track; the UI sends it commands and reads a shared `PlayState`.
use crate::{core::worker::{WorkResult, Worker}, i18n::{trf, Key, Lang}};
use eframe::egui::Context;
use symphonia::core::{
    audio::{Channels, SampleBuffer, SignalSpec},
    codecs::{DecoderOptions, CODEC_TYPE_NULL},
    formats::{FormatOptions, FormatReader, SeekMode, SeekTo},
};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{mpsc, Arc, Mutex},
    time::Duration,
};

/// Progress ticks: the UI repaints this often while something plays (~4×/s).
const TICK: Duration = Duration::from_millis(250);

/// What is loaded: `guid` None = nothing (every bubble shows ▶ and its full length).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlayState {
    pub guid: Option<i64>,
    pub playing: bool,
    pub pos_ms: u64,
    pub len_ms: u64,
}

impl PlayState {
    /// Whether `guid` is playing now (not paused, not another message).
    pub fn is_playing(&self, guid: i64) -> bool {
        self.guid == Some(guid) && self.playing
    }
}

enum Cmd {
    Toggle(i64, PathBuf),
    Seek(i64, f32),
    /// Read the length of `guid`'s file into `Player::lengths`.
    Probe(i64, PathBuf),
}

type Lengths = Arc<Mutex<HashMap<i64, Option<u64>>>>;

pub struct Player {
    tx: mpsc::Sender<Cmd>,
    state: Arc<Mutex<PlayState>>,
    /// Voice lengths by message guid, filled by the player thread; `None` = the file has no readable duration.
    lengths: Lengths,
    /// Guids whose probe was already requested.
    asked: RefCell<HashSet<i64>>,
}

impl Player {
    /// Starts the player thread; the audio device is opened on the first play (and retried after a failure).
    /// Playback errors reach the status line through `worker`.
    pub fn spawn(ctx: Context, worker: Worker, lang: Lang) -> Player {
        let (tx, rx) = mpsc::channel();
        let state = Arc::new(Mutex::new(PlayState::default()));
        let lengths = Lengths::default();
        let (shared, lens) = (state.clone(), lengths.clone());
        std::thread::spawn(move || {
            #[cfg(not(test))]
            {
                // ponytail: the stream is opened once and kept; if the device goes away (headphones unplugged), playback
                // stays silent until the app restarts. Only a failed open is retried.
                let mut out: Option<(rodio::OutputStream, rodio::OutputStreamHandle)> = None;
                run(rx, shared, lens, ctx, worker, lang, move |path| {
                    if out.is_none() {
                        out = Some(rodio::OutputStream::try_default().map_err(|e| e.to_string())?);
                    }
                    let sink = rodio::Sink::try_new(&out.as_ref().expect("opened above").1).map_err(|e| e.to_string())?;
                    let sound = Decoded::open(path)?;
                    let len = sound.len_ms;
                    sink.append(sound);
                    Ok((sink, len))
                });
            }
            // Tests never touch an audio device.
            #[cfg(test)]
            run(rx, shared, lens, ctx, worker, lang, |p| Ok((tests::Fake::default(), probe_duration(p).unwrap_or(0))));
        });
        Player { tx, state, lengths, asked: RefCell::default() }
    }

    /// Play/pause `guid`; another message stops the current one and starts from the beginning.
    pub fn toggle(&self, guid: i64, path: PathBuf) {
        let _ = self.tx.send(Cmd::Toggle(guid, path));
    }

    /// Moves the loaded `guid` to `frac` (0..=1) of its length.
    // ponytail: a message that is not loaded ignores seeks; press ▶ first.
    pub fn seek(&self, guid: i64, frac: f32) {
        let _ = self.tx.send(Cmd::Seek(guid, frac));
    }

    pub fn state(&self) -> PlayState {
        *self.state.lock().unwrap()
    }

    /// Length of the voice message `guid` stored at `path`; None until the player thread has probed it (once per
    /// guid, then a repaint) or when the file has no readable length.
    pub fn len_ms(&self, guid: i64, path: &Path) -> Option<u64> {
        if let Some(len) = self.lengths.lock().unwrap().get(&guid) {
            return *len;
        }
        if self.asked.borrow_mut().insert(guid) {
            let _ = self.tx.send(Cmd::Probe(guid, path.to_path_buf()));
        }
        None
    }
}

/// Opens `path` with symphonia's container readers. The `File` itself is the source: the MP4 reader needs the byte
/// length, which rodio 0.20's own symphonia decoder hides ("stream is not seekable" for every m4a).
fn probe(path: &Path) -> Result<Box<dyn FormatReader>, String> {
    use symphonia::core::{io::MediaSourceStream, meta::MetadataOptions, probe::Hint};
    let src = MediaSourceStream::new(Box::new(std::fs::File::open(path).map_err(|e| e.to_string())?), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let opts = FormatOptions { enable_gapless: true, ..Default::default() };
    let probed = symphonia::default::get_probe().format(&hint, src, &opts, &MetadataOptions::default()).map_err(|e| e.to_string())?;
    Ok(probed.format)
}

/// Length of an audio file from its container (symphonia), without decoding it.
pub fn probe_duration(path: &Path) -> Option<u64> {
    len_of(&probe(path).ok()?.default_track()?.codec_params)
}

fn len_of(p: &symphonia::core::codecs::CodecParameters) -> Option<u64> {
    let frames = p.n_frames?;
    match p.time_base {
        Some(tb) => {
            let t = tb.calc_time(frames);
            Some(t.seconds * 1000 + (t.frac * 1000.) as u64)
        }
        None => Some(frames * 1000 / u64::from(p.sample_rate?)),
    }
}

/// A file decoded by symphonia as it plays, as a rodio source (interleaved f32, one packet buffered).
struct Decoded {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn symphonia::core::codecs::Decoder>,
    track: u32,
    spec: SignalSpec,
    buf: SampleBuffer<f32>,
    next: usize,
    /// From the container; 0 = unknown.
    len_ms: u64,
}

impl Decoded {
    fn open(path: &Path) -> Result<Decoded, String> {
        let format = probe(path)?;
        let track = format.tracks().iter().find(|t| t.codec_params.codec != CODEC_TYPE_NULL).ok_or("no audio track")?;
        let decoder = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default()).map_err(|e| e.to_string())?;
        let (len_ms, track, spec) = (len_of(&track.codec_params).unwrap_or(0), track.id, SignalSpec::new(44100, Channels::FRONT_LEFT));
        let mut d = Decoded { format, decoder, track, spec, buf: SampleBuffer::new(0, spec), next: 0, len_ms };
        if !d.refill() {
            return Err("no audio".into());
        }
        Ok(d)
    }

    /// Decodes the next non-empty packet into `buf`; false at the end (or after 3 bad packets in a row, like rodio).
    fn refill(&mut self) -> bool {
        let mut bad = 0;
        loop {
            let Ok(packet) = self.format.next_packet() else { return false };
            if packet.track_id() != self.track {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(audio) => {
                    self.spec = *audio.spec();
                    self.buf = SampleBuffer::new(audio.capacity() as u64, self.spec);
                    self.buf.copy_interleaved_ref(audio);
                    self.next = 0;
                    if !self.buf.is_empty() {
                        return true;
                    }
                }
                Err(symphonia::core::errors::Error::DecodeError(_)) if bad < 3 => bad += 1,
                Err(_) => return false,
            }
        }
    }
}

impl Iterator for Decoded {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        let s = *self.buf.samples().get(self.next)?;
        self.next += 1;
        // Refilled at once, so `current_frame_len` is 0 only at the end.
        if self.next == self.buf.len() {
            self.refill();
        }
        Some(s)
    }
}

impl rodio::Source for Decoded {
    fn current_frame_len(&self) -> Option<usize> {
        Some(self.buf.len() - self.next)
    }
    fn channels(&self) -> u16 {
        self.spec.channels.count() as u16
    }
    fn sample_rate(&self) -> u32 {
        self.spec.rate
    }
    fn total_duration(&self) -> Option<Duration> {
        None // rodio does not need it; the player reads `len_ms`
    }
    fn try_seek(&mut self, pos: Duration) -> Result<(), rodio::source::SeekError> {
        // ponytail: coarse — lands on a packet boundary (23 ms for AAC), not the exact sample.
        let time = symphonia::core::units::Time::new(pos.as_secs(), f64::from(pos.subsec_nanos()) / 1e9);
        self.format
            .seek(SeekMode::Coarse, SeekTo::Time { time, track_id: Some(self.track) })
            .map_err(|e| rodio::source::SeekError::Other(Box::new(e)))?;
        self.decoder.reset();
        // The buffered packet is from before the seek: replace it (or leave nothing at the end).
        if !self.refill() {
            self.next = self.buf.len();
        }
        Ok(())
    }
}

/// The player thread's view of a playing sound: `rodio::Sink` in the app, a fake in tests.
trait Track {
    fn play(&self);
    fn pause(&self);
    fn is_paused(&self) -> bool;
    fn stop(&self);
    fn seek(&self, to: Duration);
    fn pos(&self) -> Duration;
    /// Played to the end.
    fn done(&self) -> bool;
}

#[cfg(not(test))]
impl Track for rodio::Sink {
    fn play(&self) { rodio::Sink::play(self) }
    fn pause(&self) { rodio::Sink::pause(self) }
    fn is_paused(&self) -> bool { rodio::Sink::is_paused(self) }
    fn stop(&self) { rodio::Sink::stop(self) }
    fn seek(&self, to: Duration) {
        // ponytail: a failed seek (format without seeking) just keeps playing where it was.
        let _ = self.try_seek(to);
    }
    fn pos(&self) -> Duration { self.get_pos() }
    fn done(&self) -> bool { self.empty() }
}

/// The current track: (guid, sound, length in ms; 0 = unknown).
struct Machine<T> {
    cur: Option<(i64, T, u64)>,
}

impl<T: Track> Machine<T> {
    /// `open` gives the sound and its length in ms (0 = unknown).
    fn toggle(&mut self, guid: i64, path: &Path, open: &mut impl FnMut(&Path) -> Result<(T, u64), String>) -> Result<(), String> {
        if let Some((g, t, _)) = &self.cur
            && *g == guid
        {
            if t.is_paused() { t.play() } else { t.pause() }
            return Ok(());
        }
        if let Some((_, t, _)) = self.cur.take() {
            t.stop();
        }
        let (t, len) = open(path)?;
        self.cur = Some((guid, t, len));
        Ok(())
    }

    fn seek(&mut self, guid: i64, frac: f32) {
        if let Some((g, t, len)) = &self.cur
            && *g == guid
        {
            t.seek(Duration::from_millis((*len as f64 * f64::from(frac.clamp(0., 1.))) as u64));
        }
    }

    /// The state to show; a track that played to the end is unloaded (its bubble goes back to ▶ and the length).
    fn state(&mut self) -> PlayState {
        if self.cur.as_ref().is_some_and(|(_, t, _)| t.done()) {
            self.cur = None;
        }
        match &self.cur {
            Some((g, t, len)) => {
                let pos = t.pos().as_millis() as u64;
                PlayState { guid: Some(*g), playing: !t.is_paused(), pos_ms: if *len > 0 { pos.min(*len) } else { pos }, len_ms: *len }
            }
            None => PlayState::default(),
        }
    }
}

fn run<T: Track>(
    rx: mpsc::Receiver<Cmd>,
    shared: Arc<Mutex<PlayState>>,
    lengths: Lengths,
    ctx: Context,
    worker: Worker,
    lang: Lang,
    mut open: impl FnMut(&Path) -> Result<(T, u64), String>,
) {
    let mut m = Machine { cur: None };
    loop {
        // Idle: sleep until a command; loaded: wake every tick for the position.
        let cmd = if m.cur.is_some() { rx.recv_timeout(TICK) } else { rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected) };
        match cmd {
            Ok(Cmd::Toggle(guid, path)) => {
                if let Err(e) = m.toggle(guid, &path, &mut open) {
                    let text = trf(lang, Key::CannotPlay, &[&e]);
                    worker.submit(Box::new(move |_| WorkResult::Error(text)));
                }
            }
            Ok(Cmd::Seek(guid, frac)) => m.seek(guid, frac),
            Ok(Cmd::Probe(guid, path)) => {
                let len = probe_duration(&path);
                lengths.lock().unwrap().insert(guid, len);
                ctx.request_repaint();
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        let now = m.state();
        let changed = std::mem::replace(&mut *shared.lock().unwrap(), now) != now;
        if changed || now.playing {
            ctx.request_repaint();
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};

    pub(crate) const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/voice_message.m4a");

    /// A sound that never ends; `log` records what the machine did to it.
    #[derive(Default)]
    pub(crate) struct Fake {
        name: String,
        paused: Cell<bool>,
        pos: Cell<Duration>,
        log: Rc<RefCell<Vec<String>>>,
    }

    impl Track for Fake {
        fn play(&self) { self.paused.set(false) }
        fn pause(&self) { self.paused.set(true) }
        fn is_paused(&self) -> bool { self.paused.get() }
        fn stop(&self) { self.log.borrow_mut().push(format!("stop {}", self.name)) }
        fn seek(&self, to: Duration) { self.pos.set(to) }
        fn pos(&self) -> Duration { self.pos.get() }
        fn done(&self) -> bool { self.name == "done" }
    }

    #[test]
    fn decode_android_voice_fixture() {
        let ms = probe_duration(Path::new(FIXTURE)).expect("AAC-LC in MP4 probes");
        assert!(ms.abs_diff(2000) <= 100, "{ms} ms");
        assert_eq!(probe_duration(Path::new("no/such/file.m4a")), None);
        // It decodes to samples without ffmpeg, through the source the player thread hands to rodio.
        use rodio::Source as _;
        let mut dec = Decoded::open(Path::new(FIXTURE)).expect("symphonia opens the m4a");
        assert_eq!((dec.channels(), dec.sample_rate()), (1, 44100));
        let peak = dec.by_ref().take(44100).fold(0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.1, "a tone, not silence: peak {peak}");
        let n = 44100 + dec.count();
        assert!(n.abs_diff(88200) < 4410, "{n} samples ≈ 2 s at 44.1 kHz mono");
        // Seeking to 1.5 s leaves about 0.5 s.
        let mut dec = Decoded::open(Path::new(FIXTURE)).unwrap();
        dec.try_seek(Duration::from_millis(1500)).unwrap();
        let rest = dec.count();
        assert!(rest.abs_diff(22050) < 4410, "{rest} samples left after the seek");
        assert!(Decoded::open(Path::new(file!())).is_err(), "not audio");
        // current_frame_len counts the samples left in the packet, also right after a seek.
        let mut dec = Decoded::open(Path::new(FIXTURE)).unwrap();
        let f = dec.current_frame_len().unwrap();
        dec.next();
        assert_eq!(dec.current_frame_len(), Some(f - 1));
        dec.try_seek(Duration::from_millis(500)).unwrap();
        assert!(dec.current_frame_len().unwrap() > 0, "a packet from after the seek is buffered");
        dec.by_ref().for_each(drop);
        assert_eq!(dec.current_frame_len(), Some(0), "nothing left at the end");
        assert_eq!(Decoded::open(Path::new(FIXTURE)).unwrap().len_ms, ms, "the length comes with the opened file");
    }

    #[test]
    fn len_probe_runs_off_the_ui_thread() {
        let (_tmp, core) = crate::test_support::test_app();
        let ctx = Context::default();
        let worker = Worker::spawn(core, mpsc::channel().0, ctx.clone());
        let player = Player::spawn(ctx, worker, Lang::Ru);
        assert_eq!(player.len_ms(1, Path::new(FIXTURE)), None, "unknown yet: no file I/O on the caller's thread");
        let mut len = None;
        for _ in 0..300 {
            len = player.len_ms(1, Path::new(FIXTURE));
            if len.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(len.is_some_and(|ms| ms.abs_diff(2000) <= 100), "{len:?}");
        assert_eq!(player.len_ms(2, Path::new("no/such.m4a")), None);
    }

    #[test]
    fn toggle_switches_track() {
        let log = Rc::new(RefCell::new(vec![]));
        let mut opened = vec![];
        let mut open = |p: &Path| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            opened.push(name.clone());
            Ok((Fake { name, log: log.clone(), ..Default::default() }, probe_duration(p).unwrap_or(0)))
        };
        let mut m = Machine { cur: None };
        assert_eq!(m.state(), PlayState::default(), "idle");
        m.toggle(1, Path::new(FIXTURE), &mut open).unwrap();
        assert_eq!(m.state(), PlayState { guid: Some(1), playing: true, pos_ms: 0, len_ms: m.cur.as_ref().unwrap().2 });
        assert!(m.state().len_ms.abs_diff(2000) <= 100, "length from the file");
        m.seek(1, 0.5);
        m.seek(2, 0.9); // not loaded: ignored
        assert_eq!(m.state().pos_ms, m.state().len_ms / 2);
        m.toggle(1, Path::new(FIXTURE), &mut open).unwrap();
        assert!(!m.state().playing && m.state().guid == Some(1), "same message: pause, position kept");
        m.toggle(1, Path::new(FIXTURE), &mut open).unwrap();
        assert!(m.state().playing, "and resume");
        m.toggle(2, Path::new("b.mp3"), &mut open).unwrap();
        assert_eq!(m.state(), PlayState { guid: Some(2), playing: true, pos_ms: 0, len_ms: 0 }, "another message starts from 0");
        assert_eq!(*log.borrow(), ["stop voice_message.m4a"], "the old one stopped");
        // A failed open leaves nothing loaded.
        let mut fail = |_: &Path| -> Result<(Fake, u64), String> { Err("no device".into()) };
        assert_eq!(m.toggle(3, Path::new("c.mp3"), &mut fail), Err("no device".into()));
        assert_eq!(m.state(), PlayState::default());
        // Played to the end: unloaded.
        m.toggle(4, Path::new("done"), &mut open).unwrap();
        assert_eq!(m.state(), PlayState::default());
        assert_eq!(opened, ["voice_message.m4a", "b.mp3", "done"], "opened once each");
    }
}

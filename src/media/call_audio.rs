//! The call audio engine: what Android's AudioSender / AudioReceiver (1:1) and GroupAudioSender / GroupAudioMixer
//! (group) do, through persistent ffmpeg processes (the runtime voice messages already need).
//!
//! Wire format (Android's, binding): bare AAC-LC frames, 44.1 kHz mono, 48 kbit/s, 1024 samples (~23 ms) each, no
//! ADTS header (Android primes its decoders with the AudioSpecificConfig [0x12, 0x10]). The encoder writes ADTS, whose
//! headers are stripped before sending; each received frame gets a synthesized header on its way into a decoder.
//!
//! Per-frame callbacks (~43/s) never reach the UI queue: the core's Bridge hands them to [Sink]. Only failures come
//! back, as `Event::CallAudio`; the call itself (signalling) is the core's and goes on without audio.
use crate::core::events::Event;
use eframe::egui;
use mimir::MimirApp;
use std::{
    collections::{HashMap, VecDeque},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdout, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering::SeqCst},
        mpsc::{self, Sender, SyncSender, TrySendError},
        Arc, Mutex, MutexGuard,
    },
    time::{Duration, Instant},
};

pub const SAMPLE_RATE: u32 = 44_100;
const BITRATE: u32 = 48_000;
pub const FRAME_SAMPLES: usize = 1024;
/// Decoded frames per sender: how many to gather before playing, and the most kept (older ones are dropped). 1:1 has
/// AudioReceiver's 5-packet cap; group has GroupParticipantAudio's initial depth 3 and cap 6.
const PEER_JITTER: (usize, usize) = (2, 5);
const GROUP_JITTER: (usize, usize) = (3, 6);
/// GroupAudioSender's VAD/DTX: the RMS (16-bit scale) of a voiced frame, and how many frames go out after the last one.
const VAD_RMS: f32 = 400.;
const VAD_HANGOVER: u32 = 10;
/// The output device has not asked for samples this long: it is gone.
const OUTPUT_STALL: Duration = Duration::from_secs(2);
/// A decoder whose ffmpeg quit is replaced, at most this often per sender.
const RESPAWN: Duration = Duration::from_secs(1);

/// Why call audio does not work. Each kind is reported once per session.
#[derive(Debug, Clone, PartialEq)]
pub enum AudioError {
    /// No input device, or it failed (unplugged).
    Microphone(String),
    /// No output device, or it stopped.
    Speaker(String),
    /// ffmpeg is missing, failed to start, or the encoder quit.
    Codec(String),
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

// ---------------------------------------------------------------------------------------------------- the engine

/// One call's audio at a time, owned by the app; `sink` goes to the core's Bridge.
pub struct CallAudio {
    core: Arc<MimirApp>,
    ffmpeg: Option<PathBuf>,
    report: Reporter,
    sink: Arc<Sink>,
    muted: Arc<AtomicBool>,
    session: Option<Arc<Shared>>,
}

impl CallAudio {
    pub fn new(core: Arc<MimirApp>, ffmpeg: Option<PathBuf>, events: Sender<Event>, ctx: egui::Context) -> CallAudio {
        let report = Reporter { events, ctx };
        CallAudio { core, ffmpeg, report, sink: Arc::default(), muted: Arc::default(), session: None }
    }

    /// Whether calls can have audio at all (ffmpeg was found); the call buttons follow it.
    pub fn available(&self) -> bool {
        self.ffmpeg.is_some()
    }

    /// Where the Bridge delivers received frames.
    pub fn sink(&self) -> Arc<Sink> {
        self.sink.clone()
    }

    /// A 1:1 call with `pubkey` is up: microphone to `send_call_packet`, its `on_call_packet` frames to the speaker.
    pub fn start_peer(&mut self, pubkey: Vec<u8>) {
        self.start(Target::Peer(pubkey));
    }

    /// A group call is joined: microphone (with VAD/DTX) to `send_group_call_audio`, every sender mixed to the speaker.
    pub fn start_group(&mut self) {
        self.start(Target::Group);
    }

    /// Ends the session (if any): its threads stop and release the devices and ffmpeg within a fraction of a second.
    pub fn stop(&mut self) {
        *lock(&self.sink.session) = None;
        if let Some(s) = self.session.take() {
            s.stop.store(true, SeqCst);
            lock(&s.decoders).clear();
        }
        self.muted.store(false, SeqCst);
    }

    /// Muted, captured samples are dropped and no packets go out (Android). Kept across `start_*` (joining muted),
    /// reset by `stop`.
    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, SeqCst);
    }

    /// Group participants heard in the frame just played (the speaking indicator).
    pub fn speaking(&self) -> Vec<Vec<u8>> {
        self.session.as_ref().map(|s| lock(&s.mix).speaking()).unwrap_or_default()
    }

    /// The microphone hears voice (and is not muted).
    pub fn mic_voiced(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.voiced.load(SeqCst))
    }

    fn start(&mut self, target: Target) {
        let muted = self.muted.load(SeqCst);
        self.stop();
        self.muted.store(muted, SeqCst);
        let shared = Arc::new(Shared::new(target, self.ffmpeg.clone(), self.report.clone()));
        *lock(&self.sink.session) = Some(shared.clone());
        match self.ffmpeg.clone() {
            Some(bin) => {
                let (s, core, muted) = (shared.clone(), self.core.clone(), self.muted.clone());
                if let Err(e) = std::thread::Builder::new().name("call-mic".into()).spawn(move || capture(s, core, bin, muted)) {
                    shared.fail(AudioError::Microphone(e.to_string()));
                }
            }
            None => shared.fail(AudioError::Codec(NO_FFMPEG.into())),
        }
        let s = shared.clone();
        if let Err(e) = std::thread::Builder::new().name("call-out".into()).spawn(move || playout(s)) {
            shared.fail(AudioError::Speaker(e.to_string()));
        }
        self.session = Some(shared);
    }
}

impl Drop for CallAudio {
    fn drop(&mut self) {
        self.stop();
    }
}

const NO_FFMPEG: &str = "ffmpeg not found";

#[derive(Debug, Clone, PartialEq)]
enum Target {
    Peer(Vec<u8>),
    Group,
}

#[derive(Clone)]
struct Reporter {
    events: Sender<Event>,
    ctx: egui::Context,
}

/// A session's state, shared by its threads, the Bridge (through [Sink]) and the playout source.
struct Shared {
    target: Target,
    ffmpeg: Option<PathBuf>,
    report: Reporter,
    stop: AtomicBool,
    /// Bits of the `AudioError` kinds already reported, and `NO_DECODER`.
    reported: AtomicU8,
    voiced: AtomicBool,
    mix: Mutex<Mix>,
    decoders: Mutex<HashMap<Vec<u8>, Decoder>>,
    /// When the output device last asked for samples.
    pulled: Mutex<Instant>,
}

impl Shared {
    fn new(target: Target, ffmpeg: Option<PathBuf>, report: Reporter) -> Shared {
        let (stop, reported, voiced) = (AtomicBool::new(false), AtomicU8::new(0), AtomicBool::new(false));
        Shared { target, ffmpeg, report, stop, reported, voiced, mix: Mutex::default(), decoders: Mutex::default(), pulled: Mutex::new(Instant::now()) }
    }

    fn depth(&self) -> (usize, usize) {
        if self.target == Target::Group { GROUP_JITTER } else { PEER_JITTER }
    }

    /// Reports `e` unless its kind already was, or the session is over (its threads failing as they are torn down).
    fn fail(&self, e: AudioError) {
        let bit = match e {
            AudioError::Microphone(_) => 1,
            AudioError::Speaker(_) => 2,
            AudioError::Codec(_) => 4,
        };
        if self.stop.load(SeqCst) || self.reported.fetch_or(bit, SeqCst) & bit != 0 {
            return;
        }
        tracing::warn!(error = ?e, "call audio");
        let _ = self.report.events.send(Event::CallAudio(e));
        self.report.ctx.request_repaint();
    }

    /// One received frame from `from` into its decoder, started on its first frame.
    fn decode(self: &Arc<Self>, from: Vec<u8>, mut frame: Vec<u8>) {
        let mut decoders = lock(&self.decoders);
        // Checked under the lock `CallAudio::stop` clears the map with: no decoder outlives its session.
        if self.stop.load(SeqCst) {
            return;
        }
        if let Some(d) = decoders.get(&from) {
            match d.frames.try_send(frame) {
                Err(TrySendError::Disconnected(f)) if d.born.elapsed() >= RESPAWN => frame = f,
                // Queued, dropped because ffmpeg is behind, or its decoder just died and is not replaced yet.
                _ => return,
            }
            tracing::warn!("call decoder quit; restarting it");
        }
        if self.reported.load(SeqCst) & NO_DECODER != 0 {
            return; // ffmpeg would not start: not tried again on every frame
        }
        let Some(bin) = &self.ffmpeg else { return self.fail(AudioError::Codec(NO_FFMPEG.into())) };
        match spawn_decoder(bin, self.clone(), from.clone()) {
            Ok(d) => {
                let _ = d.frames.try_send(frame);
                decoders.insert(from, d);
            }
            Err(e) => {
                self.reported.fetch_or(NO_DECODER, SeqCst);
                self.fail(AudioError::Codec(e));
            }
        }
    }
}

/// A `reported` bit: a decoder failed to start. Apart from `Codec`'s, so an encoder that quit leaves decoding alone.
const NO_DECODER: u8 = 8;

/// The receive side, called on the core's callback threads; frames for no running session (or another one) are dropped.
#[derive(Default)]
pub struct Sink {
    session: Mutex<Option<Arc<Shared>>>,
}

impl Sink {
    /// `on_call_packet`: a frame of the 1:1 call with `from`.
    pub fn peer_frame(&self, from: Vec<u8>, frame: Vec<u8>) {
        self.route(Target::Peer(from.clone()), from, frame);
    }

    /// `on_group_call_audio`: a frame from group call participant `from`.
    // ponytail: frames play in arrival order (the mediator relays them nearly in order); seq is not used to reorder.
    pub fn group_frame(&self, from: Vec<u8>, frame: Vec<u8>) {
        self.route(Target::Group, from, frame);
    }

    fn route(&self, kind: Target, from: Vec<u8>, frame: Vec<u8>) {
        let session = lock(&self.session).clone();
        if let Some(s) = session.filter(|s| s.target == kind) {
            s.decode(from, frame);
        }
    }
}

// ---------------------------------------------------------------------------------------------------- framing

/// The 7-byte ADTS header (no CRC) of one AAC-LC 44.1 kHz mono frame of `len` bytes.
fn adts_header(len: usize) -> [u8; 7] {
    let full = len + 7;
    [
        0xFF,
        0xF1,                                // MPEG-4, no CRC
        0x50,                                // profile LC, 44.1 kHz (index 4), channels high bit 0
        0x40 | ((full >> 11) & 0x03) as u8,  // 1 channel, length bits 12..11
        ((full >> 3) & 0xFF) as u8,          // length bits 10..3
        (((full & 0x07) << 5) | 0x1F) as u8, // length bits 2..0, buffer fullness 0x7FF
        0xFC,
    ]
}

/// The complete frames at the start of an ADTS stream, headers (and CRC) stripped; the rest stays in `buf`.
fn split_adts(buf: &mut Vec<u8>) -> Vec<Vec<u8>> {
    let (mut frames, mut pos) = (Vec::new(), 0);
    while buf.len() >= pos + 7 {
        let full = ((buf[pos + 3] as usize & 0x03) << 11) | ((buf[pos + 4] as usize) << 3) | (buf[pos + 5] as usize >> 5);
        let header = if buf[pos + 1] & 0x01 == 0 { 9 } else { 7 };
        if buf[pos] != 0xFF || buf[pos + 1] & 0xF0 != 0xF0 || full < header {
            pos += 1; // not a header: resync
            continue;
        }
        if buf.len() < pos + full {
            break; // the rest of this frame is still on its way
        }
        frames.push(buf[pos + header..pos + full].to_vec());
        pos += full;
    }
    buf.drain(..pos);
    frames
}

/// Decoded f32 bytes into 1024-sample frames; a partial frame (or sample) waits for the next read.
#[derive(Default)]
struct Framer {
    carry: Vec<u8>,
}

impl Framer {
    fn feed(&mut self, bytes: &[u8]) -> Vec<Vec<f32>> {
        const BYTES: usize = FRAME_SAMPLES * 4;
        self.carry.extend_from_slice(bytes);
        let whole = self.carry.len() / BYTES * BYTES;
        let frames = self.carry[..whole].as_chunks::<BYTES>().0.iter().map(|f| f.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()).collect();
        self.carry.drain(..whole);
        frames
    }
}

/// RMS on the 16-bit scale Android's thresholds use.
fn rms(pcm: &[f32]) -> f32 {
    if pcm.is_empty() {
        return 0.;
    }
    (pcm.iter().map(|s| (s * 32768.).powi(2)).sum::<f32>() / pcm.len() as f32).sqrt()
}

/// GroupAudioSender's DTX gate: a frame goes out while voiced and for the hangover after.
#[derive(Default)]
struct Vad {
    hang: u32,
}

impl Vad {
    fn gate(&mut self, voiced: bool) -> bool {
        self.hang = if voiced { VAD_HANGOVER } else { self.hang.saturating_sub(1) };
        self.hang > 0
    }
}

// ---------------------------------------------------------------------------------------------------- playout

/// One sender's decoded frames.
struct Jitter {
    frames: VecDeque<Vec<f32>>,
    prime: usize,
    cap: usize,
    primed: bool,
    /// RMS of the frame last played.
    level: f32,
}

impl Jitter {
    fn new((prime, cap): (usize, usize)) -> Jitter {
        Jitter { frames: VecDeque::new(), prime, cap, primed: false, level: 0. }
    }

    /// Beyond the cap the oldest frame goes: skipping beats piling up latency.
    fn push(&mut self, frame: Vec<f32>) {
        self.frames.push_back(frame);
        while self.frames.len() > self.cap {
            self.frames.pop_front();
        }
    }

    /// The next frame to play; none while gathering `prime` frames, which starts over whenever it runs dry (a late
    /// packet, or the sender's DTX silence).
    fn pull(&mut self) -> Option<Vec<f32>> {
        self.primed |= self.frames.len() >= self.prime;
        let frame = if self.primed { self.frames.pop_front() } else { None };
        self.primed &= frame.is_some();
        self.level = frame.as_deref().map_or(0., rms);
        frame
    }
}

#[derive(Default)]
struct Mix {
    senders: HashMap<Vec<u8>, Jitter>,
}

impl Mix {
    fn push(&mut self, from: &[u8], depth: (usize, usize), frame: Vec<f32>) {
        match self.senders.get_mut(from) {
            Some(j) => j.push(frame),
            None => {
                let mut j = Jitter::new(depth);
                j.push(frame);
                self.senders.insert(from.to_vec(), j);
            }
        }
    }

    /// GroupAudioMixer's step: a frame from every sender that has one, summed and hard-clipped; silence otherwise.
    fn next_frame(&mut self) -> Vec<f32> {
        let mut out = vec![0f32; FRAME_SAMPLES];
        for frame in self.senders.values_mut().filter_map(Jitter::pull) {
            out.iter_mut().zip(frame).for_each(|(o, s)| *o += s);
        }
        out.iter_mut().for_each(|s| *s = s.clamp(-1., 1.));
        out
    }

    fn speaking(&self) -> Vec<Vec<u8>> {
        self.senders.iter().filter(|(_, j)| j.level >= VAD_RMS).map(|(k, _)| k.clone()).collect()
    }
}

/// The session's endless mono 44.1 kHz source; rodio resamples it for the device, whose clock paces the mixing.
struct Playout {
    shared: Arc<Shared>,
    frame: Vec<f32>,
    pos: usize,
}

impl Iterator for Playout {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        if self.pos == self.frame.len() {
            if self.shared.stop.load(SeqCst) {
                return None;
            }
            self.frame = lock(&self.shared.mix).next_frame();
            self.pos = 0;
            *lock(&self.shared.pulled) = Instant::now();
        }
        self.pos += 1;
        Some(self.frame[self.pos - 1])
    }
}

impl rodio::Source for Playout {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> u16 {
        1
    }
    fn sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

/// The output thread: holds the (!Send) output stream for the session and watches that it keeps playing.
fn playout(shared: Arc<Shared>) {
    let source = Playout { shared: shared.clone(), frame: Vec::new(), pos: 0 };
    crate::media::pin_audio_enumerator();
    let opened = rodio::OutputStream::try_default()
        .map_err(|e| e.to_string())
        .and_then(|(stream, handle)| handle.play_raw(source).map(|()| stream).map_err(|e| e.to_string()));
    let _stream = match opened {
        Ok(s) => s,
        Err(e) => return shared.fail(AudioError::Speaker(e)),
    };
    *lock(&shared.pulled) = Instant::now();
    while !shared.stop.load(SeqCst) {
        std::thread::sleep(Duration::from_millis(250));
        // ponytail: rodio 0.20 reports no lost device; a source it stopped pulling is how a lost speaker shows. Playout
        // does not move to another device.
        if lock(&shared.pulled).elapsed() > OUTPUT_STALL {
            shared.fail(AudioError::Speaker("audio output stopped".into()));
        }
    }
}

// ---------------------------------------------------------------------------------------------------- ffmpeg

/// Raw f32 PCM as captured in, Android's AAC as an ADTS stream out, one packet at a time.
pub(super) fn encoder_args(rate: u32, channels: u16) -> Vec<String> {
    let (rate, channels, bitrate, out_rate) = (rate.to_string(), channels.to_string(), BITRATE.to_string(), SAMPLE_RATE.to_string());
    // No probing (it holds ~5 s of input back before the first frame); no -fflags nobuffer: with it ffmpeg 8 drops all
    // raw input ("Output file is empty").
    ["-hide_banner", "-loglevel", "error", "-probesize", "32", "-analyzeduration", "0", "-f", "f32le", "-ar", &rate, "-ac", &channels, "-i", "pipe:0"]
        .into_iter()
        .chain(["-ar", &out_rate, "-ac", "1", "-c:a", "aac", "-b:a", &bitrate, "-f", "adts", "-flush_packets", "1", "pipe:1"])
        .map(String::from)
        .collect()
}

/// ADTS in (no probing: the format is known), f32 44.1 kHz mono out as soon as each frame is decoded.
pub(super) fn decoder_args() -> Vec<String> {
    let rate = SAMPLE_RATE.to_string();
    ["-hide_banner", "-loglevel", "error", "-fflags", "nobuffer", "-probesize", "32", "-analyzeduration", "0", "-f", "aac", "-i", "pipe:0"]
        .into_iter()
        .chain(["-f", "f32le", "-ar", &rate, "-ac", "1", "-flush_packets", "1", "pipe:1"])
        .map(String::from)
        .collect()
}

fn spawn_ffmpeg(bin: &Path, args: Vec<String>) -> std::io::Result<Child> {
    // stderr is not read, so it must not be a pipe that could fill up and stall ffmpeg.
    crate::media::ffmpeg::command(bin).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()
}

/// A sender's decoder: frames queue to a writer thread (the core's callback thread never blocks on a pipe), a reader
/// thread frames the PCM into the mix. Dropping it ends both, and ffmpeg.
struct Decoder {
    frames: SyncSender<Vec<u8>>,
    born: Instant,
}

fn spawn_decoder(bin: &Path, shared: Arc<Shared>, from: Vec<u8>) -> Result<Decoder, String> {
    let mut child = spawn_ffmpeg(bin, decoder_args()).map_err(|e| e.to_string())?;
    let (mut stdin, mut stdout) = (child.stdin.take().expect("piped stdin"), child.stdout.take().expect("piped stdout"));
    let (frames, queue) = mpsc::sync_channel::<Vec<u8>>(16);
    std::thread::spawn(move || {
        for frame in queue {
            if stdin.write_all(&adts_header(frame.len())).and_then(|()| stdin.write_all(&frame)).is_err() {
                break; // ffmpeg quit: the closed queue tells `decode` to start another
            }
        }
        drop(stdin);
        let _ = child.kill();
        let _ = child.wait();
    });
    let depth = shared.depth();
    std::thread::spawn(move || {
        let (mut framer, mut buf) = (Framer::default(), [0u8; 8192]);
        while let Ok(n @ 1..) = stdout.read(&mut buf) {
            let decoded = framer.feed(&buf[..n]);
            let mut mix = lock(&shared.mix);
            decoded.into_iter().for_each(|f| mix.push(&from, depth, f));
        }
    });
    Ok(Decoder { frames, born: Instant::now() })
}

/// The capture thread: microphone to the encoder until the session stops.
fn capture(shared: Arc<Shared>, core: Arc<MimirApp>, bin: PathBuf, muted: Arc<AtomicBool>) {
    let lost = shared.clone();
    let opened = crate::media::recorder::open_input(move |e| lost.fail(AudioError::Microphone(e.to_string())));
    let (buf, rate, channels, stream) = match opened {
        Ok(input) => input,
        Err(e) => return shared.fail(AudioError::Microphone(e)),
    };
    let mut child = match spawn_ffmpeg(&bin, encoder_args(rate, channels)) {
        Ok(c) => c,
        Err(e) => return shared.fail(AudioError::Codec(e.to_string())),
    };
    let (input, output) = (pipe_writer(child.stdin.take().expect("piped stdin")), child.stdout.take().expect("piped stdout"));
    let sender = {
        let shared = shared.clone();
        std::thread::spawn(move || send_frames(&shared, &core, output))
    };
    while !shared.stop.load(SeqCst) {
        std::thread::sleep(Duration::from_millis(20));
        let pcm = std::mem::take(&mut *lock(&buf));
        if muted.load(SeqCst) {
            shared.voiced.store(false, SeqCst);
            continue;
        }
        if pcm.is_empty() {
            continue;
        }
        shared.voiced.store(rms(&pcm) >= VAD_RMS, SeqCst);
        let bytes: Vec<u8> = pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        if !feed(&shared, &input, bytes) {
            break;
        }
    }
    drop(stream); // the microphone first: killing a hung ffmpeg is what ends its writer
    drop(input);
    let _ = child.kill();
    let _ = child.wait();
    let _ = sender.join();
}

/// Captured chunks (~20 ms each) the encoder may fall behind by; past that it is taken to be hung.
const ENCODER_QUEUE: usize = 100;

/// The encoder's stdin on a thread of its own: the capture loop never blocks on an ffmpeg that hangs without quitting.
fn pipe_writer(mut input: impl Write + Send + 'static) -> SyncSender<Vec<u8>> {
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(ENCODER_QUEUE);
    std::thread::spawn(move || {
        for chunk in rx {
            if input.write_all(&chunk).is_err() {
                break;
            }
        }
    });
    tx
}

/// One captured chunk to the encoder; false when capture must end: the encoder quit (`send_frames` reports it) or
/// hangs (reported here, and the microphone is released).
fn feed(shared: &Shared, input: &SyncSender<Vec<u8>>, bytes: Vec<u8>) -> bool {
    match input.try_send(bytes) {
        Ok(()) => true,
        Err(TrySendError::Full(_)) => {
            shared.fail(AudioError::Codec("ffmpeg encoder stalled".into()));
            false
        }
        Err(TrySendError::Disconnected(_)) => false,
    }
}

/// The encoder's ADTS stream, stripped, to the core; group frames through the DTX gate.
// ponytail: an encoder that quits mid-call is reported, not restarted; the UI ends the call or carries on listening.
fn send_frames(shared: &Shared, core: &MimirApp, mut out: ChildStdout) {
    let (mut pending, mut buf, mut vad) = (Vec::new(), [0u8; 4096], Vad::default());
    while let Ok(n @ 1..) = out.read(&mut buf) {
        pending.extend_from_slice(&buf[..n]);
        for frame in split_adts(&mut pending) {
            if shared.stop.load(SeqCst) {
                return;
            }
            let sent = match &shared.target {
                Target::Peer(pubkey) => core.send_call_packet(pubkey.clone(), frame),
                Target::Group if vad.gate(shared.voiced.load(SeqCst)) => core.send_group_call_audio(frame),
                Target::Group => continue,
            };
            if let Err(e) = sent {
                tracing::debug!(error = %e, "call frame not sent");
            }
        }
    }
    shared.fail(AudioError::Codec("ffmpeg encoder quit".into()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared(target: Target, ffmpeg: Option<PathBuf>) -> (Arc<Shared>, mpsc::Receiver<Event>) {
        let (events, rx) = mpsc::channel();
        (Arc::new(Shared::new(target, ffmpeg, Reporter { events, ctx: egui::Context::default() })), rx)
    }

    #[test]
    fn adts_header_round_trips() {
        let frame: Vec<u8> = (0..300u32).map(|i| i as u8).collect();
        let mut stream = adts_header(frame.len()).to_vec();
        stream.extend_from_slice(&frame);
        assert_eq!(&stream[..7], &[0xFF, 0xF1, 0x50, 0x40, 0x26, 0x7F, 0xFC], "LC, 44.1 kHz, mono, length 307");
        // Garbage before it, and the frame arriving in two reads.
        let mut buf = vec![0x00, 0xFF, 0x12];
        buf.extend_from_slice(&stream[..100]);
        assert!(split_adts(&mut buf).is_empty(), "half a frame waits");
        buf.extend_from_slice(&stream[100..]);
        buf.extend_from_slice(&stream[..3]);
        assert_eq!(split_adts(&mut buf), std::slice::from_ref(&frame));
        assert_eq!(buf, &stream[..3], "the next header's start is kept");
        // A header with CRC (protection_absent 0) is 9 bytes.
        let mut crc = adts_header(frame.len() + 2).to_vec();
        crc[1] = 0xF0;
        crc.extend_from_slice(&[0xAA, 0xBB]);
        crc.extend_from_slice(&frame);
        assert_eq!(split_adts(&mut crc), [frame]);
        assert!(crc.is_empty());
    }

    #[test]
    fn decoded_pcm_is_framed_by_1024_samples() {
        let samples: Vec<f32> = (0..FRAME_SAMPLES * 2 + 3).map(|i| i as f32).collect();
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut framer = Framer::default();
        // Reads that split samples in the middle.
        let frames: Vec<Vec<f32>> = bytes.chunks(4093).flat_map(|c| framer.feed(c)).collect();
        assert_eq!(frames.len(), 2);
        assert!(frames.iter().all(|f| f.len() == FRAME_SAMPLES));
        assert_eq!(frames.concat(), samples[..FRAME_SAMPLES * 2]);
        assert_eq!(framer.carry.len(), 12, "three samples wait for the next frame");
        assert!((FRAME_SAMPLES as f64 / SAMPLE_RATE as f64 - 0.0232).abs() < 0.0001, "~23 ms a frame");
    }

    #[test]
    fn jitter_primes_and_drops_oldest() {
        let mut j = Jitter::new(GROUP_JITTER);
        let frame = |v: f32| vec![v; FRAME_SAMPLES];
        j.push(frame(1.));
        j.push(frame(2.));
        assert_eq!(j.pull(), None, "gathering 3 first");
        (3..=8).for_each(|v| j.push(frame(v as f32)));
        let played: Vec<f32> = std::iter::from_fn(|| j.pull()).map(|f| f[0]).collect();
        assert_eq!(played, [3., 4., 5., 6., 7., 8.], "capped at 6: the oldest went");
        j.push(frame(9.));
        assert_eq!(j.pull(), None, "ran dry: gathers 3 again");
        j.push(frame(10.));
        j.push(frame(11.));
        assert_eq!(j.pull().map(|f| f[0]), Some(9.));
        let mut peer = Jitter::new(PEER_JITTER);
        (0..8).for_each(|v| peer.push(frame(v as f32)));
        assert_eq!(peer.frames.len(), 5, "1:1 keeps AudioReceiver's 5");
    }

    #[test]
    fn mixer_sums_and_clips() {
        let mut mix = Mix::default();
        for (from, v) in [(&[1u8][..], 0.75f32), (&[2], 0.5), (&[3], -0.1)] {
            (0..3).for_each(|_| mix.push(from, GROUP_JITTER, vec![v; FRAME_SAMPLES]));
        }
        let out = mix.next_frame();
        assert_eq!(out.len(), FRAME_SAMPLES);
        assert!(out.iter().all(|&s| s == 1.), "0.75 + 0.5 - 0.1 clips to 1");
        let mut speaking = mix.speaking();
        speaking.sort();
        assert_eq!(speaking, [vec![1u8], vec![2], vec![3]]);
        let mut quiet = Mix::default();
        (0..3).for_each(|_| quiet.push(&[4], GROUP_JITTER, vec![-0.9; FRAME_SAMPLES]));
        (0..3).for_each(|_| quiet.push(&[5], GROUP_JITTER, vec![-0.9; FRAME_SAMPLES]));
        assert!(quiet.next_frame().iter().all(|&s| s == -1.), "clipped below too");
        let mut empty = Mix::default();
        assert!(empty.next_frame().iter().all(|&s| s == 0.), "nobody: silence");
        assert!(empty.speaking().is_empty());
    }

    #[test]
    fn vad_hangover() {
        let mut vad = Vad::default();
        assert!(!vad.gate(false), "silence from the start sends nothing");
        assert!(vad.gate(true));
        let after: Vec<bool> = (0..12).map(|_| vad.gate(false)).collect();
        assert_eq!(after.iter().filter(|&&s| s).count(), 9, "9 more silent frames go out after the voiced one");
        assert!(after[..9].iter().all(|&s| s) && !after[9]);
        assert!(rms(&[0.02; 64]) >= VAD_RMS && rms(&[0.01; 64]) < VAD_RMS, "-40 dBFS gate");
    }

    #[test]
    fn sink_routes_only_to_the_running_session() {
        let sink = Sink::default();
        sink.peer_frame(vec![7; 32], vec![1, 2, 3]);
        let (peer, rx) = shared(Target::Peer(vec![7; 32]), None);
        *lock(&sink.session) = Some(peer);
        sink.peer_frame(vec![8; 32], vec![1, 2, 3]);
        sink.group_frame(vec![7; 32], vec![1, 2, 3]);
        assert!(rx.try_recv().is_err(), "another caller's or a group's frames are dropped");
        sink.peer_frame(vec![7; 32], vec![1, 2, 3]);
        sink.peer_frame(vec![7; 32], vec![1, 2, 3]);
        assert!(matches!(rx.try_recv(), Ok(Event::CallAudio(AudioError::Codec(e))) if e == NO_FFMPEG));
        assert!(rx.try_recv().is_err(), "reported once");
        let (group, rx) = shared(Target::Group, None);
        group.stop.store(true, SeqCst);
        *lock(&sink.session) = Some(group);
        sink.group_frame(vec![9; 32], vec![1]);
        assert!(rx.try_recv().is_err(), "a stopped session reports nothing");
    }

    #[test]
    fn decoders_outlive_the_encoder() {
        // Any program stands in for ffmpeg: only that a decoder is started is checked.
        let (s, rx) = shared(Target::Group, Some(std::env::current_exe().unwrap()));
        s.fail(AudioError::Codec("ffmpeg encoder quit".into()));
        assert!(rx.try_recv().is_ok());
        s.decode(vec![5; 32], vec![1, 2, 3]);
        assert!(lock(&s.decoders).contains_key(&vec![5u8; 32]), "someone heard after the encoder died gets a decoder");
        lock(&s.decoders).clear();
        let (s, rx) = shared(Target::Group, Some(PathBuf::from("no-such-ffmpeg")));
        s.decode(vec![5; 32], vec![1]);
        s.decode(vec![6; 32], vec![1]);
        assert!(matches!(rx.try_recv(), Ok(Event::CallAudio(AudioError::Codec(_)))));
        assert!(rx.try_recv().is_err() && lock(&s.decoders).is_empty(), "a failed start is reported once, not retried");
    }

    #[test]
    fn a_hung_encoder_is_reported_not_waited_on() {
        /// An ffmpeg that takes no input until released.
        struct Hung(mpsc::Receiver<()>);
        impl Write for Hung {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                let _ = self.0.recv();
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let (release, hold) = mpsc::channel();
        let (s, rx) = shared(Target::Group, None);
        let input = pipe_writer(Hung(hold));
        let fed = (0..ENCODER_QUEUE + 2).take_while(|_| feed(&s, &input, vec![0; 4])).count();
        assert!(fed <= ENCODER_QUEUE + 1, "capture gives up instead of blocking: {fed}");
        assert!(matches!(rx.try_recv(), Ok(Event::CallAudio(AudioError::Codec(e))) if e.contains("stalled")));
        drop(release);
    }

    /// The real ffmpeg: a tone through the encoder (headers stripped like the wire), then the frames one by one into
    /// a decoder, whose PCM must arrive while it is fed (streaming, not at the end of input).
    #[test]
    #[ignore = "needs ffmpeg"]
    fn real_ffmpeg_round_trip() {
        let bin = crate::media::ffmpeg::locate().expect("ffmpeg");
        let mut enc = spawn_ffmpeg(&bin, encoder_args(48_000, 1)).unwrap();
        let mut stdin = enc.stdin.take().unwrap();
        // 2 s of tone at the capture pump's pace: 20 ms (960 samples) every 20 ms.
        let tone: Vec<u8> = (0..96_000).flat_map(|i| ((i as f32 * 0.06).sin() * 0.3).to_le_bytes()).collect();
        let feeding = Arc::new(AtomicBool::new(true));
        let still = feeding.clone();
        let started = Instant::now();
        let writer = std::thread::spawn(move || {
            for chunk in tone.chunks(960 * 4) {
                stdin.write_all(chunk).unwrap();
                std::thread::sleep(Duration::from_millis(20));
            }
            still.store(false, SeqCst);
        });
        let (mut out, mut adts, mut buf, mut first) = (enc.stdout.take().unwrap(), Vec::new(), [0u8; 4096], None);
        while let Ok(n @ 1..) = out.read(&mut buf) {
            adts.extend_from_slice(&buf[..n]);
            if first.is_none() && feeding.load(SeqCst) {
                first = Some(started.elapsed());
            }
        }
        writer.join().unwrap();
        assert!(enc.wait().unwrap().success());
        let first = first.expect("encoded frames while PCM was still being fed");
        assert!(first < Duration::from_secs(1), "encoder latency {first:?}");
        let frames = split_adts(&mut adts);
        assert!(adts.is_empty(), "the stream is whole ADTS frames");
        assert!((86..=90).contains(&frames.len()), "2 s is ~86 frames of 1024 samples: {}", frames.len());
        assert!(frames.iter().all(|f| !f.is_empty() && f.len() < 1000 && !(f[0] == 0xFF && f[1] & 0xF0 == 0xF0)), "bare frames");

        let (s, rx) = shared(Target::Group, Some(bin));
        let from = vec![5u8; 32];
        let fed = Instant::now();
        let mut heard_at = None;
        for f in &frames {
            s.decode(from.clone(), f.clone());
            std::thread::sleep(Duration::from_millis(23));
            if heard_at.is_none() && lock(&s.mix).senders.get(&from).is_some_and(|j| !j.frames.is_empty()) {
                heard_at = Some(fed.elapsed());
            }
        }
        let heard_at = heard_at.expect("decoded PCM while frames were still being fed");
        assert!(heard_at < Duration::from_millis(500), "decoder latency {heard_at:?}");
        let out = lock(&s.mix).next_frame();
        assert!(rms(&out) > 3000., "the tone comes back: rms {}", rms(&out));
        assert!(rx.try_recv().is_err(), "no errors");
        lock(&s.decoders).clear();
    }
}

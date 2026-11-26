//! Microphone capture: the default input device at its own rate and channel count, every sample as f32.
//! The cpal stream is !Send, so it lives (and is dropped) on a thread of its own; the UI holds a \`Recorder\`.
use cpal::{traits::{DeviceTrait, HostTrait, StreamTrait}, FromSample, Sample, SampleFormat, SizedSample};
use std::{
    cell::RefCell,
    sync::{mpsc::{self, Receiver, RecvTimeoutError, Sender}, Arc, Mutex},
    time::{Duration, Instant},
};

/// Android's setMaxDuration(60000) and its 1000 ms minimum.
pub const MAX_MS: u64 = 60_000;
pub const MIN_MS: u64 = 1_000;
/// \`finish\`'s error for a take under a second (Android \`recording_too_short\`).
pub const TOO_SHORT: &str = "recording_too_short";
/// The error when there is no input device (shown as «Микрофон не найден»).
pub const NO_MIC: &str = "No microphone found";

/// Interleaved samples as captured.
#[derive(Debug)]
pub struct Recording {
    pub pcm: Vec<f32>,
    pub rate: u32,
    pub channels: u16,
}

enum Ctl {
    Finish,
    Cancel,
}

pub struct Recorder {
    ctl: Sender<Ctl>,
    done: Receiver<Result<Recording, String>>,
    started: Instant,
    /// A capture error, once the thread reported it.
    error: RefCell<Option<String>>,
}

impl Recorder {
    /// Starts capturing from the default microphone. Opening it happens on the capture thread (it can take a while):
    /// a device error shows up in \`failed\`.
    pub fn start() -> Result<Recorder, String> {
        Self::spawn(Instant::now(), capture)
    }

    fn spawn(started: Instant, run: impl FnOnce(Receiver<Ctl>) -> Result<Recording, String> + Send + 'static) -> Result<Recorder, String> {
        let (ctl, ctl_rx) = mpsc::channel();
        let (tx, done) = mpsc::channel();
        std::thread::Builder::new().name("mic".into()).spawn(move || drop(tx.send(run(ctl_rx)))).map_err(|e| e.to_string())?;
        Ok(Recorder { ctl, done, started, error: RefCell::new(None) })
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    /// Why the capture stopped on its own (no device, device error), if it did.
    pub fn failed(&self) -> Option<String> {
        let mut error = self.error.borrow_mut();
        // Before \`finish\` the thread only ever reports a failure.
        if let Ok(Err(e)) = self.done.try_recv() {
            *error = Some(e);
        }
        error.clone()
    }

    /// Stops and throws the take away.
    pub fn cancel(self) {
        let _ = self.ctl.send(Ctl::Cancel);
    }

    /// Stops and hands over the take, at most 60 s of it; under a second is \`TOO_SHORT\`. Waits for the capture
    /// thread: call it off the UI thread.
    pub fn finish(self) -> Result<Recording, String> {
        if let Some(e) = self.failed() {
            return Err(e);
        }
        let _ = self.ctl.send(Ctl::Finish);
        checked(self.done.recv().map_err(|e| e.to_string())??)
    }

    /// A recorder that "captured" \`rec\` and began at \`started\` (tests never open a device).
    #[cfg(test)]
    pub fn fake(rec: Recording, started: Instant) -> Recorder {
        Self::spawn(started, move |ctl| match ctl.recv() {
            Ok(Ctl::Finish) => Ok(rec),
            _ => Err("cancelled".into()),
        })
        .unwrap()
    }

    /// A recorder whose device failed to open with \`err\`.
    #[cfg(test)]
    pub fn failing(err: &'static str) -> Recorder {
        Self::spawn(Instant::now(), move |_| Err(err.into())).unwrap()
    }
}

fn checked(mut rec: Recording) -> Result<Recording, String> {
    let per_sec = rec.rate as u64 * rec.channels as u64;
    rec.pcm.truncate((per_sec * MAX_MS / 1000) as usize);
    if per_sec == 0 || (rec.pcm.len() as u64) * 1000 < per_sec * MIN_MS {
        return Err(TOO_SHORT.into());
    }
    Ok(rec)
}

/// The capture thread: records until told to stop or until the 60 s cap, which stops the capture and waits.
fn capture(ctl: Receiver<Ctl>) -> Result<Recording, String> {
    // ponytail: a stream error mid-take (device unplugged) is not reported; the take just ends where it stopped.
    let (buf, rate, channels, stream) = open_input(|_| {})?;
    let first = ctl.recv_timeout(Duration::from_millis(MAX_MS));
    drop(stream);
    let finish = match first {
        Ok(c) => matches!(c, Ctl::Finish),
        Err(RecvTimeoutError::Timeout) => matches!(ctl.recv(), Ok(Ctl::Finish)),
        Err(RecvTimeoutError::Disconnected) => false,
    };
    if !finish {
        return Err("cancelled".into());
    }
    let pcm = std::mem::take(&mut *buf.lock().unwrap());
    Ok(Recording { pcm, rate, channels })
}

type Input = (Arc<Mutex<Vec<f32>>>, u32, u16, cpal::Stream);

/// Opens the input stream, with I32 (some WASAPI/ALSA devices) and the conversion done by cpal's FromSample.
/// `on_error` hears what fails once it runs (the device unplugged).
pub(crate) fn open_input(on_error: impl FnMut(cpal::StreamError) + Send + 'static) -> Result<Input, String> {
    crate::media::pin_audio_enumerator();
    let device = cpal::default_host().default_input_device().ok_or(NO_MIC)?;
    let supported = device.default_input_config().map_err(|e| e.to_string())?;
    let (rate, channels, config) = (supported.sample_rate().0, supported.channels(), supported.config());
    let buf = Arc::new(Mutex::new(Vec::new()));
    let stream = match supported.sample_format() {
        SampleFormat::F32 => build::<f32>(&device, &config, &buf, on_error),
        SampleFormat::I16 => build::<i16>(&device, &config, &buf, on_error),
        SampleFormat::U16 => build::<u16>(&device, &config, &buf, on_error),
        SampleFormat::I32 => build::<i32>(&device, &config, &buf, on_error),
        other => return Err(format!("Unsupported microphone sample format {other}")),
    }
    .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok((buf, rate, channels, stream))
}

fn build<T: SizedSample>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    buf: &Arc<Mutex<Vec<f32>>>,
    on_error: impl FnMut(cpal::StreamError) + Send + 'static,
) -> Result<cpal::Stream, cpal::BuildStreamError>
where
    f32: FromSample<T>,
{
    let buf = buf.clone();
    device.build_input_stream(config, move |data: &[T], _: &cpal::InputCallbackInfo| buf.lock().unwrap().extend(data.iter().map(|&s| f32::from_sample(s))), on_error, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: f32, rate: u32, channels: u16) -> Recording {
        Recording { pcm: vec![0.1; (s * rate as f32) as usize * channels as usize], rate, channels }
    }

    #[test]
    fn too_short_recording() {
        assert_eq!(checked(secs(0.9, 8000, 2)).err().as_deref(), Some(TOO_SHORT));
        assert_eq!(checked(Recording { pcm: vec![], rate: 0, channels: 0 }).err().as_deref(), Some(TOO_SHORT));
        assert_eq!(checked(secs(1.0, 8000, 2)).unwrap().pcm.len(), 16000);
        let rec = Recorder::fake(secs(0.5, 8000, 1), Instant::now());
        assert_eq!(rec.finish().err().as_deref(), Some(TOO_SHORT));
    }

    #[test]
    fn sixty_second_cap_trims() {
        let rec = Recorder::fake(secs(61.5, 1000, 2), Instant::now() - Duration::from_millis(61_500));
        assert!(rec.elapsed_ms() >= MAX_MS);
        let r = rec.finish().unwrap();
        assert_eq!((r.pcm.len(), r.rate, r.channels), (120_000, 1000, 2), "exactly 60 s kept");
    }

    #[test]
    fn cancel_and_failure() {
        let rec = Recorder::fake(secs(2., 1000, 1), Instant::now());
        assert_eq!(rec.failed(), None);
        rec.cancel();
        let broken = Recorder::failing("No microphone found");
        let end = Instant::now() + Duration::from_secs(5);
        while broken.failed().is_none() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(broken.failed().as_deref(), Some("No microphone found"), "kept until the bar shows it");
    }

    /// Needs a microphone: records 1.5 s from the default input and encodes it with the real ffmpeg.
    #[test]
    #[ignore = "needs a microphone and ffmpeg"]
    fn records_and_encodes_real_mic() {
        let rec = Recorder::start().unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        assert_eq!(rec.failed(), None);
        let r = rec.finish().unwrap();
        assert!(r.rate >= 8000 && r.channels >= 1 && !r.pcm.is_empty());
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("v.m4a");
        crate::media::ffmpeg::encode_voice(&crate::media::ffmpeg::locate().unwrap(), &r.pcm, r.rate, r.channels, 32000, &out).unwrap();
        assert!(std::fs::metadata(&out).unwrap().len() > 1000);
    }
}

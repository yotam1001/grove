//! Grove's librespot sink: equalizer and fades on the decoded samples, then
//! a rodio output on the chosen Windows device. It reopens the output when
//! the choice or the Windows default changes, and reports a lost output
//! instead of blocking, so librespot pauses and Grove can resume later.
use super::{
    control::control,
    dsp::{self, Equalizer, GainRamp, CHANNELS, SAMPLE_RATE},
    fade::{self, TrackClock},
    settings::SYSTEM_DEFAULT,
};
use cpal::traits::{DeviceTrait, HostTrait};
use librespot_playback::{
    audio_backend::{Sink, SinkError, SinkResult},
    convert::Converter,
    decoder::AudioPacket,
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

/// About half a second of audio queued in rodio, like librespot's own sink.
const QUEUED_CHUNKS: usize = 26;
/// A queue that does not drain for this long means the device went away.
const STALL: Duration = Duration::from_secs(3);

struct Output {
    sink: rodio::Sink,
    _stream: rodio::OutputStream,
}

pub struct GroveSink {
    out: Option<Output>,
    opened_generation: u64,
    failed: Arc<AtomicBool>,
    eq: Equalizer,
    eq_generation: u64,
    ramp: GainRamp,
    clock: TrackClock,
    frames: u64,
}

impl GroveSink {
    pub fn new() -> Self {
        Self {
            out: None,
            opened_generation: 0,
            failed: Arc::new(AtomicBool::new(false)),
            eq: Equalizer::default(),
            eq_generation: 0,
            ramp: GainRamp::default(),
            clock: TrackClock::default(),
            frames: 0,
        }
    }

    /// Equalizer, fades and sleep level, in that order.
    fn process(&mut self, samples: &mut [f64]) {
        let control = control();
        let generation = control.eq_generation();
        if generation != self.eq_generation {
            self.eq_generation = generation;
            self.eq.retune(&control.settings().eq_bands());
        }
        let clock = control.clock();
        if clock.epoch != self.clock.epoch {
            self.clock = clock;
            self.frames = 0;
        }
        self.eq.process(samples);
        let position_ms =
            self.clock.anchor_ms + self.frames as f64 * 1000.0 / f64::from(SAMPLE_RATE);
        let clock = TrackClock {
            fade_out: fade::fade_out(&self.clock, control.next_gapless()),
            ..self.clock
        };
        let target = fade::gain(&clock, position_ms) * control.sleep_gain();
        self.ramp.apply(samples, target);
        dsp::clamp(samples);
        self.frames += (samples.len() / CHANNELS) as u64;
    }

    fn ensure_open(&mut self) -> SinkResult<()> {
        let control = control();
        let generation = control.device_generation();
        if self.out.is_some()
            && (generation != self.opened_generation || self.failed.load(Ordering::Acquire))
        {
            self.out = None;
        }
        if self.out.is_none() {
            let wanted = control.settings().device;
            match open(&wanted, self.failed.clone()) {
                Ok((output, name)) => {
                    output.sink.play();
                    self.opened_generation = generation;
                    self.out = Some(output);
                    control.set_output(|o| {
                        o.opened = name;
                        o.lost = false;
                    });
                }
                Err(error) => {
                    control.set_output(|o| {
                        o.opened = None;
                        o.lost = true;
                    });
                    return Err(SinkError::ConnectionRefused(error));
                }
            }
        }
        Ok(())
    }

    fn lose_output(&mut self) -> SinkError {
        self.out = None;
        control().set_output(|o| {
            o.opened = None;
            o.lost = true;
        });
        SinkError::OnWrite("Audio output unavailable".into())
    }
}

impl Default for GroveSink {
    fn default() -> Self {
        Self::new()
    }
}

fn open(wanted: &str, failed: Arc<AtomicBool>) -> Result<(Output, Option<String>), String> {
    let host = cpal::default_host();
    let device = if wanted == SYSTEM_DEFAULT {
        host.default_output_device()
    } else {
        host.output_devices()
            .map_err(|e| e.to_string())?
            .find(|d| d.name().ok().as_deref() == Some(wanted))
    }
    .ok_or_else(|| "Audio output unavailable".to_string())?;
    let name = device.name().ok();
    failed.store(false, Ordering::Release);
    let flag = failed.clone();
    let mut stream = rodio::OutputStreamBuilder::from_device(device)
        .map_err(|e| e.to_string())?
        .with_error_callback(move |_error: cpal::StreamError| {
            // Device removal, an invalidated endpoint or a dead stream: the
            // next write reopens (following the default) or reports it.
            flag.store(true, Ordering::Release);
        })
        .open_stream_or_fallback()
        .map_err(|e| e.to_string())?;
    stream.log_on_drop(false);
    let sink = rodio::Sink::connect_new(stream.mixer());
    Ok((
        Output {
            sink,
            _stream: stream,
        },
        name,
    ))
}

impl Sink for GroveSink {
    fn start(&mut self) -> SinkResult<()> {
        self.ensure_open()?;
        if let Some(out) = &self.out {
            out.sink.play();
        }
        Ok(())
    }

    /// Never fails: librespot exits the process if stopping a sink errors.
    fn stop(&mut self) -> SinkResult<()> {
        if let Some(out) = &self.out {
            let started = Instant::now();
            while !out.sink.empty()
                && !self.failed.load(Ordering::Acquire)
                && started.elapsed() < Duration::from_millis(1500)
            {
                thread::sleep(Duration::from_millis(10));
            }
            out.sink.pause();
        }
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, _converter: &mut Converter) -> SinkResult<()> {
        let mut samples = packet
            .samples()
            .map_err(|e| SinkError::OnWrite(e.to_string()))?
            .to_vec();
        self.process(&mut samples);
        self.ensure_open()?;
        let Some(out) = &self.out else {
            return Err(self.lose_output());
        };
        let samples: Vec<f32> = samples.iter().map(|s| *s as f32).collect();
        out.sink.append(rodio::buffer::SamplesBuffer::new(
            CHANNELS as u16,
            SAMPLE_RATE as u32,
            samples,
        ));
        let started = Instant::now();
        let mut stalled = false;
        while out.sink.len() > QUEUED_CHUNKS {
            if self.failed.load(Ordering::Acquire) || started.elapsed() > STALL {
                stalled = true;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        if stalled {
            return Err(self.lose_output());
        }
        Ok(())
    }
}

/// Output names Windows reports, plus the current default.
pub fn scan() -> (Vec<String>, Option<String>) {
    let host = cpal::default_host();
    let default = host.default_output_device().and_then(|d| d.name().ok());
    let present = host
        .output_devices()
        .map(|devices| {
            devices
                .filter_map(|d| d.name().ok())
                .filter(|name| super::settings::valid_device(name) && name != SYSTEM_DEFAULT)
                .take(32)
                .collect()
        })
        .unwrap_or_default();
    (present, default)
}

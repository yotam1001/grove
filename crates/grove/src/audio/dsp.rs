//! Sample processing for Grove's sink: a biquad equalizer and smoothed gain.
//! Pure and allocation-free per sample, so it is unit-tested without audio.
//!
//! Filters follow Robert Bristow-Johnson's "Audio EQ Cookbook": peaking bands
//! in the middle, shelves at the two ends of the curve.

/// librespot always decodes to 44.1 kHz stereo.
pub const SAMPLE_RATE: f32 = 44_100.0;
pub const CHANNELS: usize = 2;

pub const EQ_FREQUENCIES_10: [f32; 10] = [
    31.0, 62.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
];
pub const EQ_FREQUENCIES_5: [f32; 5] = [60.0, 250.0, 1000.0, 4000.0, 12000.0];

/// Fixed presets as (name, bands of (Hz, dB)). They mirror Tide's preset list.
pub const EQ_PRESETS: [(&str, &[(f32, f32)]); 7] = [
    ("Flat", &[]),
    ("Bass boost", &[(60.0, 6.0), (150.0, 3.0)]),
    ("Bass reducer", &[(60.0, -6.0), (150.0, -3.0)]),
    ("Treble boost", &[(6000.0, 4.0), (12000.0, 5.0)]),
    ("Vocal", &[(250.0, -2.0), (2500.0, 4.0)]),
    ("Loudness", &[(60.0, 5.0), (10000.0, 4.0)]),
    ("Acoustic", &[(125.0, 2.0), (1000.0, -1.0), (4000.0, 2.5)]),
];
/// The preset index after [`EQ_PRESETS`] means the custom curve.
pub const CUSTOM_PRESET: usize = EQ_PRESETS.len();

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    LowShelf,
    Peaking,
    HighShelf,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coefficients {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Coefficients {
    pub fn new(shape: Shape, frequency: f32, gain_db: f32, q: f32) -> Self {
        let frequency = f64::from(frequency.clamp(10.0, SAMPLE_RATE * 0.45));
        let a = 10f64.powf(f64::from(gain_db) / 40.0);
        let w0 = 2.0 * std::f64::consts::PI * frequency / f64::from(SAMPLE_RATE);
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * f64::from(q.max(0.1)));
        let (b0, b1, b2, a0, a1, a2) = match shape {
            Shape::Peaking => (
                1.0 + alpha * a,
                -2.0 * cos,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cos,
                1.0 - alpha / a,
            ),
            Shape::LowShelf => {
                let k = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) - (a - 1.0) * cos + k),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                    a * ((a + 1.0) - (a - 1.0) * cos - k),
                    (a + 1.0) + (a - 1.0) * cos + k,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                    (a + 1.0) + (a - 1.0) * cos - k,
                )
            }
            Shape::HighShelf => {
                let k = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) + (a - 1.0) * cos + k),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                    a * ((a + 1.0) + (a - 1.0) * cos - k),
                    (a + 1.0) - (a - 1.0) * cos + k,
                    2.0 * ((a - 1.0) - (a + 1.0) * cos),
                    (a + 1.0) - (a - 1.0) * cos - k,
                )
            }
        };
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
        }
    }

    /// Magnitude response in dB at `frequency`, for tests and the curve preview.
    pub fn response_db(&self, frequency: f32) -> f64 {
        let w = 2.0 * std::f64::consts::PI * f64::from(frequency) / f64::from(SAMPLE_RATE);
        let (c1, s1) = (w.cos(), w.sin());
        let (c2, s2) = ((2.0 * w).cos(), (2.0 * w).sin());
        let num_re = self.b0 + self.b1 * c1 + self.b2 * c2;
        let num_im = -(self.b1 * s1 + self.b2 * s2);
        let den_re = 1.0 + self.a1 * c1 + self.a2 * c2;
        let den_im = -(self.a1 * s1 + self.a2 * s2);
        let magnitude =
            ((num_re * num_re + num_im * num_im) / (den_re * den_re + den_im * den_im)).sqrt();
        20.0 * magnitude.log10()
    }
}

/// Transposed direct form II state for one channel.
#[derive(Clone, Copy, Debug, Default)]
struct State {
    z1: f64,
    z2: f64,
}

impl State {
    #[inline]
    fn process(&mut self, c: &Coefficients, x: f64) -> f64 {
        let y = c.b0 * x + self.z1;
        self.z1 = c.b1 * x - c.a1 * y + self.z2;
        self.z2 = c.b2 * x - c.a2 * y;
        y
    }
}

/// A cascade of biquads over interleaved stereo samples.
#[derive(Clone, Debug, Default)]
pub struct Equalizer {
    filters: Vec<(Coefficients, [State; CHANNELS])>,
    preamp: f64,
    channel: usize,
}

impl Equalizer {
    /// Build from (Hz, dB) bands. Boosts lower the level first so they cannot clip.
    pub fn new(bands: &[(f32, f32)]) -> Self {
        let bands: Vec<(f32, f32)> = bands
            .iter()
            .copied()
            .filter(|(f, g)| f.is_finite() && g.is_finite() && g.abs() >= 0.05)
            .take(10)
            .collect();
        let count = bands.len();
        let filters = bands
            .iter()
            .enumerate()
            .map(|(index, &(frequency, gain))| {
                // Shelves only on a curve with at least five points, so a
                // two-band preset keeps both of its bands as bells.
                let shape = if count >= 5 && index == 0 {
                    Shape::LowShelf
                } else if count >= 5 && index == count - 1 {
                    Shape::HighShelf
                } else {
                    Shape::Peaking
                };
                let q = if shape == Shape::Peaking { 1.1 } else { 0.71 };
                (
                    Coefficients::new(shape, frequency, gain.clamp(-12.0, 12.0), q),
                    [State::default(); CHANNELS],
                )
            })
            .collect::<Vec<_>>();
        let peak = curve_peak_db(&filters.iter().map(|(c, _)| *c).collect::<Vec<_>>());
        Self {
            filters,
            preamp: 10f64.powf(-peak.max(0.0) / 20.0),
            channel: 0,
        }
    }

    #[cfg(test)]
    pub fn is_flat(&self) -> bool {
        self.filters.is_empty()
    }

    /// Keep each filter's history when only the curve changes, so a slider
    /// move never clicks.
    pub fn retune(&mut self, bands: &[(f32, f32)]) {
        let mut next = Self::new(bands);
        if next.filters.len() == self.filters.len() {
            for (new, old) in next.filters.iter_mut().zip(&self.filters) {
                new.1 = old.1;
            }
        }
        next.channel = self.channel;
        *self = next;
    }

    /// Process interleaved stereo samples in place.
    pub fn process(&mut self, samples: &mut [f64]) {
        if self.filters.is_empty() {
            return;
        }
        for sample in samples.iter_mut() {
            let mut value = *sample * self.preamp;
            for (coefficients, states) in &mut self.filters {
                value = states[self.channel].process(coefficients, value);
            }
            *sample = value;
            self.channel = (self.channel + 1) % CHANNELS;
        }
    }

    #[cfg(test)]
    pub fn preamp_db(&self) -> f64 {
        20.0 * self.preamp.log10()
    }
}

/// Highest point of the combined curve over the audible range, in dB.
fn curve_peak_db(filters: &[Coefficients]) -> f64 {
    if filters.is_empty() {
        return 0.0;
    }
    (0..96)
        .map(|step| 20.0 * 1000f32.powf(step as f32 / 95.0))
        .map(|frequency| {
            filters
                .iter()
                .map(|c| c.response_db(frequency))
                .sum::<f64>()
        })
        .fold(f64::MIN, f64::max)
}

/// A linear gain that glides to its target over `ramp_samples`, so fades and
/// sleep-timer steps sound continuous even though targets arrive per packet.
#[derive(Clone, Debug)]
pub struct GainRamp {
    current: f64,
    ramp_samples: f64,
}

impl Default for GainRamp {
    fn default() -> Self {
        Self::new(0.02)
    }
}

impl GainRamp {
    pub fn new(ramp_seconds: f64) -> Self {
        Self {
            current: 1.0,
            ramp_samples: (ramp_seconds * f64::from(SAMPLE_RATE) * CHANNELS as f64).max(1.0),
        }
    }

    #[cfg(test)]
    pub fn current(&self) -> f64 {
        self.current
    }

    pub fn apply(&mut self, samples: &mut [f64], target: f64) {
        let target = if target.is_finite() {
            target.clamp(0.0, 1.0)
        } else {
            1.0
        };
        if (self.current - target).abs() < 1e-6 {
            self.current = target;
            if target < 1.0 {
                for sample in samples.iter_mut() {
                    *sample *= target;
                }
            }
            return;
        }
        let step = (target - self.current) / self.ramp_samples;
        for sample in samples.iter_mut() {
            if (target - self.current).abs() > step.abs() {
                self.current += step;
            } else {
                self.current = target;
            }
            *sample *= self.current;
        }
    }
}

/// Keep processed samples inside the converter's range.
pub fn clamp(samples: &mut [f64]) {
    for sample in samples.iter_mut() {
        *sample = sample.clamp(-1.0, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(frequency: f32, seconds: f32) -> Vec<f64> {
        let frames = (SAMPLE_RATE * seconds) as usize;
        (0..frames)
            .flat_map(|n| {
                let v = (2.0 * std::f64::consts::PI * f64::from(frequency) * n as f64
                    / f64::from(SAMPLE_RATE))
                .sin()
                    * 0.25;
                [v, v]
            })
            .collect()
    }

    fn rms_db(samples: &[f64]) -> f64 {
        // Skip the first quarter so filter warm-up does not count.
        let tail = &samples[samples.len() / 4..];
        let rms = (tail.iter().map(|s| s * s).sum::<f64>() / tail.len() as f64).sqrt();
        20.0 * rms.log10()
    }

    #[test]
    fn flat_equalizer_is_bit_identical() {
        let mut eq = Equalizer::new(&[]);
        assert!(eq.is_flat());
        let input = sine(440.0, 0.1);
        let mut output = input.clone();
        eq.process(&mut output);
        assert_eq!(input, output);
        assert!(Equalizer::new(&[(1000.0, 0.0), (60.0, 0.01)]).is_flat());
    }

    #[test]
    fn peaking_band_boosts_its_centre_and_leaves_distant_frequencies() {
        let c = Coefficients::new(Shape::Peaking, 1000.0, 6.0, 1.1);
        assert!((c.response_db(1000.0) - 6.0).abs() < 0.05);
        assert!(c.response_db(60.0).abs() < 0.3);
        assert!(c.response_db(16000.0).abs() < 0.3);
        let cut = Coefficients::new(Shape::Peaking, 1000.0, -6.0, 1.1);
        assert!((cut.response_db(1000.0) + 6.0).abs() < 0.05);
    }

    #[test]
    fn shelves_lift_their_side_of_the_spectrum() {
        let low = Coefficients::new(Shape::LowShelf, 100.0, 6.0, 0.71);
        assert!((low.response_db(20.0) - 6.0).abs() < 0.5);
        assert!(low.response_db(5000.0).abs() < 0.2);
        let high = Coefficients::new(Shape::HighShelf, 8000.0, -6.0, 0.71);
        assert!((high.response_db(18000.0) + 6.0).abs() < 1.0);
        assert!(high.response_db(200.0).abs() < 0.2);
    }

    #[test]
    fn boosting_curves_get_headroom_so_a_full_scale_tone_does_not_clip() {
        let eq = Equalizer::new(EQ_PRESETS[1].1);
        assert!(eq.preamp_db() < -5.0, "Bass boost lowers the level first");
        let cut = Equalizer::new(EQ_PRESETS[2].1);
        assert_eq!(cut.preamp_db(), 0.0, "A cut needs no headroom");
        let mut eq = Equalizer::new(&[(60.0, 12.0), (62.0, 12.0)]);
        let mut loud: Vec<f64> = sine(60.0, 0.5).iter().map(|s| s * 4.0).collect();
        eq.process(&mut loud);
        assert!(
            loud.iter().all(|s| s.abs() <= 1.05),
            "Headroom prevents clipping"
        );
    }

    #[test]
    fn processed_tones_follow_the_curve_and_channels_stay_independent() {
        let mut eq = Equalizer::new(&[(1000.0, -9.0)]);
        let mut tone = sine(1000.0, 0.4);
        let before = rms_db(&tone);
        eq.process(&mut tone);
        assert!((rms_db(&tone) - before + 9.0).abs() < 0.5);
        // Left silent and right loud stay that way: no crosstalk.
        let mut eq = Equalizer::new(&[(1000.0, 6.0)]);
        let mut stereo: Vec<f64> = sine(1000.0, 0.1)
            .chunks(2)
            .flat_map(|f| [0.0, f[1]])
            .collect();
        eq.process(&mut stereo);
        assert!(stereo.chunks(2).all(|f| f[0] == 0.0));
    }

    #[test]
    fn every_preset_and_custom_extreme_is_stable() {
        let mut curves: Vec<Vec<(f32, f32)>> = EQ_PRESETS.iter().map(|(_, b)| b.to_vec()).collect();
        curves.push(EQ_FREQUENCIES_10.iter().map(|f| (*f, 12.0)).collect());
        curves.push(EQ_FREQUENCIES_5.iter().map(|f| (*f, -12.0)).collect());
        curves.push(
            EQ_FREQUENCIES_10
                .iter()
                .enumerate()
                .map(|(i, f)| (*f, if i % 2 == 0 { 12.0 } else { -12.0 }))
                .collect(),
        );
        for bands in curves {
            let mut eq = Equalizer::new(&bands);
            let mut noise: Vec<f64> = (0..44_100)
                .map(|n| ((n * 7919 % 2003) as f64 / 1001.5 - 1.0) * 0.5)
                .collect();
            eq.process(&mut noise);
            assert!(
                noise.iter().all(|s| s.is_finite() && s.abs() < 4.0),
                "{bands:?} diverged"
            );
        }
    }

    #[test]
    fn retuning_keeps_filter_history() {
        let mut eq = Equalizer::new(&[(1000.0, 6.0)]);
        let mut tone = sine(1000.0, 0.05);
        eq.process(&mut tone);
        let state = eq.filters[0].1;
        eq.retune(&[(1000.0, 5.0)]);
        assert_eq!(eq.filters[0].1[0].z1, state[0].z1);
    }

    #[test]
    fn gain_ramp_glides_without_steps_and_settles_on_target() {
        let mut ramp = GainRamp::new(0.01);
        let mut ones = vec![1.0; 2000];
        ramp.apply(&mut ones, 0.0);
        let steps: Vec<f64> = ones.windows(2).map(|w| (w[0] - w[1]).abs()).collect();
        assert!(steps.iter().all(|s| *s < 0.002), "No audible step");
        assert_eq!(ramp.current(), 0.0);
        assert_eq!(*ones.last().unwrap(), 0.0);
        let mut more = vec![1.0; 10];
        ramp.apply(&mut more, 0.0);
        assert!(more.iter().all(|s| *s == 0.0));
        let mut unity = vec![0.5; 10];
        let mut ramp = GainRamp::new(0.01);
        ramp.apply(&mut unity, f64::NAN);
        assert!(
            unity.iter().all(|s| *s == 0.5),
            "Unity gain is bit-identical"
        );
        let mut wild = vec![2.0, -3.0];
        clamp(&mut wild);
        assert_eq!(wild, vec![1.0, -1.0]);
    }
}

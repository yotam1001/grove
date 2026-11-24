//! State shared by the UI thread, the librespot worker, the sink thread and
//! the device scanner. Small locks only; nothing here blocks on audio.
use super::{fade::TrackClock, settings::AudioSettings};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Mutex, MutexGuard, OnceLock,
};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DeviceScan {
    pub present: Vec<String>,
    pub default: Option<String>,
    pub scanned: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct OutputState {
    /// The device the sink has open.
    pub opened: Option<String>,
    /// The last open or write failed; librespot paused.
    pub lost: bool,
}

pub struct Control {
    settings: Mutex<AudioSettings>,
    eq_generation: AtomicU64,
    device_generation: AtomicU64,
    clock: Mutex<TrackClock>,
    sleep_gain: AtomicU64,
    output: Mutex<OutputState>,
    devices: Mutex<DeviceScan>,
    resume_dirty: AtomicBool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Default for Control {
    fn default() -> Self {
        Self {
            settings: Mutex::new(AudioSettings::default()),
            eq_generation: AtomicU64::new(1),
            device_generation: AtomicU64::new(1),
            clock: Mutex::new(TrackClock::default()),
            sleep_gain: AtomicU64::new(1f64.to_bits()),
            output: Mutex::new(OutputState::default()),
            devices: Mutex::new(DeviceScan::default()),
            resume_dirty: AtomicBool::new(false),
        }
    }
}

/// The process-wide control block.
pub fn control() -> &'static Control {
    static CONTROL: OnceLock<Control> = OnceLock::new();
    CONTROL.get_or_init(Control::default)
}

impl Control {
    pub fn settings(&self) -> AudioSettings {
        lock(&self.settings).clone()
    }

    /// Replace the settings and tell the sink what changed.
    pub fn set_settings(&self, next: AudioSettings) {
        self.update_settings(|s| *s = next);
    }

    /// Change the settings in place (sanitized); returns (before, after).
    pub fn update_settings(
        &self,
        f: impl FnOnce(&mut AudioSettings),
    ) -> (AudioSettings, AudioSettings) {
        let mut current = lock(&self.settings);
        let before = current.clone();
        f(&mut current);
        current.sanitize();
        if before.eq_bands() != current.eq_bands() {
            self.eq_generation.fetch_add(1, Ordering::AcqRel);
        }
        if before.device != current.device {
            self.device_generation.fetch_add(1, Ordering::AcqRel);
        }
        (before, current.clone())
    }

    pub fn with_settings<R>(&self, f: impl FnOnce(&mut AudioSettings) -> R) -> R {
        f(&mut lock(&self.settings))
    }

    pub fn eq_generation(&self) -> u64 {
        self.eq_generation.load(Ordering::Acquire)
    }

    pub fn device_generation(&self) -> u64 {
        self.device_generation.load(Ordering::Acquire)
    }

    /// Ask the sink to reopen its output on the next packet.
    pub fn reopen(&self) {
        self.device_generation.fetch_add(1, Ordering::AcqRel);
    }

    pub fn clock(&self) -> TrackClock {
        *lock(&self.clock)
    }

    pub fn update_clock(&self, f: impl FnOnce(&mut TrackClock)) {
        let mut clock = lock(&self.clock);
        f(&mut clock);
        clock.epoch = clock.epoch.wrapping_add(1);
    }

    pub fn sleep_gain(&self) -> f64 {
        f64::from_bits(self.sleep_gain.load(Ordering::Acquire))
    }

    pub fn set_sleep_gain(&self, gain: f64) {
        let gain = if gain.is_finite() {
            gain.clamp(0.0, 1.0)
        } else {
            1.0
        };
        self.sleep_gain.store(gain.to_bits(), Ordering::Release);
    }

    pub fn output(&self) -> OutputState {
        lock(&self.output).clone()
    }

    pub fn set_output(&self, f: impl FnOnce(&mut OutputState)) {
        f(&mut lock(&self.output));
    }

    pub fn devices(&self) -> DeviceScan {
        lock(&self.devices).clone()
    }

    pub fn set_devices(&self, scan: DeviceScan) {
        *lock(&self.devices) = scan;
    }

    pub fn mark_resume_dirty(&self) {
        self.resume_dirty.store(true, Ordering::Release);
    }

    pub fn take_resume_dirty(&self) -> bool {
        self.resume_dirty.swap(false, Ordering::AcqRel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_relevant_changes_wake_the_sink() {
        let control = Control::default();
        let (eq, device) = (control.eq_generation(), control.device_generation());
        let mut next = control.settings();
        next.fade_seconds = 4;
        control.set_settings(next.clone());
        assert_eq!(
            (control.eq_generation(), control.device_generation()),
            (eq, device)
        );
        next.eq_preset = 1;
        control.set_settings(next.clone());
        assert_eq!(control.eq_generation(), eq + 1);
        next.device = "Headphones".into();
        control.set_settings(next);
        assert_eq!(control.device_generation(), device + 1);
        control.set_sleep_gain(f64::NAN);
        assert_eq!(control.sleep_gain(), 1.0);
        let epoch = control.clock().epoch;
        control.update_clock(|c| c.anchor_ms = 5.0);
        assert_eq!(control.clock().epoch, epoch + 1);
    }
}

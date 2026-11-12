//! Pure button edges, interface pinning, and recorder ownership.

use std::collections::HashSet;

use crate::app_settings::{HotkeyBinding, HotkeyKey, NativeMouseButton, modifiers_from_flags};
use crate::audio::CaptureInstant;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Edge {
    pub binding: NativeMouseButton,
    pub source: u64,
    pub down: bool,
    pub at: CaptureInstant,
}

/// Unknown and initially held inputs must lift before they can create a press.
#[derive(Clone, Copy, Debug)]
pub(super) struct ButtonState {
    down: Option<bool>,
    latest: CaptureInstant,
}

impl ButtonState {
    pub fn new(down: Option<bool>, latest: CaptureInstant) -> Self {
        Self { down, latest }
    }

    pub fn needs_recovery(&self) -> bool {
        self.down != Some(false)
    }

    pub fn receive(&mut self, down: bool, at: CaptureInstant) -> bool {
        if at <= self.latest {
            return false;
        }
        self.latest = at;
        let previous = self.down.replace(down);
        previous != Some(down) && (previous.is_some() || !down)
    }

    /// Sampling can release a button, never press it. Samples carry the device's
    /// value timestamp, so a queued older down cannot undo recovery.
    pub fn sample(&mut self, down: Option<bool>, at: CaptureInstant) -> bool {
        if down != Some(false) || at <= self.latest {
            return false;
        }
        self.receive(false, at)
    }
}

#[derive(Default)]
pub(super) struct NativePolicy {
    source: Option<u64>,
    down: bool,
    latest: Option<CaptureInstant>,
}

impl NativePolicy {
    pub fn receive(&mut self, event: Edge) -> Option<bool> {
        if event.source == 0
            || self.source.is_some_and(|source| source != event.source)
            || self.latest.is_some_and(|latest| event.at <= latest)
        {
            return None;
        }
        if self.source.is_none() && !event.down {
            self.latest = Some(event.at);
            return None;
        }
        self.source = Some(event.source);
        self.latest = Some(event.at);
        if self.down == event.down {
            return None;
        }
        self.down = event.down;
        Some(event.down)
    }

    pub fn remove(&mut self, source: u64, at: CaptureInstant) -> Option<bool> {
        if self.source != Some(source) {
            return None;
        }
        self.source = None;
        self.latest = Some(self.latest.map_or(at, |latest| latest.max(at)));
        std::mem::take(&mut self.down).then_some(false)
    }
}

#[derive(Clone, Copy)]
pub(super) enum MousePhase {
    Down,
    Drag,
    Up,
}

#[derive(Default)]
pub(super) struct RecordingFilter {
    initially_held: HashSet<u16>,
    consumed: HashSet<u16>,
    sampled_releases: HashSet<u16>,
}

impl RecordingFilter {
    pub fn new(held: impl IntoIterator<Item = u16>) -> Self {
        Self {
            initially_held: held.into_iter().collect(),
            ..Self::default()
        }
    }

    pub fn receive(&mut self, button: u16, phase: MousePhase, flags: u64, listening: bool) -> bool {
        if matches!(phase, MousePhase::Up) {
            self.initially_held.remove(&button);
            self.sampled_releases.remove(&button);
            return self.consumed.remove(&button);
        }
        if self.consumed.contains(&button) {
            return true;
        }
        let mut modifiers = modifiers_from_flags(flags);
        modifiers.function = false;
        if !listening
            || !matches!(phase, MousePhase::Down)
            || self.initially_held.contains(&button)
            || (button < 2 && modifiers.is_empty())
            || button >= 32
        {
            return false;
        }
        self.consumed.insert(button);
        true
    }

    /// A sample releases an initially held button and reports a missed owned up.
    /// Ownership remains until the actual (possibly queued) up or recorder teardown.
    pub fn sample(&mut self, held: impl IntoIterator<Item = u16>) -> Vec<u16> {
        let held: HashSet<_> = held.into_iter().collect();
        self.initially_held.retain(|button| held.contains(button));
        let released: Vec<_> = self
            .consumed
            .difference(&held)
            .filter(|button| !self.sampled_releases.contains(button))
            .copied()
            .collect();
        self.sampled_releases.extend(&released);
        released
    }

    pub fn interrupted(&mut self, held: impl IntoIterator<Item = u16>) {
        self.initially_held.extend(
            held.into_iter()
                .filter(|button| !self.consumed.contains(button)),
        );
    }

    pub fn owned_are_up(&self, held: impl IntoIterator<Item = u16>) -> bool {
        !held
            .into_iter()
            .any(|button| self.consumed.contains(&button))
    }

    pub fn pending(&self) -> bool {
        !self.consumed.is_empty()
    }
}

const STANDARD_WINDOW: std::time::Duration = std::time::Duration::from_millis(80);

/// Prefer a standard button observed near the same native press. Complete on
/// release and after the correlation window, so the recording press cannot dictate.
#[derive(Default)]
pub(super) struct MouseChoice {
    candidate: Option<Candidate>,
}

struct Candidate {
    binding: HotkeyBinding,
    source: Option<u64>,
    at: CaptureInstant,
    released: bool,
}

impl MouseChoice {
    pub fn down(&mut self, key: HotkeyKey, source: Option<u64>, flags: u64, at: CaptureInstant) {
        let mut modifiers = modifiers_from_flags(flags).without_side_constraints();
        modifiers.function = false;
        let binding = HotkeyBinding {
            modifiers,
            key: Some(key),
        };
        let Some(candidate) = &mut self.candidate else {
            self.candidate = Some(Candidate {
                binding,
                source,
                at,
                released: false,
            });
            return;
        };
        let close = at
            .checked_duration_since(candidate.at)
            .or_else(|| candidate.at.checked_duration_since(at))
            .is_some_and(|delta| delta <= STANDARD_WINDOW);
        if source.is_none()
            && candidate.source.is_some()
            && close
            && candidate.binding.key.as_ref().map(|key| key.code)
                == binding.key.as_ref().map(|key| key.code)
        {
            candidate.binding = binding;
            candidate.source = None;
        }
    }

    pub fn up(&mut self, code: u16, source: Option<u64>) {
        if let Some(candidate) = &mut self.candidate
            && candidate.source == source
            && candidate
                .binding
                .key
                .as_ref()
                .is_some_and(|key| key.code == code)
        {
            candidate.released = true;
        }
    }

    pub fn cancel_native(&mut self) {
        if self
            .candidate
            .as_ref()
            .is_some_and(|candidate| candidate.source.is_some())
        {
            self.candidate = None;
        }
    }

    pub fn removed(&mut self, source: u64) {
        if self
            .candidate
            .as_ref()
            .is_some_and(|candidate| candidate.source == Some(source))
        {
            self.candidate = None;
        }
    }

    pub fn take(&mut self, now: CaptureInstant) -> Option<HotkeyBinding> {
        let candidate = self.candidate.as_ref()?;
        if !candidate.released || now.checked_duration_since(candidate.at)? < STANDARD_WINDOW {
            return None;
        }
        self.candidate.take().map(|candidate| candidate.binding)
    }

    pub fn pending(&self) -> bool {
        self.candidate.is_some()
    }
}

#[cfg(test)]
mod tests;

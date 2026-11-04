//! A temporary global filtering tap, scoped to recording and its owned release.

use super::super::{
    AttachedTap, CFRunLoopGetCurrent, CGEventGetFlags, CGEventGetIntegerValueField,
    CGEventGetTimestamp, CGEventSourceButtonState, CGEventTapEnable, EVENT_TAP_DISABLED_BY_TIMEOUT,
    EVENT_TAP_DISABLED_BY_USER_INPUT, EventRef, MOUSE_EVENT_BUTTON_NUMBER, MOUSE_MASK,
};
use super::{
    Access, MonitorStatus, Timer, access,
    hid::{Event, Monitor},
    policy::{MouseChoice, MousePhase, RecordingFilter},
    publish,
};
use crate::{
    app_settings::{HotkeyBinding, HotkeyKey},
    audio::CaptureInstant,
};
use color_eyre::eyre::{Result, eyre};
use std::{cell::RefCell, ptr, rc::Rc, sync::atomic::AtomicPtr, time::Duration};

pub(crate) struct Recorder(Rc<RefCell<Core>>);

struct Core {
    tap: Option<AttachedTap>,
    timer: Option<Timer>,
    hid: Option<Box<Monitor>>,
    filter: RecordingFilter,
    choice: MouseChoice,
    active: bool,
    finish_at: Option<CaptureInstant>,
    started: CaptureInstant,
    neutral_samples: u8,
    on_binding: Box<dyn Fn(HotkeyBinding)>,
    on_down: Box<dyn Fn(u16, u64)>,
}

impl Recorder {
    pub fn start(
        on_binding: impl Fn(HotkeyBinding) + 'static,
        on_down: impl Fn(u16, u64) + 'static,
    ) -> Result<Self> {
        let core = Rc::new(RefCell::new(Core {
            tap: None,
            timer: None,
            hid: None,
            filter: RecordingFilter::new(held()),
            choice: MouseChoice::default(),
            active: true,
            finish_at: None,
            started: CaptureInstant::now(),
            neutral_samples: 0,
            on_binding: Box::new(on_binding),
            on_down: Box::new(on_down),
        }));
        // SAFETY: core remains alive until the tap is disabled and removed. This
        // session tap is upstream of Superhex's annotated-session listener.
        let tap = unsafe {
            AttachedTap::create(
                1,
                0,
                MOUSE_MASK,
                event,
                Rc::as_ptr(&core).cast_mut().cast(),
                CFRunLoopGetCurrent(),
                &AtomicPtr::new(ptr::null_mut()),
            )
        }
        .map_err(|message| eyre!(message))?;
        core.borrow_mut().tap = Some(tap);
        let tick = core.clone();
        let Some(timer) = Timer::start(0.02, move || Self::poll(&tick)) else {
            return Err(eyre!("could not create input run-loop sources"));
        };
        core.borrow_mut().timer = Some(timer);
        Self::poll(&core);
        Ok(Self(core))
    }

    pub fn pending_input(&self) -> impl Fn() -> bool + 'static {
        let core = Rc::downgrade(&self.0);
        move || {
            core.upgrade()
                .is_some_and(|core| core.borrow().choice.pending())
        }
    }

    fn poll(core: &Rc<RefCell<Core>>) {
        let mut state = core.borrow_mut();
        if !state.active {
            state.drain();
            return;
        }
        if access() != Access::Granted {
            state.hid = None;
            state.choice.cancel_native();
            publish(true, MonitorStatus::AccessRequired);
        } else if state.hid.is_none() {
            let weak = Rc::downgrade(core);
            state.hid = Monitor::start(None, move |event| {
                if let Some(core) = weak.upgrade() {
                    core.borrow_mut().hid_event(event);
                }
            });
        }
        let events = state.hid.as_mut().map_or_else(Vec::new, |hid| hid.sample());
        events.into_iter().for_each(|event| state.hid_event(event));
        if let Some(hid) = &state.hid {
            publish(true, hid.status);
        }
        let held = held();
        state.neutral_samples = if state.filter.owned_are_up(held.iter().copied()) {
            state.neutral_samples.saturating_add(1)
        } else {
            0
        };
        if state.filter.pending() && state.neutral_samples < 2 {
            return;
        }
        for button in state.filter.sample(held) {
            if let Some(code) = HotkeyKey::mouse_code(button) {
                state.choice.up(code, None);
            }
        }
        if let Some(binding) = state.choice.take(CaptureInstant::now()) {
            (state.on_binding)(binding);
        }
    }
}

impl Core {
    fn mouse(&mut self, kind: u32, event: EventRef) -> bool {
        if matches!(
            kind,
            EVENT_TAP_DISABLED_BY_TIMEOUT | EVENT_TAP_DISABLED_BY_USER_INPUT
        ) {
            self.filter.interrupted(held());
            self.started = CaptureInstant::now();
            if let Some(tap) = &self.tap {
                unsafe { CGEventTapEnable(tap.tap, true) };
            }
            return false;
        }
        if event.is_null() {
            return false;
        }
        let phase = match kind {
            1 | 3 | 25 => MousePhase::Down,
            2 | 4 | 26 => MousePhase::Up,
            6 | 7 | 27 => MousePhase::Drag,
            _ => return false,
        };
        // SAFETY: CoreGraphics supplied a live mouse event.
        let (button, flags, at) = unsafe {
            (
                CGEventGetIntegerValueField(event, MOUSE_EVENT_BUTTON_NUMBER),
                CGEventGetFlags(event),
                CaptureInstant::from_nanos(CGEventGetTimestamp(event)),
            )
        };
        let Ok(button) = u16::try_from(button) else {
            return false;
        };
        let consumed = self
            .filter
            .receive(button, phase, flags, self.active && at >= self.started);
        if !consumed || !self.active {
            return consumed;
        }
        match phase {
            MousePhase::Down => {
                if let Some(key) = HotkeyKey::mouse(button) {
                    self.choice.down(key, None, flags, at);
                }
                (self.on_down)(button, flags);
            }
            MousePhase::Up => {
                if let Some(code) = HotkeyKey::mouse_code(button) {
                    self.choice.up(code, None);
                }
            }
            MousePhase::Drag => {}
        }
        consumed
    }

    fn hid_event(&mut self, event: Event) {
        if !self.active {
            return;
        }
        match event {
            Event::Button(edge) => {
                let Some(key) = edge.binding.key() else {
                    return;
                };
                if !edge.down {
                    self.choice.up(key.code, Some(edge.source));
                    return;
                }
                let flags = unsafe { super::super::CGEventSourceFlagsState(0) };
                (self.on_down)(edge.binding.usage - 1, flags);
                self.choice.down(key, Some(edge.source), flags, edge.at);
            }
            Event::Removed(source) => self.choice.removed(source),
        }
    }

    fn drain(&mut self) {
        // Cancellation stops candidate recording immediately. Only a consumed
        // press's drag/up remain owned; two neutral samples recover a missed up.
        if !self.filter.pending() {
            self.stop();
            return;
        }
        self.neutral_samples = if self.filter.owned_are_up(held()) {
            self.neutral_samples.saturating_add(1)
        } else {
            0
        };
        let expired = self.finish_at.is_some_and(|at| {
            CaptureInstant::now()
                .checked_duration_since(at)
                .is_some_and(|duration| duration >= Duration::from_secs(30))
        });
        if self.neutral_samples >= 2 || expired {
            self.stop();
        }
    }

    fn stop(&mut self) {
        self.active = false;
        self.hid = None;
        self.tap = None;
        self.timer = None;
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let mut core = self.0.borrow_mut();
        core.active = false;
        core.finish_at = Some(CaptureInstant::now());
        core.hid = None;
        publish(true, MonitorStatus::Stopped);
        core.drain();
    }
}

fn held() -> Vec<u16> {
    // SAFETY: read-only physical state, also seeds the click that opened the recorder.
    (0..32)
        .filter(|button| unsafe {
            CGEventSourceButtonState(1, u32::from(*button))
                || CGEventSourceButtonState(0, u32::from(*button))
        })
        .collect()
}

unsafe extern "C" fn event(
    _: *mut std::ffi::c_void,
    kind: u32,
    event: EventRef,
    context: *mut std::ffi::c_void,
) -> EventRef {
    if context.is_null() {
        return event;
    }
    // SAFETY: the timer/Recorder keeps this stable Rc alive until the tap is removed.
    let core = unsafe { &*context.cast::<RefCell<Core>>() };
    if core.borrow_mut().mouse(kind, event) {
        ptr::null_mut()
    } else {
        event
    }
}

//! Native mouse edges enter the existing shortcut machine on its tap thread.

use super::super::{
    CGEventSourceFlagsState, COMBINED_SESSION_STATE, EventTapContext, InputEvent, send_input,
};
use super::{
    Access, MonitorStatus, Timer, access,
    hid::{Event, Monitor},
    policy::NativePolicy,
    publish,
};
use crate::{
    app_settings::{self, NativeMouseButton},
    audio::CaptureInstant,
};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::atomic::{AtomicU32, Ordering},
};

static HELD: AtomicU32 = AtomicU32::new(0);

pub(in crate::suppression) struct Runtime {
    _timer: Timer,
    core: Rc<RefCell<Core>>,
}

struct Core {
    context: *const EventTapContext,
    bindings: Vec<(NativeMouseButton, NativePolicy)>,
    monitor: Option<Box<Monitor>>,
}

impl Runtime {
    /// `context` and this run loop outlive this runtime (run_event_tap).
    pub fn start(context: *const EventTapContext) -> Option<Self> {
        let core = Rc::new(RefCell::new(Core {
            context,
            bindings: Vec::new(),
            monitor: None,
        }));
        let tick = core.clone();
        let timer = Timer::start(0.1, move || Self::refresh(&tick))?;
        Self::refresh(&core);
        Some(Self {
            _timer: timer,
            core,
        })
    }

    fn refresh(core: &Rc<RefCell<Core>>) {
        let mut seen = std::collections::HashSet::new();
        let bindings: Vec<_> = app_settings::native_mouse_bindings()
            .into_iter()
            .filter(|native| seen.insert(*native))
            .collect();
        let mut state = core.borrow_mut();
        let changed = state
            .bindings
            .iter()
            .map(|(binding, _)| *binding)
            .ne(bindings.iter().copied());
        if changed
            || app_settings::hotkey_capture_active()
            || (!bindings.is_empty() && access() != Access::Granted)
        {
            state.release_all();
            state.monitor = None;
            state.bindings = bindings
                .iter()
                .map(|binding| (*binding, NativePolicy::default()))
                .collect();
        }
        if bindings.is_empty() || app_settings::hotkey_capture_active() {
            publish(false, MonitorStatus::Stopped);
            return;
        }
        if access() != Access::Granted {
            publish(false, MonitorStatus::AccessRequired);
            return;
        }
        if state.monitor.is_none() {
            let weak = Rc::downgrade(core);
            state.monitor = Monitor::start(Some(bindings.clone()), move |event| {
                if let Some(core) = weak.upgrade() {
                    core.borrow_mut().receive(event);
                }
            });
        }
        let events = state
            .monitor
            .as_mut()
            .map_or_else(Vec::new, |monitor| monitor.sample());
        events.into_iter().for_each(|event| state.receive(event));
        publish(
            false,
            state
                .monitor
                .as_ref()
                .map_or(MonitorStatus::OpenFailed(-1), |monitor| monitor.status),
        );
    }
}

impl Core {
    fn receive(&mut self, event: Event) {
        if app_settings::hotkey_capture_active() {
            return;
        }
        for (binding, policy) in &mut self.bindings {
            let (edge, at) = match event {
                Event::Button(edge) if edge.binding == *binding => (policy.receive(edge), edge.at),
                Event::Removed(source) => {
                    let at = CaptureInstant::now();
                    (policy.remove(source, at), at)
                }
                _ => continue,
            };
            if let Some(down) = edge {
                emit(self.context, *binding, down, at);
            }
        }
    }

    fn release_all(&mut self) {
        for (binding, _) in &self.bindings {
            if binding.key().is_some_and(|key| native_held(key.code)) {
                emit(self.context, *binding, false, CaptureInstant::now());
            }
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.core.borrow_mut().release_all();
        publish(false, MonitorStatus::Stopped);
    }
}

fn emit(
    context: *const EventTapContext,
    binding: NativeMouseButton,
    down: bool,
    at: CaptureInstant,
) {
    let Some(code) = binding.key().map(|key| key.code) else {
        return;
    };
    let Some(button) = app_settings::HotkeyKey::mouse_button(code) else {
        return;
    };
    if down {
        HELD.fetch_or(1 << button, Ordering::Release);
    } else {
        HELD.fetch_and(!(1 << button), Ordering::Release);
    }
    // SAFETY: the event-tap thread owns the context and runtime together.
    let context = unsafe { &*context };
    let flags = unsafe { CGEventSourceFlagsState(COMBINED_SESSION_STATE) };
    let input = InputEvent::Key { code, down, flags };
    let delivered = send_input(context, input, at);
    let mut hotkeys = app_settings::runtime_hotkeys();
    if !context.takes_cancel.load(Ordering::Acquire) {
        hotkeys.cancel = None;
    }
    let suppressed = context
        .shortcut_suppression
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .process_all(
            input,
            hotkeys,
            delivered,
            context.takes_escape.load(Ordering::Acquire),
        );
    context.activity.observe(input, suppressed);
}

pub(in crate::suppression) fn native_held(code: u16) -> bool {
    app_settings::HotkeyKey::mouse_button(code)
        .is_some_and(|button| HELD.load(Ordering::Acquire) & (1 << button) != 0)
}

pub(in crate::suppression) fn native_button(button: u16) -> bool {
    app_settings::native_mouse_bindings()
        .iter()
        .any(|native| native.usage == button + 1)
}

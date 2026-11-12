//! macOS native mouse observation and explicit Input Monitoring actions.

use super::{
    CFAbsoluteTimeGetCurrent, CFRelease, CFRunLoopAddTimer, CFRunLoopGetCurrent,
    CFRunLoopTimerCreate, CFRunLoopTimerInvalidate, RunLoopTimerContext, kCFRunLoopCommonModes,
};
use std::{ffi::c_void, ptr, rc::Rc, sync::Mutex};

pub(super) mod ffi;
pub(crate) mod hid;
pub(crate) mod policy;
pub(crate) mod recorder;
mod runtime;
pub(super) use runtime::{Runtime, native_button, native_held};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Access {
    Granted,
    #[default]
    NotDetermined,
    Denied,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum MonitorStatus {
    #[default]
    Stopped,
    Listening {
        devices: usize,
    },
    AccessRequired,
    OpenFailed(i32),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Status {
    pub access: Access,
    pub recorder: MonitorStatus,
    pub runtime: MonitorStatus,
}

static STATUS: Mutex<Status> = Mutex::new(Status {
    access: Access::NotDetermined,
    recorder: MonitorStatus::Stopped,
    runtime: MonitorStatus::Stopped,
});

pub(crate) fn access() -> Access {
    // SAFETY: read-only TCC query; it never requests access or opens a device.
    match unsafe { ffi::IOHIDCheckAccess(1) } {
        0 => Access::Granted,
        1 => Access::Denied,
        _ => Access::NotDetermined,
    }
}

/// Call only in response to the person's explicit mouse access action.
pub(crate) fn request_access() -> Access {
    // SAFETY: this is the one intentional Input Monitoring prompt in Superhex.
    unsafe { ffi::IOHIDRequestAccess(1) };
    access()
}

pub(crate) fn open_input_monitoring() -> color_eyre::Result<()> {
    crate::desktop::opencode_catalog::open_url(
        "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent",
    )
}

pub(crate) fn status() -> Status {
    Status {
        access: access(),
        ..*STATUS.lock().unwrap_or_else(|error| error.into_inner())
    }
}

fn publish(recorder: bool, status: MonitorStatus) {
    let mut state = STATUS.lock().unwrap_or_else(|error| error.into_inner());
    let slot = if recorder {
        &mut state.recorder
    } else {
        &mut state.runtime
    };
    if *slot != status {
        tracing::info!(recorder, ?status, "native mouse monitor status");
        *slot = status;
    }
}

/// A CF timer retains its callback, including during invalidation from inside
/// that callback. This lets a cancelled recorder drain only its owned release.
pub(crate) struct Timer(*mut c_void);
struct TimerCall(Box<dyn Fn()>);

impl Timer {
    pub fn start(interval: f64, callback: impl Fn() + 'static) -> Option<Self> {
        let callback = Rc::new(TimerCall(Box::new(callback)));
        let mut context = RunLoopTimerContext {
            version: 0,
            info: Rc::as_ptr(&callback).cast_mut().cast(),
            retain: retain_timer as *const c_void,
            release: release_timer as *const c_void,
            copy_description: ptr::null(),
        };
        // SAFETY: CF copies/retains the context; callbacks run on this run loop only.
        let timer = unsafe {
            CFRunLoopTimerCreate(
                ptr::null(),
                CFAbsoluteTimeGetCurrent() + interval,
                interval,
                0,
                0,
                timer_fired,
                &raw mut context,
            )
        };
        if timer.is_null() {
            return None;
        }
        unsafe { CFRunLoopAddTimer(CFRunLoopGetCurrent(), timer, kCFRunLoopCommonModes) };
        Some(Self(timer))
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        // SAFETY: the timer is owned here and invalidated before releasing its context.
        unsafe {
            CFRunLoopTimerInvalidate(self.0);
            CFRelease(self.0.cast_const());
        }
    }
}

unsafe extern "C" fn retain_timer(info: *const c_void) -> *const c_void {
    unsafe { Rc::increment_strong_count(info.cast::<TimerCall>()) };
    info
}
unsafe extern "C" fn release_timer(info: *const c_void) {
    unsafe { Rc::decrement_strong_count(info.cast::<TimerCall>()) };
}
unsafe extern "C" fn timer_fired(_: *mut c_void, info: *mut c_void) {
    // SAFETY: clone before invoking, so invalidating the timer during the call is safe.
    let info = info.cast::<TimerCall>();
    unsafe { Rc::increment_strong_count(info) };
    let callback = unsafe { Rc::from_raw(info) };
    (callback.0)();
}

/// CG counterparts of a native binding are consumed, never delivered twice.
pub(super) fn consume_standard(kind: u32, event: super::EventRef) -> bool {
    if crate::app_settings::hotkey_capture_active() || !matches!(kind, 1..=4 | 6 | 7 | 25..=27) {
        return false;
    }
    // SAFETY: the caller supplies a non-null CG mouse event.
    let button =
        unsafe { super::CGEventGetIntegerValueField(event, super::MOUSE_EVENT_BUTTON_NUMBER) };
    u16::try_from(button).ok().is_some_and(native_button)
}

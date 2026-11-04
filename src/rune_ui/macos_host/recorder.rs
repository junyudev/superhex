//! Window keyboard events and global mouse recording share one capture lifetime.

use super::*;
use crate::suppression::mouse::recorder::Recorder;
use block2::RcBlock;
use objc2::{rc::Retained, runtime::AnyObject};
use objc2_app_kit::{NSEvent, NSEventMask, NSEventType};
use std::{
    cell::RefCell,
    ptr::{self, NonNull},
    rc::Rc,
};

pub(super) struct ChordWatch {
    pub(super) results: Receiver<Chord>,
    monitor: Retained<AnyObject>,
    _mouse: Recorder,
}

impl ChordWatch {
    pub(super) fn start(wake: Arc<dyn Fn() + Send + Sync>) -> Result<Self> {
        crate::app_settings::set_hotkey_capture_active(true);
        let result = Self::install(wake);
        if result.is_err() {
            crate::app_settings::set_hotkey_capture_active(false);
        }
        result
    }

    fn install(wake: Arc<dyn Fn() + Send + Sync>) -> Result<Self> {
        let (sender, results) = mpsc::channel();
        let capture = Rc::new(RefCell::new(ChordCapture::with_sides(
            crate::labs::sided_modifier_chords(),
        )));
        let mouse_sender = sender.clone();
        let mouse_wake = wake.clone();
        let mouse_capture = capture.clone();
        let mouse = Recorder::start(
            move |binding| {
                if mouse_sender.send(Chord::Captured(Ok(binding))).is_ok() {
                    mouse_wake();
                }
            },
            move |button, flags| {
                mouse_capture
                    .borrow_mut()
                    .mouse_down(button as isize, flags);
            },
        )?;
        let pending_mouse = mouse.pending_input();
        let block = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
            // SAFETY: AppKit supplies a live event for this callback.
            let native = unsafe { event.as_ref() };
            let flags = native.modifierFlags().bits() as u64;
            let chord = match native.r#type() {
                NSEventType::KeyDown if native.keyCode() == ESCAPE_KEY_CODE => capture
                    .borrow_mut()
                    .key_down(ESCAPE_KEY_CODE, native.isARepeat(), None, flags),
                _ if pending_mouse() => return event.as_ptr(),
                NSEventType::KeyDown => {
                    let characters = native
                        .charactersIgnoringModifiers()
                        .map(|characters| characters.to_string());
                    capture.borrow_mut().key_down(
                        native.keyCode(),
                        native.isARepeat(),
                        characters.as_deref(),
                        flags,
                    )
                }
                NSEventType::FlagsChanged => capture.borrow_mut().flags_changed(flags),
                _ => return event.as_ptr(),
            };
            if let Some(chord) = chord
                && sender.send(chord).is_ok()
            {
                wake();
            }
            ptr::null_mut()
        });
        // SAFETY: AppKit retains the block; Drop removes it on this main thread.
        let monitor = unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(
                NSEventMask::KeyDown | NSEventMask::FlagsChanged,
                &block,
            )
        }
        .ok_or_else(|| eyre!("could not watch the keyboard to record a shortcut"))?;
        Ok(Self {
            results,
            monitor,
            _mouse: mouse,
        })
    }
}

/// What the panes say about direct mouse access.
pub(super) fn mouse_access_view() -> super::super::platform::MouseAccessView {
    use super::super::platform::{MouseAccessNeeded, MouseAccessView};
    use crate::suppression::mouse::{Access, MonitorStatus, status};
    let status = status();
    MouseAccessView {
        needed: match status.access {
            Access::Granted => None,
            Access::NotDetermined => Some(MouseAccessNeeded::Ask),
            Access::Denied => Some(MouseAccessNeeded::Settings),
        },
        shortcut_stopped: status.runtime == MonitorStatus::AccessRequired,
    }
}

impl Drop for ChordWatch {
    fn drop(&mut self) {
        // SAFETY: this is the monitor installed by this owner on this main thread.
        unsafe { NSEvent::removeMonitor(&self.monitor) };
        crate::app_settings::set_hotkey_capture_active(false);
    }
}

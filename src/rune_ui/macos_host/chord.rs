//! Pure keyboard chord recording, shared by the local keyboard monitor and global mouse recorder.

use super::*;

/// Turns key and modifier events into a chord: a key with its held
/// modifiers, or the most modifier keys held at once when they are all
/// released without a key, with the sides `recorded_sides` keeps (or,
/// with Labs › Sided modifier chords on, the side each was pressed on).
pub(super) struct ChordCapture {
    peak: HotkeyModifiers,
    /// A key went down with these modifiers; their release records nothing.
    keyed: bool,
    /// A modifier-only chord keeps every modifier's side, such as
    /// Right ⌘ + Right ⌥, instead of widening each to either side.
    sided: bool,
    /// Names a key from the active layout when AppKit's characters are empty,
    /// as they are for a dead key (`^`, `´`, `` ` ``).
    layout_label: fn(u16) -> Option<String>,
}

impl Default for ChordCapture {
    fn default() -> Self {
        Self {
            peak: HotkeyModifiers::default(),
            keyed: false,
            sided: false,
            layout_label: |_| None,
        }
    }
}

impl ChordCapture {
    #[cfg(test)]
    pub(super) fn with_layout_label(layout_label: fn(u16) -> Option<String>) -> Self {
        Self {
            layout_label,
            ..Self::default()
        }
    }

    pub(super) fn with_sides(sided: bool) -> Self {
        Self {
            sided,
            layout_label: crate::keyboard::key_label,
            ..Self::default()
        }
    }

    pub(super) fn mouse_down(&mut self, button: isize, flags: u64) -> Option<Chord> {
        let key = u16::try_from(button).ok().and_then(HotkeyKey::mouse)?;
        let mut modifiers = modifiers_from_flags(flags).without_side_constraints();
        modifiers.function = false;
        if button < 2 && modifiers.is_empty() {
            return None;
        }
        self.keyed = !modifiers.is_empty();
        self.peak = HotkeyModifiers::default();
        Some(Chord::Captured(Ok(HotkeyBinding {
            modifiers,
            key: Some(key),
        })))
    }

    pub(super) fn key_down(
        &mut self,
        code: u16,
        repeat: bool,
        characters: Option<&str>,
        flags: u64,
    ) -> Option<Chord> {
        if repeat {
            return None;
        }
        if code == ESCAPE_KEY_CODE {
            return Some(Chord::Cancelled);
        }
        let mut modifiers = modifiers_from_flags(flags).without_side_constraints();
        // Arrow and function keys report Fn; a chord never binds it.
        modifiers.function = false;
        // Only modifiers held with the key are spent by it.
        self.keyed = !modifiers.is_empty();
        self.peak = HotkeyModifiers::default();
        let key = match hotkey_key(code, characters, || (self.layout_label)(code)) {
            Ok(key) => key,
            Err(message) => return Some(Chord::Captured(Err(message))),
        };
        // Function and navigation keys type nothing, so one may be a shortcut
        // alone. The section key does type something, but the recorder takes it
        // alone too: `settings::validate` refuses it while Labs › Section key
        // alone is off, as Linux's recorder does.
        let stands_alone =
            is_function_key(&key.label) || is_navigation_key(code) || key.is_section();
        if modifiers.is_empty() && !stands_alone {
            return Some(Chord::Captured(Err(tr("Add a modifier"))));
        }
        Some(Chord::Captured(Ok(HotkeyBinding {
            modifiers,
            key: Some(key),
        })))
    }

    /// While modifiers are down, reports the ones their release would record.
    pub(super) fn flags_changed(&mut self, flags: u64) -> Option<Chord> {
        let held = modifiers_from_flags(flags);
        let current = if self.sided {
            held
        } else {
            held.recorded_sides()
        };
        if !current.is_empty() {
            if self.keyed {
                return None;
            }
            if current.count() >= self.peak.count() {
                self.peak = current;
            }
            return Some(Chord::Held(self.peak));
        }
        let keyed = std::mem::take(&mut self.keyed);
        let peak = std::mem::take(&mut self.peak);
        if keyed || peak.is_empty() {
            return None;
        }
        Some(Chord::Captured(Ok(HotkeyBinding {
            modifiers: peak,
            key: None,
        })))
    }
}

fn is_function_key(label: &str) -> bool {
    label
        .strip_prefix('F')
        .and_then(|number| number.parse::<u8>().ok())
        .is_some_and(|number| (1..=20).contains(&number))
}

/// Home, End, Page Up, Page Down, and Help (Insert on PC keyboards): keys
/// that type nothing, so, like a function key, one may be a shortcut alone,
/// as it could before (#1138).
fn is_navigation_key(code: u16) -> bool {
    matches!(code, 115 | 119 | 116 | 121 | 114)
}

/// The key a virtual key code records, and its label. AppKit gives no
/// characters for a dead key (`^`, `´`, `` ` ``), so `layout_label` names it
/// from the active layout instead.
pub(super) fn hotkey_key(
    code: u16,
    characters: Option<&str>,
    layout_label: impl FnOnce() -> Option<String>,
) -> Result<HotkeyKey, &'static str> {
    let special = match code {
        49 => Some("Space"),
        48 => Some("Tab"),
        36 => Some("Return"),
        51 => Some(tr("Delete")),
        126 => Some("Up"),
        125 => Some("Down"),
        123 => Some(tr("Left")),
        124 => Some(tr("Right")),
        116 => Some("Page Up"),
        121 => Some("Page Down"),
        115 => Some("Home"),
        119 => Some("End"),
        117 => Some("Forward Delete"),
        114 => Some("Help"),
        122 => Some("F1"),
        120 => Some("F2"),
        99 => Some("F3"),
        118 => Some("F4"),
        96 => Some("F5"),
        97 => Some("F6"),
        98 => Some("F7"),
        100 => Some("F8"),
        101 => Some("F9"),
        109 => Some("F10"),
        103 => Some("F11"),
        111 => Some("F12"),
        105 => Some("F13"),
        107 => Some("F14"),
        113 => Some("F15"),
        106 => Some("F16"),
        64 => Some("F17"),
        79 => Some("F18"),
        80 => Some("F19"),
        90 => Some("F20"),
        _ => None,
    };
    if let Some(label) = special {
        return Ok(HotkeyKey {
            native_mouse: None,
            code,
            label: label.into(),
        });
    }
    let character = single_key_character(characters)
        .or_else(|| layout_label().and_then(|label| single_key_character(Some(&label))));
    match character {
        Some(character) => Ok(HotkeyKey {
            native_mouse: None,
            code,
            label: character.to_uppercase().collect(),
        }),
        None => Err(tr("Unsupported key")),
    }
}

/// The one visible character `text` holds, if it is exactly one non-control
/// character.
fn single_key_character(text: Option<&str>) -> Option<char> {
    let mut chars = text.unwrap_or_default().chars();
    match (chars.next(), chars.next()) {
        (Some(character), None) if !character.is_control() => Some(character),
        _ => None,
    }
}

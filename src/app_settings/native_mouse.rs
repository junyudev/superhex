//! Backward-readable identity for a physical mouse's Button-page element.

use super::*;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct NativeMouseButton {
    pub vendor: u16,
    pub product: u16,
    pub usage: u16,
}

impl NativeMouseButton {
    /// The standard path and native path share the same logical button code.
    pub fn key(self) -> Option<HotkeyKey> {
        let mut key = HotkeyKey::mouse(self.usage.checked_sub(1)?)?;
        if self.usage < 3 {
            return None;
        }
        key.native_mouse = Some(self);
        Some(key)
    }

    pub fn parse(name: &str) -> Option<HotkeyKey> {
        let (button, device) = name.split_once('@')?;
        let (vendor, product) = device.split_once(':')?;
        Self {
            vendor: vendor.parse().ok()?,
            product: product.parse().ok()?,
            usage: HotkeyKey::mouse_number(button)?,
        }
        .key()
    }
}

impl HotkeyKey {
    /// Reject metadata on keyboard codes, primary clicks, and mismatched usages.
    pub(super) fn validate_native_mouse(&self) -> Result<(), String> {
        let Some(native) = self.native_mouse else {
            return Ok(());
        };
        let expected = native
            .key()
            .ok_or("native mouse usage must be 3 through 32")?;
        if self.code != expected.code || self.label != expected.label {
            return Err("native mouse identity must match its Mouse key".into());
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
static HAS_NATIVE_MOUSE: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "macos")]
static NATIVE_MOUSE: RwLock<Vec<(NativeMouseButton, RuntimeHotkey)>> = RwLock::new(Vec::new());

#[cfg(target_os = "macos")]
pub(super) fn apply(settings: &AppSettings) {
    let mut bindings = vec![
        Some(&settings.dictation_hotkey),
        settings.paste_last_hotkey.as_ref(),
        settings.raw_dictation_hotkey.as_ref(),
        settings.active_second_dictation_hotkey(),
        settings.active_cancel_hotkey(),
    ];
    #[cfg(hex_capture)]
    bindings.push(settings.capture_on().then_some(&settings.capture.hotkey));
    let bindings: Vec<_> = bindings
        .into_iter()
        .flatten()
        .filter_map(|binding| Some((binding.key.as_ref()?.native_mouse?, binding.runtime())))
        .collect();
    let present = !bindings.is_empty();
    *NATIVE_MOUSE
        .write()
        .unwrap_or_else(|error| error.into_inner()) = bindings;
    HAS_NATIVE_MOUSE.store(present, Ordering::Release);
}

/// Only active native bindings reach the OS monitor. The ordinary runtime chord
/// stays unchanged, so keyboard matching and its consumers need no HID knowledge.
#[cfg(target_os = "macos")]
pub fn native_mouse_bindings() -> Vec<NativeMouseButton> {
    if !HAS_NATIVE_MOUSE.load(Ordering::Acquire) {
        return Vec::new();
    }
    let hotkeys = runtime_hotkeys();
    let active: Vec<_> = hotkeys
        .all()
        .into_iter()
        .chain([hotkeys.cancel])
        .flatten()
        .collect();
    NATIVE_MOUSE
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .iter()
        .filter_map(|(native, binding)| active.contains(binding).then_some(*native))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_mouse_chords_round_trip_without_changing_older_readers_keys() {
        let native = NativeMouseButton {
            vendor: 6421,
            product: 44572,
            usage: 5,
        };
        let binding = HotkeyBinding {
            modifiers: HotkeyModifiers::option(),
            key: native.key(),
        };
        let value = serde_json::to_value(&binding).unwrap();
        assert_eq!(
            serde_json::from_value::<HotkeyBinding>(value.clone()).unwrap(),
            binding
        );
        assert_eq!(binding.keycaps(), ["⌥", "Mouse 5"]);
        assert_eq!(
            binding.runtime().key_code,
            binding.key.as_ref().map(|key| key.code)
        );
        #[derive(Deserialize)]
        struct OlderKey {
            code: u16,
            label: String,
        }
        let old: OlderKey = serde_json::from_value(value["keys"][1].clone()).unwrap();
        assert_eq!(old.code, HotkeyKey::mouse(4).unwrap().code);
        assert_eq!(old.label, "Mouse 5");
        let legacy = serde_json::json!({ "modifiers": {"option": true}, "key": {"code": old.code, "label": old.label} });
        let old_binding: HotkeyBinding = serde_json::from_value(legacy).unwrap();
        assert_eq!(old_binding.key.as_ref().unwrap().native_mouse, None);
        assert!(binding.overlaps(&old_binding));
    }

    #[test]
    fn invalid_native_mouse_metadata_cannot_turn_a_keyboard_key_into_a_mouse() {
        let native = NativeMouseButton {
            vendor: 6421,
            product: 44572,
            usage: 5,
        };
        let binding = HotkeyBinding {
            modifiers: HotkeyModifiers::default(),
            key: native.key(),
        };
        for (field, value) in [
            ("code", serde_json::json!(49)),
            ("label", serde_json::json!("Space")),
        ] {
            let mut stored = serde_json::to_value(&binding).unwrap();
            stored["keys"][0][field] = value;
            assert!(serde_json::from_value::<HotkeyBinding>(stored).is_err());
        }
        for usage in [0, 1, 2, 33] {
            assert!(NativeMouseButton { usage, ..native }.key().is_none());
        }
    }
}

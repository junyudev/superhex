use super::*;
use MousePhase::*;

fn at(millis: u64) -> CaptureInstant {
    CaptureInstant::from_nanos(millis * 1_000_000)
}
fn button() -> NativeMouseButton {
    NativeMouseButton {
        vendor: 6421,
        product: 44572,
        usage: 5,
    }
}
fn edge(source: u64, down: bool, millis: u64) -> Edge {
    Edge {
        binding: button(),
        source,
        down,
        at: at(millis),
    }
}

#[test]
fn recorder_ignores_buttons_held_at_start_until_they_lift() {
    let mut filter = RecordingFilter::new([0, 2, 4]);
    assert!(!filter.receive(4, Down, 0, true));
    assert!(!filter.receive(4, Drag, 0, true));
    assert!(!filter.receive(4, Up, 0, true));
    assert!(filter.receive(4, Down, 0, true));
    assert!(filter.receive(4, Drag, 0, true));
    assert!(filter.receive(4, Up, 0, true));
    assert!(!filter.pending());
}

#[test]
fn recorder_passes_bare_primary_clicks_but_records_modified_ones() {
    let option = crate::app_settings::OPTION_KEY_MASK;
    for button in [0, 1] {
        let mut filter = RecordingFilter::default();
        for phase in [Down, Drag, Up] {
            assert!(!filter.receive(button, phase, 0, true));
        }
        assert!(filter.receive(button, Down, option, true));
        assert!(filter.receive(button, Drag, 0, true));
        assert!(filter.receive(button, Up, 0, true));
    }
    let mut filter = RecordingFilter::default();
    assert!(filter.receive(2, Down, 0, true));
    assert!(filter.receive(2, Up, 0, true));
}

#[test]
fn cancellation_keeps_only_the_consumed_release_and_drag() {
    let mut filter = RecordingFilter::default();
    assert!(filter.receive(4, Down, 0, true));
    assert!(!filter.receive(3, Down, 0, false));
    assert!(!filter.receive(0, Down, 0, false));
    assert!(filter.receive(4, Drag, 0, false));
    assert!(filter.receive(4, Up, 0, false));
    assert!(!filter.pending());
    assert!(!filter.receive(4, Down, 0, false));
}

#[test]
fn interruption_and_neutral_sampling_never_invent_a_press() {
    let mut filter = RecordingFilter::default();
    filter.interrupted([4]);
    assert!(!filter.receive(4, Down, 0, true));
    assert!(!filter.receive(4, Up, 0, true));
    assert!(filter.receive(4, Down, 0, true));
    assert!(!filter.owned_are_up([4]));
    assert!(filter.owned_are_up([]));
    // A sample does not drop ownership of a queued up.
    assert!(filter.receive(4, Up, 0, false));
}

#[test]
fn recorder_recovers_missed_ups_without_leaking_the_queued_release() {
    let mut filter = RecordingFilter::new([3]);
    assert!(filter.sample([]).is_empty());
    assert!(filter.receive(3, Down, 0, true));
    assert!(filter.sample([3]).is_empty());
    assert_eq!(filter.sample([]), [3]);
    assert!(filter.sample([]).is_empty());
    assert!(filter.receive(3, Up, 0, false));
    assert!(!filter.pending());
}

#[test]
fn losing_hid_access_discards_only_the_native_recording_candidate() {
    for native in [true, false] {
        let mut choice = MouseChoice::default();
        let key = if native {
            button().key().unwrap()
        } else {
            HotkeyKey::mouse(4).unwrap()
        };
        choice.down(key, native.then_some(10), 0, at(100));
        choice.cancel_native();
        assert_eq!(choice.pending(), !native);
    }
}

#[test]
fn native_interfaces_are_pinned_until_removal_even_between_presses() {
    let mut unpinned = NativePolicy::default();
    assert_eq!(unpinned.receive(edge(10, false, 30)), None);
    assert_eq!(unpinned.receive(edge(11, true, 20)), None);
    assert_eq!(unpinned.receive(edge(11, true, 31)), Some(true));
    let mut policy = NativePolicy::default();
    assert_eq!(policy.receive(edge(10, true, 10)), Some(true));
    assert_eq!(policy.receive(edge(11, true, 11)), None);
    assert_eq!(policy.receive(edge(11, false, 12)), None);
    assert_eq!(policy.receive(edge(10, false, 13)), Some(false));
    assert_eq!(policy.receive(edge(11, true, 14)), None);
    assert_eq!(policy.receive(edge(10, true, 15)), Some(true));
    assert_eq!(policy.receive(edge(10, false, 14)), None);
    assert_eq!(policy.remove(11, at(16)), None);
    assert_eq!(policy.remove(10, at(16)), Some(false));
    assert_eq!(
        policy.receive(edge(11, true, 15)),
        None,
        "a queued duplicate from before removal cannot re-press"
    );
    assert_eq!(policy.receive(edge(11, false, 16)), None);
    assert_eq!(policy.receive(edge(11, true, 17)), Some(true));
}

#[test]
fn native_mouse_edges_keep_hold_toggle_and_double_tap_semantics() {
    use crate::suppression::{
        DictationHotkey,
        HotkeyAction::{Finish, Start},
        InputEvent,
    };
    let binding = HotkeyBinding {
        modifiers: Default::default(),
        key: button().key(),
    }
    .runtime();
    let cases = [
        (
            false,
            false,
            vec![(10, true, Some(Start)), (1010, false, Some(Finish))],
        ),
        (
            true,
            false,
            vec![
                (10, true, Some(Start)),
                (60, false, None),
                (1010, true, Some(Finish)),
            ],
        ),
        (
            false,
            true,
            vec![
                (10, true, Some(Start)),
                (60, false, Some(Finish)),
                (110, true, Some(Start)),
                (160, false, None),
                (1010, true, Some(Finish)),
            ],
        ),
    ];
    for (toggle, double_tap, events) in cases {
        let mut hotkey = DictationHotkey::new(at(1), double_tap, binding);
        hotkey.set_toggle(toggle);
        let mut policy = NativePolicy::default();
        for (millis, down, expected) in events {
            let input_edge = edge(10, down, millis);
            assert_eq!(policy.receive(input_edge), Some(down));
            assert_eq!(
                hotkey.process(
                    InputEvent::Key {
                        code: binding.key_code.unwrap(),
                        down,
                        flags: 0
                    },
                    input_edge.at
                ),
                expected
            );
            assert_eq!(
                policy.receive(input_edge),
                None,
                "duplicate reports must not enter the machine"
            );
            assert_eq!(policy.receive(edge(11, down, millis + 1)), None);
        }
    }
}

#[test]
fn unknown_and_held_initial_hid_state_require_a_release() {
    for initial in [None, Some(true)] {
        let mut state = ButtonState::new(initial, at(1));
        assert!(!state.receive(true, at(2)));
        assert!(state.receive(false, at(3)));
        assert!(state.receive(true, at(4)));
        assert!(!state.receive(true, at(5)));
    }
}

#[test]
fn sampled_release_recovers_a_missed_up_and_fences_older_queued_input() {
    let mut state = ButtonState::new(Some(false), at(1));
    assert!(state.receive(true, at(10)));
    assert!(!state.sample(Some(true), at(20)));
    assert!(!state.sample(None, at(20)));
    assert!(!state.sample(Some(false), at(9)));
    assert!(state.sample(Some(false), at(30)));
    assert!(!state.receive(true, at(25)));
    assert!(!state.receive(false, at(30)));
    assert!(state.receive(true, at(31)));
    assert!(!state.receive(false, at(29)));
    assert!(state.receive(false, at(32)));
}

#[test]
fn sampling_unknown_state_arms_it_without_pressing_it() {
    let mut state = ButtonState::new(None, at(1));
    assert!(!state.sample(Some(true), at(2)));
    assert!(state.sample(Some(false), at(3)));
    assert!(state.receive(true, at(4)));
}

#[test]
fn recording_prefers_standard_input_in_either_callback_order() {
    for native_first in [false, true] {
        let mut choice = MouseChoice::default();
        let standard = HotkeyKey::mouse(4).unwrap();
        let native = button().key().unwrap();
        if native_first {
            choice.down(native.clone(), Some(10), 0, at(100));
        }
        choice.down(standard.clone(), None, 0, at(110));
        if !native_first {
            choice.down(native, Some(10), 0, at(100));
        }
        choice.up(standard.code, Some(10));
        assert!(choice.take(at(300)).is_none());
        choice.up(standard.code, None);
        assert_eq!(choice.take(at(300)).unwrap().key, Some(standard));
    }
}

#[test]
fn native_only_recording_waits_for_release_and_the_correlation_window() {
    let mut choice = MouseChoice::default();
    let key = button().key().unwrap();
    choice.down(key.clone(), Some(10), 0, at(100));
    assert!(choice.take(at(300)).is_none());
    choice.up(key.code, Some(11));
    assert!(choice.take(at(300)).is_none());
    choice.up(key.code, Some(10));
    assert!(choice.take(at(150)).is_none());
    assert_eq!(choice.take(at(181)).unwrap().key, Some(key));
}

#[test]
fn a_removed_recording_candidate_and_an_unrelated_standard_press_do_not_save_it() {
    let mut choice = MouseChoice::default();
    let key = button().key().unwrap();
    choice.down(key.clone(), Some(10), 0, at(100));
    choice.down(HotkeyKey::mouse(3).unwrap(), None, 0, at(110));
    choice.up(key.code, None);
    assert!(choice.take(at(300)).is_none());
    choice.removed(10);
    assert!(!choice.pending());
    choice.down(key.clone(), Some(11), 0, at(400));
    choice.down(HotkeyKey::mouse(4).unwrap(), None, 0, at(500));
    choice.up(key.code, Some(11));
    assert_eq!(choice.take(at(600)).unwrap().key, Some(key));
}

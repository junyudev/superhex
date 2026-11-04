//! The capsule for a dictation MouseVoice couldn't finish: what went wrong,
//! that the recording is still held, and the ways out. Like the meeting
//! capsules it is a non-activating overlay, so its buttons take clicks while
//! the app being dictated into keeps focus, and a retry pastes there. Its
//! audio lives in memory only (`dictation::mouse_voice`), and the capsule
//! leaves with the last held recording.

use rune::{Align, Anim, Cx, El, OverlayHandle, OverlayPlacement, OverlaySpec, ScreenEdge, row};
use rune_kit::{Button, ButtonVariant, ControlSize, IconButton, IconButtonVariant};

use super::controls::{Paint, PushSize, status_dot};
use super::live_text::Spot;
use super::meeting_capsules::{panel_radius, pill_body};
use super::mouse_voice::reason_copy;
use crate::desktop::i18n::{tr, tr_fill};
use crate::dictation::backend::{FailedAction, FailedDictation};

/// Room for the title beside two buttons and the ✕.
const NARROW: f32 = 520.0;
/// A third button.
const WIDE: f32 = 660.0;
const HEIGHT: f32 = 64.0;
/// The capsule's own margin inside the overlay.
const PAD: f32 = 4.0;

#[derive(Default)]
pub(crate) struct Capsule {
    failed: Vec<FailedDictation>,
    /// A local model is ready to take over.
    local: bool,
    open_settings: Option<fn()>,
}

/// What the capsule offers for a failed dictation, most useful first.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Offer {
    Act(FailedAction),
    OpenSettings,
}

fn offers(failed: &FailedDictation, local: bool) -> Vec<Offer> {
    let first = if failed.reason.retryable() {
        Offer::Act(FailedAction::Retry)
    } else {
        Offer::OpenSettings
    };
    let mut offers = vec![first];
    if local {
        offers.push(Offer::Act(FailedAction::RetryLocal));
    }
    if !failed.heard.trim().is_empty() {
        offers.push(Offer::Act(FailedAction::PasteHeard));
    }
    offers
}

fn width(failed: &FailedDictation, local: bool) -> f32 {
    if offers(failed, local).len() > 2 {
        WIDE
    } else {
        NARROW
    }
}

/// That the audio is safe, and for how long.
fn kept_line(failed: &[FailedDictation]) -> String {
    match failed {
        [_] => tr("Recording kept for 10 minutes").to_owned(),
        many => tr_fill("{} recordings kept for 10 minutes", &many.len().to_string()),
    }
}

fn offer_button(p: Paint, offer: Offer, id: u64, primary: bool, open: Option<fn()>) -> El {
    let (key, label) = match offer {
        Offer::Act(FailedAction::Retry) => ("retry", tr("Try again")),
        Offer::Act(FailedAction::RetryLocal) => ("retry-local", tr("Use local model")),
        Offer::Act(FailedAction::PasteHeard) => ("paste-heard", tr("Paste heard text")),
        Offer::Act(FailedAction::Dismiss) => ("dismiss", tr("Close")),
        Offer::OpenSettings => ("open-settings", tr("Open Settings")),
    };
    Button::new(label)
        .variant(if primary {
            ButtonVariant::Contrast
        } else {
            ButtonVariant::Neutral
        })
        .on_press(move |_: &mut Capsule| match offer {
            Offer::Act(action) => act(id, action),
            Offer::OpenSettings => {
                if let Some(open) = open {
                    open();
                }
            }
        })
        .build_push(p.k)
        .key(key)
}

#[cfg(target_os = "macos")]
fn act(id: u64, action: FailedAction) {
    crate::dictation::mouse_voice::act_on_failed(id, action);
}

#[cfg(not(target_os = "macos"))]
fn act(_: u64, _: FailedAction) {}

fn view(capsule: &Capsule, cx: &mut Cx<Capsule>) -> El {
    let p = Paint::new(cx.system_dark(), cx.env().scale);
    let root = row()
        .key("capsule")
        .w_full()
        .h_full()
        .px(PAD)
        .py(PAD)
        .hover_motion_within(Anim::INSTANT, Anim::INSTANT);
    let Some(newest) = capsule.failed.last() else {
        return root;
    };
    let id = newest.id;
    let actions =
        row()
            .key("actions")
            .gap(8.0)
            .center_y()
            .children(offers(newest, capsule.local).into_iter().enumerate().map(
                |(index, offer)| offer_button(p, offer, id, index == 0, capsule.open_settings),
            ))
            .child(
                IconButton::new(&rune_theme_opencode::icons::CLOSE)
                    .variant(IconButtonVariant::Ghost)
                    .control_size(ControlSize::Large)
                    .label(tr("Close"))
                    .on_press(move |_: &mut Capsule| act(id, FailedAction::Dismiss))
                    .build(p.k)
                    .key("dismiss"),
            );
    root.child(
        pill_body(
            p,
            status_dot(p.danger()),
            reason_copy(newest.reason),
            &kept_line(&capsule.failed),
            panel_radius(p),
        )
        .w_full()
        .child(actions),
    )
}

/// Where the capsule goes: where the words were, just past the HUD.
fn placement(spot: Spot, width: f32) -> OverlayPlacement {
    match spot {
        Spot::Global { x, y, below } => OverlayPlacement::Rect {
            x: x - width / 2.0,
            y: if below { y - PAD } else { y - HEIGHT + PAD },
            width,
            height: HEIGHT,
        },
        Spot::Edge(edge, margin) => OverlayPlacement::Edge {
            edge,
            margin: margin - PAD,
            align: Align::Center,
            inset: 0.0,
        },
    }
}

fn show(overlay: &OverlayHandle<Capsule>, width: f32, place: super::live_text::Place) {
    overlay.resize((width, HEIGHT));
    overlay.set_placement(Some(placement(place(), width)));
    overlay.show();
}

/// Adds the capsule to the app, shown while a failed dictation is held.
#[cfg(target_os = "macos")]
pub(crate) fn install<S: 'static>(
    app: &mut rune::App<S>,
    place: super::live_text::Place,
    open_settings: fn(),
) {
    let overlay = app.overlay(
        OverlaySpec {
            name: "Hex dictation failed",
            size: (NARROW, HEIGHT),
            edge: ScreenEdge::Top,
            margin: 0.0,
            align: Align::Center,
            inset: 0.0,
            interactive: true,
            view,
        },
        Capsule {
            open_settings: Some(open_settings),
            ..Capsule::default()
        },
    );
    crate::dictation::mouse_voice::watch_failed(move |failed| {
        let failed = failed.to_vec();
        let (_, selection) = crate::app_settings::transcription_selection();
        let local = crate::transcription::models::validate(&selection).is_ok_and(|model| {
            crate::transcription::models::is_installed(model, &selection.language)
        });
        let handle = overlay.clone();
        overlay.update(move |capsule, tx| {
            let newest = failed.last().map(|newest| width(newest, local));
            capsule.failed = failed;
            capsule.local = local;
            tx.instant();
            match newest {
                Some(width) => show(&handle, width, place),
                None => handle.hide(),
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use rune::Runtime;
    use rune::text::TextSystem;

    use super::*;
    use crate::dictation::backend::FailureReason;

    fn failed(reason: FailureReason, heard: &str) -> FailedDictation {
        FailedDictation {
            id: 1,
            audio_ms: 12_400,
            reason,
            heard: heard.into(),
        }
    }

    fn runtime(list: Vec<FailedDictation>, local: bool, dark: bool) -> (Runtime<Capsule>, f32) {
        let width = width(list.last().unwrap(), local);
        let capsule = Capsule {
            failed: list,
            local,
            open_settings: None,
        };
        let mut rt = crate::rune_ui::offscreen::runtime(
            capsule,
            view,
            (width, HEIGHT),
            rune::Env {
                system_dark: dark,
                scale: 2.0,
            },
            TextSystem::new(rune_theme_opencode::FONTS, rune_theme_opencode::FAMILIES),
        );
        rt.settle_all();
        rt.frame(0.0);
        (rt, width)
    }

    fn rect(rt: &Runtime<Capsule>, key: &str) -> Option<[f32; 4]> {
        rt.elements()
            .into_iter()
            .find(|e| !e.exiting && e.key.as_deref() == Some(key))
            .map(|e| e.rect)
    }

    #[test]
    fn a_refused_configuration_is_never_offered_a_retry() {
        for reason in [FailureReason::Rejected, FailureReason::NotSetUp] {
            assert_eq!(
                offers(&failed(reason, ""), false),
                [Offer::OpenSettings],
                "{reason:?}"
            );
        }
        assert_eq!(
            offers(&failed(FailureReason::Connection, ""), false),
            [Offer::Act(FailedAction::Retry)]
        );
    }

    #[test]
    fn what_can_stand_in_for_the_service_joins_the_offers() {
        assert_eq!(
            offers(&failed(FailureReason::TimedOut, "so the plan"), true),
            [
                Offer::Act(FailedAction::Retry),
                Offer::Act(FailedAction::RetryLocal),
                Offer::Act(FailedAction::PasteHeard),
            ]
        );
        // Blank text is nothing to paste.
        assert_eq!(
            offers(&failed(FailureReason::TimedOut, "  "), false),
            [Offer::Act(FailedAction::Retry)]
        );
    }

    #[test]
    fn the_capsule_says_how_much_audio_is_safe() {
        let one = failed(FailureReason::Connection, "");
        assert_eq!(
            kept_line(std::slice::from_ref(&one)),
            "Recording kept for 10 minutes"
        );
        assert_eq!(
            kept_line(&[one.clone(), one]),
            "2 recordings kept for 10 minutes"
        );
    }

    #[test]
    fn every_button_fits_in_the_longest_language_free_layout() {
        for (reason, heard, local) in [
            (FailureReason::Connection, "", false),
            (FailureReason::Connection, "", true),
            (FailureReason::TimedOut, "so the plan", true),
            (FailureReason::Rejected, "", true),
            (FailureReason::NotSetUp, "", false),
        ] {
            let (rt, width) = runtime(vec![failed(reason, heard)], local, false);
            let [x, _, w, _] = rect(&rt, "actions").expect("the actions draw");
            assert!(
                x + w <= width - PAD,
                "{reason:?} {local}: {x} {w} of {width}"
            );
            let [title_x, _, title_w, _] = rect(&rt, "title").expect("the title draws");
            assert!(
                title_x + title_w <= x,
                "{reason:?} {local}: title {title_x} {title_w} before {x}"
            );
        }
    }

    /// Renders the capsule to `HEX_LIVE_TEXT_SNAPS/failed-*.png` in both
    /// appearances, to look at.
    #[test]
    #[ignore = "set HEX_LIVE_TEXT_SNAPS to a folder; needs a GPU"]
    fn snapshots() {
        let folder = std::path::PathBuf::from(std::env::var("HEX_LIVE_TEXT_SNAPS").unwrap());
        let gpu = rune::render::Gpu::try_new(rune::render::Gpu::instance(), None).unwrap();
        for (name, list, local) in [
            (
                "connection",
                vec![failed(FailureReason::Connection, "")],
                true,
            ),
            (
                "timed-out-heard",
                vec![
                    failed(FailureReason::TimedOut, "so the plan"),
                    failed(FailureReason::TimedOut, "so the plan"),
                ],
                true,
            ),
            ("rejected", vec![failed(FailureReason::Rejected, "")], false),
        ] {
            for dark in [false, true] {
                let (mut rt, width) = runtime(list.clone(), local, dark);
                let (w, h) = ((width * 2.0) as u32, (HEIGHT * 2.0) as u32);
                let mut renderer = crate::rune_ui::offscreen::renderer(&gpu.device);
                let target = crate::rune_ui::offscreen::target(&gpu.device, w, h);
                renderer.upload_images(&gpu.device, &gpu.queue, &mut rt.images);
                renderer.prepare(
                    &gpu.device,
                    &gpu.queue,
                    &mut rt.text.atlas,
                    &rt.instances,
                    (w, h),
                );
                let mut encoder = gpu.device.create_command_encoder(&Default::default());
                let backdrop = if dark { 0.08 } else { 0.62 };
                renderer.draw(
                    &mut encoder,
                    &target.view,
                    [backdrop, backdrop, backdrop, 1.0],
                );
                target.copy(&mut encoder);
                gpu.queue.submit([encoder.finish()]);
                let theme = if dark { "dark" } else { "light" };
                let path = folder.join(format!("failed-{name}-{theme}.png"));
                rune::render::write_png(&path, w, h, &target.read(&gpu.device)).unwrap();
            }
        }
    }
}

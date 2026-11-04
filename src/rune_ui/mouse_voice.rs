//! MouseVoice in the window: General's Speech rows, setting it up in place,
//! and its options sheet. Importing a configuration is the person's consent
//! that audio leaves this computer, so every way to one says where it goes and
//! nothing here imports by itself.

use std::sync::mpsc::{Receiver, TryRecvError, channel};

use rune::{El, col, el, row, text};

use super::Ui;
use super::controls::{
    ButtonKind, CardRow, DESCRIPTION, Paint, button, card, copy_row, described_row,
    icon_described_row, icon_row, modal_scrim_transition, modal_transition, segmented, toggle_row,
};
use super::general::value_button;
use super::icons::{CLOUD, WAVE};
use crate::app_settings::AppSettings;
use crate::desktop::file_dialog::{Dialog, Filter, Pending, Target};
use crate::desktop::i18n::{tr, tr_fill};
use crate::dictation::backend::{
    DictationBackend, FailureReason, FoundConfig, MouseVoicePreferences, Setup,
};
#[cfg(target_os = "macos")]
use crate::dictation::mouse_voice as host;

/// Only macOS has MouseVoice; the panes draw it from fixtures everywhere.
#[cfg(not(target_os = "macos"))]
mod host {
    use super::{FailureReason, FoundConfig, Setup};

    pub(super) fn setup() -> Setup {
        Setup::Missing
    }

    pub(super) fn found() -> Vec<FoundConfig> {
        Vec::new()
    }

    pub(super) fn import(_: &std::path::Path) -> Result<Setup, String> {
        Err(String::new())
    }

    pub(super) fn forget() -> Result<(), String> {
        Ok(())
    }

    pub(super) fn check_connection() -> Result<(), FailureReason> {
        Err(FailureReason::NotSetUp)
    }
}

const SHEET_WIDTH: f32 = 520.0;
const SHEET_PADDING: f32 = 18.0;

/// The connection check Options runs.
#[derive(Default)]
enum Check {
    #[default]
    Idle,
    Running(Receiver<Result<(), FailureReason>>),
    Passed,
    Failed(FailureReason),
}

#[derive(Default)]
pub(super) struct MouseVoicePane {
    pub(super) setup: Setup,
    /// Configurations the person's other apps saved, offered during setup.
    pub(super) found: Vec<FoundConfig>,
    found_read: bool,
    chooser: Option<Pending>,
    importing: Option<Receiver<Result<Setup, String>>>,
    pub(super) error: Option<String>,
    check: Check,
    /// Remove… was pressed once; the next press forgets the configuration.
    forget_armed: bool,
    pub(super) options_open: bool,
}

impl MouseVoicePane {
    /// A file is being chosen or imported.
    pub(super) fn busy(&self) -> bool {
        self.chooser.is_some() || self.importing.is_some()
    }
}

fn available(ui: &Ui) -> bool {
    ui.platform.caps.mouse_voice
}

/// The person chose MouseVoice, whether or not it can run yet.
pub(super) fn chosen(ui: &Ui) -> bool {
    available(ui) && ui.settings.dictation_backend == DictationBackend::MouseVoice
}

/// Dictation goes through MouseVoice now, so it needs no local model.
pub(super) fn in_effect(ui: &Ui) -> bool {
    chosen(ui) && matches!(ui.mouse_voice.setup, Setup::Ready { .. })
}

/// [`in_effect`] before the window has read anything, as the tour starts.
pub(super) fn saved_in_effect(settings: &AppSettings) -> bool {
    #[cfg(target_os = "macos")]
    return host::effective_backend(settings) == DictationBackend::MouseVoice;
    #[cfg(not(target_os = "macos"))]
    {
        let _ = settings;
        false
    }
}

/// Reads the saved configuration before the tour decides what this run needs.
pub(super) fn load(ui: &mut Ui) {
    if available(ui) && ui.live() {
        ui.mouse_voice.setup = host::setup();
    }
}

/// Follows the saved configuration, a file being chosen or imported, and a
/// connection check.
pub(super) fn poll(ui: &mut Ui) -> bool {
    if !available(ui) || !ui.live() {
        return false;
    }
    let mut changed = false;
    let setup = host::setup();
    if setup != ui.mouse_voice.setup {
        ui.mouse_voice.setup = setup;
        ui.mouse_voice.found_read = false;
        changed = true;
    }
    if ui.mouse_voice.setup == Setup::Missing && !ui.mouse_voice.found_read && asks_for_setup(ui) {
        ui.mouse_voice.found = host::found();
        ui.mouse_voice.found_read = true;
        changed = true;
    }
    changed |= poll_chooser(ui);
    changed |= poll_import(ui);
    changed |= poll_check(ui);
    changed
}

/// Other apps' files are read only when the person is looking at setup.
fn asks_for_setup(ui: &Ui) -> bool {
    ui.pane == super::Pane::General || ui.first_run.is_some()
}

fn poll_chooser(ui: &mut Ui) -> bool {
    let Some(choice) = ui.mouse_voice.chooser.as_ref().and_then(Pending::poll) else {
        return false;
    };
    ui.mouse_voice.chooser = None;
    match choice {
        Ok(Some(path)) => start_import(ui, path),
        Ok(None) => {}
        Err(error) => ui.mouse_voice.error = Some(error),
    }
    true
}

fn poll_import(ui: &mut Ui) -> bool {
    let result = match ui.mouse_voice.importing.as_ref().map(Receiver::try_recv) {
        Some(Ok(result)) => result,
        Some(Err(TryRecvError::Disconnected)) => Err(tr("The import stopped.").to_owned()),
        Some(Err(TryRecvError::Empty)) | None => return false,
    };
    ui.mouse_voice.importing = None;
    match result {
        Ok(setup) => {
            ui.mouse_voice.setup = setup;
            ui.mouse_voice.error = None;
            ui.mouse_voice.check = Check::Idle;
            // The import filled in the file's voice options on disk.
            let voice =
                AppSettings::read().map_or(ui.settings.mouse_voice, |saved| saved.mouse_voice);
            ui.update(|settings| {
                settings.dictation_backend = DictationBackend::MouseVoice;
                settings.mouse_voice = voice;
            });
        }
        Err(error) => {
            // The reason names fields, never their values.
            tracing::warn!(%error, "could not import a MouseVoice configuration");
            ui.mouse_voice.error =
                Some(tr("That file isn't a usable MouseVoice configuration").to_owned());
        }
    }
    true
}

fn poll_check(ui: &mut Ui) -> bool {
    let Check::Running(receiver) = &ui.mouse_voice.check else {
        return false;
    };
    let result = match receiver.try_recv() {
        Ok(result) => result,
        Err(TryRecvError::Disconnected) => Err(FailureReason::Connection),
        Err(TryRecvError::Empty) => return false,
    };
    ui.mouse_voice.check = match result {
        Ok(()) => Check::Passed,
        Err(reason) => Check::Failed(reason),
    };
    true
}

/// Opens the file dialog for a configuration.
pub(super) fn choose_file(ui: &mut Ui) {
    if !ui.live() || ui.mouse_voice.busy() {
        return;
    }
    ui.mouse_voice.error = None;
    let waker = ui.modes.waker.clone();
    ui.mouse_voice.chooser = Some(crate::desktop::file_dialog::open(
        Dialog {
            title: tr("Choose a MouseVoice configuration").into(),
            target: Target::File(vec![
                Filter {
                    label: tr("JSON files").into(),
                    extensions: &["json"],
                },
                Filter {
                    label: tr("All files").into(),
                    extensions: &["*"],
                },
            ]),
            directory: None,
        },
        move || {
            if let Some(waker) = &waker {
                waker();
            }
        },
    ));
}

/// Validates and saves a private copy of the file at `path`, off the window's
/// thread.
pub(super) fn start_import(ui: &mut Ui, path: std::path::PathBuf) {
    if !ui.live() || ui.mouse_voice.importing.is_some() {
        return;
    }
    let (sender, receiver) = channel();
    let waker = ui.modes.waker.clone();
    let started = std::thread::Builder::new()
        .name("hex-mouse-voice-import".into())
        .spawn(move || {
            let _ = sender.send(host::import(&path));
            if let Some(waker) = &waker {
                waker();
            }
        });
    match started {
        Ok(_) => ui.mouse_voice.importing = Some(receiver),
        Err(error) => ui.mouse_voice.error = Some(error.to_string()),
    }
}

fn start_check(ui: &mut Ui) {
    if !ui.live() || matches!(ui.mouse_voice.check, Check::Running(_)) {
        return;
    }
    let (sender, receiver) = channel();
    let waker = ui.modes.waker.clone();
    let started = std::thread::Builder::new()
        .name("hex-mouse-voice-check".into())
        .spawn(move || {
            let _ = sender.send(host::check_connection());
            if let Some(waker) = &waker {
                waker();
            }
        });
    ui.mouse_voice.check = match started {
        Ok(_) => Check::Running(receiver),
        Err(_) => Check::Failed(FailureReason::Connection),
    };
}

/// What a failure reads as, in the window and on the failure capsule.
pub(crate) fn reason_copy(reason: FailureReason) -> &'static str {
    match reason {
        FailureReason::Connection => tr("Couldn't reach MouseVoice"),
        FailureReason::TimedOut => tr("MouseVoice didn't answer in time"),
        FailureReason::Rejected => tr("MouseVoice refused this configuration"),
        FailureReason::Service => tr("MouseVoice couldn't finish this one"),
        FailureReason::NotSetUp => tr("Set up MouseVoice or a local model"),
    }
}

fn audio_goes_to(host: &str) -> String {
    tr_fill("Your voice goes to {} while you dictate", host)
}

/// General's choice of who turns speech into text.
pub(super) fn backend_row(ui: &Ui, p: Paint) -> El {
    let selected = usize::from(ui.settings.dictation_backend != DictationBackend::MouseVoice);
    icon_row(
        p,
        "speech",
        &WAVE,
        tr("Speech"),
        None,
        segmented(
            p,
            "speech-backend",
            &[tr("MouseVoice"), tr("Local model")],
            selected,
            |ui: &mut Ui, index| {
                ui.update(|settings| {
                    settings.dictation_backend = if index == 0 {
                        DictationBackend::MouseVoice
                    } else {
                        DictationBackend::Local
                    };
                });
            },
        ),
    )
}

/// Under the choice of MouseVoice: where it stands, and setting it up right
/// here while it can't run.
pub(super) fn status_row(ui: &Ui, p: Paint) -> CardRow {
    match &ui.mouse_voice.setup {
        Setup::Ready { host } => ready_row(p, host).into(),
        Setup::Missing => setup_row(ui, p).into(),
        Setup::Broken { reason } => broken_row(ui, p, reason).into(),
    }
}

fn ready_row(p: Paint, host: &str) -> El {
    let description = text(audio_goes_to(host))
        .key("description")
        .font(DESCRIPTION)
        .color(p.muted())
        .wrap();
    copy_row(
        p,
        "mouse-voice",
        Some(&CLOUD),
        tr("MouseVoice"),
        Some(description),
        value_button(
            p,
            "mouse-voice-options",
            tr("Options"),
            &rune_theme_opencode::icons::CHEVRON_RIGHT,
            open,
        ),
        false,
    )
}

/// What setup does, where the voice goes, and what happens meanwhile.
fn setup_copy(ui: &Ui) -> String {
    if let Some(error) = &ui.mouse_voice.error {
        return error.clone();
    }
    let lead = match ui.mouse_voice.found.first() {
        Some(found) => tr_fill(
            "Found MouseVoice's configuration on this Mac. Your voice goes to {}.",
            &found.host,
        ),
        None => tr("Choose a context.json. Your voice goes to the service it names.").to_owned(),
    };
    let meanwhile = if ui.picker.selected_installed(&ui.settings) {
        tr_fill(
            "Until then, Hex uses {}.",
            crate::transcription::models::definition(ui.settings.transcription.model).name,
        )
    } else {
        tr("Until then, Hex can't listen.").to_owned()
    };
    format!("{lead} {meanwhile}")
}

fn setup_row(ui: &Ui, p: Paint) -> El {
    let found = ui.mouse_voice.found.first().cloned();
    let color = if ui.mouse_voice.error.is_some() {
        p.danger()
    } else {
        p.muted()
    };
    let choose = button(
        p,
        "mouse-voice-choose",
        tr("Choose file…"),
        if found.is_some() {
            ButtonKind::Secondary
        } else {
            ButtonKind::Primary
        },
        |ui: &mut Ui, _| choose_file(ui),
    );
    let controls = row()
        .key("mouse-voice-controls")
        .gap(8.0)
        .center_y()
        .child(choose)
        .children(found.map(|found| {
            button(
                p,
                "mouse-voice-use",
                tr("Use it"),
                ButtonKind::Primary,
                move |ui: &mut Ui, _| start_import(ui, found.path.clone()),
            )
        }));
    icon_described_row(
        p,
        "mouse-voice",
        &CLOUD,
        tr("Set up MouseVoice"),
        Some((setup_copy(ui), color)),
        controls,
    )
}

fn broken_row(ui: &Ui, p: Paint, reason: &str) -> El {
    let reason = ui.mouse_voice.error.as_deref().unwrap_or(tr(reason));
    icon_described_row(
        p,
        "mouse-voice",
        &CLOUD,
        tr("MouseVoice can't run"),
        Some((reason.to_owned(), p.danger())),
        button(
            p,
            "mouse-voice-choose",
            tr("Choose file…"),
            ButtonKind::Primary,
            |ui: &mut Ui, _| choose_file(ui),
        ),
    )
}

/// Opens the options sheet.
pub(super) fn open(ui: &mut Ui) {
    super::settings::cancel_capture(ui);
    super::picker::close(ui);
    super::explainer::close(ui);
    super::sound_sets::close(ui);
    super::vocabulary::close(ui);
    ui.open = None;
    ui.mouse_voice.forget_armed = false;
    ui.mouse_voice.options_open = true;
}

pub(super) fn close(ui: &mut Ui) {
    ui.mouse_voice.options_open = false;
    ui.mouse_voice.forget_armed = false;
    if !matches!(ui.mouse_voice.check, Check::Running(_)) {
        ui.mouse_voice.check = Check::Idle;
    }
}

fn set(ui: &mut Ui, change: impl FnOnce(&mut MouseVoicePreferences)) {
    ui.update(|settings| change(&mut settings.mouse_voice));
}

fn option_rows(ui: &Ui, p: Paint) -> Vec<El> {
    let saved = ui.settings.mouse_voice;
    vec![
        toggle_row(
            p,
            "mouse-voice-polish",
            tr("Polish the final text"),
            tr("Tidies the words after you stop, which takes a moment longer"),
            saved.polish,
            |ui: &mut Ui| set(ui, |saved| saved.polish = !saved.polish),
        ),
        toggle_row(
            p,
            "mouse-voice-redundancy",
            tr("Remove repetition"),
            None,
            saved.reduce_redundancy,
            |ui: &mut Ui| {
                set(ui, |saved| {
                    saved.reduce_redundancy = !saved.reduce_redundancy
                })
            },
        ),
        toggle_row(
            p,
            "mouse-voice-paragraph",
            tr("Break long text into paragraphs"),
            None,
            saved.paragraph,
            |ui: &mut Ui| set(ui, |saved| saved.paragraph = !saved.paragraph),
        ),
        toggle_row(
            p,
            "mouse-voice-traditional",
            tr("Traditional Chinese"),
            None,
            saved.traditional,
            |ui: &mut Ui| set(ui, |saved| saved.traditional = !saved.traditional),
        ),
    ]
}

/// How the last check went, under the row's title.
fn check_copy(ui: &Ui, p: Paint) -> Option<(String, rune::Color)> {
    match &ui.mouse_voice.check {
        Check::Idle => None,
        Check::Running(_) => Some((tr("Connecting…").to_owned(), p.muted())),
        Check::Passed => Some((tr("Connected").to_owned(), p.success())),
        Check::Failed(reason) => Some((reason_copy(*reason).to_owned(), p.danger())),
    }
}

/// The connection and the configuration behind it.
fn service_rows(ui: &Ui, p: Paint, host: &str) -> Vec<El> {
    let armed = ui.mouse_voice.forget_armed;
    vec![
        described_row(
            p,
            "mouse-voice-connection",
            tr("Connection"),
            check_copy(ui, p),
            button(
                p,
                "mouse-voice-test",
                tr("Test connection"),
                ButtonKind::Secondary,
                |ui: &mut Ui, _| start_check(ui),
            ),
        ),
        described_row(
            p,
            "mouse-voice-configuration",
            tr("Configuration"),
            Some((host.to_owned(), p.muted())),
            row()
                .key("mouse-voice-configuration-controls")
                .gap(8.0)
                .center_y()
                .child(button(
                    p,
                    "mouse-voice-replace",
                    tr("Replace…"),
                    ButtonKind::Secondary,
                    |ui: &mut Ui, _| choose_file(ui),
                ))
                .child(button(
                    p,
                    "mouse-voice-remove",
                    if armed {
                        tr("Confirm remove")
                    } else {
                        tr("Remove…")
                    },
                    if armed {
                        ButtonKind::Danger
                    } else {
                        ButtonKind::Secondary
                    },
                    |ui: &mut Ui, _| forget(ui),
                )),
        ),
    ]
}

fn forget(ui: &mut Ui) {
    if !ui.mouse_voice.forget_armed {
        ui.mouse_voice.forget_armed = true;
        return;
    }
    ui.mouse_voice.forget_armed = false;
    match if ui.live() { host::forget() } else { Ok(()) } {
        Ok(()) => {
            ui.mouse_voice.setup = Setup::Missing;
            ui.mouse_voice.found_read = false;
            close(ui);
        }
        Err(error) => ui.mouse_voice.error = Some(error),
    }
}

/// The options sheet: what the service does with the words, and the
/// configuration behind it.
pub(super) fn modal(ui: &Ui, p: Paint) -> Option<[El; 2]> {
    if !ui.mouse_voice.options_open {
        return None;
    }
    let c = &p.k.c;
    let host = match &ui.mouse_voice.setup {
        Setup::Ready { host } => host.as_str(),
        _ => "",
    };
    let heading = col()
        .key("heading")
        .gap(4.0)
        .child(
            text(tr("MouseVoice"))
                .key("title")
                .font(p.k.ty.body_medium)
                .color(p.text()),
        )
        .child(
            text(audio_goes_to(host))
                .key("description")
                .font(DESCRIPTION)
                .color(p.muted())
                .wrap(),
        );
    let mut panel = col()
        .key("mouse-voice-sheet")
        .role("dialog")
        .label(tr("MouseVoice"))
        .w(SHEET_WIDTH)
        .px(SHEET_PADDING)
        .py(SHEET_PADDING)
        .gap(14.0)
        .radius(p.k.radius.xl)
        .fill(c.bg_base)
        .border(p.hair, c.border_weak)
        // Presses inside the panel stay inside.
        .on_press(|_: &mut Ui| {})
        .child(heading)
        .child(card(p, "mouse-voice-choices", option_rows(ui, p)))
        .child(card(p, "mouse-voice-service", service_rows(ui, p, host)))
        .children(ui.mouse_voice.error.as_deref().map(|error| {
            text(error.to_owned())
                .key("mouse-voice-error")
                .font(DESCRIPTION)
                .color(p.danger())
                .wrap()
        }))
        .child(
            row().key("footer").justify_end().child(
                button(
                    p,
                    "mouse-voice-done",
                    tr("Done"),
                    ButtonKind::Secondary,
                    |ui: &mut Ui, _| close(ui),
                )
                .default_button(),
            ),
        );
    for shadow in &p.k.elevation.overlay {
        panel = panel.box_shadow(*shadow);
    }
    let layer = |key: &str| {
        el().key(key)
            .role("none")
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
    };
    Some([
        layer("mouse-voice-scrim")
            .fill(c.scrim)
            .on_press(close)
            .transition(modal_scrim_transition(p)),
        layer("mouse-voice-layer")
            .center()
            .transition(modal_transition(p))
            .child(panel.modal(true, close)),
    ])
}

/// A fixture's configuration, so scenes never read this computer's.
pub(super) fn fixture(ui: &mut Ui, setup: Setup, found: bool) {
    ui.settings.dictation_backend = DictationBackend::MouseVoice;
    ui.mouse_voice.setup = setup;
    ui.mouse_voice.found = found
        .then(|| FoundConfig {
            path: "/fixture/context.json".into(),
            host: FIXTURE_HOST.into(),
        })
        .into_iter()
        .collect();
    ui.mouse_voice.found_read = true;
}

pub(super) const FIXTURE_HOST: &str = "speech.example.com";

/// A fixture with the options sheet open.
pub(super) fn options_fixture(ui: &mut Ui, passed: bool) {
    ui.mouse_voice.options_open = true;
    if passed {
        ui.mouse_voice.check = Check::Passed;
    }
}

#[cfg(test)]
mod tests {
    use super::super::scenes::mac_fixture;
    use super::*;

    fn ready() -> Setup {
        Setup::Ready {
            host: FIXTURE_HOST.into(),
        }
    }

    #[test]
    fn it_takes_the_choice_and_a_configuration_to_be_in_effect() {
        let mut ui = mac_fixture();
        fixture(&mut ui, ready(), false);
        assert!(in_effect(&ui));
        ui.settings.dictation_backend = DictationBackend::Local;
        assert!(!in_effect(&ui));
        fixture(&mut ui, Setup::Missing, false);
        assert!(chosen(&ui) && !in_effect(&ui));
    }

    #[test]
    fn removing_the_configuration_takes_a_second_press() {
        let mut ui = mac_fixture();
        fixture(&mut ui, ready(), false);
        open(&mut ui);
        forget(&mut ui);
        assert_eq!(ui.mouse_voice.setup, ready());
        assert!(ui.mouse_voice.options_open);
        forget(&mut ui);
        assert_eq!(ui.mouse_voice.setup, Setup::Missing);
        assert!(!ui.mouse_voice.options_open);
    }

    #[test]
    fn setup_says_where_the_voice_goes_and_what_runs_meanwhile() {
        let mut ui = mac_fixture();
        fixture(&mut ui, Setup::Missing, true);
        let copy = setup_copy(&ui);
        assert!(copy.contains(FIXTURE_HOST), "{copy}");
        assert!(copy.contains("Until then"), "{copy}");
        ui.mouse_voice.error = Some("not a configuration".into());
        assert_eq!(setup_copy(&ui), "not a configuration");
    }
}

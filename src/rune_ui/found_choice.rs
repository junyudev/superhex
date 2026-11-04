//! Onboarding's speech list, shown where VoiceBuddy can run or when the
//! hub-cache scan found a compatible model (`found_models`): the first run's
//! model step (`onboarding_kit::live`) shows one card of selectable rows.
//! VoiceBuddy comes first and selected where this OS has it, then Hex's
//! recommended model, then the models already on this computer. Picking a row
//! starts it at once: VoiceBuddy imports the configuration the row names (or
//! asks for one), ours downloads (or, when its exact file is already here, is
//! used in place), and a found model is used where it is. Continue starts the
//! selected row when none was picked. Without either the step is exactly as
//! before.

use super::Ui;
use crate::desktop::i18n::{tr, tr_fill};
use crate::desktop::onboarding::{Input, ItemStatus, SetupItem};
use crate::dictation::backend::{DictationBackend, Setup};
use crate::transcription::found_models::Found;
use crate::transcription::models::{self as transcription_models, TranscriptionSelection};

/// Whether the list leads with VoiceBuddy.
fn offers_voice_buddy(ui: &Ui) -> bool {
    ui.platform.caps.voice_buddy
}

/// Whether the tour's own download waits: until the person has picked, where
/// the list offers VoiceBuddy, since it needs no model at all.
pub(super) fn holds_download(ui: &Ui) -> bool {
    offers_voice_buddy(ui) && ui.found.picked.is_none()
}

/// Whether onboarding shows the list: there is a choice and the speech step
/// isn't ready yet, or the person picked from it (it stays, so a ready pick
/// moves on from the list and Back returns to it).
pub(super) fn offered(ui: &Ui, model: ItemStatus) -> bool {
    (offers_voice_buddy(ui) || !ui.found.models.is_empty())
        && (model != ItemStatus::Ready || ui.found.picked.is_some())
}

/// Whether the list waits for its first pick, so Continue starts the
/// selected row.
pub(super) fn pending(ui: &Ui, model: ItemStatus) -> bool {
    offered(ui, model)
        && matches!(model, ItemStatus::Todo | ItemStatus::Failed)
        && ui.found.picked.is_none()
        && !ui.found.adopting()
}

/// Hex's recommended model as the list first showed it: picking a found
/// model changes the saved selection, and the rows stay as they were.
fn recommended(ui: &Ui) -> TranscriptionSelection {
    ui.found
        .recommended
        .clone()
        .unwrap_or_else(|| ui.settings.transcription.clone())
}

/// One row of the list: VoiceBuddy, Hex's recommended model, or a found file.
enum Entry<'a> {
    VoiceBuddy,
    Recommended { on_disk: bool },
    Found(&'a Found),
}

fn entries(ui: &Ui) -> Vec<Entry<'_>> {
    let recommended = recommended(ui).model;
    let on_disk = ui
        .found
        .models
        .iter()
        .any(|found| found.catalog == Some(recommended));
    offers_voice_buddy(ui)
        .then_some(Entry::VoiceBuddy)
        .into_iter()
        .chain(std::iter::once(Entry::Recommended { on_disk }))
        .chain(
            ui.found
                .models
                .iter()
                .filter(|found| found.catalog != Some(recommended))
                .map(Entry::Found),
        )
        .collect()
}

/// One row of the list as the first run shows it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ChoiceRow {
    pub(super) name: String,
    pub(super) line: String,
    pub(super) recommended: bool,
    pub(super) selected: bool,
    /// The row sends speech to a service rather than running on this computer.
    pub(super) service: bool,
}

/// The list, one radio row per model.
pub(super) fn rows(ui: &Ui, model: ItemStatus) -> Vec<ChoiceRow> {
    let selected = ui.found.picked.unwrap_or(0);
    entries(ui)
        .into_iter()
        .enumerate()
        .map(|(index, entry)| row(ui, &entry, index == selected, model))
        .collect()
}

/// Where VoiceBuddy stands, and where the voice would go.
fn voice_buddy_line(ui: &Ui) -> String {
    if let Some(error) = &ui.voice_buddy.error {
        return error.clone();
    }
    if ui.voice_buddy.busy() {
        return tr("Checking…").to_owned();
    }
    match (&ui.voice_buddy.setup, ui.voice_buddy.found.first()) {
        (Setup::Ready { host }, _) => tr_fill("Your voice goes to {}", host),
        (Setup::Broken { reason }, _) => reason.clone(),
        (Setup::Missing, Some(found)) => tr_fill("Your voice goes to {}", &found.host),
        (Setup::Missing, None) => tr("Hex sets it up next").to_owned(),
    }
}

fn row(ui: &Ui, entry: &Entry<'_>, selected: bool, model: ItemStatus) -> ChoiceRow {
    if matches!(entry, Entry::VoiceBuddy) {
        return ChoiceRow {
            name: tr("VoiceBuddy").to_owned(),
            line: voice_buddy_line(ui),
            recommended: true,
            selected,
            service: true,
        };
    }
    let name = match entry {
        Entry::VoiceBuddy => "",
        Entry::Recommended { .. } => transcription_models::definition(recommended(ui).model).name,
        Entry::Found(found) => found.catalog.map_or(found.name.as_str(), |id| {
            transcription_models::definition(id).name
        }),
    };
    let started = selected && ui.found.picked.is_some();
    let line = match (entry, started, model) {
        (Entry::VoiceBuddy, ..) => String::new(),
        (_, true, ItemStatus::Failed) => tr("Couldn't prepare this model").to_owned(),
        (_, true, ItemStatus::Ready) if !ui.found.adopting() => tr("Ready").to_owned(),
        (Entry::Recommended { on_disk: false }, true, ItemStatus::Downloading(progress)) => {
            tr_fill(
                "Downloading… {}%",
                &((progress * 100.0).round() as u32).to_string(),
            )
        }
        (_, true, _) if ui.found.adopting() || model != ItemStatus::Ready => {
            tr("Checking…").to_owned()
        }
        (Entry::Recommended { on_disk: false }, ..) => tr_fill(
            "{} download",
            &transcription_models::definition(recommended(ui).model).size_label(),
        ),
        (Entry::Recommended { on_disk: true }, ..) => tr("On this computer").to_owned(),
        (Entry::Found(found), ..) if found.catalog.is_some() => tr("On this computer").to_owned(),
        (Entry::Found(_), ..) => tr("On this computer · not tested by Hex").to_owned(),
    };
    ChoiceRow {
        name: name.to_owned(),
        line,
        // One recommendation: VoiceBuddy's, where the list has it.
        recommended: matches!(entry, Entry::Recommended { .. }) && !offers_voice_buddy(ui),
        selected,
        service: false,
    }
}

/// Whether the pick is under way or ready, so the first run can move on:
/// ours downloading, or a model on this computer checked and in use.
pub(super) fn started(ui: &Ui, model: ItemStatus) -> bool {
    ui.found.picked.is_some()
        && !ui.found.adopting()
        && matches!(model, ItemStatus::Ready | ItemStatus::Downloading(_))
}

/// VoiceBuddy was picked and nothing more is coming: the file dialog was
/// cancelled, or the file was refused. The list asks again.
pub(super) fn voice_buddy_pick_stalled(ui: &Ui) -> bool {
    offers_voice_buddy(ui)
        && ui.found.picked == Some(0)
        && !ui.voice_buddy.busy()
        && !matches!(ui.voice_buddy.setup, Setup::Ready { .. })
}

/// Picks VoiceBuddy: what is saved, what another of the person's apps saved
/// (the row said where the voice goes), or a file they choose.
fn pick_voice_buddy(ui: &mut Ui) {
    ui.voice_buddy.error = None;
    if matches!(ui.voice_buddy.setup, Setup::Ready { .. }) {
        ui.update(|settings| settings.dictation_backend = DictationBackend::VoiceBuddy);
        return;
    }
    match ui.voice_buddy.found.first().map(|found| found.path.clone()) {
        Some(path) => super::voice_buddy::start_import(ui, path),
        None => super::voice_buddy::choose_file(ui),
    }
}

/// Starts the selected row when the person continues without picking one.
pub(super) fn start_selected(ui: &mut Ui) {
    pick(ui, ui.found.picked.unwrap_or(0));
}

/// Picks row `index` and starts it. A newer pick supersedes one still
/// preparing, as the picker's does.
pub(super) fn pick(ui: &mut Ui, index: usize) {
    let failed = ui
        .tour
        .as_ref()
        .is_some_and(|tour| tour.flow.view().model == ItemStatus::Failed);
    if (ui.found.picked == Some(index) && !failed) || ui.found.adopting() {
        return;
    }
    let found = match entries(ui).get(index) {
        None => return,
        Some(Entry::VoiceBuddy) => {
            ui.found.picked = Some(index);
            ui.found.error = None;
            pick_voice_buddy(ui);
            return;
        }
        Some(Entry::Recommended { .. }) => None,
        Some(Entry::Found(found)) => Some((*found).clone()),
    };
    ui.found.picked = Some(index);
    ui.found.error = None;
    if offers_voice_buddy(ui) {
        ui.update(|settings| settings.dictation_backend = DictationBackend::Local);
    }
    match found {
        None => prepare(ui, recommended(ui)),
        Some(found) => match found.catalog {
            Some(model) => {
                let language = transcription_models::definition(model)
                    .language_for(&recommended(ui).language)
                    .to_owned();
                prepare(ui, TranscriptionSelection { model, language });
            }
            // Hashed once and registered in place first, then prepared.
            None => super::found::adopt(ui, found, true),
        },
    }
}

/// Prepares `selection` for onboarding: through the tour the first time,
/// so its progress shows, and as a newer choice afterwards.
pub(super) fn prepare(ui: &mut Ui, selection: TranscriptionSelection) {
    let requested = ui.tour.as_ref().is_some_and(|tour| {
        !matches!(
            tour.flow.view().model,
            ItemStatus::Todo | ItemStatus::Failed
        )
    });
    if requested {
        ui.send(super::Request::ChooseModel {
            model: selection.model,
            language: selection.language,
        });
    } else {
        ui.found.choice = Some(selection);
        super::onboarding::input(ui, Input::Grant(SetupItem::Model));
    }
}

#[cfg(test)]
mod tests {
    use super::super::scenes::mac_fixture;
    use super::super::voice_buddy::{FIXTURE_HOST, fixture};
    use super::*;

    fn new_mac(found: bool) -> Ui {
        let mut ui = mac_fixture();
        fixture(&mut ui, Setup::Missing, found);
        ui
    }

    #[test]
    fn voice_buddy_leads_the_list_and_says_where_the_voice_goes() {
        let ui = new_mac(true);
        assert!(offered(&ui, ItemStatus::Todo));
        assert!(holds_download(&ui), "nothing downloads before a pick");
        let rows = rows(&ui, ItemStatus::Todo);
        let (service, local) = (&rows[0], &rows[1]);
        assert!(service.service && service.selected && service.recommended);
        assert!(service.line.contains(FIXTURE_HOST), "{}", service.line);
        assert!(!local.service && !local.selected && !local.recommended);
    }

    #[test]
    fn without_a_configuration_the_row_says_hex_sets_it_up() {
        let ui = new_mac(false);
        let rows = rows(&ui, ItemStatus::Todo);
        assert_eq!(rows[0].line, "Hex sets it up next");
    }

    #[test]
    fn a_ready_speech_step_asks_nothing() {
        let ui = new_mac(true);
        assert!(!offered(&ui, ItemStatus::Ready));
    }

    #[test]
    fn picking_a_local_model_chooses_local_and_lets_it_download() {
        let mut ui = new_mac(true);
        pick(&mut ui, 1);
        assert_eq!(ui.settings.dictation_backend, DictationBackend::Local);
        assert!(!holds_download(&ui));
        assert!(!voice_buddy_pick_stalled(&ui));
    }

    #[test]
    fn a_voice_buddy_pick_that_leads_nowhere_asks_again() {
        // A fixture runs no dialog or import, as a cancelled dialog leaves it.
        let mut ui = new_mac(false);
        pick(&mut ui, 0);
        assert_eq!(ui.found.picked, Some(0));
        assert!(voice_buddy_pick_stalled(&ui));
        // Set up after all: the pick stands.
        ui.voice_buddy.setup = Setup::Ready {
            host: FIXTURE_HOST.into(),
        };
        assert!(!voice_buddy_pick_stalled(&ui));
    }

    #[test]
    fn where_voice_buddy_is_absent_the_list_is_as_before() {
        let mut ui = new_mac(false);
        ui.platform.caps.voice_buddy = false;
        assert!(!offered(&ui, ItemStatus::Todo));
        assert!(!holds_download(&ui));
    }
}

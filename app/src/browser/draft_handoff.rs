//! Unsent composer drafts handed from one document of this tab to the next.
//!
//! Typed text lives in its composer element until it is sent, so a reload
//! would discard it. A document stores its drafts as it leaves, and the
//! document that replaces it, possibly a newer app generation, adopts them.

use std::cell::Cell;
use std::collections::BTreeMap;

use wasm_bindgen::JsCast as _;
use wire::TaskStamp;

use crate::composer_drafts::{DraftHandoff, DraftStore, HandedNote, LiveText};

/// Session storage is tab-scoped, so drafts reach only this tab's next document.
const STORAGE: &str = "spice:draft-handoff:v1";

thread_local! {
    /// Whether this document is leaving with its drafts already stored, so its
    /// unload needs no confirmation.
    static LEAVING: Cell<bool> = const { Cell::new(false) };
}

/// Whether this document stored its drafts for a reload it started itself.
pub(super) fn leaving() -> bool {
    LEAVING.get()
}

/// Store every unsent draft for the next document of this tab, returning
/// whether they are stored. With nothing unsent, no handoff remains.
pub(super) fn store(drafts: &DraftStore, notes: &BTreeMap<TaskStamp, String>) -> bool {
    let Some(storage) = session_storage() else {
        return false;
    };
    let handoff = collect(drafts, notes);
    if handoff == DraftHandoff::default() {
        return storage.remove_item(STORAGE).is_ok();
    }
    serde_json::to_string(&handoff)
        .is_ok_and(|document| storage.set_item(STORAGE, &document).is_ok())
}

/// Every unsent draft, with each mounted element's current text winning over
/// the text it was drawn from.
pub(super) fn collect(drafts: &DraftStore, notes: &BTreeMap<TaskStamp, String>) -> DraftHandoff {
    let (composers, live_notes) = live_text();
    let mut notes = notes.clone();
    notes.extend(live_notes);
    DraftHandoff {
        composers: drafts.hand_off(composers),
        notes: notes
            .into_iter()
            .filter(|(_, text)| !text.is_empty())
            .map(|(task, text)| HandedNote { task, text })
            .collect(),
    }
}

/// Reload this document once its drafts are stored for its successor.
pub(super) fn reload(window: &web_sys::Window) {
    LEAVING.set(true);
    let _ = window.location().reload();
}

/// Take the drafts the previous document of this tab handed off.
pub(super) fn take() -> Result<DraftHandoff, String> {
    let Some(storage) = session_storage() else {
        return Ok(DraftHandoff::default());
    };
    let Some(document) = storage.get_item(STORAGE).ok().flatten() else {
        return Ok(DraftHandoff::default());
    };
    let _ = storage.remove_item(STORAGE);
    serde_json::from_str(&document)
        .map_err(|error| format!("Drafts kept across the reload could not be restored: {error}"))
}

fn session_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.session_storage().ok().flatten()
}

/// The current text of every mounted composer and observation element.
fn live_text() -> (Vec<LiveText>, BTreeMap<TaskStamp, String>) {
    let mut composers = Vec::new();
    let mut notes = BTreeMap::new();
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return (composers, notes);
    };
    for textarea in textareas(&document, "textarea[data-composer-lane]") {
        if let Some(target) = textarea.get_attribute("data-composer-lane") {
            composers.push(LiveText {
                target,
                quote: textarea.get_attribute("data-quote-draft-id"),
                continuation: textarea.has_attribute("data-composer-continuation"),
                text: textarea.value(),
            });
        }
    }
    for textarea in textareas(&document, "textarea[data-note-task]") {
        if let Some(task) = textarea
            .get_attribute("data-note-task")
            .and_then(|task| task.parse().ok())
        {
            notes.insert(task, textarea.value());
        }
    }
    (composers, notes)
}

fn textareas(
    document: &web_sys::Document,
    selector: &str,
) -> impl Iterator<Item = web_sys::HtmlTextAreaElement> {
    let nodes = document.query_selector_all(selector).ok();
    let length = nodes.as_ref().map_or(0, web_sys::NodeList::length);
    (0..length).filter_map(move |index| {
        nodes
            .as_ref()?
            .get(index)?
            .dyn_into::<web_sys::HtmlTextAreaElement>()
            .ok()
    })
}

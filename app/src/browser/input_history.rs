//! Every lane's sent composer input, retained in local storage.
//!
//! Each use reads the stored document afresh and each send writes it back, so
//! tabs sharing the origin add to one history instead of overwriting it. A
//! document that is corrupt or beyond the current bounds is replaced by what
//! [`InputHistory::restore`] keeps of it.

use std::cell::RefCell;
use std::collections::BTreeMap;

use crate::composer_drafts::ComposerDraft;
use crate::composer_history::InputHistory;

const STORAGE: &str = "spice:input-history:v1";

thread_local! {
    /// Sends from each lane in this document, which end recall walks begun
    /// before them.
    static SENDS: RefCell<BTreeMap<String, u64>> = const { RefCell::new(BTreeMap::new()) };
}

fn storage() -> Option<web_sys::Storage> {
    web_sys::window().and_then(|window| window.local_storage().ok().flatten())
}

fn restore(storage: &web_sys::Storage) -> InputHistory {
    InputHistory::restore(&storage.get_item(STORAGE).ok().flatten().unwrap_or_default())
}

/// `lane`'s retained entries, oldest first.
pub(super) fn entries(lane: &str) -> Vec<String> {
    storage().map_or_else(Vec::new, |storage| restore(&storage).entries(lane).to_vec())
}

/// Sends from `lane` in this document so far.
pub(super) fn sends(lane: &str) -> u64 {
    SENDS.with(|sends| sends.borrow().get(lane).copied().unwrap_or_default())
}

/// Record a send from `lane` that carried `texts`.
pub(super) fn record(lane: &str, texts: &[String]) {
    SENDS.with(|sends| *sends.borrow_mut().entry(lane.to_owned()).or_default() += 1);
    let Some(storage) = storage() else {
        return;
    };
    let mut history = restore(&storage);
    history.record(lane, texts.iter().map(String::as_str));
    let _ = storage.set_item(STORAGE, &history.document());
}

/// Every text typed into one send, each quote reply and then the primary
/// body, which is what that send contributes to its lane's history.
pub(super) fn typed_texts(draft: &ComposerDraft) -> Vec<String> {
    draft
        .quotes
        .iter()
        .map(|quote| &quote.body)
        .chain([&draft.body, &draft.continuation])
        .filter(|text| !text.trim().is_empty())
        .cloned()
        .collect()
}

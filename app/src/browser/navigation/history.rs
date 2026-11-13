//! Ownership of same-tab history entries; never infer a return from history.length.

use wasm_bindgen::JsCast as _;

use crate::view::{AppRoute, EntryState, HistoryEffect, ImageTarget, Presentation, image_path};

const OWNER_KEY: &str = "spice.navigation.owner";
/// The `history.state` key holding the app's namespace beside other owners'.
const STATE_KEY: &str = "spiceNavigation";

pub(super) struct History {
    owner: String,
}

impl History {
    pub(super) fn new() -> Self {
        let storage = window().session_storage().ok().flatten();
        let owner = storage
            .as_ref()
            .and_then(|storage| storage.get_item(OWNER_KEY).ok().flatten())
            .unwrap_or_else(|| {
                let owner = super::super::sender::random_uuid()
                    .expect("navigation owner identity")
                    .to_string();
                if let Some(storage) = &storage {
                    let _ = storage.set_item(OWNER_KEY, &owner);
                }
                owner
            });
        Self { owner }
    }

    /// The entry this tab owns at the current location. Another owner, another
    /// namespace version or a path the location no longer matches is not ours
    /// and carries nothing to restore.
    pub(super) fn entry(&self) -> Option<EntryState> {
        let state = window().history().ok()?.state().ok()?;
        let value = js_sys::Reflect::get(&state, &STATE_KEY.into()).ok()?;
        if !value.is_object() {
            return None;
        }
        let json = js_sys::JSON::stringify(&value).ok()?.as_string()?;
        let entry = EntryState::decode(&json, &self.owner)?;
        (window().location().hash().ok()? == format!("#{}", entry.path)).then_some(entry)
    }

    /// Normalize an entry without discarding other history-state owners.
    pub(super) fn replace(&self, route: &AppRoute) {
        self.replace_location(route, None);
    }

    /// Normalize a page and its optional image overlay without discarding
    /// other history-state owners.
    pub(super) fn replace_location(&self, route: &AppRoute, image: Option<&ImageTarget>) {
        let mut entry = self
            .entry()
            .unwrap_or_else(|| EntryState::new(&self.owner, 0, None, String::new()));
        entry.path = image_path(route, image);
        if image.is_none() && !route.is_panel() {
            entry.opener = None;
        }
        self.write(&entry, false);
    }

    /// Own the current location as it stands, so what the reader does here is
    /// remembered on this entry.
    pub(super) fn own(&self, route: &AppRoute) {
        if self.entry().is_none() {
            self.replace(route);
        }
    }

    /// Rewrite the owned entry's presentation, leaving its chain place as is.
    /// Returns whether a write was attempted: an equal presentation spends
    /// none of the browser's history budget. Presentation is best-effort, so
    /// a browser that refuses the write keeps the app running.
    pub(super) fn present(&self, presentation: &Presentation) -> bool {
        let Some(mut entry) = self
            .entry()
            .filter(|entry| entry.presentation != *presentation)
        else {
            return false;
        };
        entry.presentation = presentation.clone();
        let _ = self.try_write(&entry, false);
        true
    }

    pub(super) fn navigate(
        &self,
        current: &AppRoute,
        next: &AppRoute,
        effect: HistoryEffect,
        departing: &Presentation,
    ) {
        match effect {
            HistoryEffect::Unchanged => {
                self.present(departing);
            }
            HistoryEffect::Replace => {
                self.present(departing);
                self.replace(next);
            }
            HistoryEffect::Push => {
                let mut base = self
                    .entry()
                    .filter(|entry| entry.path == current.path())
                    .unwrap_or_else(|| EntryState::new(&self.owner, 0, None, current.path()));
                // First own the departure as it was left. A cold panel has no
                // Chat opener.
                base.presentation = departing.clone();
                self.write(&base, false);
                let Some(position) = base.position.checked_add(1) else {
                    return;
                };
                let opener = if next.is_panel() {
                    if current.is_panel() {
                        base.opener
                    } else {
                        Some(base.position)
                    }
                } else {
                    None
                };
                self.write(
                    &EntryState::new(&self.owner, position, opener, next.path()),
                    true,
                );
            }
        }
    }

    /// Push one addressed image over `current`, preserving the exact entry
    /// that opening or Close returns to.
    pub(super) fn open_image(
        &self,
        current: &AppRoute,
        next: &AppRoute,
        image: &ImageTarget,
        departing: &Presentation,
    ) {
        let mut base = self
            .entry()
            .filter(|entry| entry.path == current.path())
            .unwrap_or_else(|| EntryState::new(&self.owner, 0, None, current.path()));
        base.presentation = departing.clone();
        self.write(&base, false);
        let Some(position) = base.position.checked_add(1) else {
            return;
        };
        self.write(
            &EntryState::new(
                &self.owner,
                position,
                Some(base.position),
                image_path(next, Some(image)),
            ),
            true,
        );
    }

    /// Move an open viewer to an adjacent addressed image without adding a
    /// browser-history step.
    pub(super) fn replace_image(&self, route: &AppRoute, image: &ImageTarget) {
        let mut entry = self
            .entry()
            .unwrap_or_else(|| EntryState::new(&self.owner, 0, None, String::new()));
        entry.path = image_path(route, Some(image));
        self.write(&entry, false);
    }

    /// True means a popstate will finish closing; otherwise remove only the
    /// image locator in place.
    pub(super) fn close_image(&self, route: &AppRoute, image: &ImageTarget) -> bool {
        if let Some(entry) = self
            .entry()
            .filter(|entry| entry.path == image_path(route, Some(image)))
            && let Some(opener) = entry.opener.filter(|opener| *opener < entry.position)
        {
            window()
                .history()
                .expect("navigation history")
                .go_with_delta(opener - entry.position)
                .expect("navigation to image opener");
            return true;
        }
        self.replace(route);
        false
    }

    /// True means a popstate will finish closing; otherwise replace in place.
    pub(super) fn close(&self, current: &AppRoute) -> bool {
        if let Some(entry) = self.entry().filter(|entry| entry.path == current.path())
            && let Some(opener) = entry.opener.filter(|opener| *opener < entry.position)
        {
            window()
                .history()
                .expect("navigation history")
                .go_with_delta(opener - entry.position)
                .expect("navigation to owned opener");
            return true;
        }
        self.replace(&AppRoute::chat());
        false
    }

    fn write(&self, entry: &EntryState, push: bool) {
        self.try_write(entry, push)
            .expect("navigation history write");
    }

    /// Browsers refuse history writes past a rate budget (WebKit: 100 per
    /// 30 s), so the refusal is returned rather than assumed impossible.
    fn try_write(&self, entry: &EntryState, push: bool) -> Result<(), wasm_bindgen::JsValue> {
        let window = window();
        let history = window.history()?;
        let state = if push {
            js_sys::Object::new()
        } else {
            merged_state(&history)
        };
        let own = js_sys::JSON::parse(&entry.encode())?;
        js_sys::Reflect::set(&state, &STATE_KEY.into(), &own)?;
        let url = format!("{}#{}", window.location().pathname()?, entry.path);
        if push {
            history.push_state_with_url(&state, "", Some(&url))
        } else {
            history.replace_state_with_url(&state, "", Some(&url))
        }
    }
}

/// A copy of the current entry's state with every foreign field kept.
fn merged_state(history: &web_sys::History) -> js_sys::Object {
    let copy = js_sys::Object::new();
    if let Ok(state) = history.state()
        && state.is_object()
        && !state.is_null()
    {
        js_sys::Object::assign(&copy, state.unchecked_ref());
    }
    copy
}

fn window() -> web_sys::Window {
    web_sys::window().expect("navigation requires a browser window")
}

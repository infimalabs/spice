//! Tab and Shift+Tab through member composers in on-screen reading order.

use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;
use yew::prelude::*;

use crate::composer_reading_order::{self, Section, Stop};

/// Candidates for sequential focus; [`focusable`] removes what the browser
/// would skip.
const CANDIDATES: &str = "a[href], area[href], button:not([disabled]), \
    input:not([disabled]):not([type=\"hidden\"]), select:not([disabled]), \
    textarea:not([disabled]), summary, iframe, [tabindex], \
    [contenteditable]:not([contenteditable=\"false\"])";
const SECTION: &str = ".team-agents > .team-agent";

fn focusable(element: &web_sys::HtmlElement) -> bool {
    let rect = element.get_bounding_client_rect();
    element.tab_index() >= 0
        && (rect.width() > 0.0 || rect.height() > 0.0)
        && element.closest("[inert]").ok().flatten().is_none()
        // A closed disclosure keeps only its summary in sequential focus.
        && element
            .closest("details:not([open]) > :not(summary)")
            .ok()
            .flatten()
            .is_none()
}

/// The element Tab should focus from `active` when reading order departs from
/// document order, or `None` to leave the step to the browser, which also
/// keeps an open modal dialog's own focus containment.
fn reading_target(
    document: &web_sys::Document,
    active: &web_sys::Element,
    backward: bool,
) -> Option<web_sys::HtmlElement> {
    document.query_selector(SECTION).ok().flatten()?;
    if document
        .query_selector("dialog:modal")
        .ok()
        .flatten()
        .is_some()
    {
        return None;
    }
    let nodes = document.query_selector_all(CANDIDATES).ok()?;
    let mut elements = Vec::with_capacity(nodes.length() as usize);
    let mut stops = Vec::with_capacity(elements.capacity());
    let mut strip: Option<(web_sys::Element, usize)> = None;
    let mut strips = 0;
    for index in 0..nodes.length() {
        let Some(element) = nodes
            .item(index)
            .and_then(|node| node.dyn_into::<web_sys::HtmlElement>().ok())
            .filter(focusable)
        else {
            continue;
        };
        let section = element
            .closest(SECTION)
            .ok()
            .flatten()
            .and_then(|section| Some((section.parent_element()?, section)))
            .map(|(parent, section)| {
                let ordinal = match &strip {
                    Some((current, ordinal)) if *current == parent => *ordinal,
                    _ => {
                        strips += 1;
                        strip = Some((parent, strips));
                        strips
                    }
                };
                Section {
                    strip: ordinal,
                    left: section.get_bounding_client_rect().left(),
                }
            });
        stops.push(Stop { section });
        elements.push(element);
    }
    let current = elements
        .iter()
        .position(|element| AsRef::<web_sys::Element>::as_ref(element) == active)?;
    let target = composer_reading_order::redirect(&stops, current, backward)?;
    elements.into_iter().nth(target)
}

#[hook]
pub(in crate::browser) fn use_composer_reading_focus() {
    use_effect_with((), move |()| {
        let registration = web_sys::window()
            .and_then(|window| window.document())
            .map(|document| {
                let listener = document.clone();
                let keydown = Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(
                    move |event: web_sys::KeyboardEvent| {
                        if event.key() != "Tab"
                            || event.default_prevented()
                            || event.is_composing()
                            || event.alt_key()
                            || event.ctrl_key()
                            || event.meta_key()
                        {
                            return;
                        }
                        let Some(target) = listener.active_element().and_then(|active| {
                            reading_target(&listener, &active, event.shift_key())
                        }) else {
                            return;
                        };
                        event.prevent_default();
                        let _ = target.focus();
                    },
                );
                let _ = document
                    .add_event_listener_with_callback("keydown", keydown.as_ref().unchecked_ref());
                (document, keydown)
            });
        move || {
            if let Some((document, keydown)) = registration {
                let _ = document.remove_event_listener_with_callback(
                    "keydown",
                    keydown.as_ref().unchecked_ref(),
                );
            }
        }
    });
}

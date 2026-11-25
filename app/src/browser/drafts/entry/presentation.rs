//! One left overlay whose expanded pixels fade beneath its persistent strip.
//! It stays non-modal in the top layer, so the right-side Spice overlay can
//! stand open beside it.

use super::*;
use crate::browser::overlay;

/// The sheet's shadow fades with it; that fade's end is the close boundary.
const FADE: &str = "box-shadow";
/// A press inside any of these leaves the sheet open: the sheet itself, a
/// dialog or popover raised over it, the Spice overlay beside it, and the
/// launcher that opens that overlay.
const COMPANIONS: &str = "dialog, [popover], .app-sidebar-layer, .app-spice-button";

pub(super) struct Presentation {
    pub expanded: bool,
    pub visible: bool,
    pub closing: bool,
    pub finish: Callback<()>,
    pub open: Callback<()>,
    pub close: Callback<()>,
}

#[hook]
pub(super) fn use_presentation(
    anchor: &NodeRef,
    dialog: &NodeRef,
    requested: UseStateHandle<bool>,
) -> Presentation {
    let expanded = use_state_eq(|| false);
    let presented = use_state_eq(|| false);
    {
        let expanded = expanded.clone();
        let presented = presented.clone();
        use_effect_with(*requested, move |requested| {
            if *requested {
                expanded.set(true);
            } else {
                presented.set(false);
            }
        });
    }
    let finish = {
        let expanded = expanded.clone();
        let presented = presented.clone();
        Callback::from(move |()| {
            expanded.set(false);
            presented.set(false);
        })
    };
    let closing = *expanded && !*requested;
    overlay::use_present(*requested && *expanded && !*presented, presented.clone());
    overlay::use_close_transition(
        closing,
        *expanded && !overlay::reduced_motion(),
        dialog.clone(),
        FADE,
        finish.clone(),
    );
    use_dialog(anchor, dialog, *expanded);
    let close = {
        let requested = requested.clone();
        Callback::from(move |()| requested.set(false))
    };
    let open = {
        let requested = requested.clone();
        Callback::from(move |()| {
            if !closing {
                requested.set(true);
            }
        })
    };
    let dismissible = *expanded && !closing;
    overlay::use_escape_owner(dismissible, close.clone());
    use_outside_press(dismissible, close.clone());
    Presentation {
        expanded: *expanded,
        visible: *requested && *presented,
        closing,
        finish,
        open,
        close,
    }
}

/// A press anywhere but the sheet and its companions dismisses it, as a veil
/// would without hiding the page beneath.
#[hook]
fn use_outside_press(active: bool, close: Callback<()>) {
    use_effect_with(active, move |&active| {
        let registration = active
            .then(|| {
                let document = web_sys::window()?.document()?;
                let pressed = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::Event)>::new(
                    move |event: web_sys::Event| {
                        let inside = event
                            .target()
                            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
                            .and_then(|target| target.closest(COMPANIONS).ok().flatten())
                            .is_some();
                        if !inside {
                            close.emit(());
                        }
                    },
                );
                document
                    .add_event_listener_with_callback(
                        "pointerdown",
                        pressed.as_ref().unchecked_ref(),
                    )
                    .ok()?;
                Some((document, pressed))
            })
            .flatten();
        move || {
            if let Some((document, pressed)) = registration {
                let _ = document.remove_event_listener_with_callback(
                    "pointerdown",
                    pressed.as_ref().unchecked_ref(),
                );
            }
        }
    });
}

/// Show a manual popover once, reflectively as the toasts do, so the build
/// needs no unstable `web-sys` flag.
fn show_popover(element: &web_sys::Element) {
    if element.matches(":popover-open").unwrap_or(true) {
        return;
    }
    if let Ok(show) = js_sys::Reflect::get(element, &wasm_bindgen::JsValue::from_str("showPopover"))
        && let Ok(show) = show.dyn_into::<js_sys::Function>()
    {
        let _ = show.call0(element);
    }
}

#[hook]
fn use_dialog(anchor: &NodeRef, dialog: &NodeRef, expanded: bool) {
    let anchor = anchor.clone();
    let dialog = dialog.clone();
    use_effect_with(expanded, move |&expanded| {
        let align = {
            let anchor = anchor.clone();
            let dialog = dialog.clone();
            move || {
                if let Some(anchor) = anchor.cast::<web_sys::HtmlElement>()
                    && let Some(dialog) = dialog.cast::<web_sys::HtmlDialogElement>()
                {
                    let rect = anchor.get_bounding_client_rect();
                    let Some(root) = anchor
                        .owner_document()
                        .and_then(|document| document.document_element())
                    else {
                        return;
                    };
                    // With a definite width and both insets, CSS keeps the
                    // inline-start edge: left in LTR, right in RTL. The sheet
                    // can widen inward without moving its laid-out editor.
                    for (name, value) in [
                        ("left", rect.left()),
                        ("right", f64::from(root.client_width()) - rect.right()),
                        ("top", rect.top()),
                        ("--app-compose-anchor-width", rect.width()),
                    ] {
                        let _ = dialog.style().set_property(name, &format!("{value}px"));
                    }
                }
            }
        };
        align();
        let observed = anchor.cast::<web_sys::Element>().and_then(|anchor| {
            let resized = wasm_bindgen::closure::Closure::<dyn FnMut()>::new(align);
            let observer = web_sys::ResizeObserver::new(resized.as_ref().unchecked_ref()).ok()?;
            observer.observe(&anchor);
            Some((observer, resized))
        });
        if let Some(dialog) = dialog.cast::<web_sys::HtmlDialogElement>() {
            show_popover(&dialog);
            if expanded {
                if let Ok(Some(target)) =
                    dialog.query_selector(".draft-context .draft-context-editor textarea")
                    && let Some(target) = target.dyn_ref::<web_sys::HtmlElement>()
                {
                    let _ = target.focus();
                }
            } else if let Ok(Some(focused)) = dialog.query_selector(":focus")
                && let Some(focused) = focused.dyn_ref::<web_sys::HtmlElement>()
            {
                // A closed sheet holds no focus: its strip opens it.
                let _ = focused.blur();
            }
        }
        move || {
            if let Some((observer, _)) = &observed {
                observer.disconnect();
            }
            drop(observed);
        }
    });
}

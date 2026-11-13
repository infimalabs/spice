//! One sheet whose expanded pixels fade beneath its persistent closed tab.

use super::*;
use crate::browser::overlay;

/// The sheet's shadow fades with its veil; that fade's end is the close boundary.
const FADE: &str = "box-shadow";

pub(super) struct Presentation {
    pub expanded: bool,
    pub visible: bool,
    pub closing: bool,
    pub finish: Callback<()>,
    pub open: Callback<()>,
    pub close: Callback<()>,
    pub closed: Callback<Event>,
    pub cancel: Callback<Event>,
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
    // Leaving modality hands focus back to the writing line it came from.
    // That return is not a request to open.
    let returning = use_mut_ref(|| false);
    use_dialog(anchor, dialog, *expanded, returning.clone());
    let close = {
        let requested = requested.clone();
        Callback::from(move |()| requested.set(false))
    };
    let open = {
        let requested = requested.clone();
        Callback::from(move |()| {
            if !closing && !*returning.borrow() {
                requested.set(true);
            }
        })
    };
    let closed = {
        let dialog = dialog.clone();
        let close = close.clone();
        Callback::from(move |_: Event| {
            if dialog
                .cast::<web_sys::HtmlDialogElement>()
                .is_some_and(|dialog| !dialog.open())
            {
                close.emit(());
            }
        })
    };
    let cancel = {
        let close = close.clone();
        Callback::from(move |event: Event| {
            event.prevent_default();
            close.emit(());
        })
    };
    Presentation {
        expanded: *expanded,
        visible: *requested && *presented,
        closing,
        finish,
        open,
        close,
        closed,
        cancel,
    }
}

#[hook]
fn use_dialog(
    anchor: &NodeRef,
    dialog: &NodeRef,
    expanded: bool,
    returning: Rc<std::cell::RefCell<bool>>,
) {
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
            if expanded {
                if !dialog.matches(":modal").unwrap_or(false) {
                    dialog.close();
                    let _ = dialog.show_modal();
                }
                if let Ok(Some(target)) =
                    dialog.query_selector(".draft-context > .draft-context-editor textarea")
                    && let Some(target) = target.dyn_ref::<web_sys::HtmlElement>()
                {
                    let _ = target.focus();
                }
            } else {
                // A closed sheet holds no focus: its writing line opens it.
                *returning.borrow_mut() = true;
                dialog.close();
                dialog.set_open(true);
                if let Ok(Some(focused)) = dialog.query_selector(":focus")
                    && let Some(focused) = focused.dyn_ref::<web_sys::HtmlElement>()
                {
                    let _ = focused.blur();
                }
                *returning.borrow_mut() = false;
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

//! The keyboard help sheet: one centered top-layer modal over the shared
//! frosted veil, opened by a bare `?` outside text entry, top-level Escape,
//! or the Help tab in the global composer's rail.
//!
//! [`SECTIONS`] is the inventory authority for operator shortcuts. Every row
//! names a key its handler acts on today; a change to any of those handlers
//! updates its row here in the same change, and a command the app no longer
//! handles leaves the sheet.
//!
//! Presentation reuses the sidebar's vocabulary: the sheet and veil fade
//! together after the modal opens, the fade's real end closes it, and focus
//! returns to the opener only while that opener can still take it. Escape,
//! Close, and the veil dismiss only this sheet. A key never summons it over
//! another native modal, so image viewing and annotation keep ownership of
//! the keys; the composer's own `?` segment is an explicit press and opens it
//! above the expanded composer too.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::*;
use yew::prelude::*;

use super::overlay;
use crate::view::{AppRoute, WorkRoute};

mod documents;

#[derive(Properties, PartialEq)]
pub(super) struct HelpProps {
    pub(super) connection: super::Connection,
    pub(super) on_action: Callback<crate::client_action::ClientAction, bool>,
    /// The surface whose [`HelpTrigger`] opens this sheet.
    pub(super) children: Html,
}

/// What a [`HelpTrigger`] needs from its [`Help`]: whether the sheet is
/// shown, and how to open it from the pressed control.
#[derive(Clone, PartialEq)]
struct HelpControl {
    shown: bool,
    open: Callback<Option<web_sys::Element>>,
}

const DIALOG_ID: &str = "app-help-dialog";
const HEADING_ID: &str = "app-help-heading";

/// One shortcut: alternative key combinations, each a sequence of keys held
/// together, and what they do.
struct Row {
    keys: &'static [&'static [&'static str]],
    action: &'static str,
}

struct Section {
    id: &'static str,
    title: &'static str,
    rows: &'static [Row],
}

const COMMAND: &str = "⌘/Ctrl";

const SECTIONS: &[Section] = &[
    Section {
        id: "app-help-global",
        title: "Anywhere",
        rows: &[
            Row {
                keys: &[&["?"]],
                action: "Open this help, outside text fields",
            },
            Row {
                keys: &[&["a"], &["t"], &["p"], &["d"]],
                action: "Open the global composer in Agent/Ask, Task, Plan, or Design mode",
            },
            Row {
                keys: &[&["Esc"]],
                action: "Close the topmost sheet, panel, or menu; open Help at the top level",
            },
            Row {
                keys: &[&["Tab"], &["Shift", "Tab"]],
                action: "Move between team composers in on-screen reading order",
            },
        ],
    },
    Section {
        id: "app-help-composers",
        title: "Composers",
        rows: &[
            Row {
                keys: &[&[COMMAND, "Enter"]],
                action: "Send a message, record an author-held Draft, or submit one for triage",
            },
            Row {
                keys: &[&[COMMAND, "Shift", "Enter"]],
                action: "Broadcast a message: from the top composer to every occupied agent, \
                         from a team composer to its team",
            },
            Row {
                keys: &[&["↑"], &["↓"]],
                action: "On the first or last line, recall a team composer's earlier sends",
            },
        ],
    },
    Section {
        id: "app-help-work",
        title: "Work",
        rows: &[
            Row {
                keys: &[&[COMMAND, "Enter"]],
                action: "Add a task note as an observation, or save a Draft",
            },
            Row {
                keys: &[&[COMMAND, "Shift", "Enter"]],
                action: "Finish a task note with its offered Close or Conclude",
            },
            Row {
                keys: &[&["+"], &["−"], &["0"]],
                action: "Zoom the focused task graph in, out, or to fit",
            },
        ],
    },
    Section {
        id: "app-help-images",
        title: "Full-size images",
        rows: &[
            Row {
                keys: &[&["←"], &["→"]],
                action: "Show the older or newer image",
            },
            Row {
                keys: &[&["A"]],
                action: "Annotate the image",
            },
            Row {
                keys: &[&["D"]],
                action: "Download the original",
            },
            Row {
                keys: &[&["Esc"]],
                action: "Close the viewer",
            },
        ],
    },
    Section {
        id: "app-help-annotation",
        title: "Annotation",
        rows: &[
            Row {
                keys: &[&["V"], &["R"], &["L"], &["A"], &["T"]],
                action: "Select, rectangle, line, arrow, or text",
            },
            Row {
                keys: &[&["1–6"]],
                action: "Red, yellow, green, blue, black, or white",
            },
            Row {
                keys: &[&["G"]],
                action: "Cycle the route: straight, elbow, or curve",
            },
            Row {
                keys: &[&["Alt/Option"], &[COMMAND], &["Shift"]],
                action: "While drawing a line or arrow: elbow, curve, or snap to 45° \
                         (mirrors a bent route)",
            },
            Row {
                keys: &[&["M"]],
                action: "Mirror the selected elbow or curve",
            },
            Row {
                keys: &[&["←↑↓→"], &["Shift", "←↑↓→"]],
                action: "Nudge the selected mark one pixel, or ten",
            },
            Row {
                keys: &[&["Enter"]],
                action: "Edit the selected label",
            },
            Row {
                keys: &[&["Delete"], &["Backspace"]],
                action: "Delete the selected mark",
            },
            Row {
                keys: &[&[COMMAND, "Z"]],
                action: "Undo",
            },
            Row {
                keys: &[&[COMMAND, "Shift", "Z"], &[COMMAND, "Y"]],
                action: "Redo",
            },
            Row {
                keys: &[&[COMMAND, "Enter"]],
                action: "Apply the annotations",
            },
            Row {
                keys: &[&["Esc"]],
                action: "Clear the selection, then cancel",
            },
        ],
    },
];

/// Ask for help with `?` outside text entry or an unclaimed Escape.
/// Window bubbling lets document-level dismissal owners consume Escape first;
/// native modals keep their own cancel behavior.
fn summons(event: &web_sys::KeyboardEvent, document: &web_sys::Document) -> bool {
    (event.key() == "Escape" || (event.key() == "?" && !overlay::typing(event)))
        && !event.ctrl_key()
        && !event.meta_key()
        && !event.alt_key()
        && !event.is_composing()
        && !event.repeat()
        && !event.default_prevented()
        && !overlay::native_modal_open(document)
}

/// The element a summons came from, when focus rests on a real control.
fn focused_opener() -> Option<web_sys::Element> {
    let document = web_sys::window()?.document()?;
    let active = document.active_element()?;
    let body = document.body().map(web_sys::Element::from);
    (Some(&active) != body.as_ref()).then_some(active)
}

/// Listen after document-level shortcut and dismissal handlers. Only an
/// unconsumed top-level Escape falls through to Help.
#[hook]
fn use_summons(open: Callback<Option<web_sys::Element>>) {
    let current = use_mut_ref(|| open.clone());
    *current.borrow_mut() = open;
    use_effect_with((), move |()| {
        let registration = web_sys::window().and_then(|window| {
            let observed = window.document()?;
            let key = Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(
                move |event: web_sys::KeyboardEvent| {
                    if summons(&event, &observed) {
                        event.prevent_default();
                        let open = current.borrow().clone();
                        open.emit(focused_opener());
                    }
                },
            );
            let _ =
                window.add_event_listener_with_callback("keydown", key.as_ref().unchecked_ref());
            Some((window, key))
        });
        move || {
            if let Some((window, key)) = registration {
                let _ = window
                    .remove_event_listener_with_callback("keydown", key.as_ref().unchecked_ref());
            }
        }
    });
}

/// Show the dialog modally while `shown`; closing it returns focus to the
/// opener, which a destination link clears before leaving.
#[hook]
fn use_modality(dialog: &NodeRef, shown: bool, opener: Rc<RefCell<Option<web_sys::Element>>>) {
    let dialog = dialog.clone();
    use_effect_with(shown, move |&shown| {
        if let Some(dialog) = dialog.cast::<web_sys::HtmlDialogElement>() {
            if shown && !dialog.open() {
                // The browser restores its own remembered focus on close;
                // only an eligible opener may take it back.
                if let Some(active) = dialog
                    .owner_document()
                    .and_then(|document| document.active_element())
                    .and_then(|active| active.dyn_into::<web_sys::HtmlElement>().ok())
                {
                    let _ = active.blur();
                }
                let _ = dialog.show_modal();
            } else if !shown && dialog.open() {
                dialog.close();
                if let Some(opener) = opener.borrow_mut().take() {
                    overlay::return_focus(&opener);
                }
            }
        }
    });
}

#[function_component(Help)]
pub(super) fn help(props: &HelpProps) -> Html {
    let dialog = use_node_ref();
    let surface = use_node_ref();
    let opener = use_mut_ref(|| None::<web_sys::Element>);
    let requested = use_state_eq(|| false);
    let shown = use_state_eq(|| false);
    let presented = use_state_eq(|| false);
    let documentation = use_state_eq(|| false);
    let closing = *shown && !*requested;
    {
        let presented = presented.clone();
        use_effect_with(*requested, move |requested| {
            if !*requested {
                presented.set(false);
            }
        });
    }
    let finish = {
        let shown = shown.clone();
        let presented = presented.clone();
        Callback::from(move |()| {
            shown.set(false);
            presented.set(false);
        })
    };
    overlay::use_present(*requested && *shown && !*presented, presented.clone());
    overlay::use_close_transition(
        closing,
        *shown && !overlay::reduced_motion(),
        surface.clone(),
        overlay::FADE,
        finish.clone(),
    );
    use_modality(&dialog, *shown, opener.clone());
    let open = {
        let requested = requested.clone();
        let shown = shown.clone();
        let opener = opener.clone();
        let documentation = documentation.clone();
        Callback::from(move |from: Option<web_sys::Element>| {
            if closing || *shown {
                return;
            }
            *opener.borrow_mut() = from;
            documentation.set(false);
            shown.set(true);
            requested.set(true);
        })
    };
    use_summons(open.clone());
    let close = {
        let requested = requested.clone();
        Callback::from(move |()| requested.set(false))
    };
    let cancel = close.reform(|event: Event| event.prevent_default());
    // Native close events are queued: an earlier close may arrive after the
    // next summons reopened the dialog. Only settle a dialog still closed.
    let closed = {
        let dialog = dialog.clone();
        Callback::from(move |_: Event| {
            if let Some(dialog) = dialog.cast::<web_sys::HtmlDialogElement>()
                && !dialog.open()
            {
                finish.emit(());
            }
        })
    };
    let veil = {
        let close = close.clone();
        Callback::from(move |event: MouseEvent| {
            if event
                .target()
                .and_then(|target| target.dyn_into::<web_sys::HtmlDialogElement>().ok())
                .is_some()
            {
                close.emit(());
            }
        })
    };
    // A destination leaves at once and gives focus to where it leads; the
    // app's link handling performs the navigation.
    let leave = {
        let dialog = dialog.clone();
        Callback::from(move |_: MouseEvent| {
            opener.borrow_mut().take();
            if let Some(dialog) = dialog.cast::<web_sys::HtmlDialogElement>() {
                dialog.close();
            }
        })
    };
    let control = HelpControl {
        shown: *shown,
        open,
    };
    html! {
        <ContextProvider<HelpControl> context={control}>
            {props.children.clone()}
            <dialog ref={dialog} id={DIALOG_ID} class="app-modal app-help" aria-modal="true"
                aria-labelledby={HEADING_ID} data-presented={(*requested && *presented).to_string()}
                inert={closing.then_some("true")}
                oncancel={cancel} onclose={closed} onclick={veil}>
                if *shown {
                    {render_sheet(&surface, &close, &leave, &documentation, props)}
                }
            </dialog>
        </ContextProvider<HelpControl>>
    }
}

/// The Help tab that opens the enclosing [`Help`] sheet; closing the sheet
/// returns focus to it. Its plain uppercase label is not a count badge.
#[function_component(HelpTrigger)]
pub(in crate::browser) fn help_trigger() -> Html {
    let trigger = use_node_ref();
    let Some(control) = use_context::<HelpControl>() else {
        return Html::default();
    };
    let shown = control.shown;
    let pressed = {
        let trigger = trigger.clone();
        Callback::from(move |_: MouseEvent| control.open.emit(trigger.cast::<web_sys::Element>()))
    };
    html! {
        <button ref={trigger} type="button" class="app-horn-tab app-horn-tab--down app-compose-utility-tab app-help-trigger"
            aria-haspopup="dialog" aria-controls={DIALOG_ID} aria-expanded={shown.to_string()}
            aria-keyshortcuts="? Escape" aria-label="Keyboard shortcuts and places"
            title="Keyboard shortcuts and places (?, Escape at top level)" onclick={pressed}>
            <span aria-hidden="true">{"Help"}</span>
        </button>
    }
}

fn render_sheet(
    surface: &NodeRef,
    close: &Callback<()>,
    leave: &Callback<MouseEvent>,
    documentation: &UseStateHandle<bool>,
    props: &HelpProps,
) -> Html {
    let places = [
        (
            "Chat",
            "Teams, their composers, and agent messages",
            AppRoute::chat(),
        ),
        (
            "Work",
            "Open tasks, Drafts, and Designs",
            AppRoute::work(WorkRoute::AllOpen),
        ),
        (
            "Spice",
            "Resident, providers, jobs, and agents",
            AppRoute::Spice,
        ),
    ];
    html! {
        <div ref={surface.clone()} class="app-help-sheet app-overlay-panel">
            <header class="app-help-header">
                <h2 id={HEADING_ID}>{"Help"}</h2>
                <button type="button" class="app-compact-control" autofocus=true
                    onclick={close.reform(|_: MouseEvent| ())}>{"Close"}</button>
            </header>
            <div class="app-control-rail app-segment-slider app-help-modes"
                role="group" aria-label="Help view" data-stop={if **documentation { "1" } else { "0" }}>
                {for [(false, "Help"), (true, "Documentation")].map(|(selected, label)| {
                    let documentation = documentation.clone();
                    html! {
                        <button type="button" class="app-choice-chip"
                            aria-pressed={(*documentation == selected).to_string()}
                            onclick={Callback::from(move |_| documentation.set(selected))}>{label}</button>
                    }
                })}
            </div>
            <div hidden={**documentation}>
            <nav class="app-help-places" aria-label="Places">
                {for places.into_iter().map(|(label, detail, route)| html! {
                    <a class="app-help-place" href={format!("#{}", route.path())} onclick={leave.clone()}>
                        <strong>{label}</strong>
                        <span>{detail}</span>
                    </a>
                })}
            </nav>
            <div class="app-help-sections">
                {for SECTIONS.iter().map(render_section)}
            </div>
            </div>
            <documents::Documentation active={**documentation}
                connection={props.connection.clone()} on_action={props.on_action.clone()} />
        </div>
    }
}

fn render_section(section: &Section) -> Html {
    html! {
        <section class="app-help-section" aria-labelledby={section.id}>
            <h3 id={section.id}>{section.title}</h3>
            <dl>
                {for section.rows.iter().map(|row| html! {
                    <div class="app-help-row">
                        <dt>{render_keys(row.keys)}</dt>
                        <dd>{row.action}</dd>
                    </div>
                })}
            </dl>
        </section>
    }
}

fn render_keys(alternatives: &[&[&str]]) -> Html {
    alternatives
        .iter()
        .enumerate()
        .map(|(index, combination)| {
            let keys = combination.iter().enumerate().map(|(position, key)| {
                html! {
                    <>
                        if position > 0 {
                            <span class="app-help-join" aria-hidden="true">{"+"}</span>
                        }
                        <kbd>{*key}</kbd>
                    </>
                }
            });
            html! {
                <>
                    if index > 0 {
                        <span class="app-help-or">{"or"}</span>
                    }
                    <span class="app-help-combination">{for keys}</span>
                </>
            }
        })
        .collect()
}

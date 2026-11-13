//! The composer controls frame, which alone follows typed drafts.
//!
//! Typed text lives in its composer element until sent, so drafting is heard
//! where it happens: every composer change raises an `input` event that
//! bubbles to the composer form, including values the app writes itself. The
//! frame keeps which composers a press would deliver and re-renders only
//! itself, and only when that answer changes, so a keystroke never renders
//! the app. Each team's member composers share it.

use std::fmt::Write as _;

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use yew::prelude::*;

/// One composer as the send button sees it, in composer order.
#[derive(Clone, PartialEq)]
pub(in crate::browser) struct SendSlot {
    /// The attribute selector naming this composer's `.team-agent` section.
    pub section: String,
    pub accent: usize,
    /// A press can deliver this composer to the lane.
    pub reachable: bool,
    /// A quote or image is drafted, which sends without any text.
    pub attached: bool,
    /// A keyboard broadcast from this team fans out to this member.
    pub broadcast: bool,
    /// This composer's sent content is locked awaiting durable acceptance,
    /// so a press does not deliver it again.
    pub sending: bool,
}

#[derive(Properties, PartialEq)]
pub(in crate::browser) struct FrameProps {
    /// Nothing runs and no quote or image is drafted.
    pub dormant: bool,
    /// Send is admissible apart from whether anything is drafted.
    pub ready: bool,
    /// A topology change may add or remove composers.
    pub topology_revision: u64,
    pub slots: Vec<SendSlot>,
    /// The press's word, beneath FULL when every composer receives.
    pub verb: &'static str,
    /// What one slot is, as the press's title counts it.
    pub noun: &'static str,
    pub speech: Option<&'static str>,
    pub drive: bool,
    pub drain: bool,
    pub policy_pending: bool,
    /// The paired round controls in the first row.
    pub turbo: Html,
    pub menu: Html,
    pub children: Html,
}

#[derive(Clone, Default, PartialEq)]
struct Drafting {
    /// Some composer holds typed text.
    typed: bool,
    /// Accents of the members a press would send to, in member order.
    lit: Vec<usize>,
}

#[function_component(ControlsFrame)]
pub(in crate::browser) fn controls_frame(props: &FrameProps) -> Html {
    let root = use_node_ref();
    let send = use_node_ref();
    let drafting = use_state_eq(Drafting::default);
    {
        // A topology change, or quotes and images coming or going, may add or
        // remove composers without any of them raising input, so the answer
        // is read again whenever the slots change as well as on every edit.
        let root = root.clone();
        let drafting = drafting.clone();
        use_effect_with(
            (props.slots.clone(), props.topology_revision),
            move |(slots, _)| {
                let form = form_of(&root);
                if let Some(form) = &form {
                    drafting.set(read_drafting(form, slots));
                }
                let listener = form.map(|form| {
                    let scanned = form.clone();
                    let slots = slots.clone();
                    let callback = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
                        let edited = event.target().is_some_and(|target| {
                            target.has_type::<web_sys::HtmlTextAreaElement>()
                        });
                        if edited {
                            drafting.set(read_drafting(&scanned, &slots));
                        }
                    });
                    form.add_event_listener_with_callback(
                        "input",
                        callback.as_ref().unchecked_ref(),
                    )
                    .expect("team form accepts its drafting listener");
                    (form, callback)
                });
                move || {
                    if let Some((form, callback)) = listener {
                        let _ = form.remove_event_listener_with_callback(
                            "input",
                            callback.as_ref().unchecked_ref(),
                        );
                    }
                }
            },
        );
    }
    let stepped_back = props.dormant && !drafting.typed;
    let idle = stepped_back;
    let sending = props.slots.iter().any(|slot| slot.sending);
    // A send that is refused outright reaches no one, so it lights nothing.
    let lit = if props.ready {
        drafting.lit.clone()
    } else {
        Vec::new()
    };
    // Recipients of a keyboard broadcast, which never depend on drafts.
    let broadcast = props
        .slots
        .iter()
        .filter(|slot| slot.broadcast)
        .map(|slot| slot.accent)
        .collect::<Vec<_>>();
    let broadcast_count = broadcast.len();
    {
        // Recipient gradients paint only the rim. The CSP requires CSSOM
        // rather than style attributes; broadcast previews select their
        // own rim without changing drafting state on modifier presses.
        let send = send.clone();
        use_effect_with((lit.clone(), broadcast), move |(lit, broadcast)| {
            if let Some(button) = send.cast::<web_sys::HtmlElement>() {
                let style = button.style();
                for (property, accents) in [
                    ("--team-send-recipient-rim", lit),
                    ("--team-send-broadcast-rim", broadcast),
                ] {
                    let _ = if accents.is_empty() {
                        style.remove_property(property).map(drop)
                    } else {
                        style.set_property(property, &recipient_rim(accents))
                    };
                }
            }
        });
    }
    html! {
        <aside
            ref={root}
            class="app-glass-control-surface team-composer-controls"
            data-speech-mode={props.speech}
            data-drive={props.drive.to_string()}
            data-drain={props.drain.to_string()}
            data-policy-pending={props.policy_pending.to_string()}
            data-idle={idle.to_string()}
        >
            {props.turbo.clone()}
            {props.menu.clone()}
            {props.children.clone()}
            {send_button(SendButton {
                send,
                // While a send is in flight, a press with nothing else
                // drafted could only repeat it.
                disabled: stepped_back || !props.ready || (sending && lit.is_empty()),
                sending,
                recipients: lit.len(),
                broadcast_recipients: broadcast_count,
                members: props.slots.len(),
                verb: props.verb,
                noun: props.noun,
            })}
        </aside>
    }
}

struct SendButton {
    send: NodeRef,
    disabled: bool,
    /// Some composer's send awaits durable acceptance.
    sending: bool,
    recipients: usize,
    broadcast_recipients: usize,
    members: usize,
    verb: &'static str,
    noun: &'static str,
}

fn send_button(button: SendButton) -> Html {
    let SendButton {
        send,
        disabled,
        sending,
        recipients,
        broadcast_recipients,
        members,
        verb,
        noun,
    } = button;
    let title = match recipients {
        0 => verb.to_owned(),
        1 => format!("{verb} to 1 {noun}"),
        count => format!("{verb} to {count} {noun}s"),
    };
    let full_send = members > 1 && recipients == members;
    let full_broadcast = members > 1 && broadcast_recipients == members;
    // Touch engines need not hold :active until release. Project contact
    // only: do not prevent native activation or change submission authority.
    let touch_down = {
        let send = send.clone();
        Callback::from(move |event: web_sys::PointerEvent| {
            if !event.is_primary() || event.pointer_type() != "touch" {
                return;
            }
            if let Some(button) = send.cast::<web_sys::HtmlElement>()
                && !button.has_attribute("disabled")
            {
                let _ = button.set_attribute("data-touch-contact", "true");
            }
        })
    };
    let touch_release = {
        let send = send.clone();
        Callback::from(move |event: web_sys::PointerEvent| {
            if event.is_primary()
                && event.pointer_type() == "touch"
                && let Some(button) = send.cast::<web_sys::HtmlElement>()
            {
                let _ = button.remove_attribute("data-touch-contact");
            }
        })
    };
    html! {
        <button
            ref={send}
            class="team-send"
            type="submit"
            data-lit={(recipients != 0).to_string()}
            data-sending={sending.to_string()}
            aria-busy={sending.then_some("true")}
            data-recipient-count={recipients.to_string()}
            data-broadcast-recipient-count={broadcast_recipients.to_string()}
            data-full-send={full_send.to_string()}
            data-full-broadcast={full_broadcast.to_string()}
            {disabled}
            {title}
            onpointerdown={touch_down}
            onpointerup={touch_release.clone()}
            onpointercancel={touch_release.clone()}
            onlostpointercapture={touch_release}
        >
            <span class="team-send-label">
                if members > 1 {
                    <span class="team-send-full" aria-hidden="true">{"Full"}</span>
                }
                <span class="team-send-word">{verb}</span>
            </span>
        </button>
    }
}

/// Angular width of the seam separating recipient accents on the rim.
const SEAM_DEGREES: f64 = 1.4;

/// One equal arc per recipient: the first opens at the right edge and the
/// rest follow counterclockwise. A darker seam distinguishes neighbours
/// even when they share an accent; one recipient colors the whole rim.
fn recipient_rim(accents: &[usize]) -> String {
    if let [accent] = accents {
        return format!("var(--accent-{accent})");
    }
    // CSS sweeps clockwise from its origin, so lay the members out in reverse
    // from the right edge and put each seam where its counterclockwise arc opens.
    let slice = 360.0 / accents.len() as f64;
    let mut gradient = String::from("conic-gradient(from 90deg");
    for (position, accent) in accents.iter().rev().enumerate() {
        let start = position as f64 * slice;
        let end = start + slice;
        let seam = end - SEAM_DEGREES;
        let _ = write!(
            gradient,
            ", var(--accent-{accent}) {start:.2}deg {seam:.2}deg, \
             color-mix(in srgb, var(--accent-{accent}) 50%, var(--app-shadow-source)) \
             {seam:.2}deg {end:.2}deg"
        );
    }
    gradient.push(')');
    gradient
}

fn form_of(root: &NodeRef) -> Option<web_sys::Element> {
    root.cast::<web_sys::Element>()?
        .closest("form")
        .ok()
        .flatten()
}

/// What a press would send: a reachable composer is lit when it holds text or
/// a drafted quote or image, the same content a submit prepares.
fn read_drafting(form: &web_sys::Element, slots: &[SendSlot]) -> Drafting {
    let mut drafting = Drafting::default();
    for slot in slots {
        if slot.sending {
            continue;
        }
        let typed = form
            .query_selector(&format!(".team-agent{}", slot.section))
            .ok()
            .flatten()
            .is_some_and(|section| holds_text(&section));
        drafting.typed |= typed;
        if slot.reachable && (typed || slot.attached) {
            drafting.lit.push(slot.accent);
        }
    }
    drafting
}

/// Whether any textarea in this composer section holds non-blank text.
pub(in crate::browser) fn holds_text(section: &web_sys::Element) -> bool {
    let Ok(textareas) = section.query_selector_all("textarea") else {
        return false;
    };
    (0..textareas.length()).any(|index| {
        textareas
            .item(index)
            .and_then(|node| node.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
            .is_some_and(|textarea| !textarea.value().trim().is_empty())
    })
}

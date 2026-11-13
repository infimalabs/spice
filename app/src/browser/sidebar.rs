//! Content-agnostic lifecycle, focus, and dismissal for the app sidebar.

use super::*;
use crate::work::TaskOverviewSelection;

/// The one content mounted in the app sidebar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Content {
    Spice,
    Tasks(TaskOverviewSelection),
}

impl Content {
    pub(super) const fn tasks(&self) -> Option<&TaskOverviewSelection> {
        match self {
            Self::Spice => None,
            Self::Tasks(selection) => Some(selection),
        }
    }

    const fn focus_key(&self) -> Option<&'static str> {
        match self {
            Self::Spice => Some("spice"),
            Self::Tasks(_) => None,
        }
    }
}
const RETURN_FOCUS_KEY: &str = "data-sidebar-focus-key";

struct ReturnFocus {
    target: web_sys::Element,
    key: Option<String>,
}

impl ReturnFocus {
    fn new(target: web_sys::Element) -> Self {
        // Yew delegates some click events through a non-focusable ancestor.
        // The browser's active control is the authoritative launcher then.
        let focusable = target
            .dyn_ref::<web_sys::HtmlElement>()
            .is_some_and(|element| element.tab_index() >= 0);
        let target = if focusable {
            target
        } else {
            active_focus_target().unwrap_or(target)
        };
        let key = target.get_attribute(RETURN_FOCUS_KEY);
        Self { target, key }
    }

    fn resolve(self) -> Option<web_sys::Element> {
        self.key
            .as_deref()
            .and_then(Self::resolve_key)
            .or_else(|| overlay::returnable(&self.target).then_some(self.target))
    }

    fn resolve_key(key: &str) -> Option<web_sys::Element> {
        let document = web_sys::window()?.document()?;
        let candidates = document
            .query_selector_all("[data-sidebar-focus-key]")
            .ok()?;
        (0..candidates.length())
            .filter_map(|index| candidates.item(index))
            .filter_map(|node| node.dyn_into::<web_sys::Element>().ok())
            .find(|candidate| {
                candidate.get_attribute(RETURN_FOCUS_KEY).as_deref() == Some(key)
                    && overlay::returnable(candidate)
            })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Request {
    pub content: Option<Content>,
    /// The destination owns focus, so ending this request skips the close slide
    /// and focus return.
    pub leaving: bool,
}

pub(super) struct Controls {
    pub content: UseStateHandle<Option<Content>>,
    pub presented: UseStateHandle<bool>,
    pub closing: UseStateHandle<bool>,
    pub close_ref: NodeRef,
    pub sheet_ref: NodeRef,
    /// Override implicit capture with a launcher. Its stable focus key recovers
    /// an equivalent element if rendering replaces the original node.
    pub return_focus_to: Callback<web_sys::Element>,
    pub close: Callback<()>,
}

#[hook]
pub(super) fn use_controls(mounted: bool, requested: Request, close: Callback<()>) -> Controls {
    let content = use_state(|| None::<Content>);
    let return_focus = use_mut_ref(|| None::<ReturnFocus>);
    let presented = use_state_eq(|| false);
    let closing = use_state_eq(|| false);
    let close_ref = use_node_ref();
    let sheet_ref = use_node_ref();
    let open = open_callback(
        content.clone(),
        presented.clone(),
        closing.clone(),
        return_focus.clone(),
    );
    let request_close = {
        let closing = closing.clone();
        Callback::from(move |()| {
            closing.set(true);
            close.emit(());
        })
    };
    let complete_close = complete_close_callback(
        content.clone(),
        presented.clone(),
        closing.clone(),
        return_focus.clone(),
    );
    let leave = leave_callback(
        content.clone(),
        presented.clone(),
        closing.clone(),
        return_focus.clone(),
    );
    use_request(requested, open.clone(), leave, &content, closing.clone());
    overlay::use_present(
        content.is_some() && mounted && !*presented,
        presented.clone(),
    );
    overlay::use_close_transition(
        *closing,
        *presented && mounted && !overlay::reduced_motion(),
        sheet_ref.clone(),
        overlay::FADE,
        complete_close,
    );
    use_focus(&content, mounted, close_ref.clone());
    overlay::use_escape_owner(
        content.is_some() && !*closing && mounted,
        request_close.clone(),
    );
    let return_focus_to = {
        let return_focus = return_focus.clone();
        Callback::from(move |target| *return_focus.borrow_mut() = Some(ReturnFocus::new(target)))
    };
    Controls {
        content,
        presented,
        closing,
        close_ref,
        sheet_ref,
        return_focus_to,
        close: request_close,
    }
}

#[hook]
fn use_request(
    requested: Request,
    open: Callback<Content>,
    leave: Callback<()>,
    content: &UseStateHandle<Option<Content>>,
    closing: UseStateHandle<bool>,
) {
    let shown = content.is_some();
    let prior = use_mut_ref(|| None::<Content>);
    let leaving = requested.leaving;
    use_effect_with(requested.content, move |requested| {
        let was_requested = prior.borrow().is_some();
        *prior.borrow_mut() = requested.clone();
        match requested {
            Some(content) => open.emit(content.clone()),
            None if shown && was_requested && leaving => leave.emit(()),
            None if shown && was_requested => closing.set(true),
            None => {}
        }
    });
}

/// Swap content in place when already open. Only opening from empty or
/// interrupting a close begins a presentation transition and captures focus.
fn open_callback(
    content: UseStateHandle<Option<Content>>,
    presented: UseStateHandle<bool>,
    closing: UseStateHandle<bool>,
    return_focus: Rc<RefCell<Option<ReturnFocus>>>,
) -> Callback<Content> {
    Callback::from(move |next: Content| {
        if content.as_ref() == Some(&next) && !*closing {
            return;
        }
        if content.is_none() && return_focus.borrow().is_none() {
            *return_focus.borrow_mut() = active_focus_target().map(ReturnFocus::new);
        }
        if content.is_none() || *closing {
            presented.set(false);
            closing.set(false);
        }
        content.set(Some(next));
    })
}

pub(super) fn active_focus_target() -> Option<web_sys::Element> {
    web_sys::window()?
        .document()?
        .active_element()
        .filter(|element| {
            element
                .dyn_ref::<web_sys::HtmlElement>()
                .is_some_and(|element| element.tab_index() >= 0)
        })
}

/// Drop the sidebar for a destination that owns focus, without a close slide.
fn leave_callback(
    content: UseStateHandle<Option<Content>>,
    presented: UseStateHandle<bool>,
    closing: UseStateHandle<bool>,
    return_focus: Rc<RefCell<Option<ReturnFocus>>>,
) -> Callback<()> {
    Callback::from(move |()| {
        return_focus.borrow_mut().take();
        presented.set(false);
        closing.set(false);
        content.set(None);
    })
}

fn complete_close_callback(
    content: UseStateHandle<Option<Content>>,
    presented: UseStateHandle<bool>,
    closing: UseStateHandle<bool>,
    return_focus: Rc<RefCell<Option<ReturnFocus>>>,
) -> Callback<()> {
    Callback::from(move |()| {
        let launcher = return_focus
            .borrow_mut()
            .take()
            .and_then(ReturnFocus::resolve)
            .or_else(|| {
                content
                    .as_ref()
                    .and_then(Content::focus_key)
                    .and_then(ReturnFocus::resolve_key)
            });
        if let Some(element) =
            launcher.and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
        {
            let options = web_sys::FocusOptions::new();
            options.set_prevent_scroll(true);
            let _ = element.focus_with_options(&options);
        }
        presented.set(false);
        closing.set(false);
        content.set(None);
    })
}

/// Focus enters the sidebar when it opens. Content swaps leave focus inside the
/// already-presented sheet; close completion restores its surviving launcher.
#[hook]
fn use_focus(content: &UseStateHandle<Option<Content>>, mounted: bool, close: NodeRef) {
    use_effect_with(content.is_some() && mounted, move |active| {
        if *active && let Some(button) = close.cast::<web_sys::HtmlElement>() {
            let options = web_sys::FocusOptions::new();
            options.set_prevent_scroll(true);
            let _ = button.focus_with_options(&options);
        }
    });
}

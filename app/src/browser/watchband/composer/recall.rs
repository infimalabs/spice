use super::*;

fn vertical_edge(event: &KeyboardEvent) -> Option<crate::composer_history::RecallEdge> {
    match event.key().as_str() {
        "ArrowUp" => Some(crate::composer_history::RecallEdge::Up),
        "ArrowDown" => Some(crate::composer_history::RecallEdge::Down),
        _ => None,
    }
}

fn unmodified(event: &KeyboardEvent) -> bool {
    !event.is_composing()
        && !event.shift_key()
        && !event.alt_key()
        && !event.ctrl_key()
        && !event.meta_key()
}

pub(super) struct Listeners {
    textarea: web_sys::HtmlTextAreaElement,
    keydown: Closure<dyn FnMut(KeyboardEvent)>,
    keyup: Closure<dyn FnMut(KeyboardEvent)>,
    selection: Closure<dyn FnMut(web_sys::Event)>,
    reset: Closure<dyn FnMut(web_sys::Event)>,
}

impl Drop for Listeners {
    fn drop(&mut self) {
        for (name, callback) in [
            ("keydown", self.keydown.as_ref().unchecked_ref()),
            ("keyup", self.keyup.as_ref().unchecked_ref()),
            ("select", self.selection.as_ref().unchecked_ref()),
            ("input", self.reset.as_ref().unchecked_ref()),
            ("blur", self.reset.as_ref().unchecked_ref()),
            ("pointerdown", self.reset.as_ref().unchecked_ref()),
        ] {
            let _ = self
                .textarea
                .remove_event_listener_with_callback(name, callback);
        }
    }
}

pub(super) fn listen(
    textarea: web_sys::HtmlTextAreaElement,
    lane: String,
    onkeydown: Callback<KeyboardEvent>,
    recall: Rc<RefCell<crate::composer_history::Recall>>,
    trace_instance: u64,
    trace_kind: &'static str,
) -> Listeners {
    reset_recall_edge(&recall);
    let keydown = {
        let textarea = textarea.clone();
        let recall = recall.clone();
        Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
            let _trace = crate::browser::diagnostics::span("composer.keydown", || {
                crate::browser::diagnostics::Metadata {
                    instance: Some(trace_instance),
                    kind: Some(trace_kind),
                    event: Some("keydown"),
                    focused: textarea.matches(":focus").ok(),
                    selection_start: textarea.selection_start().ok().flatten(),
                    selection_end: textarea.selection_end().ok().flatten(),
                    ..crate::browser::diagnostics::Metadata::default()
                }
            });
            if textarea.read_only() {
                return;
            }
            if !recall_arrow(&event, &textarea, &lane, &recall) {
                onkeydown.emit(event);
            }
        })
    };
    let keyup = {
        let textarea = textarea.clone();
        let recall = recall.clone();
        Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
            recall_keyup(&event, &textarea, &recall);
        })
    };
    let selection = {
        let textarea = textarea.clone();
        let recall = recall.clone();
        Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            recall_selection(&textarea, &recall);
        })
    };
    let reset = {
        let recall = recall.clone();
        Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            reset_recall_edge(&recall);
        })
    };
    for (name, callback) in [
        ("keydown", keydown.as_ref().unchecked_ref()),
        ("keyup", keyup.as_ref().unchecked_ref()),
        ("select", selection.as_ref().unchecked_ref()),
        ("input", reset.as_ref().unchecked_ref()),
        ("blur", reset.as_ref().unchecked_ref()),
        ("pointerdown", reset.as_ref().unchecked_ref()),
    ] {
        textarea
            .add_event_listener_with_callback(name, callback)
            .expect("composer textarea accepts its history listener");
    }
    Listeners {
        textarea,
        keydown,
        keyup,
        selection,
        reset,
    }
}

/// Recall the lane's history only after the same vertical arrow first moved or
/// rested a collapsed caret at that rendered edge.
fn recall_arrow(
    event: &KeyboardEvent,
    textarea: &web_sys::HtmlTextAreaElement,
    lane: &str,
    recall: &RefCell<crate::composer_history::Recall>,
) -> bool {
    let Some(edge) = vertical_edge(event) else {
        recall.borrow_mut().edge_reset();
        return false;
    };
    if !unmodified(event) {
        recall.borrow_mut().edge_reset();
        return false;
    }
    let (Ok(Some(start)), Ok(Some(end))) = (textarea.selection_start(), textarea.selection_end())
    else {
        recall.borrow_mut().edge_reset();
        return false;
    };
    let value = textarea.value();
    let outer = start == end
        && on_outer_rendered_line(textarea, &value, start, edge)
            .unwrap_or_else(|| at_document_edge(&value, start, edge));
    let shown = {
        let mut recall = recall.borrow_mut();
        if !recall.edge_keydown(edge, start, end, outer) {
            return false;
        }
        recall.sync(crate::browser::input_history::sends(lane));
        match edge {
            crate::composer_history::RecallEdge::Up => {
                recall.older(|| crate::browser::input_history::entries(lane), &value)
            }
            crate::composer_history::RecallEdge::Down => recall.newer(&value),
        }
    };
    let Some(shown) = shown else {
        return false;
    };
    event.prevent_default();
    textarea.set_value(&shown);
    let caret = match edge {
        crate::composer_history::RecallEdge::Up => 0,
        crate::composer_history::RecallEdge::Down => {
            u32::try_from(shown.encode_utf16().count()).unwrap_or(u32::MAX)
        }
    };
    let _ = textarea.set_selection_range(caret, caret);
    announce_value(textarea);
    recall.borrow_mut().edge_replaced(edge, caret);
    true
}

fn recall_keyup(
    event: &KeyboardEvent,
    textarea: &web_sys::HtmlTextAreaElement,
    recall: &RefCell<crate::composer_history::Recall>,
) {
    let Some(edge) = vertical_edge(event).filter(|_| unmodified(event)) else {
        return;
    };
    let (Ok(Some(start)), Ok(Some(end))) = (textarea.selection_start(), textarea.selection_end())
    else {
        recall.borrow_mut().edge_reset();
        return;
    };
    recall.borrow_mut().edge_keyup(edge, start, end);
}

fn recall_selection(
    textarea: &web_sys::HtmlTextAreaElement,
    recall: &RefCell<crate::composer_history::Recall>,
) {
    let (Ok(Some(start)), Ok(Some(end))) = (textarea.selection_start(), textarea.selection_end())
    else {
        recall.borrow_mut().edge_reset();
        return;
    };
    recall.borrow_mut().edge_selection(start, end);
}

fn reset_recall_edge(recall: &RefCell<crate::composer_history::Recall>) {
    recall.borrow_mut().edge_reset();
}

fn at_document_edge(value: &str, offset: u32, edge: crate::composer_history::RecallEdge) -> bool {
    match edge {
        crate::composer_history::RecallEdge::Up => offset == 0,
        crate::composer_history::RecallEdge::Down => {
            offset == u32::try_from(value.encode_utf16().count()).unwrap_or(u32::MAX)
        }
    }
}

/// Computed properties that decide where the textarea wraps its text.
const WRAPPING: [&str; 23] = [
    "direction",
    "font-family",
    "font-feature-settings",
    "font-kerning",
    "font-size",
    "font-stretch",
    "font-style",
    "font-variant",
    "font-weight",
    "letter-spacing",
    "line-height",
    "overflow-wrap",
    "padding-bottom",
    "padding-left",
    "padding-right",
    "padding-top",
    "tab-size",
    "text-align",
    "text-indent",
    "text-transform",
    "white-space",
    "word-break",
    "word-spacing",
];

/// Whether the UTF-16 caret offset sits on the first rendered line for Up or
/// the last for Down. A hidden mirror wraps the same text at the same width.
fn on_outer_rendered_line(
    textarea: &web_sys::HtmlTextAreaElement,
    value: &str,
    offset: u32,
    edge: crate::composer_history::RecallEdge,
) -> Option<bool> {
    let window = web_sys::window()?;
    let document = window.document()?;
    let computed = window.get_computed_style(textarea).ok()??;
    let mirror: web_sys::HtmlElement = document.create_element("div").ok()?.dyn_into().ok()?;
    let style = mirror.style();
    for property in WRAPPING {
        let value = computed.get_property_value(property).ok()?;
        style.set_property(property, &value).ok()?;
    }
    let width = format!("{}px", textarea.client_width());
    for (property, value) in [
        ("position", "fixed"),
        ("top", "0"),
        ("left", "0"),
        ("visibility", "hidden"),
        ("pointer-events", "none"),
        ("box-sizing", "border-box"),
        ("border", "0"),
        ("margin", "0"),
        ("height", "auto"),
        ("width", width.as_str()),
    ] {
        style.set_property(property, value).ok()?;
    }
    let marker = || -> Option<web_sys::HtmlElement> {
        document.create_element("span").ok()?.dyn_into().ok()
    };
    let (start, caret, end) = (marker()?, marker()?, marker()?);
    let (before, after) = split_utf16(value, offset);
    mirror.append_with_node_1(&start).ok()?;
    mirror.append_with_str_1(before).ok()?;
    mirror.append_with_node_1(&caret).ok()?;
    mirror.append_with_str_1(after).ok()?;
    mirror.append_with_node_1(&end).ok()?;
    document.body()?.append_with_node_1(&mirror).ok()?;
    let outer = match edge {
        crate::composer_history::RecallEdge::Up => caret.offset_top() == start.offset_top(),
        crate::composer_history::RecallEdge::Down => caret.offset_top() == end.offset_top(),
    };
    mirror.remove();
    Some(outer)
}

/// Split at a UTF-16 offset, moving back to a scalar boundary when necessary.
fn split_utf16(value: &str, offset: u32) -> (&str, &str) {
    let mut units = 0;
    for (index, character) in value.char_indices() {
        units += u32::try_from(character.len_utf16()).unwrap_or(2);
        if units > offset {
            return value.split_at(index);
        }
    }
    (value, "")
}

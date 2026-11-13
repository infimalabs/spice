//! Text labels: placing one, typing into it in place, and the font both the
//! SVG presentation and the PNG export draw it with.

use super::*;
use crate::image_annotation::text_size;

/// Bold system sans, named the same way for SVG text and the canvas.
const FAMILY: &str = "system-ui, -apple-system, \"Segoe UI\", Roboto, Helvetica, Arial, sans-serif";

/// A label being typed and the marks as they stood before typing began, so
/// the whole entry lands as one undoable edit or vanishes on Escape. Each
/// entry has its own serial, so a new entry always mounts a fresh field even
/// where the previous label stood.
#[derive(PartialEq, Eq)]
pub(super) struct Typing {
    pub(super) index: usize,
    pub(super) serial: u64,
    before: Vec<Shape>,
}

impl Typing {
    fn new(index: usize, before: Vec<Shape>) -> Self {
        thread_local! {
            static SERIAL: Cell<u64> = const { Cell::new(0) };
        }
        let serial = SERIAL.with(|serial| {
            serial.set(serial.get() + 1);
            serial.get()
        });
        Self {
            index,
            serial,
            before,
        }
    }
}

/// The canvas font for a label `size` image pixels tall.
pub(super) fn font(size: i32) -> String {
    format!("700 {size}px {FAMILY}")
}

pub(super) const fn family() -> &'static str {
    FAMILY
}

/// The width of the contrasting halo that keeps a label legible on any image.
pub(super) fn halo(size: i32) -> f64 {
    (f64::from(size) / 8.0).max(1.0)
}

/// The rendered width of `text` in image pixels, measured with the export's
/// own canvas font so the label box matches what Apply flattens.
fn measure(text: &str, size: i32) -> i32 {
    thread_local! {
        static CONTEXT: Option<web_sys::CanvasRenderingContext2d> = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.create_element("canvas").ok())
            .and_then(|canvas| canvas.dyn_into::<web_sys::HtmlCanvasElement>().ok())
            .and_then(|canvas| canvas.get_context("2d").ok().flatten())
            .and_then(|context| context.dyn_into::<web_sys::CanvasRenderingContext2d>().ok());
    }
    CONTEXT.with(|context| {
        context.as_ref().map_or_else(
            || text.chars().count() as i32 * size * 3 / 5,
            |context| {
                context.set_font(&font(size));
                context
                    .measure_text(text)
                    .map_or(0.0, |metrics| metrics.width())
                    .ceil() as i32
            },
        )
    })
}

/// A new empty label centred vertically on the press, ready for typing.
pub(super) fn place(session: &Session, point: Point, dimensions: (u32, u32), color: Color) {
    let size = text_size(dimensions.0, dimensions.1);
    let mut history = session.history.borrow_mut();
    let before = history.shapes.clone();
    let index = history.shapes.len();
    let anchor = Point {
        x: point.x,
        y: (point.y - size / 2).clamp(0, (dimensions.1 as i32 - size).max(0)),
    };
    history.shapes.push(Shape::label(anchor, color, size));
    history.selected = Some(index);
    *session.label.borrow_mut() = Some(Typing::new(index, before));
}

/// Type into an existing label.
pub(super) fn edit(session: &Session, index: usize) {
    let mut history = session.history.borrow_mut();
    history.selected = Some(index);
    let before = history.shapes.clone();
    *session.label.borrow_mut() = Some(Typing::new(index, before));
}

/// Type into the selected label, as a double click or Enter does. Pointer
/// capture retargets a double click to the SVG root, but the press that
/// began it already selected the label it landed on.
pub(super) fn reopen(session: &Session) -> bool {
    if session.gesture.borrow().is_some() || session.label.borrow().is_some() {
        return false;
    }
    let selected = {
        let history = session.history.borrow();
        history
            .selected
            .filter(|index| history.shapes[*index].kind == Kind::Text)
    };
    selected.inspect(|index| edit(session, *index)).is_some()
}

/// End typing: keep the entry as one edit, dropping a blank label, or
/// restore the marks as they were before typing began.
pub(super) fn finish(session: &Session, keep: bool) {
    let Some(typing) = session.label.borrow_mut().take() else {
        return;
    };
    let mut history = session.history.borrow_mut();
    if keep {
        if history
            .shapes
            .get(typing.index)
            .is_some_and(|shape| !shape.visible())
        {
            history.shapes.remove(typing.index);
            history.selected = None;
        }
        history.commit(typing.before);
    } else {
        history.shapes = typing.before;
        history.selected = None;
    }
    drop(history);
    session.repaint.emit(());
}

#[derive(Properties, PartialEq)]
pub(super) struct FieldProps {
    pub session: Session,
    /// The typing session this field belongs to.
    pub serial: u64,
    pub shape: Shape,
    pub dimensions: (u32, u32),
}

/// Field events the label input answers; the pointer press is kept from the
/// canvas so pressing inside the field never starts a mark.
const FIELD_EVENTS: [&str; 4] = ["input", "keydown", "blur", "pointerdown"];

/// The field's deferred end of typing: it finishes only its own entry, since
/// a press elsewhere may already have finished it and opened another, then
/// hands the keyboard back to the canvas when a key ended it.
fn settle(
    session: &Session,
    serial: u64,
    keep: bool,
    field: &web_sys::HtmlInputElement,
    keyboard: bool,
) {
    let own = session
        .label
        .borrow()
        .as_ref()
        .is_some_and(|typing| typing.serial == serial);
    if own {
        finish(session, keep);
    }
    if keyboard
        && let Some(stage) = field
            .closest(".image-annotation-stage")
            .ok()
            .flatten()
            .and_then(|stage| stage.dyn_into::<web_sys::HtmlElement>().ok())
    {
        let _ = stage.focus();
    }
}

/// The in-place text field over a label being typed. It lives in the SVG's
/// coordinates, so its type is exactly as large as the label it becomes.
/// Yew lowercases `foreignObject` and creates every descendant of an SVG
/// element in the SVG namespace, so the frame and its HTML input are created
/// here and placed as one foreign node.
#[function_component(Field)]
pub(super) fn field(props: &FieldProps) -> Html {
    let nodes = use_memo((), |()| {
        let document = web_sys::window()
            .and_then(|window| window.document())
            .expect("the annotation editor has a document");
        let frame = document
            .create_element_ns(Some("http://www.w3.org/2000/svg"), "foreignObject")
            .expect("the app document creates SVG foreign objects");
        let input = document
            .create_element("input")
            .ok()
            .and_then(|element| element.dyn_into::<web_sys::HtmlInputElement>().ok())
            .expect("the app document creates text inputs");
        input.set_class_name("image-annotation-label-field");
        input.set_type("text");
        let _ = input.set_attribute("aria-label", "Label text");
        let _ = frame.append_child(&input);
        (frame, input)
    });
    {
        let input = nodes.1.clone();
        let session = props.session.clone();
        let serial = props.serial;
        let text = props.shape.text.clone();
        use_effect_with((), move |()| {
            input.set_value(&text);
            let field = input.clone();
            // Finishing unmounts this field and drops this listener, and
            // handing focus back fires blur into it, so the listener only
            // records the outcome and the finish runs as its own task.
            let callback = Closure::<dyn Fn(web_sys::Event)>::new(move |event: web_sys::Event| {
                let keep = match event.type_().as_str() {
                    "input" => {
                        typed(&session, &field.value());
                        return;
                    }
                    "blur" => true,
                    "keydown" => match keyed(&event) {
                        Some(keep) => keep,
                        None => return,
                    },
                    _ => {
                        event.stop_propagation();
                        return;
                    }
                };
                let (session, field) = (session.clone(), field.clone());
                let keyboard = event.type_() == "keydown";
                spawn_local(async move { settle(&session, serial, keep, &field, keyboard) });
            });
            for kind in FIELD_EVENTS {
                let _ =
                    input.add_event_listener_with_callback(kind, callback.as_ref().unchecked_ref());
            }
            move || {
                for kind in FIELD_EVENTS {
                    let _ = input.remove_event_listener_with_callback(
                        kind,
                        callback.as_ref().unchecked_ref(),
                    );
                }
            }
        });
    }
    let size = props.shape.end.y - props.shape.start.y;
    {
        let (frame, input) = (*nodes).clone();
        use_effect_with(
            (props.shape.start, size, props.dimensions, props.shape.color),
            move |(start, size, dimensions, color)| {
                let pad = size / 4;
                let width = (dimensions.0 as i32 - start.x + pad).max(*size);
                for (name, value) in [
                    ("x", start.x - pad),
                    ("y", start.y - pad),
                    ("width", width),
                    ("height", size + 2 * pad),
                ] {
                    let _ = frame.set_attribute(name, &value.to_string());
                }
                // The app CSP refuses style attributes, so the size and ink
                // reach the field through CSSOM.
                let style = input.style();
                let _ = style.set_property("--annotation-label-size", &format!("{size}px"));
                let _ = style.set_property("--annotation-label-ink", color.ink());
                let _ = style.set_property("--annotation-label-halo", color.halo());
            },
        );
    }
    {
        // Effects run in order, so the frame has its size before focus.
        let input = nodes.1.clone();
        use_effect_with((), move |()| {
            let _ = input.focus();
            let end = input.value().len() as u32;
            let _ = input.set_selection_range(end, end);
        });
    }
    Html::VRef(nodes.0.clone().into())
}

/// Typing re-measures the label so its box always matches what Apply draws.
fn typed(session: &Session, value: &str) {
    let Some(index) = session.label.borrow().as_ref().map(|typing| typing.index) else {
        return;
    };
    let mut history = session.history.borrow_mut();
    if let Some(shape) = history.shapes.get_mut(index) {
        let size = shape.end.y - shape.start.y;
        let width = measure(value, size);
        shape.relabel(Rc::from(value), width, size);
    }
    drop(history);
    session.repaint.emit(());
}

/// Enter keeps the label and Escape abandons it; every typing key stays with
/// the field instead of reaching the canvas commands.
fn keyed(event: &web_sys::Event) -> Option<bool> {
    event.stop_propagation();
    let keep = match event.dyn_ref::<KeyboardEvent>()?.key().as_str() {
        "Enter" => true,
        "Escape" => false,
        _ => return None,
    };
    event.prevent_default();
    Some(keep)
}

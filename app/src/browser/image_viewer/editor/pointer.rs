//! Pointer gestures on the annotation canvas: draw a mark, move one, drag one
//! of its handles, or place a label, all measured in the image's own pixels.

use super::*;

/// The SVG the gestures land on and the image size its viewBox spans.
#[derive(Clone, PartialEq)]
pub(super) struct DrawingSurface {
    pub(super) svg: NodeRef,
    pub(super) dimensions: (u32, u32),
}

fn position(canvas: &DrawingSurface, event: &PointerEvent, clamp: bool) -> Option<Point> {
    let element = canvas.svg.cast::<web_sys::Element>()?;
    let bounds = element.get_bounding_client_rect();
    let (width, height) = (
        f64::from(canvas.dimensions.0),
        f64::from(canvas.dimensions.1),
    );
    let scale = (bounds.width() / width).min(bounds.height() / height);
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    let x =
        (f64::from(event.client_x()) - bounds.x() - (bounds.width() - width * scale) / 2.0) / scale;
    let y = (f64::from(event.client_y()) - bounds.y() - (bounds.height() - height * scale) / 2.0)
        / scale;
    if !clamp && (x < 0.0 || y < 0.0 || x > width || y > height) {
        return None;
    }
    Some(Point {
        x: x.round().clamp(0.0, width) as i32,
        y: y.round().clamp(0.0, height) as i32,
    })
}

pub(super) fn cancel(history: &mut History, gesture: &mut Option<Gesture>) {
    if let Some(gesture) = gesture.take() {
        history.shapes = gesture.before;
        history.selected = None;
    }
}

/// The modifier keys a mouse or pointer event reports held.
fn held(event: &MouseEvent) -> Held {
    Held {
        shift: event.shift_key(),
        option: event.alt_key(),
        command: event.meta_key() || event.ctrl_key(),
    }
}

/// Rebuild the marks for the gesture's current pointer and `held` keys.
fn replay(history: &mut History, gesture: &Gesture, held: Held, dimensions: (u32, u32)) {
    let size = (dimensions.0 as i32, dimensions.1 as i32);
    let selected = gesture.edit.apply(
        &gesture.before,
        &mut history.shapes,
        gesture.press,
        gesture.point,
        held,
        size,
    );
    history.selected = Some(selected);
}

/// A modifier key went down or up: the gesture under way, if any, takes the
/// keys now held without waiting for the pointer to move.
pub(super) fn modifiers(session: &Session, held: Held, dimensions: (u32, u32)) {
    if let Some(gesture) = session.gesture.borrow().as_ref() {
        replay(&mut session.history.borrow_mut(), gesture, held, dimensions);
        session.repaint.emit(());
    }
}

/// The mark under the press and the handle grabbed on it, if any.
fn hit(history: &History, event: &PointerEvent) -> (Option<usize>, Option<String>) {
    let target = event
        .target()
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok());
    let handle = target
        .as_ref()
        .and_then(|target| target.get_attribute("data-annotation-handle"));
    let index = target
        .and_then(|target| target.closest("[data-annotation-shape]").ok().flatten())
        .and_then(|target| target.get_attribute("data-annotation-shape"))
        .and_then(|index| index.parse::<usize>().ok())
        .filter(|index| *index < history.shapes.len());
    (index, handle)
}

/// What a press begins: an edit of the mark it hit when the Select tool is
/// active or a handle was grabbed, otherwise a new mark of the drawing tool.
/// A Select press on bare image only clears the selection and begins nothing.
fn begin(history: &mut History, tool: Tool, pen: Pen, event: &PointerEvent) -> Option<Edit> {
    let (index, handle) = hit(history, event);
    if let Some(index) = index.filter(|_| tool == Tool::Select || handle.is_some()) {
        return Some(match handle.as_deref() {
            Some("start") => Edit::Start(index),
            Some("end") => Edit::End(index),
            _ => Edit::Move(index),
        });
    }
    let kind = match tool {
        Tool::Select | Tool::Text => {
            history.selected = None;
            return None;
        }
        Tool::Rectangle => Kind::Rectangle,
        Tool::Line => Kind::Line,
        Tool::Arrow => Kind::Arrow,
    };
    Some(Edit::Draw {
        kind,
        route: pen.route,
        color: pen.color,
    })
}

pub(super) fn down(
    event: &PointerEvent,
    canvas: &DrawingSurface,
    surface: &NodeRef,
    session: &Session,
    tool: Tool,
    pen: Pen,
) {
    if event.button() != 0 || session.gesture.borrow().is_some() {
        return;
    }
    *session.gesture_click.borrow_mut() = false;
    let Some(point) = position(canvas, event, false) else {
        return;
    };
    event.prevent_default();
    // A press elsewhere keeps the label being typed before it acts, then
    // takes the keyboard back to the stage.
    label::finish(session, true);
    if let Some(surface) = surface.cast::<web_sys::HtmlElement>() {
        let _ = surface.focus();
    }
    if tool == Tool::Text {
        let (index, handle) = hit(&session.history.borrow(), event);
        let editable = index.filter(|index| {
            handle.is_none() && session.history.borrow().shapes[*index].kind == Kind::Text
        });
        if handle.is_none() {
            match editable {
                Some(index) => label::edit(session, index),
                None => label::place(session, point, canvas.dimensions, pen.color),
            }
            // The click this press generates places the label; the backdrop
            // must not take it as a press on bare image and deselect it.
            *session.gesture_click.borrow_mut() = true;
            session.repaint.emit(());
            return;
        }
    }
    let mut history = session.history.borrow_mut();
    if let Some(edit) = begin(&mut history, tool, pen, event) {
        if let Some(element) = canvas.svg.cast::<web_sys::Element>() {
            let _ = element.set_pointer_capture(event.pointer_id());
        }
        let gesture = Gesture {
            pointer: event.pointer_id(),
            edit,
            before: history.shapes.clone(),
            press: point,
            point,
        };
        replay(&mut history, &gesture, held(event), canvas.dimensions);
        *session.gesture.borrow_mut() = Some(gesture);
    }
    drop(history);
    session.repaint.emit(());
}

pub(super) fn moved(event: &PointerEvent, canvas: &DrawingSurface, session: &Session) {
    {
        let mut gesture = session.gesture.borrow_mut();
        let Some(gesture) = gesture
            .as_mut()
            .filter(|gesture| gesture.pointer == event.pointer_id())
        else {
            return;
        };
        let Some(point) = position(canvas, event, true) else {
            return;
        };
        event.prevent_default();
        gesture.point = point;
        replay(
            &mut session.history.borrow_mut(),
            gesture,
            held(event),
            canvas.dimensions,
        );
    }
    session.repaint.emit(());
}

/// Release ends the gesture as one undoable edit; a mark too small to see is
/// dropped rather than committed.
pub(super) fn up(event: &PointerEvent, canvas: &DrawingSurface, session: &Session) {
    if !session.owns(event) {
        return;
    }
    *session.gesture_click.borrow_mut() = true;
    moved(event, canvas, session);
    let finished = session.gesture.borrow_mut().take().expect("owned gesture");
    let mut history = session.history.borrow_mut();
    if let Some(index) = history.selected
        && !history.shapes[index].visible()
    {
        history.shapes.remove(index);
        history.selected = None;
    }
    history.commit(finished.before);
    if let Some(element) = canvas.svg.cast::<web_sys::Element>() {
        let _ = element.release_pointer_capture(event.pointer_id());
    }
    session.repaint.emit(());
}

pub(super) fn interrupted(event: &PointerEvent, session: &Session) {
    if session.owns(event) {
        *session.gesture_click.borrow_mut() = true;
        cancel(
            &mut session.history.borrow_mut(),
            &mut session.gesture.borrow_mut(),
        );
        session.repaint.emit(());
    }
}

/// A click on the SVG root outside any gesture clears selection without
/// dismissing the editor; the click a finished gesture generates is swallowed.
pub(super) fn backdrop(event: &MouseEvent, svg: &NodeRef, session: &Session) {
    if std::mem::take(&mut *session.gesture_click.borrow_mut()) {
        return;
    }
    if session.gesture.borrow().is_none()
        && event.target().is_some_and(|target| {
            svg.cast::<web_sys::Element>()
                .is_some_and(|svg| JsValue::from(svg) == JsValue::from(target))
        })
    {
        session.history.borrow_mut().selected = None;
        session.repaint.emit(());
    }
}

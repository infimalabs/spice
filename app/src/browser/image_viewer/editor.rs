use super::*;
use crate::composer_drafts::DraftImage;
use crate::image_annotation::{Color, Edit, Editor as History, Held, Kind, Point, Route, Shape};
use std::cell::{Cell, RefCell};
use wasm_bindgen_futures::{JsFuture, spawn_local};

mod export;
mod keys;
mod label;
mod pointer;
mod shape;
mod view;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tool {
    Select,
    Rectangle,
    Line,
    Arrow,
    Text,
}

/// What the next mark is drawn with: the route a line or arrow starts with
/// and the colour of every new mark.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Pen {
    route: Route,
    color: Color,
}

/// One pointer gesture, replayed from the marks `before` it whenever the
/// pointer moves or a modifier key changes.
#[derive(PartialEq, Eq)]
struct Gesture {
    pointer: i32,
    edit: Edit,
    before: Vec<Shape>,
    /// Where the press began and where the pointer is now, in image pixels.
    press: Point,
    point: Point,
}

pub(super) struct Applied {
    pub revision: u64,
    pub media: wire::Media,
    pub shapes: Rc<Vec<Shape>>,
}

#[derive(Properties, PartialEq)]
pub(super) struct EditorProps {
    pub image: DraftImage,
    pub onapply: Callback<Applied>,
    pub oncancel: Callback<()>,
    pub ondirty: Callback<bool>,
}

/// The editing session every handler shares. Its cells live as long as the
/// editor, so handlers compare by identity and read current state when called.
#[derive(Clone)]
struct Session {
    history: Rc<RefCell<History>>,
    gesture: Rc<RefCell<Option<Gesture>>>,
    /// The label whose text is being typed, with the marks before it began.
    label: Rc<RefCell<Option<label::Typing>>>,
    /// Pointer capture can retarget a gesture's generated click to the SVG
    /// root. That click belongs to the edit, not to the surrounding backdrop.
    gesture_click: Rc<RefCell<bool>>,
    repaint: Callback<()>,
}

impl PartialEq for Session {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.history, &other.history)
            && Rc::ptr_eq(&self.gesture, &other.gesture)
            && Rc::ptr_eq(&self.label, &other.label)
            && Rc::ptr_eq(&self.gesture_click, &other.gesture_click)
            && self.repaint == other.repaint
    }
}

#[derive(Clone, PartialEq)]
struct KeyboardCommands {
    tool: Callback<Tool>,
    route: Callback<()>,
    color: Callback<Color>,
    cancel: Callback<()>,
    apply: Callback<()>,
    route_enabled: bool,
    apply_enabled: bool,
}

impl Session {
    fn owns(&self, event: &PointerEvent) -> bool {
        self.gesture
            .borrow()
            .as_ref()
            .is_some_and(|gesture| gesture.pointer == event.pointer_id())
    }
}

/// Decode the frozen original once for the intrinsic size that every mark and
/// the export are measured in.
#[hook]
fn use_natural_size(
    source: Rc<AttrValue>,
    issue: UseStateHandle<AttrValue>,
    alive: Rc<Cell<bool>>,
) -> (u32, u32) {
    let dimensions = use_state_eq(|| (0_u32, 0_u32));
    {
        let dimensions = dimensions.clone();
        use_effect_with(source, move |source| {
            let source = source.clone();
            let pending = alive.clone();
            spawn_local(async move {
                let result = async {
                    let image = web_sys::HtmlImageElement::new()?;
                    image.set_src(&source);
                    JsFuture::from(image.decode()).await?;
                    Ok::<_, JsValue>((image.natural_width(), image.natural_height()))
                }
                .await;
                if pending.get() {
                    match result {
                        Ok(size) => dimensions.set(size),
                        Err(_) => issue.set("Image unavailable for annotation".into()),
                    }
                }
            });
            move || alive.set(false)
        });
    }
    *dimensions
}

/// Focus the stage when the editor opens and track its laid-out size, which
/// keeps handles a constant on-screen size.
#[hook]
fn use_stage(surface: NodeRef) -> (f64, f64) {
    let viewport = use_state_eq(|| (0.0_f64, 0.0_f64));
    {
        let viewport = viewport.clone();
        use_effect_with(surface, move |surface| {
            let element = surface
                .cast::<web_sys::HtmlElement>()
                .expect("annotation surface");
            let _ = element.focus();
            let measured = element.clone();
            let resized = Closure::<dyn FnMut()>::new(move || {
                let bounds = measured.get_bounding_client_rect();
                viewport.set((bounds.width(), bounds.height()));
            });
            let observer = web_sys::ResizeObserver::new(resized.as_ref().unchecked_ref())
                .expect("annotation size observer");
            observer.observe(&element);
            move || {
                observer.disconnect();
                drop(resized);
            }
        });
    }
    *viewport
}

/// A toolbar history command: it abandons any gesture in progress first.
#[hook]
fn use_history_command(session: Session, command: fn(&mut History)) -> Callback<MouseEvent> {
    use_callback(session, move |_: MouseEvent, session| {
        {
            let mut history = session.history.borrow_mut();
            pointer::cancel(&mut history, &mut session.gesture.borrow_mut());
            command(&mut history);
        }
        session.repaint.emit(());
    })
}

/// The toolbar tool choice. Taking up a different tool lets go of the
/// selected mark, so the route and palette controls then set up the next mark
/// rather than restyling the last one.
#[hook]
fn use_tool_command(session: Session, tool: UseStateHandle<Tool>) -> Callback<Tool> {
    use_callback((session, tool), |value: Tool, (session, tool)| {
        if **tool == value {
            return;
        }
        {
            let mut history = session.history.borrow_mut();
            pointer::cancel(&mut history, &mut session.gesture.borrow_mut());
            history.selected = None;
        }
        tool.set(value);
        session.repaint.emit(());
    })
}

/// The toolbar route control: it cycles the selected line or arrow's route
/// as one edit, or else the route the next line or arrow starts with.
#[hook]
fn use_route_command(session: Session, pen: UseStateHandle<Pen>) -> Callback<()> {
    use_callback((session, pen), |(), (session, pen)| {
        let route = {
            let mut history = session.history.borrow_mut();
            pointer::cancel(&mut history, &mut session.gesture.borrow_mut());
            let selected = history
                .selected
                .filter(|index| history.shapes[*index].kind.routed());
            match selected {
                Some(index) => {
                    let before = history.shapes.clone();
                    let route = history.shapes[index].route.next();
                    history.shapes[index].route = route;
                    history.commit(before);
                    route
                }
                None => pen.route.next(),
            }
        };
        pen.set(Pen { route, ..**pen });
        session.repaint.emit(());
    })
}

/// The toolbar colour control: it recolours the selected mark as one edit
/// and becomes the colour of the next mark drawn. An open label is kept
/// first, so its typing and its recolour stay separate edits.
#[hook]
fn use_color_command(session: Session, pen: UseStateHandle<Pen>) -> Callback<Color> {
    use_callback((session, pen), |color: Color, (session, pen)| {
        label::finish(session, true);
        {
            let mut history = session.history.borrow_mut();
            pointer::cancel(&mut history, &mut session.gesture.borrow_mut());
            if let Some(index) = history
                .selected
                .filter(|index| history.shapes[*index].color != color)
            {
                let before = history.shapes.clone();
                history.shapes[index].color = color;
                history.commit(before);
            }
        }
        pen.set(Pen { color, ..**pen });
        session.repaint.emit(());
    })
}

#[derive(Clone, PartialEq)]
struct Export {
    busy: UseStateHandle<bool>,
    issue: UseStateHandle<AttrValue>,
    alive: Rc<Cell<bool>>,
    onapply: Callback<Applied>,
}

/// The Apply command: it exports the current marks unless an export or a
/// gesture is already in progress.
#[hook]
fn use_apply_command(
    session: Session,
    base: UseStateHandle<DraftImage>,
    export: Export,
) -> Callback<()> {
    use_callback((session, base, export), |(), (session, base, export)| {
        if *export.busy || session.gesture.borrow().is_some() {
            return;
        }
        let shapes = Rc::new(session.history.borrow().shapes.clone());
        apply(base, shapes, export.clone());
    })
}

/// Flatten the marks onto the frozen original at its native size and hand the
/// result back tagged with the revision the session opened, so the owner can
/// refuse it if that draft image has since changed or gone.
fn apply(base: &DraftImage, shapes: Rc<Vec<Shape>>, export: Export) {
    let original = base.original.clone();
    let revision = base.revision;
    export.busy.set(true);
    export.issue.set("Applying annotations".into());
    spawn_local(async move {
        let result = export::render(&original, &shapes).await;
        if !export.alive.get() {
            return;
        }
        export.busy.set(false);
        match result {
            Ok(media) => export.onapply.emit(Applied {
                revision,
                media,
                shapes,
            }),
            Err(error) => export.issue.set(
                format!(
                    "Annotations could not be applied: {}",
                    crate::browser::clipboard::error_text(&error)
                )
                .into(),
            ),
        }
    });
}

/// The stage and canvas event handlers of one editing session.
struct CanvasHandlers {
    down: Callback<PointerEvent>,
    moved: Callback<PointerEvent>,
    up: Callback<PointerEvent>,
    interrupted: Callback<PointerEvent>,
    backdrop: Callback<MouseEvent>,
    reopen: Callback<MouseEvent>,
}

/// Pointer and keyboard input reaches the session only while no export is
/// running; moves, releases and interruptions always finish a gesture.
#[hook]
fn use_canvas_handlers(
    canvas: pointer::DrawingSurface,
    surface: NodeRef,
    session: Session,
    tool: Tool,
    pen: Pen,
    idle: bool,
) -> CanvasHandlers {
    let down = use_callback(
        (canvas.clone(), surface, session.clone(), (tool, pen), idle),
        |event: PointerEvent, (canvas, surface, session, (tool, pen), idle)| {
            if *idle {
                pointer::down(&event, canvas, surface, session, *tool, *pen);
            }
        },
    );
    let reopen = use_callback(session.clone(), |event: MouseEvent, session| {
        if label::reopen(session) {
            event.prevent_default();
            session.repaint.emit(());
        }
    });
    let moved = use_callback(
        (canvas.clone(), session.clone()),
        |event: PointerEvent, (canvas, session)| pointer::moved(&event, canvas, session),
    );
    let up = use_callback(
        (canvas.clone(), session.clone()),
        |event: PointerEvent, (canvas, session)| pointer::up(&event, canvas, session),
    );
    let interrupted = use_callback(session.clone(), |event: PointerEvent, session| {
        pointer::interrupted(&event, session)
    });
    let backdrop = use_callback(
        (canvas.svg, session),
        |event: MouseEvent, (svg, session)| pointer::backdrop(&event, svg, session),
    );
    CanvasHandlers {
        down,
        moved,
        up,
        interrupted,
        backdrop,
        reopen,
    }
}

#[function_component(Editor)]
pub(super) fn editor(props: &EditorProps) -> Html {
    // Freeze the opening revision; a later draft change cannot silently become
    // the base of this editing session or claim its pending export.
    let base = use_state(|| props.image.clone());
    let source = use_memo(base.original.clone(), |image| {
        AttrValue::from(image.data_url.clone())
    });
    let refresh = use_force_update();
    let session = Session {
        history: use_mut_ref(|| History::new((*base.shapes).clone())),
        gesture: use_mut_ref(|| None::<Gesture>),
        label: use_mut_ref(|| None::<label::Typing>),
        gesture_click: use_mut_ref(|| false),
        repaint: use_callback((), move |(), ()| refresh.force_update()),
    };
    let tool = use_state_eq(|| Tool::Rectangle);
    let pen = use_state_eq(Pen::default);
    let issue = use_state_eq(AttrValue::default);
    let busy = use_state_eq(|| false);
    let alive = use_mut_ref(|| Rc::new(Cell::new(true)));
    let dimensions = use_natural_size(source.clone(), issue.clone(), alive.borrow().clone());
    let surface = use_node_ref();
    let viewport = use_stage(surface.clone());
    let svg = use_node_ref();
    let on = use_canvas_handlers(
        pointer::DrawingSurface {
            svg: svg.clone(),
            dimensions,
        },
        surface.clone(),
        session.clone(),
        *tool,
        *pen,
        !*busy,
    );
    let tool_command = use_tool_command(session.clone(), tool.clone());
    let undo_command = use_history_command(session.clone(), History::undo);
    let redo_command = use_history_command(session.clone(), History::redo);
    let delete_command = use_history_command(session.clone(), History::delete);
    let route_command = use_route_command(session.clone(), pen.clone());
    let color_command = use_color_command(session.clone(), pen.clone());
    let apply_command = use_apply_command(
        session.clone(),
        base.clone(),
        Export {
            busy: busy.clone(),
            issue: issue.clone(),
            alive: alive.borrow().clone(),
            onapply: props.onapply.clone(),
        },
    );
    let ready = dimensions.0 > 0 && dimensions.1 > 0;
    let history = session.history.borrow();
    let route_enabled = history
        .selected
        .is_some_and(|index| history.shapes[index].kind.routed())
        || matches!(*tool, Tool::Line | Tool::Arrow);
    let keyboard = use_callback(
        (
            session.clone(),
            dimensions,
            !*busy,
            KeyboardCommands {
                tool: tool_command.clone(),
                route: route_command.clone(),
                color: color_command.clone(),
                cancel: props.oncancel.clone(),
                apply: apply_command.clone(),
                route_enabled,
                apply_enabled: !*busy && ready,
            },
        ),
        |event: KeyboardEvent, (session, dimensions, idle, commands)| {
            keys::handle(&event, session, *dimensions, *idle, commands);
        },
    );
    let released = use_callback(
        (session.clone(), dimensions),
        |event: KeyboardEvent, (session, dimensions)| {
            keys::released(&event, session, *dimensions);
        },
    );
    let actions = view::Actions {
        tool: tool_command,
        undo: undo_command,
        redo: redo_command,
        delete: delete_command,
        route: route_command.reform(|_: MouseEvent| ()),
        color: color_command,
        cancel: props.oncancel.reform(|_: MouseEvent| ()),
        apply: apply_command.reform(|_: MouseEvent| ()),
    };
    let dirty = history.differs_from(base.shapes.as_ref());
    {
        let ondirty = props.ondirty.clone();
        use_effect_with(dirty, move |dirty| ondirty.emit(*dirty));
    }
    html! {
        <div class="image-annotation-editor" onkeydown={keyboard} onkeyup={released}>
            {view::toolbar(*tool, *pen, *busy, ready, &history, actions)}
            <div class="image-annotation-stage" ref={surface} tabindex="0" role="group" aria-label="Annotation canvas. V selects; R draws rectangles; L draws lines; A draws arrows; T places text; G cycles routing; M mirrors a selected curve or elbow; 1 through 6 choose colours. Select marks to move; drag handles to adjust; arrow keys move the selected mark; Enter edits a selected label. While drawing a line or arrow or dragging one of its ends, Alt or Option turns elbows, Command or Control curves, and Shift snaps a straight one to 45 degrees or mirrors a bent one. Shift draws or resizes a rectangle as a square, and Alt or Option draws it from its centre. While moving a mark, Shift keeps it to one axis and Alt or Option moves a copy.">
                if ready {
                    <svg ref={svg} class={classes!("image-annotation-svg", (*tool != Tool::Select).then_some("image-annotation-svg--draw"))} viewBox={format!("0 0 {} {}", dimensions.0, dimensions.1)} preserveAspectRatio="xMidYMid meet"
                        onpointerdown={on.down} onpointermove={on.moved} onpointerup={on.up} onpointercancel={on.interrupted.clone()} onlostpointercapture={on.interrupted} onclick={on.backdrop} ondblclick={on.reopen} oncontextmenu={Callback::from(|event: MouseEvent| event.prevent_default())}>
                        <image href={(*source).clone()} x="0" y="0" width={dimensions.0.to_string()} height={dimensions.1.to_string()} />
                        {view::marks(&session, &history, dimensions, viewport)}
                    </svg>
                } else if issue.is_empty() {
                    <p class="image-annotation-loading" role="status">{"Loading image"}</p>
                }
            </div>
            if !issue.is_empty() { <p class="toast" role="status">{(*issue).clone()}</p> }
        </div>
    }
}

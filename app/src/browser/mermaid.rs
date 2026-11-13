//! Lazy, page-owned Mermaid rendering and its image-only display boundary.

use std::cell::Cell;
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use yew::prelude::*;

use crate::speech_queue::SpeechControl;

mod frame;
mod service;

use super::image_viewer::use_button;
use service::{Service, Shared};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Theme {
    Light,
    Dark,
}

impl Theme {
    fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    fn read() -> Result<Self, AttrValue> {
        let theme = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.document_element())
            .and_then(|root| root.get_attribute("data-app-effective-theme"));
        match theme.as_deref() {
            Some("light") => Ok(Self::Light),
            Some("dark") => Ok(Self::Dark),
            _ => Err("Effective theme is unavailable".into()),
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum RenderKind {
    Diagram,
    InlineMath,
    DisplayMath,
}

impl RenderKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Diagram => "diagram",
            Self::InlineMath => "inline-math",
            Self::DisplayMath => "display-math",
        }
    }
}

#[derive(Clone)]
struct Context {
    service: Rc<Result<Shared, AttrValue>>,
    theme: Result<Theme, AttrValue>,
}

impl PartialEq for Context {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.service, &other.service) && self.theme == other.theme
    }
}

#[derive(Properties, PartialEq)]
pub(super) struct ProviderProps {
    pub(super) children: Children,
}

#[function_component(Provider)]
pub(super) fn provider(props: &ProviderProps) -> Html {
    let service = use_memo((), |()| {
        super::connection::loaded_app_generation()
            .and_then(Service::new)
            .map_err(AttrValue::from)
    });
    let theme = use_state_eq(Theme::read);
    use_effect_with((), {
        let theme = theme.clone();
        move |()| {
            let observation = ThemeObservation::new(theme.clone());
            if observation.is_err() {
                theme.set(Err("Could not observe the theme".into()));
            }
            move || drop(observation)
        }
    });
    use_effect_with((), {
        let service = service.clone();
        let theme = theme.clone();
        move |()| {
            let registration = page_hide(service.clone());
            if registration.is_err() {
                theme.set(Err("Could not own the isolated renderer lifecycle".into()));
            }
            move || {
                if let Ok(cleanup) = registration {
                    cleanup();
                }
                if let Ok(service) = service.as_ref() {
                    Service::close(service, "Renderer unmounted");
                }
            }
        }
    });
    let context = Context {
        service,
        theme: (*theme).clone(),
    };
    html! { <ContextProvider<Context> {context}>{props.children.clone()}</ContextProvider<Context>> }
}

struct ThemeObservation {
    observer: web_sys::MutationObserver,
    _callback: Closure<dyn FnMut(js_sys::Array, web_sys::MutationObserver)>,
}

impl ThemeObservation {
    fn new(theme: UseStateHandle<Result<Theme, AttrValue>>) -> Result<Self, JsValue> {
        let root = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.document_element())
            .ok_or("No document root")?;
        let callback = Closure::new(move |_: js_sys::Array, _: web_sys::MutationObserver| {
            theme.set(Theme::read());
        });
        let observer = web_sys::MutationObserver::new(callback.as_ref().unchecked_ref())?;
        let observation = Self {
            observer,
            _callback: callback,
        };
        let options = web_sys::MutationObserverInit::new();
        options.set_attributes(true);
        options.set_attribute_filter(&js_sys::Array::of1(&JsValue::from_str(
            "data-app-effective-theme",
        )));
        observation.observer.observe_with_options(&root, &options)?;
        Ok(observation)
    }
}

impl Drop for ThemeObservation {
    fn drop(&mut self) {
        self.observer.disconnect();
    }
}

fn page_hide(service: Rc<Result<Shared, AttrValue>>) -> Result<impl FnOnce(), JsValue> {
    let window = web_sys::window().ok_or("No browser window")?;
    let callback: Closure<dyn FnMut(web_sys::Event)> = Closure::new(move |_: web_sys::Event| {
        if let Ok(service) = service.as_ref() {
            Service::close(service, "Renderer interrupted; retry to resume");
        }
    });
    window.add_event_listener_with_callback("pagehide", callback.as_ref().unchecked_ref())?;
    Ok(move || {
        let _ = window
            .remove_event_listener_with_callback("pagehide", callback.as_ref().unchecked_ref());
    })
}

#[derive(Clone, Default)]
struct Display {
    revision: AttrValue,
    theme: Option<Theme>,
    image: Option<AttrValue>,
    geometry: Option<Rc<crate::mermaid_geometry::Geometry>>,
    error: Option<AttrValue>,
    pending: bool,
}

#[derive(Properties, PartialEq)]
pub(super) struct MermaidImageProps {
    pub(super) source: AttrValue,
    #[prop_or_default]
    pub(super) node_ids: Rc<Vec<String>>,
    #[prop_or_default]
    pub(super) on_geometry: Callback<Option<Rc<crate::mermaid_geometry::Geometry>>>,
    #[prop_or_default]
    pub(super) render_geometry: Option<Callback<Rc<crate::mermaid_geometry::Geometry>, Html>>,
    #[prop_or(true)]
    pub(super) final_answer: bool,
    /// A canvas host supplies its own fixed Copy control; no full-size opener
    /// competes with its pan and zoom gestures.
    #[prop_or_default]
    pub(super) canvas: bool,
}

struct RenderSubscription {
    context: Context,
    host: NodeRef,
    source: AttrValue,
    kind: RenderKind,
    node_ids: Rc<Vec<String>>,
    on_geometry: Callback<Option<Rc<crate::mermaid_geometry::Geometry>>>,
    revision: Rc<AttrValue>,
    current_theme: Option<Theme>,
    previous: Option<AttrValue>,
    display: UseStateHandle<Display>,
}

#[hook]
fn use_render(subscription: RenderSubscription) {
    let RenderSubscription {
        context,
        host,
        kind,
        source,
        node_ids,
        on_geometry,
        revision,
        current_theme,
        previous,
        display,
    } = subscription;
    use_effect_with((revision, context, on_geometry), {
        move |(revision, context, on_geometry)| {
            let pending = Display {
                revision: revision.as_ref().clone(),
                theme: current_theme,
                image: previous.clone(),
                geometry: None,
                error: None,
                pending: true,
            };
            display.set(pending.clone());
            on_geometry.emit(None);
            let on_result = on_geometry.clone();
            let callback = Callback::from(move |outcome: service::Outcome| {
                let mut next = pending.clone();
                next.pending = false;
                match outcome {
                    Ok(render) => {
                        next.image = Some(render.image);
                        next.geometry = Some(render.geometry.clone());
                        on_result.emit(Some(render.geometry));
                    }
                    Err(error) => next.error = Some(error),
                }
                display.set(next);
            });
            let request = (|| {
                let service = context.service.as_ref().as_ref().map_err(Clone::clone)?;
                let theme = context.theme.clone()?;
                let host = host
                    .cast::<web_sys::Element>()
                    .ok_or_else(|| AttrValue::from("Render target is detached"))?;
                Service::request(
                    service,
                    service::RenderRequest {
                        kind,
                        source,
                        theme,
                        node_ids,
                        host,
                        revision: revision.as_ref().clone(),
                        callback: callback.clone(),
                    },
                )
            })();
            if let Err(error) = &request {
                callback.emit(Err(error.clone()));
            }
            move || drop(request)
        }
    });
}

/// Owned consumer: dropping/changing this component cancels its subscription.
/// The original source remains text, including after parse/load/resource errors.
#[function_component(MermaidImage)]
pub(super) fn mermaid_image(props: &MermaidImageProps) -> Html {
    let context = use_context::<Context>().expect("app diagram provider");
    let host = use_node_ref();
    let display = use_state(Display::default);
    let retry = use_state(|| 0_u32);
    let counter = use_memo((), |()| Cell::new(0_u64));
    let revision = use_memo(
        (
            props.source.clone(),
            context.theme.clone(),
            props.node_ids.clone(),
            *retry,
        ),
        move |_| {
            let next = counter.get().wrapping_add(1);
            counter.set(next);
            AttrValue::from(next.to_string())
        },
    );
    let current_theme = context.theme.as_ref().ok().copied();
    let previous = (display.theme == current_theme)
        .then(|| display.image.clone())
        .flatten();
    use_render(RenderSubscription {
        context: context.clone(),
        host: host.clone(),
        kind: RenderKind::Diagram,
        source: props.source.clone(),
        node_ids: props.node_ids.clone(),
        on_geometry: props.on_geometry.clone(),
        revision: revision.clone(),
        current_theme,
        previous: previous.clone(),
        display: display.clone(),
    });
    let pending = display.revision != *revision || display.pending;
    let error = (!pending).then(|| display.error.clone()).flatten();
    let state = if pending {
        "pending"
    } else if error.is_some() {
        "error"
    } else {
        "ready"
    };
    let status = if pending {
        AttrValue::from("Rendering diagram")
    } else if let Some(error) = &error {
        if props.final_answer {
            error.clone()
        } else {
            "Waiting for a complete diagram".into()
        }
    } else {
        "Diagram rendered".into()
    };
    let retry_action = use_callback(retry, |(), retry| {
        retry.set((**retry).wrapping_add(1));
    });
    let retry_button = use_button(retry_action);
    let geometry_overlay = display
        .geometry
        .clone()
        .zip(props.render_geometry.as_ref())
        .map_or_else(Html::default, |(geometry, render)| render.emit(geometry));
    let status_state = if error.is_some() && !props.final_answer {
        "pending"
    } else {
        state
    };
    if props.canvas {
        return html! {
            <div ref={host} class="mermaid-render" data-mermaid-revision={revision.as_ref().clone()} data-mermaid-state={state}>
                <div class="message-image">
                    if let Some(image) = previous {
                        <img src={image} alt="Mermaid diagram" draggable="false" data-mosaic-image-state="loaded" />
                    }
                    <span class="message-image-status" role="status" data-mermaid-status={status_state}>{status}</span>
                </div>
                {geometry_overlay}
                <button ref={retry_button} type="button" class="app-compact-control mermaid-retry" hidden={error.is_none()}>{"Retry diagram"}</button>
            </div>
        };
    }
    html! {
        <div ref={host} class="mermaid-render" data-mermaid-revision={revision.as_ref().clone()} data-mermaid-state={state}>
            <button type="button" class="message-image message-image--openable" data-image-preview="" data-image-name="diagram.svg" disabled={previous.is_none()} aria-label="View full-size diagram" title="View full-size diagram">
                if let Some(image) = previous {
                    <img src={image} alt="Mermaid diagram" data-mosaic-image-state="loaded" />
                }
                <span class="message-image-status" role="status" data-mermaid-status={status_state}>{status}</span>
            </button>
            {geometry_overlay}
            <super::code_copy::SourceCopy source={props.source.clone()} />
            <button ref={retry_button} type="button" class="app-compact-control mermaid-retry" hidden={error.is_none()}>{"Retry diagram"}</button>
        </div>
    }
}

#[derive(Properties, PartialEq)]
struct MathImageProps {
    source: AttrValue,
    display: bool,
}

#[function_component(MathImage)]
fn math_image(props: &MathImageProps) -> Html {
    let context = use_context::<Context>().expect("app renderer provider");
    let host = use_node_ref();
    let state = use_state(Display::default);
    // This callback is a render-effect dependency, even though math ignores geometry.
    let on_geometry = use_callback((), |_, _| {});
    let counter = use_memo((), |()| Cell::new(0_u64));
    let kind = if props.display {
        RenderKind::DisplayMath
    } else {
        RenderKind::InlineMath
    };
    let revision = use_memo(
        (props.source.clone(), context.theme.clone(), kind),
        move |_| {
            let next = counter.get().wrapping_add(1);
            counter.set(next);
            AttrValue::from(next.to_string())
        },
    );
    let current_theme = context.theme.as_ref().ok().copied();
    let previous = (state.theme == current_theme)
        .then(|| state.image.clone())
        .flatten();
    use_render(RenderSubscription {
        context,
        host: host.clone(),
        source: props.source.clone(),
        kind,
        node_ids: Rc::new(Vec::new()),
        on_geometry,
        revision: revision.clone(),
        current_theme,
        previous: previous.clone(),
        display: state.clone(),
    });
    let pending = state.revision != *revision || state.pending;
    let error = (!pending).then(|| state.error.clone()).flatten();
    let fallback = if props.display {
        format!("\\[{}\\]", props.source)
    } else {
        format!("\\({}\\)", props.source)
    };
    html! {
        <span
            ref={host}
            class={classes!("math-render", props.display.then_some("math-render--display"))}
            data-mermaid-revision={revision.as_ref().clone()}
            data-math-state={if pending { "pending" } else if error.is_some() { "error" } else { "ready" }}
            aria-busy={pending.to_string()}
            title={error}
        >
            if let Some(image) = previous {
                <img src={image} alt={format!("Math: {}", props.source)} />
            } else {
                <span class="math-source-fallback">{fallback}</span>
            }
        </span>
    }
}

#[derive(Clone, PartialEq)]
pub(super) struct MarkdownControl {
    pub(super) target: AttrValue,
    pub(super) content: Html,
}

#[derive(Properties, PartialEq)]
pub(super) struct MarkdownBodyProps {
    pub(super) html: AttrValue,
    pub(super) sources: Rc<Vec<AttrValue>>,
    pub(super) final_answer: bool,
    pub(super) quote_tasks: Rc<crate::runtime::QuoteTasks>,
    #[prop_or_default]
    pub(super) controls: Rc<Vec<MarkdownControl>>,
    /// The card's own attribution, mounted in every hunk it owns.
    #[prop_or_default]
    pub(super) signoff: Option<Html>,
    /// Controls for non-prose cards, outside all nested quote blocks.
    #[prop_or_default]
    pub(super) corner: Option<Html>,
    /// Mount one authored line as phrasing content rather than a body block.
    #[prop_or_default]
    pub(super) inline: bool,
}

const MARKDOWN_BODY_PORTALS: &str = "markdown-body-portals";

struct Slot {
    index: usize,
    target: web_sys::Element,
    key: yew::virtual_dom::Key,
}
struct MathSlot {
    source: AttrValue,
    display: bool,
    target: web_sys::Element,
    key: yew::virtual_dom::Key,
}

struct QuoteAgeSlot {
    authored: i64,
    href: Option<String>,
    target: web_sys::Element,
    key: yew::virtual_dom::Key,
}

/// A steering quote's signoff, whose task badges follow its age and name.
struct QuoteTaskSlot {
    steering: String,
    target: web_sys::Element,
    key: yew::virtual_dom::Key,
}

struct SignoffSlot {
    target: web_sys::Element,
    key: yew::virtual_dom::Key,
}
struct ControlSlot {
    index: usize,
    control: SpeechControl,
    target: web_sys::Element,
    key: yew::virtual_dom::Key,
}

fn collect_slots(
    host: &web_sys::Element,
    selector: &str,
    attribute: &str,
    count: usize,
    previous: &[Slot],
    counter: &Cell<u64>,
) -> Vec<Slot> {
    let Ok(nodes) = host.query_selector_all(selector) else {
        return Vec::new();
    };
    let mut slots = Vec::with_capacity(nodes.length() as usize);
    for position in 0..nodes.length() {
        let Some(target) = nodes
            .item(position)
            .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
        else {
            continue;
        };
        let Some(index) = target
            .get_attribute(attribute)
            .and_then(|index| index.parse::<usize>().ok())
            .filter(|index| *index < count)
        else {
            continue;
        };
        // A changed host needs a new portal event root; unchanged targets keep
        // their component-local state and DOM identity.
        let key = previous
            .iter()
            .find(|slot| slot.target == target)
            .map(|slot| slot.key.clone())
            .unwrap_or_else(|| {
                counter.set(counter.get().wrapping_add(1));
                counter.get().into()
            });
        slots.push(Slot { index, target, key });
    }
    slots
}
fn collect_math_slots(
    host: &web_sys::Element,
    previous: &[MathSlot],
    counter: &Cell<u64>,
) -> Vec<MathSlot> {
    let Ok(nodes) = host.query_selector_all(".math.math-inline, .math.math-display") else {
        return Vec::new();
    };
    let mut slots = Vec::with_capacity(nodes.length() as usize);
    for position in 0..nodes.length() {
        let Some(target) = nodes
            .item(position)
            .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
        else {
            continue;
        };
        if let Some(slot) = previous.iter().find(|slot| slot.target == target) {
            slots.push(MathSlot {
                source: slot.source.clone(),
                display: slot.display,
                target,
                key: slot.key.clone(),
            });
            continue;
        }
        let Some(source) = target.text_content().filter(|source| !source.is_empty()) else {
            continue;
        };
        let display = target.get_attribute("class").is_some_and(|classes| {
            classes
                .split_whitespace()
                .any(|class| class == "math-display")
        });
        target.set_text_content(None);
        counter.set(counter.get().wrapping_add(1));
        slots.push(MathSlot {
            source: source.into(),
            display,
            target,
            key: counter.get().into(),
        });
    }
    slots
}

fn collect_quote_age_slots(
    host: &web_sys::Element,
    previous: &[QuoteAgeSlot],
    counter: &Cell<u64>,
) -> Vec<QuoteAgeSlot> {
    let Ok(nodes) = host.query_selector_all(".message-quote-age-slot[data-quote-age-authored]")
    else {
        return Vec::new();
    };
    let mut slots = Vec::with_capacity(nodes.length() as usize);
    for position in 0..nodes.length() {
        let Some(target) = nodes
            .item(position)
            .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
        else {
            continue;
        };
        let Some(authored) = target
            .get_attribute("data-quote-age-authored")
            .and_then(|authored| authored.parse::<i64>().ok())
        else {
            continue;
        };
        let href = target
            .get_attribute("data-quote-source")
            .filter(|source| !source.is_empty())
            .map(|source| crate::view::quote_source_href(&source));
        let key = previous
            .iter()
            .find(|slot| slot.target == target)
            .map(|slot| slot.key.clone())
            .unwrap_or_else(|| {
                counter.set(counter.get().wrapping_add(1));
                counter.get().into()
            });
        slots.push(QuoteAgeSlot {
            authored,
            href,
            target,
            key,
        });
    }
    slots
}

fn collect_quote_task_slots(
    host: &web_sys::Element,
    previous: &[QuoteTaskSlot],
    counter: &Cell<u64>,
) -> Vec<QuoteTaskSlot> {
    let Ok(nodes) = host.query_selector_all(".message-quote-task-tail[data-quote-tasks]") else {
        return Vec::new();
    };
    let mut slots = Vec::with_capacity(nodes.length() as usize);
    for position in 0..nodes.length() {
        let Some(target) = nodes
            .item(position)
            .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
        else {
            continue;
        };
        let Some(steering) = target.get_attribute("data-quote-tasks") else {
            continue;
        };
        let key = previous
            .iter()
            .find(|slot| slot.target == target)
            .map(|slot| slot.key.clone())
            .unwrap_or_else(|| {
                counter.set(counter.get().wrapping_add(1));
                counter.get().into()
            });
        slots.push(QuoteTaskSlot {
            steering,
            target,
            key,
        });
    }
    slots
}

fn collect_signoff_slots(
    host: &web_sys::Element,
    previous: &[SignoffSlot],
    counter: &Cell<u64>,
) -> Vec<SignoffSlot> {
    let Ok(nodes) = host.query_selector_all(".message-signoff-slot[data-signoff-slot]") else {
        return Vec::new();
    };
    let mut slots = Vec::with_capacity(nodes.length() as usize);
    for position in 0..nodes.length() {
        let Some(target) = nodes
            .item(position)
            .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
        else {
            continue;
        };
        let key = previous
            .iter()
            .find(|slot| slot.target == target)
            .map(|slot| slot.key.clone())
            .unwrap_or_else(|| {
                counter.set(counter.get().wrapping_add(1));
                counter.get().into()
            });
        slots.push(SignoffSlot { target, key });
    }
    slots
}
fn collect_control_slots(
    host: &web_sys::Element,
    controls: &[MarkdownControl],
    previous: &[ControlSlot],
    counter: &Cell<u64>,
) -> Vec<ControlSlot> {
    let Ok(nodes) = host.query_selector_all(".message-controls-slot[data-message-controls]") else {
        return Vec::new();
    };
    let mut slots = Vec::<ControlSlot>::with_capacity(nodes.length() as usize);
    for position in 0..nodes.length() {
        let Some(target) = nodes
            .item(position)
            .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
        else {
            continue;
        };
        let Some(index) = target
            .get_attribute("data-message-controls")
            .and_then(|target| controls.iter().position(|control| control.target == target))
        else {
            continue;
        };
        // Raw inline styles are refused by the app CSP. The renderer owns this
        // numeric metadata; applying it through CSSOM keeps CSS layout generic.
        let style = target
            .dyn_ref::<web_sys::HtmlElement>()
            .expect("control slots are HTML spans")
            .style();
        let depth = target.get_attribute("data-message-quote-depth");
        let depth = depth
            .as_deref()
            .filter(|value| value.parse::<usize>().is_ok())
            .unwrap_or("0");
        style
            .set_property("--message-quote-depth", depth)
            .expect("quote depth is a valid CSS integer");
        let key = previous
            .iter()
            .find(|slot| slot.target == target)
            .map(|slot| slot.key.clone())
            .unwrap_or_else(|| {
                counter.set(counter.get().wrapping_add(1));
                counter.get().into()
            });
        let occurrence = slots.iter().filter(|slot| slot.index == index).count();
        slots.push(ControlSlot {
            index,
            control: SpeechControl::new(index, occurrence),
            target,
            key,
        });
    }
    slots
}

#[derive(Properties, PartialEq)]
struct QuoteTaskChipsProps {
    handles: Vec<wire::TaskHandle>,
}

#[function_component(QuoteTaskChips)]
fn quote_task_chips(props: &QuoteTaskChipsProps) -> Html {
    html! {
        <>
        {for props.handles.iter().map(|handle| html! {
            <span
                class="message-task-chip"
                role="button"
                tabindex="0"
                data-task-handle={handle.as_str().to_owned()}
                aria-label={format!("Open task {}", handle.as_str())}
                title={format!("Open task {}", handle.as_str())}
            >
                <span class="message-task-chip-label">{handle.as_str()}</span>
            </span>
        })}
        </>
    }
}

fn signoff_portals(signoff: Option<&Html>, slots: &[SignoffSlot]) -> Vec<Html> {
    let Some(signoff) = signoff else {
        return Vec::new();
    };
    slots
        .iter()
        .map(|slot| {
            yew::create_portal(
                html! { <span key={slot.key.clone()} class="message-signoff">{signoff.clone()}</span> },
                slot.target.clone(),
            )
        })
        .collect()
}
/// Producer-authored diagram, math, quote-age, quote-task, attribution, and
/// control slots inside this card are portal targets. Raw user HTML still
/// passes through the shared Markdown sanitizer; only parser- or resident-owned
/// nodes can name these slot kinds. Board work fills trailing task slots in
/// place, so a quote gaining task badges never replaces the card's raw nodes or
/// reloads its images.
#[function_component(MarkdownBody)]
pub(super) fn markdown_body(props: &MarkdownBodyProps) -> Html {
    let host = use_node_ref();
    super::watchband::use_markdown_image_resolution_events(host.clone(), props.html.clone());
    let diagram_slots = use_state(Vec::<Slot>::new);
    let math_slots = use_state(Vec::<MathSlot>::new);
    let quote_age_slots = use_state(Vec::<QuoteAgeSlot>::new);
    let quote_task_slots = use_state(Vec::<QuoteTaskSlot>::new);
    let signoff_slots = use_state(Vec::<SignoffSlot>::new);
    let control_slots = use_state(Vec::<ControlSlot>::new);
    let counter = use_memo((), |()| Cell::new(0_u64));
    use_effect_with(
        (
            props.html.clone(),
            props.sources.len(),
            props.signoff.is_some(),
            props.controls.clone(),
        ),
        {
            let host = host.clone();
            let diagram_slots = diagram_slots.clone();
            let math_slots = math_slots.clone();
            let quote_age_slots = quote_age_slots.clone();
            let quote_task_slots = quote_task_slots.clone();
            let signoff_slots = signoff_slots.clone();
            let control_slots = control_slots.clone();
            let counter = counter.clone();
            move |(_, source_count, has_signoff, controls)| {
                let Some(host) = host.cast::<web_sys::Element>() else {
                    return;
                };
                diagram_slots.set(collect_slots(
                    &host,
                    "[data-mermaid-slot]",
                    "data-mermaid-slot",
                    *source_count,
                    &diagram_slots,
                    &counter,
                ));
                math_slots.set(collect_math_slots(&host, &math_slots, &counter));
                quote_age_slots.set(collect_quote_age_slots(&host, &quote_age_slots, &counter));
                quote_task_slots.set(collect_quote_task_slots(&host, &quote_task_slots, &counter));
                signoff_slots.set(if *has_signoff {
                    collect_signoff_slots(&host, &signoff_slots, &counter)
                } else {
                    Vec::new()
                });
                control_slots.set(collect_control_slots(
                    &host,
                    controls,
                    &control_slots,
                    &counter,
                ));
            }
        },
    );
    let signoffs = signoff_portals(props.signoff.as_ref(), &signoff_slots);
    // Keep the portal list under a stable VDOM parent. Mixing keyed portals
    // directly with VRaw replaces the raw nodes when the first portal mounts.
    // Key each control's portal root; an inner key cannot distinguish sibling
    // providers.
    let content = html! {
        <>
            {for props.corner.clone()}
            {Html::from_html_unchecked(props.html.clone())}
            if !props.sources.is_empty() || !math_slots.is_empty() || !quote_age_slots.is_empty() || !quote_task_slots.is_empty() || !signoff_slots.is_empty() || !control_slots.is_empty() {
            <span class={MARKDOWN_BODY_PORTALS} hidden=true>
            {for diagram_slots.iter().filter_map(|slot| {
                let source = props.sources.get(slot.index)?.clone();
                Some(yew::create_portal(html! {
                    <MermaidImage key={slot.key.clone()} {source} final_answer={props.final_answer} />
                }, slot.target.clone()))
            })}
            {for math_slots.iter().map(|slot| yew::create_portal(html! {
                <MathImage
                    key={slot.key.clone()}
                    source={slot.source.clone()}
                    display={slot.display}
                />
            }, slot.target.clone()))}
            {for quote_age_slots.iter().map(|slot| {
                let href = slot.href.clone().map(AttrValue::from);
                let label = AttrValue::from("Link to quoted source");
                yew::create_portal(html! {
                    <super::clock::Age
                        key={slot.key.clone()}
                        authored={slot.authored}
                        class={classes!("relative-age", "message-quote-age")}
                        {href}
                        {label}
                    />
                }, slot.target.clone())
            })}
            {for quote_task_slots.iter().filter_map(|slot| {
                let handles = props.quote_tasks.get(&slot.steering)?.clone();
                Some(yew::create_portal(
                    html! { <QuoteTaskChips key={slot.key.clone()} {handles} /> },
                    slot.target.clone(),
                ))
            })}
            {for control_slots.iter().filter_map(|slot| {
                let control = props.controls.get(slot.index)?;
                let context = slot.control;
                Some(yew::create_portal(html! {
                    <ContextProvider<SpeechControl> key={slot.key.clone()} {context}>
                        <span>{control.content.clone()}</span>
                    </ContextProvider<SpeechControl>>
                }, slot.target.clone()))
            })}
            {for signoffs}
            </span>
            }
        </>
    };
    markdown_host(props.inline, host, content)
}

/// The element holding rendered Markdown: a phrasing span for one authored
/// line, otherwise the message body block.
fn markdown_host(inline: bool, host: NodeRef, content: Html) -> Html {
    if inline {
        return html! { <span ref={host} class="markdown-body markdown-line">{content}</span> };
    }
    html! {
        <div
            ref={host}
            class="message-body markdown-body"
            onclick={Callback::from(|event: MouseEvent| {
                super::card_target::remember_quote_return(&event);
            })}
        >
            {content}
        </div>
    }
}

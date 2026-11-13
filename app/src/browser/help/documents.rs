//! Local documentation navigation stays inside Help, not browser history.
use std::rc::Rc;

use wasm_bindgen::JsCast as _;
use wire::intent::{RackAnswer, RackContent, RackEntryKind};
use yew::prelude::*;

use crate::documentation::{Documents, Link, Target, Visit, is_markdown_content_type, resolve};

mod controller;
use controller::Controller;

#[derive(Properties, PartialEq)]
pub(super) struct DocumentationProps {
    pub(super) active: bool,
    pub(super) connection: super::super::Connection,
    pub(super) on_action: Callback<crate::client_action::ClientAction, bool>,
}

#[function_component(Documentation)]
pub(super) fn documentation(props: &DocumentationProps) -> Html {
    let state = use_mut_ref(Documents::default);
    let redraw = use_force_update();
    let content = use_node_ref();
    let previous = use_mut_ref(|| props.connection.clone());
    let controller = Controller {
        state: state.clone(),
        redraw,
        on_action: props.on_action.clone(),
        online: props.connection.is_online(),
    };
    {
        let controller = controller.clone();
        use_effect_with(
            (props.active, props.connection.clone()),
            move |(active, connection)| {
                if *previous.borrow() != *connection {
                    controller.state.borrow_mut().reset();
                    *previous.borrow_mut() = connection.clone();
                }
                if *active {
                    controller.retry();
                }
                || ()
            },
        );
    }
    {
        let state = state.clone();
        use_effect_with((), move |_| move || state.borrow_mut().reset());
    }
    let model = state.borrow();
    let busy = model.pending.is_some() || model.media_pending != 0;
    let target = model.target();
    {
        let content = content.clone();
        let fragment = target.fragment.clone();
        use_effect_with(
            (
                model.page.clone(),
                fragment,
                props.active,
                model.media_pending,
            ),
            move |(_, fragment, active, _)| {
                if *active {
                    prepare_links(&content, fragment);
                }
                || ()
            },
        );
    }
    let retry = {
        let controller = controller.clone();
        Callback::from(move |_| controller.retry())
    };
    html! {
        <section class="app-documents" hidden={!props.active} aria-label="Documentation" aria-busy={busy.to_string()}>
            {navigation(&controller, &model)}
            if let Some(issue) = &model.issue {
                <div class="app-documents-issue" role="alert">
                    <span>{issue}</span><button type="button" class="app-compact-control" onclick={retry}>{"Retry"}</button>
                </div>
            }
            if busy { <p class="app-empty-state" role="status">{"Loading documentation…"}</p> }
            <div ref={content} class="app-documents-content" onclick={link_click(controller.clone())}
                onauxclick={link_click(controller.clone())}>
                if let Some(page) = &model.page { {render_page(page, &model, &controller)} }
            </div>
        </section>
    }
}

fn navigation(controller: &Controller, state: &Documents) -> Html {
    let jump = |position: usize| {
        let controller = controller.clone();
        let target = state.history.get(position).cloned();
        Callback::from(move |_| {
            if let Some(target) = &target {
                controller.load(target.clone(), Visit::History(position));
            }
        })
    };
    let target = state.target();
    let mut crumbs = vec![(String::new(), "Documentation".to_owned())];
    let mut path = String::new();
    for part in target.path.split('/').filter(|part| !part.is_empty()) {
        if !path.is_empty() {
            path.push('/');
        }
        path.push_str(part);
        crumbs.push((path.clone(), part.to_owned()));
    }
    html! {
        <nav class="app-documents-navigation" aria-label="Documentation navigation">
            <div class="app-documents-history">
                <button type="button" class="app-compact-control" disabled={state.position == 0}
                    onclick={jump(state.position.saturating_sub(1))}>{"Back"}</button>
                <button type="button" class="app-compact-control" disabled={state.position + 1 >= state.history.len()}
                    onclick={jump(state.position + 1)}>{"Forward"}</button>
            </div>
            <ol class="app-documents-breadcrumbs" aria-label="Breadcrumb">
                {for crumbs.into_iter().map(|(path, label)| {
                    let current = path == target.path;
                    html! { <li><button type="button" class="app-compact-control"
                        aria-current={current.then_some("page")} disabled={current}
                        onclick={destination(controller, Target { path, fragment: String::new() })}>{label}</button></li> }
                })}
            </ol>
        </nav>
    }
}

fn destination(controller: &Controller, target: Target) -> Callback<MouseEvent> {
    let controller = controller.clone();
    Callback::from(move |_| controller.load(target.clone(), Visit::Push))
}

fn render_page(page: &RackAnswer, state: &Documents, controller: &Controller) -> Html {
    match &page.content {
        RackContent::Directory { entries } => html! {
            <ul class="app-documents-entries">
                {for entries.iter().map(|entry| html! {
                    <li><button type="button" class="app-help-place" disabled={entry.kind == RackEntryKind::Unsupported}
                        onclick={destination(controller, Target { path: entry.path.clone(), fragment: String::new() })}>
                        <span>{entry.path.rsplit('/').next().unwrap_or(&entry.path)}</span>
                        <small>{match entry.kind { RackEntryKind::Directory => "Directory", RackEntryKind::Document => "Document",
                            RackEntryKind::Image => "Image", RackEntryKind::Unsupported => "Unavailable" }}</small>
                    </button></li>
                })}
            </ul>
        },
        RackContent::Image { media } => {
            html! { <img class="app-documents-image" src={media.data_url.clone()} alt={media.name.clone()} /> }
        }
        RackContent::Text { content_type, text } if is_markdown_content_type(content_type) => {
            let rendered =
                crate::composer_markdown::document::render(text, &|href, image| match resolve(
                    &page.path, false, href,
                ) {
                    Link::Internal(target) if image => state
                        .cached(&page.identity, &target.path)
                        .and_then(|answer| match &answer.content {
                            RackContent::Image { media } => Some(media.data_url.clone()),
                            _ => None,
                        })
                        .unwrap_or_default(),
                    Link::Internal(target) => target.href(),
                    Link::External(url) if !image => url,
                    _ => String::new(),
                });
            html! { <super::super::mermaid::MarkdownBody html={AttrValue::from(rendered.html)}
            sources={Rc::new(rendered.diagrams.into_iter().map(AttrValue::from).collect::<Vec<_>>())}
            final_answer=true quote_tasks={Rc::default()} /> }
        }
        RackContent::Text { text, .. } => html! { <pre class="app-documents-text">{text}</pre> },
    }
}

fn link_click(controller: Controller) -> Callback<MouseEvent> {
    Callback::from(move |event: MouseEvent| {
        let Some(anchor) = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
            .and_then(|element| element.closest("a").ok().flatten())
        else {
            return;
        };
        let href = anchor.get_attribute("href").unwrap_or_default();
        event.prevent_default();
        event.stop_propagation();
        if href.starts_with("#docs/") {
            if let Link::Internal(target) = resolve("", true, &href) {
                controller.load(target, Visit::Push);
            }
        } else if let Link::External(url) = resolve("", true, &href)
            && let Some(window) = web_sys::window()
        {
            let _ =
                window.open_with_url_and_target_and_features(&url, "_blank", "noopener,noreferrer");
        }
    })
}

fn prepare_links(content: &NodeRef, fragment: &str) {
    let Some(element) = content.cast::<web_sys::Element>() else {
        return;
    };
    if let Ok(anchors) = element.query_selector_all("a") {
        for index in 0..anchors.length() {
            let Some(anchor) = anchors
                .item(index)
                .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
            else {
                continue;
            };
            let href = anchor.get_attribute("href").unwrap_or_default();
            if matches!(resolve("", true, &href), Link::External(_)) {
                let _ = anchor.set_attribute("target", "_blank");
                let _ = anchor.set_attribute("rel", "noopener noreferrer");
            } else if !href.starts_with("#docs/") {
                let _ = anchor.remove_attribute("href");
            }
        }
    }
    if !fragment.is_empty()
        && let Ok(headings) = element.query_selector_all("h1,h2,h3,h4,h5,h6,[id]")
    {
        for index in 0..headings.length() {
            let Some(heading) = headings
                .item(index)
                .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
            else {
                continue;
            };
            let slug = heading
                .text_content()
                .unwrap_or_default()
                .to_lowercase()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join("-");
            if heading.id() == fragment || slug == fragment {
                heading.scroll_into_view();
                break;
            }
        }
    }
}

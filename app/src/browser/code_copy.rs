//! The corner control on every rendered code block copies that block's code.
//!
//! Markdown reaches the page as raw HTML, so one document listener serves every
//! surface that renders it instead of a listener per block.

use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::*;
use yew::prelude::*;

use super::{clipboard, toast};

/// The control the Markdown renderer places in each `.message-code` block.
const CONTROL: &str = "button[data-code-copy]";
/// The block that owns one control and exactly one `pre`.
const BLOCK: &str = ".message-code";

/// Copy a code block's text when its corner control is activated, and report
/// how the copy settled on the root toast.
#[hook]
pub(super) fn use_code_copy() {
    let toaster = toast::use_toaster();
    use_effect_with(toaster, |toaster| {
        let document = web_sys::window()
            .expect("app window")
            .document()
            .expect("app document");
        let toaster = toaster.clone();
        let copy = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
            let Some(code) = event
                .target()
                .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
                .and_then(|target| target.closest(CONTROL).ok().flatten())
                .and_then(|control| control.closest(BLOCK).ok().flatten())
                .and_then(|block| block.query_selector(":scope > pre").ok().flatten())
                .and_then(|pre| pre.text_content())
            else {
                return;
            };
            event.prevent_default();
            let toaster = toaster.clone();
            // The clipboard write starts inside the trusted click.
            clipboard::copy_text(&code, move |result| {
                toaster.settled(match result {
                    Ok(()) => "Copied code".to_owned(),
                    Err(error) => format!("Copy failed: {error}"),
                });
            });
        });
        document
            .add_event_listener_with_callback("click", copy.as_ref().unchecked_ref())
            .expect("the app document accepts code copy");
        move || {
            let _ = document
                .remove_event_listener_with_callback("click", copy.as_ref().unchecked_ref());
        }
    });
}

#[derive(Properties, PartialEq)]
pub(in crate::browser) struct SourceCopyProps {
    pub source: AttrValue,
}

/// Diagram portals share the code-block control, but own their source in Rust.
#[function_component(SourceCopy)]
pub(in crate::browser) fn source_copy(props: &SourceCopyProps) -> Html {
    let toaster = toast::use_toaster();
    let copy = use_callback((props.source.clone(), toaster), |(), (source, toaster)| {
        let toaster = toaster.clone();
        clipboard::copy_text(source, move |result| {
            toaster.settled(match result {
                Ok(()) => "Copied Mermaid source".to_owned(),
                Err(error) => format!("Copy failed: {error}"),
            });
        });
    });
    let button = super::image_viewer::use_button(copy);
    html! {
        <button ref={button} type="button"
            class="app-card-action app-card-action--copy message-code-copy"
            title="Copy Mermaid source" aria-label="Copy Mermaid source">
            <svg class="app-card-action__icon" width="15" height="15" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
                <path d="M9 9h10v10H9V9Zm1.75 1.75v6.5h6.5v-6.5h-6.5ZM5 5h10.5v1.75H6.75v8.75H5V5Z" fill="currentColor" fill-rule="evenodd" />
            </svg>
        </button>
    }
}

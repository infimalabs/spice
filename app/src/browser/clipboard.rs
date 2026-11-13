//! The app's one route to the platform clipboard, for text and for images,
//! and the one way a browser refusal becomes operator text.
//!
//! The async Clipboard API is reached reflectively: `web-sys` gates it behind
//! an unstable-API flag that would have to be set for the whole build.
//! Browsers expose it, and `ClipboardItem`, only to secure contexts, so a
//! app opened over plain http by a LAN address has neither, even though the
//! same resident on `localhost` keeps both.

use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::*;

/// Operator text for a JS error: its `message`, else the value itself when it
/// is a string.
pub(super) fn error_text(error: &JsValue) -> String {
    js_sys::Reflect::get(error, &JsValue::from_str("message"))
        .ok()
        .and_then(|message| message.as_string())
        .or_else(|| error.as_string())
        .unwrap_or_else(|| "the browser gave no reason".to_owned())
}

fn method(object: &JsValue, name: &str) -> Option<js_sys::Function> {
    js_sys::Reflect::get(object, &JsValue::from_str(name))
        .ok()?
        .dyn_into()
        .ok()
}

/// `navigator.clipboard` and its `name` method, when this page has them.
fn clipboard(name: &str) -> Option<(JsValue, js_sys::Function)> {
    let window = web_sys::window()?;
    let navigator = js_sys::Reflect::get(&window, &JsValue::from_str("navigator")).ok()?;
    let clipboard = js_sys::Reflect::get(&navigator, &JsValue::from_str("clipboard")).ok()?;
    let write = method(&clipboard, name)?;
    Some((clipboard, write))
}

/// Put `text` on the clipboard and report how that settled.
///
/// A page without the Clipboard API copies through a selection instead. A
/// refusal from either path is reported, not retried another way.
pub(super) fn copy_text(text: &str, settled: impl FnOnce(Result<(), String>) + 'static) {
    let promise = clipboard("writeText").and_then(|(clipboard, write)| {
        write
            .call1(&clipboard, &JsValue::from_str(text))
            .ok()?
            .dyn_into::<js_sys::Promise>()
            .ok()
    });
    let Some(promise) = promise else {
        settled(select_and_copy(text));
        return;
    };
    wasm_bindgen_futures::spawn_local(async move {
        settled(
            wasm_bindgen_futures::JsFuture::from(promise)
                .await
                .map(drop)
                .map_err(|error| error_text(&error)),
        );
    });
}

/// Copy `text` through a transient selection, keeping the caller's focus.
fn select_and_copy(text: &str) -> Result<(), String> {
    let document = web_sys::window()
        .and_then(|window| window.document())
        .ok_or("no document to copy from")?;
    let body = document.body().ok_or("no document body to copy from")?;
    let area = document
        .create_element("textarea")
        .ok()
        .and_then(|element| element.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
        .ok_or("the browser refused a copy buffer")?;
    area.set_value(text);
    area.set_read_only(true);
    area.style()
        .set_property("position", "fixed")
        .and_then(|()| area.style().set_property("opacity", "0"))
        .map_err(|_| "the browser refused a copy buffer")?;
    let focused = document
        .active_element()
        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok());
    body.append_child(&area)
        .map_err(|_| "the browser refused a copy buffer")?;
    area.select();
    let copied = method(&document, "execCommand")
        .and_then(|exec| exec.call1(&document, &JsValue::from_str("copy")).ok())
        .and_then(|result| result.as_bool())
        .unwrap_or(false);
    area.remove();
    if let Some(focused) = focused {
        let _ = focused.focus();
    }
    if copied {
        Ok(())
    } else {
        Err("the browser refused to copy".to_owned())
    }
}

/// The clipboard, its `write` method and the `ClipboardItem` constructor.
fn image_writer() -> Option<(JsValue, js_sys::Function, js_sys::Function)> {
    let (clipboard, write) = clipboard("write")?;
    let window = web_sys::window()?;
    let constructor = method(&window, "ClipboardItem")?;
    Some((clipboard, write, constructor))
}

/// Why this page cannot put an image on the clipboard, or `None` when it can.
pub(super) fn image_refusal() -> Option<String> {
    image_writer().is_none().then(image_unavailable)
}

fn image_unavailable() -> String {
    const UNSUPPORTED: &str =
        "This browser cannot put images on the clipboard; Download still works.";
    let Some(window) = web_sys::window() else {
        return UNSUPPORTED.to_owned();
    };
    let secure = js_sys::Reflect::get(&window, &JsValue::from_str("isSecureContext"))
        .ok()
        .and_then(|value| value.as_bool())
        .unwrap_or(true);
    if secure {
        UNSUPPORTED.to_owned()
    } else {
        let origin = window.location().origin().unwrap_or_default();
        format!(
            "Image copy needs a secure page, and {origin} is plain http. Browsers allow it on https or localhost; Download still works."
        )
    }
}

/// Write the PNG that `png` resolves to. The write starts now, so a caller
/// inside a trusted click keeps that activation while the image converts.
pub(super) fn write_png(png: js_sys::Promise) -> Result<js_sys::Promise, JsValue> {
    let Some((clipboard, write, constructor)) = image_writer() else {
        return Err(JsValue::from_str(&image_unavailable()));
    };
    let content = js_sys::Object::new();
    js_sys::Reflect::set(&content, &JsValue::from_str("image/png"), &png)?;
    let item = js_sys::Reflect::construct(&constructor, &js_sys::Array::of1(&content))?;
    write
        .call1(&clipboard, &js_sys::Array::of1(&item))?
        .dyn_into::<js_sys::Promise>()
}

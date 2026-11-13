use super::*;
use crate::browser::clipboard;
use base64::Engine;
use futures_channel::oneshot;
use wasm_bindgen_futures::future_to_promise;

pub(super) fn readable(source: &str) -> bool {
    if source
        .get(..11)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:image/"))
    {
        return true;
    }
    web_sys::Url::new(source).ok().is_some_and(|url| {
        matches!(url.protocol().as_str(), "http:" | "https:")
            && web_sys::window()
                .and_then(|window| window.location().origin().ok())
                .is_some_and(|origin| origin == url.origin())
    })
}

/// Put `source` on the clipboard as PNG, starting the write before the image
/// converts.
pub(super) fn copy(source: AttrValue) -> Result<js_sys::Promise, JsValue> {
    clipboard::write_png(future_to_promise(async move {
        Ok(png_image(&source).await?.into())
    }))
}

pub(super) fn download(source: AttrValue, name: AttrValue) -> js_sys::Promise {
    future_to_promise(async move {
        let blob = original(&source).await?;
        let document = web_sys::window()
            .and_then(|window| window.document())
            .ok_or_else(|| JsValue::from_str("No document"))?;
        let anchor = document
            .create_element("a")?
            .dyn_into::<web_sys::HtmlAnchorElement>()?;
        let url = web_sys::Url::create_object_url_with_blob(&blob)?;
        anchor.set_href(&url);
        anchor.set_download(&filename(&name, &blob.type_()));
        // Keep the connected download target inside the modal's non-inert subtree.
        let parent = document
            .query_selector("dialog.image-lightbox[open]")?
            .ok_or_else(|| JsValue::from_str("Image viewer was closed"));
        let result = parent.and_then(|parent| {
            parent.append_child(&anchor)?;
            anchor.click();
            anchor.remove();
            Ok(JsValue::UNDEFINED)
        });
        if result.is_ok() {
            // Reuse the existing grace period: the browser may consume the URL after click returns.
            gloo_timers::callback::Timeout::new(60_000, move || {
                let _ = web_sys::Url::revoke_object_url(&url);
            })
            .forget();
        } else {
            let _ = web_sys::Url::revoke_object_url(&url);
        }
        result
    })
}

fn filename(name: &str, mime: &str) -> String {
    if !name.is_empty() {
        return name.to_owned();
    }
    let extension = match mime {
        "image/svg+xml" => "svg",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/avif" => "avif",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        _ => "img",
    };
    format!("image.{extension}")
}

async fn original(source: &str) -> Result<web_sys::Blob, JsValue> {
    if !readable(source) {
        return Err(JsValue::from_str("Image origin does not permit export"));
    }
    if let Some(data) = source.strip_prefix("data:") {
        let (metadata, payload) = data
            .split_once(',')
            .ok_or_else(|| JsValue::from_str("Invalid image data URL"))?;
        let mime = metadata
            .split(';')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let bytes = if metadata
            .rsplit(';')
            .next()
            .is_some_and(|encoding| encoding.eq_ignore_ascii_case("base64"))
        {
            base64::engine::general_purpose::STANDARD
                .decode(payload)
                .map_err(|error| JsValue::from_str(&error.to_string()))?
        } else {
            percent_bytes(payload)
        };
        let parts = js_sys::Array::new();
        parts.push(&js_sys::Uint8Array::from(bytes.as_slice()));
        let options = web_sys::BlobPropertyBag::new();
        options.set_type(&mime);
        return web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &options);
    }
    let response = gloo_net::http::Request::get(source)
        .send()
        .await
        .map_err(|error| JsValue::from_str(&error.to_string()))?;
    if !response.ok() {
        return Err(JsValue::from_str(&format!(
            "Image request returned HTTP {}",
            response.status()
        )));
    }
    let response: web_sys::Response = response.into();
    JsFuture::from(response.blob()?).await?.dyn_into()
}

fn percent_bytes(payload: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(payload.len());
    let mut input = payload.bytes().peekable();
    while let Some(byte) = input.next() {
        if byte == b'%' {
            let mut escaped = input.clone();
            if let Some(high) = escaped.next().and_then(|v| char::from(v).to_digit(16))
                && let Some(low) = escaped.next().and_then(|v| char::from(v).to_digit(16))
            {
                bytes.push((high * 16 + low) as u8);
                input = escaped;
                continue;
            }
        }
        bytes.push(byte);
    }
    bytes
}

async fn png_image(source: &str) -> Result<web_sys::Blob, JsValue> {
    let blob = original(source).await?;
    if blob.type_() == "image/png" {
        return Ok(blob);
    }
    // Decode an origin-clean data image; never insert SVG nodes or a blob document.
    let reader = web_sys::FileReader::new()?;
    let (sent, received) = oneshot::channel();
    let loaded = Closure::once(move |_: web_sys::Event| {
        let _ = sent.send(());
    });
    reader.set_onloadend(Some(loaded.as_ref().unchecked_ref()));
    reader.read_as_data_url(&blob)?;
    received
        .await
        .map_err(|_| JsValue::from_str("Image read interrupted"))?;
    reader.set_onloadend(None);
    let image = web_sys::HtmlImageElement::new()?;
    image.set_src(
        &reader
            .result()?
            .as_string()
            .ok_or_else(|| JsValue::from_str("Image read failed"))?,
    );
    JsFuture::from(image.decode()).await?;
    let canvas = web_sys::window()
        .and_then(|window| window.document())
        .ok_or_else(|| JsValue::from_str("No document"))?
        .create_element("canvas")?
        .dyn_into::<web_sys::HtmlCanvasElement>()?;
    canvas.set_width(image.natural_width());
    canvas.set_height(image.natural_height());
    let context = canvas
        .get_context("2d")?
        .ok_or_else(|| JsValue::from_str("Image conversion unavailable"))?
        .dyn_into::<web_sys::CanvasRenderingContext2d>()?;
    context.draw_image_with_html_image_element(&image, 0.0, 0.0)?;
    let (sent, received) = oneshot::channel();
    let encoded = Closure::once(move |blob: Option<web_sys::Blob>| {
        let _ = sent.send(blob);
    });
    canvas.to_blob(encoded.as_ref().unchecked_ref())?;
    received
        .await
        .map_err(|_| JsValue::from_str("Image encoding interrupted"))?
        .ok_or_else(|| JsValue::from_str("Browser could not encode this image as PNG"))
}

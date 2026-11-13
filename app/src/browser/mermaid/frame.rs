//! Only this boundary talks to the opaque renderer. SVG is decoded as an image;
//! neither its elements nor its styles enter the app document.

use std::rc::Rc;

use base64::Engine as _;
use js_sys::{Array, Object, Reflect};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use yew::AttrValue;

use super::service::{Job, Render, Service, Shared};

const OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const ERROR_UNITS: u32 = 500;
const IMAGE_PREFIX: &str = "data:image/svg+xml;base64,";
const PROTOCOL: f64 = 3.0;

/// The renderer document of one app generation.
pub(super) fn document_route(generation: &str) -> String {
    format!("/_spice/app/{generation}/renderer-000.html")
}

pub(super) struct Frame {
    parent: web_sys::Window,
    element: web_sys::HtmlIFrameElement,
    child: web_sys::Window,
    listener: Closure<dyn FnMut(web_sys::MessageEvent)>,
    pub(super) ready: bool,
}

impl Frame {
    pub(super) fn open(shared: &Shared, generation: &str) -> Result<Self, JsValue> {
        let parent = web_sys::window().ok_or("No browser window")?;
        let document = parent.document().ok_or("No browser document")?;
        let element = document
            .create_element("iframe")?
            .dyn_into::<web_sys::HtmlIFrameElement>()?;
        element.set_attribute("sandbox", "allow-scripts")?;
        element.set_attribute("aria-hidden", "true")?;
        element.set_attribute("tabindex", "-1")?;
        element.set_attribute("title", "Isolated diagram renderer")?;
        element.set_attribute("data-app-renderer", generation)?;
        // Hidden but measurable. display:none breaks Mermaid's text geometry.
        for (name, value) in [
            ("position", "fixed"),
            ("left", "-10000px"),
            ("top", "0"),
            ("width", "1200px"),
            ("height", "800px"),
            ("visibility", "hidden"),
            ("pointer-events", "none"),
            ("border", "0"),
        ] {
            element.style().set_property(name, value)?;
        }
        element.set_src(&document_route(generation));
        let weak = Rc::downgrade(shared);
        let listener = Closure::new(move |event: web_sys::MessageEvent| {
            if let Some(shared) = weak.upgrade() {
                receive(&shared, event);
            }
        });
        parent.add_event_listener_with_callback("message", listener.as_ref().unchecked_ref())?;
        let body = document
            .body()
            .ok_or_else(|| JsValue::from_str("No browser body"));
        let appended = body.and_then(|body| body.append_child(&element).map(|_| ()));
        let child =
            appended.and_then(|()| element.content_window().ok_or("No renderer window".into()));
        match child {
            Ok(child) => Ok(Self {
                parent,
                element,
                child,
                listener,
                ready: false,
            }),
            Err(error) => {
                let _ = parent.remove_event_listener_with_callback(
                    "message",
                    listener.as_ref().unchecked_ref(),
                );
                element.remove();
                Err(error)
            }
        }
    }

    pub(super) fn send(&self, job: &Job, channel: &str) -> Result<(), JsValue> {
        let message = Object::new();
        for (name, value) in [
            ("type", JsValue::from_str("render")),
            ("protocol", JsValue::from_f64(PROTOCOL)),
            ("kind", JsValue::from_str(job.key.kind.as_str())),
            ("generation", JsValue::from_str(&job.key.generation)),
            ("channel", JsValue::from_str(channel)),
            (
                "requestId",
                JsValue::from_str(&format!("render-{}", job.id)),
            ),
            ("revision", JsValue::from_f64(f64::from(job.id))),
            ("theme", JsValue::from_str(job.key.theme.as_str())),
            ("source", JsValue::from_str(&job.key.source)),
        ] {
            Reflect::set(&message, &JsValue::from_str(name), &value)?;
        }
        let node_ids = Array::new();
        for id in job.key.node_ids.iter() {
            node_ids.push(&JsValue::from_str(id));
        }
        Reflect::set(&message, &JsValue::from_str("nodeIds"), &node_ids.into())?;
        // An opaque recipient requires '*'. Admission in both directions pins
        // the WindowProxy, generation and complete request envelope instead.
        self.child.post_message(&message, "*")
    }

    fn owns(&self, event: &web_sys::MessageEvent) -> bool {
        event.origin() == "null"
            && event
                .source()
                .is_some_and(|source| JsValue::from(source) == JsValue::from(self.child.clone()))
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        let _ = self
            .parent
            .remove_event_listener_with_callback("message", self.listener.as_ref().unchecked_ref());
        self.element.remove();
    }
}

pub(super) struct DecodedImage {
    image: web_sys::HtmlImageElement,
    render: Render,
}

impl Drop for DecodedImage {
    fn drop(&mut self) {
        // Also cancels an outstanding decode after a response deadline/unmount.
        let _ = self.image.remove_attribute("src");
    }
}

fn property(data: &JsValue, name: &str) -> JsValue {
    Reflect::get(data, &JsValue::from_str(name)).unwrap_or(JsValue::UNDEFINED)
}

fn matches(data: &JsValue, name: &str, expected: &str) -> bool {
    property(data, name) == JsValue::from_str(expected)
}

fn receive(shared: &Shared, event: web_sys::MessageEvent) {
    let service = shared.borrow();
    if !service
        .frame
        .as_ref()
        .is_some_and(|frame| frame.owns(&event))
    {
        return;
    }
    let data = event.data();
    if property(&data, "protocol") != JsValue::from_f64(PROTOCOL)
        || !matches(&data, "generation", &service.generation)
    {
        return;
    }
    if matches(&data, "type", "ready") {
        drop(service);
        Service::ready(shared);
        return;
    }
    let Some(job) = service.active.as_ref() else {
        return;
    };
    if job.decoding.is_some()
        || !matches(&data, "channel", &service.channel)
        || !matches(&data, "kind", job.key.kind.as_str())
        || !matches(&data, "requestId", &format!("render-{}", job.id))
        || property(&data, "revision") != JsValue::from_f64(f64::from(job.id))
        || !matches(&data, "theme", job.key.theme.as_str())
    {
        return;
    }
    let id = job.id;
    let node_ids = job.key.node_ids.clone();
    let interested = Service::has_subscribers(job);
    drop(service);
    if !interested {
        Service::finish(shared, id, Err("Superseded render".into()));
    } else if matches(&data, "type", "error") {
        let error = bounded_string(property(&data, "error"), ERROR_UNITS)
            .filter(|error| !error.is_empty())
            .unwrap_or_else(|| "Invalid renderer error response".to_owned());
        Service::finish(shared, id, Err(error.into()));
    } else if matches(&data, "type", "rendered") {
        let result = response_geometry(&data, &node_ids)
            .and_then(|geometry| image_url(property(&data, "svg")).map(|url| (url, geometry)));
        match result {
            Ok((url, geometry)) => decode(shared, id, url, geometry),
            Err(error) => Service::finish(shared, id, Err(error.into())),
        }
    }
}

fn bounded_string(value: JsValue, units: u32) -> Option<String> {
    let value = value.dyn_into::<js_sys::JsString>().ok()?;
    (value.length() <= units)
        .then(|| value.as_string())
        .flatten()
}

fn response_geometry(
    data: &JsValue,
    expected: &[String],
) -> Result<crate::mermaid_geometry::Geometry, &'static str> {
    let geometry = property(data, "geometry");
    let view_box = number_tuple(property(&geometry, "viewBox"))?;
    let values = property(&geometry, "nodes");
    if !Array::is_array(&values) {
        return Err("Invalid diagram node geometry");
    }
    let values = Array::from(&values);
    if values.length() as usize != expected.len()
        || values.length() as usize > crate::mermaid_geometry::NODE_LIMIT
    {
        return Err("Incomplete diagram node geometry");
    }
    let mut nodes = Vec::with_capacity(values.length() as usize);
    for value in values.iter() {
        let id = bounded_string(
            property(&value, "id"),
            crate::mermaid_geometry::NODE_ID_BYTES as u32,
        )
        .ok_or("Invalid diagram node geometry ID")?;
        let rect = number_tuple(property(&value, "rect"))?;
        nodes.push((id, rect));
    }
    crate::mermaid_geometry::admit(expected, view_box, nodes)
}

fn number_tuple(value: JsValue) -> Result<[f64; 4], &'static str> {
    if !Array::is_array(&value) {
        return Err("Invalid diagram geometry tuple");
    }
    let values = Array::from(&value);
    if values.length() != 4 {
        return Err("Invalid diagram geometry tuple");
    }
    let mut tuple = [0.0; 4];
    for (index, output) in tuple.iter_mut().enumerate() {
        *output = values
            .get(index as u32)
            .as_f64()
            .ok_or("Invalid diagram geometry number")?;
    }
    Ok(tuple)
}

fn image_url(value: JsValue) -> Result<AttrValue, &'static str> {
    // Bound in JS before copying into WASM, then enforce the UTF-8 wire bound.
    let svg = bounded_string(value, OUTPUT_BYTES as u32).ok_or("SVG exceeds output bound")?;
    if svg.len() > OUTPUT_BYTES || !svg.starts_with("<svg") {
        return Err("Invalid or oversized SVG renderer response");
    }
    let mut url = String::with_capacity(IMAGE_PREFIX.len() + svg.len().div_ceil(3) * 4);
    url.push_str(IMAGE_PREFIX);
    base64::engine::general_purpose::STANDARD.encode_string(svg.as_bytes(), &mut url);
    Ok(url.into())
}

fn decode(shared: &Shared, id: u32, url: AttrValue, geometry: crate::mermaid_geometry::Geometry) {
    let Ok(image) = web_sys::HtmlImageElement::new() else {
        Service::finish(shared, id, Err("Could not create an SVG image".into()));
        return;
    };
    image.set_src(&url);
    let decoded = image.decode();
    let render = Render {
        image: url,
        geometry: Rc::new(geometry),
    };
    shared
        .borrow_mut()
        .active
        .as_mut()
        .expect("admitted render")
        .decoding = Some(DecodedImage { image, render });
    let weak = Rc::downgrade(shared);
    spawn_local(async move {
        let result = JsFuture::from(decoded).await;
        let Some(shared) = weak.upgrade() else {
            return;
        };
        let service = shared.borrow();
        let Some(job) = service.active.as_ref().filter(|job| job.id == id) else {
            return;
        };
        let Some(decoded) = &job.decoding else {
            return;
        };
        let outcome = if result.is_ok() && decoded.image.natural_width() > 0 {
            Ok(decoded.render.clone())
        } else {
            Err("Browser could not decode the diagram SVG; source is still available".into())
        };
        drop(service);
        Service::finish(&shared, id, outcome);
    });
}

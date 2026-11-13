//! Image lifecycle and host width observation.

use super::*;

fn resolve_image(image: web_sys::HtmlImageElement) {
    let outcome = if image.natural_width() > 0 {
        "loaded"
    } else {
        "error"
    };
    set_image_resolution_state(&image, outcome);
    // The browser's decode is the only source of an image's natural size, so a
    // crystal divides its card again as each of its images resolves.
    if let Ok(Some(crystal)) = image.closest(".mosaic-crystal") {
        arrange_crystal(&crystal);
    }
}

pub(crate) fn scan_complete_images(host: &web_sys::Element) {
    let Ok(images) = host.query_selector_all("img") else {
        return;
    };
    for index in 0..images.length() {
        let Some(image) = images
            .item(index)
            .and_then(|node| node.dyn_into::<web_sys::HtmlImageElement>().ok())
        else {
            continue;
        };
        if image.complete() {
            resolve_image(image);
        }
    }
}

fn set_image_resolution_state(image: &web_sys::HtmlImageElement, outcome: &str) {
    let _ = image.set_attribute("data-mosaic-image-state", outcome);
    if let Some(status) = image
        .closest(".message-image")
        .ok()
        .flatten()
        .and_then(|preview| {
            preview
                .query_selector("[data-mosaic-image-status]")
                .ok()
                .flatten()
        })
    {
        let text = match outcome {
            "loaded" => "Image loaded",
            "error" => "Image unavailable",
            _ => "Loading image",
        };
        let _ = status.set_attribute("data-mosaic-image-status", outcome);
        status.set_text_content(Some(text));
    }
}

#[hook]
pub(super) fn use_image_resolution_events(host: NodeRef, cards: Vec<ActivityCard>) {
    let image_scan = cards
        .iter()
        .map(|activity| (activity.key(), activity.measurement_fingerprint()))
        .collect::<Vec<_>>();
    use_effect_with((host, image_scan), move |(host, _)| {
        let registration = register_image_resolution_events(host);
        move || remove_image_resolution_events(registration)
    });
}

#[hook]
pub(crate) fn use_markdown_image_resolution_events(host: NodeRef, html: AttrValue) {
    use_effect_with((host, html), move |(host, _)| {
        let registration = register_image_resolution_events(host);
        move || remove_image_resolution_events(registration)
    });
}

type ImageResolutionRegistration = (web_sys::Element, Closure<dyn FnMut(web_sys::Event)>);

fn register_image_resolution_events(host: &NodeRef) -> Option<ImageResolutionRegistration> {
    let host = host.cast::<web_sys::Element>()?;
    let resolved = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
        let Some(image) = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::HtmlImageElement>().ok())
        else {
            return;
        };
        resolve_image(image);
    });
    let callback = resolved.as_ref().unchecked_ref();
    host.add_event_listener_with_callback_and_bool("load", callback, true)
        .ok()?;
    if host
        .add_event_listener_with_callback_and_bool("error", callback, true)
        .is_err()
    {
        let _ = host.remove_event_listener_with_callback_and_bool("load", callback, true);
        return None;
    }
    scan_complete_images(&host);
    Some((host, resolved))
}

fn remove_image_resolution_events(registration: Option<ImageResolutionRegistration>) {
    if let Some((host, resolved)) = registration {
        let callback = resolved.as_ref().unchecked_ref();
        let _ = host.remove_event_listener_with_callback_and_bool("load", callback, true);
        let _ = host.remove_event_listener_with_callback_and_bool("error", callback, true);
    }
}

#[hook]
pub(super) fn use_element_width(host: NodeRef) -> UseStateHandle<f64> {
    let width = use_state_eq(|| 0.0);
    {
        let width = width.clone();
        use_effect_with(host, move |host| {
            let registration = host.cast::<web_sys::Element>().and_then(|element| {
                let initial = element_content_width(&element)?;
                width.set(initial);
                let observed = element.clone();
                let width = width.clone();
                let callback = Closure::<dyn FnMut(js_sys::Array, web_sys::ResizeObserver)>::new(
                    move |_, _| {
                        if let Some(measured) = element_content_width(&observed) {
                            width.set(measured);
                        }
                    },
                );
                let observer =
                    web_sys::ResizeObserver::new(callback.as_ref().unchecked_ref()).ok()?;
                observer.observe(&element);
                Some((observer, callback))
            });
            move || {
                if let Some((observer, _callback)) = registration {
                    observer.disconnect();
                }
            }
        });
    }
    width
}

#[hook]
pub(super) fn use_element_height(host: NodeRef) -> UseStateHandle<f64> {
    let height = use_state_eq(|| 0.0);
    {
        let height = height.clone();
        use_effect_with(host, move |host| {
            let registration = host.cast::<web_sys::Element>().and_then(|element| {
                height.set(f64::from(element.client_height()));
                let observed = element.clone();
                let height = height.clone();
                let callback = Closure::<dyn FnMut(js_sys::Array, web_sys::ResizeObserver)>::new(
                    move |_, _| height.set(f64::from(observed.client_height())),
                );
                let observer =
                    web_sys::ResizeObserver::new(callback.as_ref().unchecked_ref()).ok()?;
                observer.observe(&element);
                Some((observer, callback))
            });
            move || {
                if let Some((observer, _callback)) = registration {
                    observer.disconnect();
                }
            }
        });
    }
    height
}

fn element_content_width(element: &web_sys::Element) -> Option<f64> {
    let style = web_sys::window()?.get_computed_style(element).ok()??;
    let padding = |property| {
        style
            .get_property_value(property)
            .ok()?
            .trim()
            .strip_suffix("px")?
            .parse::<f64>()
            .ok()
    };
    Some(crate::mosaic::content_box_width(
        f64::from(element.client_width()),
        padding("padding-left")?,
        padding("padding-right")?,
    ))
}

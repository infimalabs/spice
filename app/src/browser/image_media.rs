//! Browser-side normalization for image payloads that exceed one durable steer record.

use wasm_bindgen::JsCast as _;
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;

const JPEG_QUALITIES: [f64; 3] = [0.86, 0.72, 0.58];
const MAX_RESIZE_PASSES: usize = 8;
const SCALE_MARGIN: f64 = 0.92;

/// Keep a valid image exact when it fits, otherwise flatten and resize a
/// browser-decodable source to a JPEG that fits the supplied data-URL budget.
///
/// Android's camera choice can return a full-resolution image below the
/// per-image decoded-byte limit but above the smaller durable-record budget.
/// Normalizing at intake prevents that valid draft from failing only after the
/// operator presses Send. The fitted output crosses ordinary wire admission;
/// the oversized source is transient browser input and is never persisted.
pub(super) async fn fit(
    media: wire::Media,
    maximum_data_url_bytes: usize,
) -> Result<wire::Media, String> {
    if !wire::media::supports_content_type(&media.content_type) {
        return Err(wire::MediaError::UnsupportedType(media.content_type).to_string());
    }
    let prefix = format!("data:{};base64,", media.content_type);
    if !media.data_url.starts_with(&prefix) {
        return Err(wire::MediaError::DataUrl.to_string());
    }
    if media.data_url.len() <= maximum_data_url_bytes {
        media.validate().map_err(|error| error.to_string())?;
        return Ok(media);
    }
    if maximum_data_url_bytes == 0 {
        return Err("Attached images already fill the submission limit.".to_owned());
    }

    let image = web_sys::HtmlImageElement::new().map_err(js_error)?;
    image.set_src(&media.data_url);
    JsFuture::from(image.decode()).await.map_err(js_error)?;
    let natural_width = image.natural_width();
    let natural_height = image.natural_height();
    if natural_width == 0 || natural_height == 0 {
        return Err("The browser decoded an empty image.".to_owned());
    }

    let document = web_sys::window()
        .and_then(|window| window.document())
        .ok_or_else(|| "Image conversion is unavailable.".to_owned())?;
    let canvas = document
        .create_element("canvas")
        .map_err(js_error)?
        .dyn_into::<web_sys::HtmlCanvasElement>()
        .map_err(|error| js_error(error.into()))?;
    let context = canvas
        .get_context("2d")
        .map_err(js_error)?
        .ok_or_else(|| "Image conversion is unavailable.".to_owned())?
        .dyn_into::<web_sys::CanvasRenderingContext2d>()
        .map_err(|error| js_error(error.into()))?;

    let initial_scale = ((maximum_data_url_bytes as f64 / media.data_url.len() as f64).sqrt()
        * SCALE_MARGIN)
        .min(1.0);
    let mut width = scaled_dimension(natural_width, initial_scale);
    let mut height = scaled_dimension(natural_height, initial_scale);
    let name = jpeg_name(&media.name);

    for _ in 0..MAX_RESIZE_PASSES {
        canvas.set_width(width);
        canvas.set_height(height);
        context.set_fill_style_str("#ffffff");
        context.fill_rect(0.0, 0.0, f64::from(width), f64::from(height));
        context
            .draw_image_with_html_image_element_and_dw_and_dh(
                &image,
                0.0,
                0.0,
                f64::from(width),
                f64::from(height),
            )
            .map_err(js_error)?;

        let mut shortest = usize::MAX;
        for quality in JPEG_QUALITIES {
            let encoded = canvas
                .to_data_url_with_type_and_encoder_options(
                    "image/jpeg",
                    &JsValue::from_f64(quality),
                )
                .map_err(js_error)?;
            shortest = shortest.min(encoded.len());
            if encoded.len() <= maximum_data_url_bytes {
                return wire::Media::admit(name.clone(), "image/jpeg", encoded)
                    .map_err(|error| error.to_string());
            }
        }

        if width == 1 && height == 1 {
            break;
        }
        let scale = ((maximum_data_url_bytes as f64 / shortest as f64).sqrt() * SCALE_MARGIN)
            .clamp(0.1, 0.85);
        let next_width = scaled_dimension(width, scale);
        let next_height = scaled_dimension(height, scale);
        if next_width == width && next_height == height {
            break;
        }
        width = next_width;
        height = next_height;
    }

    Err("The image could not be reduced to the submission limit.".to_owned())
}

fn scaled_dimension(value: u32, scale: f64) -> u32 {
    (f64::from(value) * scale)
        .round()
        .clamp(1.0, f64::from(u32::MAX)) as u32
}

fn jpeg_name(name: &str) -> String {
    let stem = name
        .rsplit_once('.')
        .map_or(name, |(stem, _)| stem)
        .trim_end();
    let stem = if stem.is_empty() { "image" } else { stem };
    let mut end = stem
        .len()
        .min(wire::media::MAX_MEDIA_NAME_BYTES - ".jpg".len());
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}.jpg", &stem[..end])
}

fn js_error(error: JsValue) -> String {
    error
        .as_string()
        .unwrap_or_else(|| "Image conversion failed.".to_owned())
}

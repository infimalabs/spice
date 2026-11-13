use super::*;
use crate::image_annotation::{Course, stroke_width};

pub(super) async fn render(
    original: &wire::Media,
    shapes: &[Shape],
) -> Result<wire::Media, JsValue> {
    if shapes.is_empty() {
        return Ok(original.clone());
    }
    let image = web_sys::HtmlImageElement::new()?;
    image.set_src(&original.data_url);
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
        .ok_or_else(|| JsValue::from_str("Image export unavailable"))?
        .dyn_into::<web_sys::CanvasRenderingContext2d>()?;
    context.draw_image_with_html_image_element(&image, 0.0, 0.0)?;
    let stroke = stroke_width(image.natural_width(), image.natural_height());
    context.set_line_join("round");
    context.set_line_cap("round");
    for shape in shapes {
        context.set_stroke_style_str(shape.color.ink());
        context.set_fill_style_str(shape.color.ink());
        context.set_line_width(stroke);
        match shape.kind {
            Kind::Rectangle => {
                let (low, high) = shape.bounds();
                context.stroke_rect(
                    f64::from(low.x),
                    f64::from(low.y),
                    f64::from(high.x - low.x),
                    f64::from(high.y - low.y),
                );
            }
            Kind::Line | Kind::Arrow => {
                context.begin_path();
                match shape.course() {
                    Course::Polyline(points) => {
                        for (index, (x, y)) in points.into_iter().enumerate() {
                            if index == 0 {
                                context.move_to(x, y);
                            } else {
                                context.line_to(x, y);
                            }
                        }
                    }
                    Course::Quadratic {
                        start,
                        control,
                        end,
                    } => {
                        context.move_to(start.0, start.1);
                        context.quadratic_curve_to(control.0, control.1, end.0, end.1);
                    }
                }
                context.stroke();
                if shape.kind == Kind::Arrow {
                    let [tip, left, right] = shape.arrowhead(stroke);
                    context.begin_path();
                    context.move_to(tip.0, tip.1);
                    context.line_to(left.0, left.1);
                    context.line_to(right.0, right.1);
                    context.close_path();
                    context.fill();
                }
            }
            Kind::Text => {
                let size = shape.end.y - shape.start.y;
                let (x, y) = (
                    f64::from(shape.start.x),
                    f64::from(shape::label_baseline(shape)),
                );
                context.set_font(&label::font(size));
                context.set_text_baseline("alphabetic");
                context.set_stroke_style_str(shape.color.halo());
                context.set_line_width(label::halo(size));
                context.stroke_text(&shape.text, x, y)?;
                context.fill_text(&shape.text, x, y)?;
            }
        }
    }
    let data_url = canvas.to_data_url_with_type("image/png")?;
    let stem = original
        .name
        .rsplit_once('.')
        .map_or(original.name.as_str(), |(stem, _)| stem);
    let media = wire::Media {
        name: format!("{stem}.png"),
        content_type: "image/png".to_owned(),
        data_url,
    };
    crate::browser::image_media::fit(media, wire::media::MAX_MEDIA_DATA_URL_BYTES)
        .await
        .map_err(|error| JsValue::from_str(&error))
}

use super::*;

pub(super) fn render(shape: &Shape, stroke: f64) -> Html {
    let ink = shape.color.ink();
    match shape.kind {
        Kind::Rectangle => {
            let (low, high) = shape.bounds();
            html! {
                <>
                    <rect class="image-annotation-mark" x={low.x.to_string()} y={low.y.to_string()} width={(high.x-low.x).to_string()} height={(high.y-low.y).to_string()} fill="none" stroke={ink} stroke-width={stroke.to_string()} />
                    <rect class="image-annotation-hit" x={low.x.to_string()} y={low.y.to_string()} width={(high.x-low.x).to_string()} height={(high.y-low.y).to_string()} />
                </>
            }
        }
        Kind::Line | Kind::Arrow => {
            let path = shape.course().path_data();
            let head = (shape.kind == Kind::Arrow).then(|| shape.arrowhead(stroke));
            html! {
                <>
                    <path class="image-annotation-mark" d={path.clone()} fill="none" stroke={ink} stroke-width={stroke.to_string()} stroke-linecap="round" />
                    if let Some(head) = head {
                        <polygon fill={ink} points={format!("{},{} {},{} {},{}", head[0].0, head[0].1, head[1].0, head[1].1, head[2].0, head[2].1)} />
                    }
                    <path class="image-annotation-hit" d={path} />
                </>
            }
        }
        Kind::Text => {
            let (low, high) = shape.bounds();
            let size = shape.end.y - shape.start.y;
            html! {
                <>
                    <text class="image-annotation-mark" x={shape.start.x.to_string()} y={label_baseline(shape).to_string()} font-size={size.to_string()} font-weight="700" font-family={label::family()} fill={ink} stroke={shape.color.halo()} stroke-width={label::halo(size).to_string()} stroke-linejoin="round" paint-order="stroke">{shape.text.to_string()}</text>
                    <rect class="image-annotation-hit" x={low.x.to_string()} y={low.y.to_string()} width={(high.x-low.x).to_string()} height={(high.y-low.y).to_string()} />
                </>
            }
        }
    }
}

/// A label's alphabetic baseline, shared by the SVG text and the canvas.
pub(super) fn label_baseline(shape: &Shape) -> i32 {
    shape.start.y + (shape.end.y - shape.start.y) * 4 / 5
}

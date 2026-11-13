//! Trusted controls aligned to validated inert Mermaid geometry.

use std::rc::Rc;

use wasm_bindgen::JsCast;
use yew::prelude::*;

use wire::TaskStamp;

use super::viewport::Canvas;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MapNodeTarget {
    pub(super) id: String,
    pub(super) task: TaskStamp,
    pub(super) label: String,
    pub(super) selected: bool,
    pub(super) availability: Option<&'static str>,
    pub(super) disposition: Option<&'static str>,
    pub(super) mark: Option<&'static str>,
    pub(super) state: Option<&'static str>,
}

#[derive(Properties, PartialEq)]
struct GeometryOverlayProps {
    geometry: Rc<crate::mermaid_geometry::Geometry>,
    targets: Rc<Vec<MapNodeTarget>>,
    select: Callback<TaskStamp>,
    canvas: Rc<Canvas>,
}

/// Node controls carry their rectangle in diagram units; the frame's custom
/// properties place them over the drawing at every zoom.
#[function_component(GeometryOverlay)]
fn geometry_overlay(props: &GeometryOverlayProps) -> Html {
    let host = use_node_ref();
    use_effect_with(
        (
            host.clone(),
            props.geometry.clone(),
            props.targets.clone(),
            props.canvas.clone(),
        ),
        |(host, geometry, _, canvas)| {
            apply_node_rects(host);
            canvas.show(geometry.view_box);
        },
    );
    let view = props.geometry.view_box;
    html! {
        <div
            ref={host}
            class="work-map-node-layer"
            role="group"
            aria-label="Interactive task nodes"
        >
            {for props.geometry.nodes.iter().filter_map(|node| {
                let target = props.targets.iter().find(|target| target.id == node.id)?;
                let activate = {
                    let select = props.select.clone();
                    let task = target.task.clone();
                    Callback::from(move |_| select.emit(task.clone()))
                };
                let label = target.state.map_or_else(
                    || format!("Focus {} in the task overview", target.label),
                    |state| format!("Focus {} in the task overview — {state}", target.label),
                );
                Some(html! {
                    <button
                        key={node.id.clone()}
                        type="button"
                        class={classes!("work-map-node-button", target.selected.then_some("selected"))}
                        data-map-x={(node.rect.x - view.x).to_string()}
                        data-map-y={(node.rect.y - view.y).to_string()}
                        data-map-width={node.rect.width.to_string()}
                        data-map-height={node.rect.height.to_string()}
                        data-map-node-id={node.id.clone()}
                        data-task-stamp={target.task.to_string()}
                        data-work-availability={target.availability}
                        data-work-disposition={target.disposition}
                        data-work-mark={target.mark}
                        data-work-state={target.state}
                        aria-label={label.clone()}
                        title={label}
                        data-task-selected={target.selected.to_string()}
                        onclick={activate}
                    ></button>
                })
            })}
        </div>
    }
}

/// Inline style attributes are not admitted, so each rectangle reaches CSS
/// through the object model.
fn apply_node_rects(host: &NodeRef) {
    let Some(layer) = host.cast::<web_sys::Element>() else {
        return;
    };
    let buttons = layer
        .query_selector_all(".work-map-node-button")
        .expect("the fixed Work map node selector is valid");
    for index in 0..buttons.length() {
        let button = buttons
            .item(index)
            .expect("the Work map node list retains every indexed node")
            .dyn_into::<web_sys::HtmlElement>()
            .expect("a Work map node control is an HTML element");
        for (property, attribute) in [
            ("--map-node-x", "data-map-x"),
            ("--map-node-y", "data-map-y"),
            ("--map-node-width", "data-map-width"),
            ("--map-node-height", "data-map-height"),
        ] {
            let value = button
                .get_attribute(attribute)
                .expect("Work map geometry provides every layout attribute");
            button
                .style()
                .set_property(property, &value)
                .unwrap_or_else(|_| panic!("Work map layout writes {property}"));
        }
    }
}

pub(super) fn renderer(
    targets: Rc<Vec<MapNodeTarget>>,
    select: Callback<TaskStamp>,
    canvas: Rc<Canvas>,
) -> Callback<Rc<crate::mermaid_geometry::Geometry>, Html> {
    Callback::from(move |geometry: Rc<crate::mermaid_geometry::Geometry>| {
        html! {
            <GeometryOverlay
                {geometry}
                targets={targets.clone()}
                select={select.clone()}
                canvas={canvas.clone()}
            />
        }
    })
}

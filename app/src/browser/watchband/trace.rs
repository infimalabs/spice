//! The activity trace across the foot of a lane's composer: a glowing curve per
//! kind of work over a fading fill, a dot where each curve meets now, and
//! lifecycle markers. Everything is SVG geometry in the pane's measured CSS
//! pixels, which the app's `style-src 'self'` policy admits where inline styles
//! are refused; colours and glow stay in the stylesheet.

use std::rc::Rc;

use gloo_timers::callback::Timeout;
use wasm_bindgen::JsCast;
use yew::prelude::*;

use super::super::clock::now_millis;
use crate::watchband::{
    Occupancy, Stroke, TRACE_STEP_MILLIS, TRACE_WINDOW_MILLIS, TraceActivity, TraceCurve,
    TraceMarker, TraceMarkerKind, TraceOff, TraceSpan, curve_path, drawn_spans, level_y,
    off_intervals, trace,
};

#[derive(Properties, PartialEq)]
pub(crate) struct ActivityTraceProps {
    /// Names the trace's gradients, unique on the page: the lane.
    pub(crate) id: AttrValue,
    /// The traced activities, in any order.
    pub(crate) activities: Rc<[TraceActivity]>,
    /// Intervals the traced lane was off, which its curves leave undrawn.
    #[prop_or_default]
    pub(crate) off: Rc<[TraceOff]>,
}

/// One lane's retained activity, which its trace keeps to the window. Agent
/// activity carries reasoning, tool, and prose rates. Operator direction is
/// retained on message cards rather than in that activity stream, so it joins
/// here as a marker, as does each SYN another lane sent this lane, wearing its
/// sender's accent.
fn lane_activities(runtime: &crate::AppRuntime, lane: wire::CanonicalUuid) -> Rc<[TraceActivity]> {
    let lanes = runtime.lanes().map_or(&[][..], |view| &view.lanes);
    let directions = runtime.messages().iter().filter_map(|message| {
        let accent = if message.lane == lane {
            // The operator's own direction keeps the neutral dot.
            if message.activity != wire::AgentActivityKind::Direction {
                return None;
            }
            None
        } else {
            let addressed = message.sends.iter().any(|send| {
                send.lanes.contains(&lane)
                    || send
                        .recipients
                        .iter()
                        .any(|recipient| recipient.lane == lane)
            });
            if !addressed {
                return None;
            }
            // A sender outside the roster has no accent to tell it from the
            // operator, so it marks nothing.
            Some(crate::watchband::lane_accent(lanes, message.lane)?)
        };
        Some(TraceActivity {
            kind: wire::AgentActivityKind::Direction,
            millis: message.display_millis(),
            bytes: 0,
            context_bytes: 0,
            accent,
        })
    });
    runtime
        .activities()
        .iter()
        .filter(|activity| activity.lane() == lane)
        .map(|activity| TraceActivity {
            kind: activity.kind(),
            millis: activity.display_millis(),
            bytes: activity.text_bytes(),
            context_bytes: activity.context_bytes(),
            accent: None,
        })
        .chain(directions)
        .collect()
}

/// The intervals one lane was off, from its retained occupancy transitions and
/// whether it is `seated_now`.
fn lane_off(
    runtime: &crate::AppRuntime,
    lane: wire::CanonicalUuid,
    seated_now: bool,
) -> Rc<[TraceOff]> {
    off_intervals(
        runtime.lane_presence_transitions(lane).map(|transition| {
            let occupancy = match (transition.occupant(), transition.renewed()) {
                (None, _) => Occupancy::Vacant,
                (Some(_), false) => Occupancy::Started,
                (Some(_), true) => Occupancy::Renewed,
            };
            (transition.display_millis(), occupancy)
        }),
        seated_now,
    )
    .into()
}

/// One lane's trace across the foot of its primary composer, drawn at the
/// rail's own size.
pub(super) fn render_composer_rail(
    runtime: &crate::AppRuntime,
    member: &crate::runtime::LaneMember,
) -> Html {
    let seated = member.occupant.is_some();
    let activities = lane_activities(runtime, member.lane);
    let off = lane_off(runtime, member.lane, seated);
    html! {
        <span class="composer-trace-rail" data-lane-seated={seated.to_string()} aria-hidden="true">
            <ActivityTrace id={format!("composer-{}", member.lane)} {activities} {off} />
        </span>
    }
}

/// One rendered trace frame and the instant its historical layer is crossing
/// toward, if any.
#[derive(Clone, Copy, PartialEq, Eq)]
struct TraceFrame {
    at: i64,
    target: Option<i64>,
}

impl TraceFrame {
    fn settled(at: i64) -> Self {
        Self { at, target: None }
    }

    fn moving_to(self, target: i64) -> Self {
        Self {
            at: self.at,
            target: Some(target),
        }
    }

    fn is_moving(self) -> bool {
        self.target.is_some()
    }
}

/// Time left until the frame's exact next sampled position. Transition time
/// consumes part of the interval rather than making the cadence drift.
fn next_sample_delay(frame_at: i64, now: i64) -> u32 {
    let elapsed = now
        .saturating_sub(frame_at)
        .clamp(0, i64::from(TRACE_STEP_MILLIS));
    TRACE_STEP_MILLIS - elapsed as u32
}

/// A boundary moves for one sixteenth of its sample interval, then stays
/// still. This hides the sampled jump without keeping Firefox's SVG renderer
/// active between authoritative positions.
const TRACE_TRANSITION_MILLIS: u32 = TRACE_STEP_MILLIS / 16;

/// Space the now-dot needs inside the pane's right edge, in CSS pixels.
const DOT_ROOM: f64 = 1.5;

/// Radius of a direction dot, the operator's or another agent's, in CSS pixels.
const DIRECTION_RADIUS: f64 = 1.8;

/// Samples per curve: about one per six pixels, between five and nine, so a
/// narrow pip still bends and a wide one stays smooth.
fn samples(width: f64) -> usize {
    // Truncation is the intent: whole three-pixel columns.
    let columns = ((width / 3.0).floor() as usize).clamp(6, 16);
    (columns.div_ceil(2) + 1).max(5)
}

/// The pane's width across which the window's time runs, start to now.
fn span(width: f64) -> f64 {
    width - DOT_ROOM
}

/// How far one trace step carries the drawing, in pane pixels: the distance a
/// frame slides before the next replaces it, and so how far past now its
/// curves must reach.
fn step(width: f64) -> f64 {
    span(width) * f64::from(TRACE_STEP_MILLIS) / TRACE_WINDOW_MILLIS as f64
}

/// The measured pane and its CSP-safe transition variables.
#[hook]
fn use_trace_pane() -> (NodeRef, (f64, f64)) {
    let pane = use_node_ref();
    let size = use_state_eq(|| (0.0_f64, 0.0_f64));
    {
        let pane = pane.clone();
        let size = size.clone();
        use_effect(move || {
            if let Some(pane) = pane.cast::<web_sys::HtmlElement>() {
                let style = pane.style();
                style
                    .set_property(
                        "--trace-flow-shift",
                        &format!("{:.3}px", -step(size.0).max(0.0)),
                    )
                    .expect("the trace writes its CSP-safe boundary shift");
                style
                    .set_property(
                        "--trace-flow-transition-duration",
                        &format!("{TRACE_TRANSITION_MILLIS}ms"),
                    )
                    .expect("the trace writes its CSP-safe transition duration");
                let rect = pane.get_bounding_client_rect();
                size.set((rect.width(), rect.height()));
            }
        });
    }
    (pane, *size)
}

/// Schedule the exact next sampled position and finish it from the CSS
/// transition's own completion event.
#[hook]
fn use_trace_boundary(
    frame: UseStateHandle<TraceFrame>,
    shown: bool,
) -> Callback<web_sys::TransitionEvent> {
    {
        let frame_state = frame.clone();
        use_effect_with((shown, *frame), move |&(shown, frame)| {
            let timeout = (shown && !frame.is_moving()).then(|| {
                Timeout::new(next_sample_delay(frame.at, now_millis()), move || {
                    let boundary = now_millis();
                    if super::super::overlay::reduced_motion() {
                        frame_state.set(TraceFrame::settled(boundary));
                    } else {
                        frame_state.set(frame.moving_to(boundary));
                    }
                })
            });
            move || drop(timeout)
        });
    }
    let current = *frame;
    Callback::from(move |event: web_sys::TransitionEvent| {
        let connected = event
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Node>().ok())
            .is_some_and(|target| target.is_connected());
        // A new data frame replaces the keyed moving node. Its cancellation
        // must not overwrite that newer frame with this callback's boundary.
        if connected
            && event.property_name() == "transform"
            && let Some(boundary) = current.target
        {
            frame.set(TraceFrame::settled(boundary));
        }
    })
}

/// Redraws on new activity and at the exact next trace step. At a time step,
/// the already-rendered history crosses one sampled position briefly; its
/// `transitionend` replaces it with the frame for the new instant. Nothing
/// animates between those boundaries. The pane's size is measured after each
/// draw.
#[function_component(ActivityTrace)]
pub(crate) fn activity_trace(props: &ActivityTraceProps) -> Html {
    let frame = use_state(|| TraceFrame::settled(now_millis()));
    let initial_inputs = use_mut_ref(|| true);
    {
        let frame = frame.clone();
        let initial_inputs = initial_inputs.clone();
        use_effect_with((props.activities.clone(), props.off.clone()), move |_| {
            let initial = std::mem::take(&mut *initial_inputs.borrow_mut());
            if !initial {
                frame.set(TraceFrame::settled(now_millis()));
            }
            || ()
        });
    }
    let (pane, (width, height)) = use_trace_pane();
    let now = frame.at;
    let placed = trace(&props.activities, now, samples(width));
    let spans = drawn_spans(&props.off, now, samples(width));
    let finish_boundary = use_trace_boundary(frame.clone(), !placed.is_empty());
    if placed.is_empty() {
        return Html::default();
    }
    let drawn = width > DOT_ROOM && height > DOT_ROOM;
    let gradient = |key: &str| format!("trace-{}-{key}", props.id);
    let whole = matches!(spans.as_slice(), [only] if only.from <= 0.0 && only.to >= 1.0);
    let clip = (!whole).then(|| format!("trace-{}-on", props.id));
    let pane_clip = format!("trace-{}-pane", props.id);
    html! {
        <span ref={pane} class="activity-trace" aria-hidden="true">
            if drawn {
                <svg viewBox={format!("0 0 {width:.2} {height:.2}")}>
                    <defs>
                        // A literal tag is normalised to lowercase, and SVG knows
                        // no `lineargradient`; the dynamic form keeps its case.
                        {for placed.curves.iter().map(|curve| html! {
                            <@{"linearGradient"}
                                id={gradient(curve.kind.key())}
                                data-trace-kind={curve.kind.key()}
                                x1="0" y1="0" x2="0" y2="1"
                            >
                                <stop offset="0" stop-opacity=".42" />
                                <stop offset="1" stop-opacity="0" />
                            </@>
                        })}
                        if let Some(clip) = &clip {
                            // `html!` lowercases a literal tag, which SVG does not
                            // recognise as a clip; a dynamic tag keeps its case.
                            <@{"clipPath"} id={clip.clone()}>
                                {for on_rects(&spans, width, height).into_iter().map(|(x, y, w, h)| html! {
                                    <rect x={format!("{x:.2}")} y={format!("{y:.2}")} width={format!("{w:.2}")} height={format!("{h:.2}")} />
                                })}
                            </@>
                        }
                        // The pane's own bleed, cut at its right edge: the
                        // overscan past now stays hidden until the slide brings
                        // it in.
                        <@{"clipPath"} id={pane_clip.clone()}>
                            <rect
                                x={format!("{:.2}", -CLIP_BLEED)}
                                y={format!("{:.2}", -CLIP_BLEED)}
                                width={format!("{:.2}", width + CLIP_BLEED)}
                                height={format!("{:.2}", height + 2.0 * CLIP_BLEED)}
                            />
                        </@>
                    </defs>
                    <g clip-path={format!("url(#{pane_clip})")}>
                        // A settled frame stays still. At its next sample
                        // boundary it crosses one step; the transition event
                        // then mounts the frame computed for that new instant.
                        <g
                            key={now.to_string()}
                            class="trace-flow"
                            data-trace-moving={frame.is_moving().to_string()}
                            ontransitionend={finish_boundary.clone()}
                            ontransitioncancel={finish_boundary}
                        >
                            <g clip-path={clip.as_ref().map(|clip| format!("url(#{clip})"))}>
                                {for placed.curves.iter().map(|curve| render_curve(curve, &gradient(curve.kind.key()), width, height))}
                            </g>
                            {for placed.markers.iter().map(|marker| render_marker(*marker, width, height))}
                        </g>
                    </g>
                    // The now-dots mark the instant itself and hold the right
                    // edge while the curves slide beneath them.
                    <g clip-path={clip.as_ref().map(|clip| format!("url(#{clip})"))}>
                        {for placed.curves.iter().map(|curve| render_now(curve, width, height))}
                    </g>
                </svg>
            }
        </span>
    }
}

/// One curve's fill and line, run on through the overscan past now. The fill
/// outlines the whole curve; the line draws only where the curve leaves the
/// floor, so an idle stretch shows nothing at the composer's foot.
fn render_curve(curve: &TraceCurve, fill: &str, width: f64, height: f64) -> Html {
    let points = points(&curve.levels, width, height);
    let outline = curve_path(&points, height, Stroke::Whole);
    let line = curve_path(&points, height, Stroke::Risen);
    let end = width + step(width);
    html! {
        <g data-trace-kind={curve.kind.key()}>
            <path
                class="trace-fill"
                d={format!("{outline} L{end:.2},{height:.2} L0,{height:.2} Z")}
                fill={format!("url(#{fill})")}
            />
            <path class="trace-line trace-lit" d={line} />
        </g>
    }
}

/// One curve's dot at now, its last sample.
fn render_now(curve: &TraceCurve, width: f64, height: f64) -> Html {
    let (x, y) = point(&curve.levels, curve.levels.len() - 1, width, height);
    html! {
        <circle
            data-trace-kind={curve.kind.key()}
            class="trace-now trace-lit"
            cx={format!("{x:.2}")}
            cy={format!("{y:.2}")}
            r="1.1"
        />
    }
}

/// Room the clip keeps beyond the pane's outer edges, so a line's stroke and
/// glow there are not shaved; only a gap's own edges cut the curves.
const CLIP_BLEED: f64 = 3.0;

/// The drawn spans as rectangles in pane pixels: time maps to x as the curve
/// points map it, and the span reaching now runs on through the overscan.
fn on_rects(spans: &[TraceSpan], width: f64, height: f64) -> Vec<(f64, f64, f64, f64)> {
    spans
        .iter()
        .map(|&TraceSpan { from, to }| {
            let left = if from <= 0.0 {
                -CLIP_BLEED
            } else {
                from * span(width)
            };
            let right = if to >= 1.0 {
                width + step(width) + CLIP_BLEED
            } else {
                to * span(width)
            };
            (left, -CLIP_BLEED, right - left, height + 2.0 * CLIP_BLEED)
        })
        .collect()
}

/// Sample `index` of `levels` in pane pixels: time runs across to the
/// now-dot's room, level rises from the floor to the headroom under the top.
fn point(levels: &[f64], index: usize, width: f64, height: f64) -> (f64, f64) {
    let x = index as f64 * span(width) / (levels.len() - 1) as f64;
    (x, level_y(levels[index], height))
}

/// The curve's points in pane pixels, held level one step past now so the
/// slide never uncovers a bare edge before the next frame.
fn points(levels: &[f64], width: f64, height: f64) -> Vec<(f64, f64)> {
    let mut points = (0..levels.len())
        .map(|index| point(levels, index, width, height))
        .collect::<Vec<_>>();
    let (x, y) = points[points.len() - 1];
    points.push((x + step(width) + DOT_ROOM, y));
    points
}

/// Snap to the half pixel, a whole device pixel on a 2x display.
fn snap(value: f64) -> f64 {
    (value * 2.0).round() / 2.0
}

fn render_marker(marker: TraceMarker, width: f64, height: f64) -> Html {
    let key = marker.kind.key();
    let x = snap(marker.x * (width - DOT_ROOM));
    // A one-pixel rule covering the device pixels from its snapped x: whole
    // pixels on a 2x display, and on 1x whenever x snapped to a whole pixel.
    let rule = x + 0.5;
    match marker.kind {
        // Held a radius inside either edge, so a direction sent just now or
        // leaving the window is drawn whole. Another agent's wears its accent.
        TraceMarkerKind::Direction => {
            let cx = rule.min(width - DIRECTION_RADIUS).max(DIRECTION_RADIUS);
            html! {
                <circle
                    data-trace-marker={key}
                    data-accent={marker.accent.map(|accent| accent.to_string())}
                    cx={format!("{cx:.2}")} cy="2" r={DIRECTION_RADIUS.to_string()}
                />
            }
        }
        TraceMarkerKind::Failure => html! {
            <rect data-trace-marker={key} x={format!("{x:.2}")} y="0" width="1" height={format!("{height:.2}")} />
        },
        // A fresh session ticks forward from its rule; a sign-out ticks back.
        TraceMarkerKind::Session | TraceMarkerKind::SignOut => {
            let tick = if marker.kind == TraceMarkerKind::Session {
                2.5
            } else {
                -2.5
            };
            html! {
                <path
                    data-trace-marker={key}
                    d={format!("M{rule:.2},{height:.2} V0.5 H{:.2}", rule + tick)}
                />
            }
        }
        TraceMarkerKind::Turn | TraceMarkerKind::Compaction => html! {
            <line
                data-trace-marker={key}
                x1={format!("{rule:.2}")} y1="0"
                x2={format!("{rule:.2}")} y2={format!("{height:.2}")}
            />
        },
    }
}

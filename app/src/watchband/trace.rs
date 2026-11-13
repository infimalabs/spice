//! What one lane has been doing lately.
//!
//! A trace is a fixed window of wall time ending at the instant it is drawn,
//! shared by every composer rail so neighbours compare honestly; activity moves
//! left as time passes and leaves once it is a whole window old. Work becomes
//! one curve per kind. Reasoning and prose rise with that kind's recent rate
//! of weighted activity. The tool curve rises with the rate at which tool
//! results returned output into the model's context, so it follows how much
//! the lane is reading rather than how often it calls. Each kind has one fixed
//! scale; lifecycle activity becomes markers. The curves are sampled at evenly
//! spaced instants through a kernel that slides with time, so a redraw moves
//! them smoothly instead of jumping between buckets. Positions and levels are
//! fractions; the browser owns every pixel except the curve's path command,
//! built here from its pixel points so its floor rule is checked natively.
//! Intervals the lane was off become gaps in its rail once they last at least
//! the spacing between two samples.

use wire::AgentActivityKind;

/// Wall time one trace spans, ending at the instant it is drawn.
pub const TRACE_WINDOW_MILLIS: i64 = 10 * 60 * 1_000;

/// Redraw cadence of a non-empty trace.
pub const TRACE_STEP_MILLIS: u32 = 4_000;

/// Text length at which an activity reaches full weight.
const FULL_WEIGHT_BYTES: usize = 1_600;

/// Weight every reasoning or prose activity carries before its text, so a
/// terse one still counts.
const BASE_WEIGHT: f64 = 0.35;

/// Weighted activities per minute at which the reasoning or prose curve
/// reaches the top.
const FULL_RATE_PER_MINUTE: f64 = 3.0;

/// Tool output returned to the model per minute at which the tool curve
/// reaches the top. Measured lanes return a median near 6.6 KiB a minute while
/// working, a quarter of the time under 1.8 KiB or over 16 KiB, and over
/// 84 KiB one time in a hundred; with the square root [`TraceKind::level`]
/// takes, those quartiles draw near 0.17, 0.32 and 0.50 of the height.
const FULL_CONTEXT_BYTES_PER_MINUTE: f64 = 65_536.0;

/// Kernel half-width as a fraction of the ten-minute window. It is fixed in
/// wall time so the same activity has the same height at every pip width.
const KERNEL_HALF_WINDOW: f64 = 1.5 / 8.0;

/// Level below which a kind draws no curve.
const VISIBLE_LEVEL: f64 = 0.02;

/// The part of one retained activity a trace draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceActivity {
    pub kind: AgentActivityKind,
    /// Display instant in epoch milliseconds.
    pub millis: i64,
    /// Length of the activity's text, which weights reasoning and prose.
    pub bytes: usize,
    /// Bytes of tool output the activity returned into the model's context,
    /// which the tool curve draws.
    pub context_bytes: usize,
    /// Accent slot of the lane that spoke a direction, when another agent
    /// rather than the operator directed this lane.
    pub accent: Option<usize>,
}

/// Kind of work a curve shows, in drawing order: prose paints last, on top.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceKind {
    Reasoning,
    Tool,
    Prose,
}

impl TraceKind {
    const ALL: [Self; 3] = [Self::Reasoning, Self::Tool, Self::Prose];

    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Reasoning => "reasoning",
            Self::Tool => "tool",
            Self::Prose => "prose",
        }
    }

    /// What one activity adds to this kind's rate: the output a tool result
    /// returned to the model, or the weight of a reasoning or prose activity.
    fn amount(self, activity: &TraceActivity) -> f64 {
        match self {
            Self::Tool => activity.context_bytes as f64,
            Self::Reasoning | Self::Prose => activity_weight(activity.bytes),
        }
    }

    /// The level a rate of those amounts per minute draws at. Tool output
    /// spans three orders of magnitude between a run of edits and a run of
    /// file reads, so its level is the square root of its share of the full
    /// rate: quadrupling what a lane reads doubles the height.
    fn level(self, per_minute: f64) -> f64 {
        let level = match self {
            Self::Tool => (per_minute / FULL_CONTEXT_BYTES_PER_MINUTE).sqrt(),
            Self::Reasoning | Self::Prose => per_minute / FULL_RATE_PER_MINUTE,
        };
        level.min(1.0)
    }
}

/// One kind's rate through the window.
#[derive(Clone, Debug, PartialEq)]
pub struct TraceCurve {
    pub kind: TraceKind,
    /// Levels in `0..=1` at evenly spaced instants, the window's start first
    /// and now last.
    pub levels: Vec<f64>,
}

/// Lifecycle activity drawn as a marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceMarkerKind {
    Turn,
    Direction,
    Failure,
    Compaction,
    Session,
    SignOut,
}

impl TraceMarkerKind {
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Turn => "turn",
            Self::Direction => "direction",
            Self::Failure => "failure",
            Self::Compaction => "compaction",
            Self::Session => "session",
            Self::SignOut => "sign-out",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TraceMarker {
    pub kind: TraceMarkerKind,
    /// Fraction of the window, its start at 0 and now at 1.
    pub x: f64,
    /// Accent slot of the agent that directed the lane; operator direction
    /// and lifecycle markers have none.
    pub accent: Option<usize>,
}

/// One lane's trace at one instant.
#[derive(Clone, Debug, PartialEq)]
pub struct Trace {
    /// Kinds with visible activity, in drawing order.
    pub curves: Vec<TraceCurve>,
    /// Markers in time order.
    pub markers: Vec<TraceMarker>,
}

impl Trace {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.curves.is_empty() && self.markers.is_empty()
    }
}

enum Placement {
    Curve(TraceKind),
    Marker(TraceMarkerKind),
}

const fn placement(kind: AgentActivityKind) -> Placement {
    match kind {
        AgentActivityKind::Prose => Placement::Curve(TraceKind::Prose),
        AgentActivityKind::Tool | AgentActivityKind::OutboxTransport => {
            Placement::Curve(TraceKind::Tool)
        }
        AgentActivityKind::Reasoning => Placement::Curve(TraceKind::Reasoning),
        AgentActivityKind::Turn => Placement::Marker(TraceMarkerKind::Turn),
        AgentActivityKind::Direction => Placement::Marker(TraceMarkerKind::Direction),
        AgentActivityKind::Failure => Placement::Marker(TraceMarkerKind::Failure),
        AgentActivityKind::Compaction => Placement::Marker(TraceMarkerKind::Compaction),
        AgentActivityKind::Session => Placement::Marker(TraceMarkerKind::Session),
        AgentActivityKind::SignOut => Placement::Marker(TraceMarkerKind::SignOut),
    }
}

/// Trace one lane's activities, given in any order, through the window ending
/// at `now`, sampling each curve at no fewer than `samples` instants. An
/// activity a whole window old or older is gone.
///
/// The grid is always odd so the window start, midpoint, and now are shared
/// samples at every browser-selected density. A sample's level is its kind's
/// rate near that instant: each activity spreads through a triangular
/// kernel of fixed wall-time width, and where the window's edge cuts the kernel
/// short the rate is taken from the part inside, so the level at now reports
/// the current rate rather than half of it.
#[must_use]
pub fn trace(activities: &[TraceActivity], now: i64, samples: usize) -> Trace {
    let samples = grid(samples);
    let since = now - TRACE_WINDOW_MILLIS;
    let window = TRACE_WINDOW_MILLIS as f64;
    let x_of = |millis: i64| ((millis - since) as f64 / window).clamp(0.0, 1.0);
    let half_width = KERNEL_HALF_WINDOW;
    let mut sums = [(); 3].map(|()| vec![0.0_f64; samples]);
    let mut markers = Vec::new();
    for activity in activities.iter().filter(|activity| activity.millis > since) {
        let x = x_of(activity.millis);
        match placement(activity.kind) {
            Placement::Curve(kind) => {
                let weight = kind.amount(activity);
                let sums = &mut sums[kind as usize];
                for (index, sum) in sums.iter_mut().enumerate() {
                    let at = index as f64 / (samples - 1) as f64;
                    *sum += weight * (1.0 - (at - x).abs() / half_width).max(0.0);
                }
            }
            Placement::Marker(kind) => markers.push(TraceMarker {
                kind,
                x,
                accent: activity.accent,
            }),
        }
    }
    markers.sort_by(|a, b| a.x.total_cmp(&b.x));
    let window_minutes = window / 60_000.0;
    let curves = TraceKind::ALL
        .into_iter()
        .zip(sums)
        .map(|(kind, sums)| {
            let levels = sums
                .iter()
                .enumerate()
                .map(|(index, sum)| {
                    let at = index as f64 / (samples - 1) as f64;
                    // Kernel area inside the window, in window fractions.
                    let area = inside(at, half_width);
                    kind.level(sum / (area * window_minutes))
                })
                .collect::<Vec<_>>();
            TraceCurve { kind, levels }
        })
        .filter(|curve| curve.levels.iter().any(|level| *level >= VISIBLE_LEVEL))
        .collect();
    Trace { curves, markers }
}

/// Area of a unit-height triangular kernel of half-width `half` centred at
/// `at` that lies within `0..=1`.
fn inside(at: f64, half: f64) -> f64 {
    // Area of the kernel beyond an edge `reach` away from its centre.
    let cut = |reach: f64| {
        let over = (half - reach).max(0.0);
        over * over / (2.0 * half)
    };
    half - cut(at) - cut(1.0 - at)
}

/// What one reasoning or prose activity counts for: a base every activity
/// carries, so a terse one still counts, plus a text weight growing with the
/// square root of its length, so a long reply counts for more without
/// drowning out a run of terse ones.
pub(super) fn activity_weight(bytes: usize) -> f64 {
    BASE_WEIGHT + (bytes.min(FULL_WEIGHT_BYTES) as f64 / FULL_WEIGHT_BYTES as f64).sqrt()
}

/// The odd sample count at or above `samples`, so the window start, midpoint,
/// and now are shared samples at every density.
fn grid(samples: usize) -> usize {
    let samples = samples.max(2);
    samples + usize::from(samples.is_multiple_of(2))
}

/// One interval through which a lane had no occupant, in epoch milliseconds.
/// One that began before the retained record has no start, and one still
/// running has no end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceOff {
    pub from: Option<i64>,
    pub until: Option<i64>,
}

/// Part of the window, as fractions from its start at 0 to now at 1, through
/// which the curves draw.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TraceSpan {
    pub from: f64,
    pub to: f64,
}

/// What one occupancy transition did to a lane's seat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Occupancy {
    /// An occupant started in the vacant seat.
    Started,
    /// A new occupant took over from the one before.
    Renewed,
    /// The seat was left without an occupant: seated empty or ended.
    Vacant,
}

/// The intervals a lane was off, from its occupancy transitions in journal
/// order, each its instant and what it did to the seat. A vacancy turns the
/// lane off and an occupant's start or renewal turns it on. Before its first
/// retained transition a lane counts as on, so no gap is drawn that the record
/// cannot date, unless that transition is a start: the seat was vacant until
/// then. A lane with no retained transition has kept its seat as it is now
/// since the record began, so it is off throughout exactly when `seated_now`
/// is false.
pub fn off_intervals(
    transitions: impl IntoIterator<Item = (i64, Occupancy)>,
    seated_now: bool,
) -> Vec<TraceOff> {
    let mut intervals = Vec::new();
    let mut transitions = transitions.into_iter().peekable();
    let mut open = match transitions.peek() {
        Some((_, Occupancy::Started)) => Some(None),
        None if !seated_now => Some(None),
        _ => None,
    };
    for (millis, occupancy) in transitions {
        match (occupancy, open) {
            (Occupancy::Vacant, None) => open = Some(Some(millis)),
            (Occupancy::Started | Occupancy::Renewed, Some(from)) => {
                intervals.push(TraceOff {
                    from,
                    until: Some(millis),
                });
                open = None;
            }
            _ => {}
        }
    }
    if let Some(from) = open {
        intervals.push(TraceOff { from, until: None });
    }
    intervals
}

/// The spans of the window ending at `now` through which a lane's curves draw,
/// in time order, given the intervals it was `off` and the `samples` the
/// curves are drawn at as [`trace`] draws them. A completed off interval
/// shorter than the spacing between two samples would vanish at that density
/// and leaves the curves whole. A currently open interval always ends the
/// curves exactly where the lane went off, however recently that happened.
#[must_use]
pub fn drawn_spans(off: &[TraceOff], now: i64, samples: usize) -> Vec<TraceSpan> {
    let since = now - TRACE_WINDOW_MILLIS;
    let window = TRACE_WINDOW_MILLIS as f64;
    let spacing = window / (grid(samples) - 1) as f64;
    let x_of = |millis: i64| ((millis.max(since) - since) as f64 / window).min(1.0);
    let mut gaps = off
        .iter()
        .filter_map(|off| {
            let open = off.until.is_none();
            let until = off.until.unwrap_or(now).min(now);
            let from = off.from.unwrap_or(i64::MIN);
            ((open || until.saturating_sub(from) as f64 >= spacing) && until > since)
                .then(|| (x_of(from), x_of(until)))
        })
        .collect::<Vec<_>>();
    gaps.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut spans = Vec::with_capacity(gaps.len() + 1);
    let mut start = 0.0;
    for (from, to) in gaps {
        if from > start {
            spans.push(TraceSpan {
                from: start,
                to: from,
            });
        }
        start = f64::max(start, to);
    }
    if start < 1.0 {
        spans.push(TraceSpan {
            from: start,
            to: 1.0,
        });
    }
    spans
}

/// Which segments of a curve a path strokes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stroke {
    /// Every segment, floor included: the fill's outline.
    Whole,
    /// Only segments with an end above the floor, each run its own subpath.
    /// An idle lane's stroke along the floor would read as the composer's
    /// focus underline, drawn at the same edge.
    Risen,
}

/// Clear band under the pane's top that no curve enters, in CSS pixels, so a
/// lifecycle marker's top stands above the highest peak: the graph is the
/// rail less this headroom, 28 of a 32-pixel rail.
pub const CURVE_HEADROOM: f64 = 4.0;

/// A curve level in pane pixels from the top: level 0 on the floor, half a
/// pixel above the pane's bottom, level 1 at the headroom under its top.
#[must_use]
pub fn level_y(level: f64, height: f64) -> f64 {
    let floor = height - 0.5;
    floor - level * (floor - CURVE_HEADROOM)
}

/// A Catmull-Rom curve through `points` in pane pixels as cubic segments, its
/// control points held inside the pane of `height` so a sharp rise cannot
/// overshoot the edge. The floor is half a pixel above the pane's bottom.
#[must_use]
pub fn curve_path(points: &[(f64, f64)], height: f64, stroke: Stroke) -> String {
    use std::fmt::Write as _;

    let clamp = |y: f64| y.clamp(0.25, height - 0.25);
    let floor = height - 0.5;
    let on_floor = |(_, y): (f64, f64)| y >= floor - 1e-6;
    let mut d = String::new();
    let mut pen_down = false;
    for index in 0..points.len().saturating_sub(1) {
        let p0 = points[index.saturating_sub(1)];
        let p1 = points[index];
        let p2 = points[index + 1];
        let p3 = points[(index + 2).min(points.len() - 1)];
        if stroke == Stroke::Risen && on_floor(p1) && on_floor(p2) {
            pen_down = false;
            continue;
        }
        if !pen_down {
            let _ = write!(d, "M{:.2},{:.2}", p1.0, p1.1);
            pen_down = true;
        }
        let c1 = (
            p1.0 + (p2.0 - p0.0) / 6.0,
            clamp(p1.1 + (p2.1 - p0.1) / 6.0),
        );
        let c2 = (
            p2.0 - (p3.0 - p1.0) / 6.0,
            clamp(p2.1 - (p3.1 - p1.1) / 6.0),
        );
        let _ = write!(
            d,
            " C{:.2},{:.2} {:.2},{:.2} {:.2},{:.2}",
            c1.0, c1.1, c2.0, c2.1, p2.0, p2.1
        );
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000_000;
    const SAMPLES: usize = 9;

    fn at(kind: AgentActivityKind, seconds_ago: i64) -> TraceActivity {
        TraceActivity {
            kind,
            millis: NOW - seconds_ago * 1_000,
            bytes: 400,
            context_bytes: 400,
            accent: None,
        }
    }

    fn levels(placed: &Trace, kind: TraceKind) -> &[f64] {
        &placed
            .curves
            .iter()
            .find(|curve| curve.kind == kind)
            .expect("the kind is drawn")
            .levels
    }

    const HEIGHT: f64 = 32.0;

    /// Points as the browser places them: 20px apart.
    fn pixels(levels: &[f64]) -> Vec<(f64, f64)> {
        levels
            .iter()
            .enumerate()
            .map(|(index, level)| (index as f64 * 20.0, level_y(*level, HEIGHT)))
            .collect()
    }

    #[test]
    fn a_curve_spans_the_floor_to_the_headroom_under_the_top() {
        assert!((level_y(0.0, HEIGHT) - 31.5).abs() < 1e-9);
        assert!((level_y(1.0, HEIGHT) - CURVE_HEADROOM).abs() < 1e-9);
        assert!((level_y(0.5, HEIGHT) - 17.75).abs() < 1e-9);
    }

    fn subpaths(d: &str) -> Vec<&str> {
        d.split('M').filter(|part| !part.is_empty()).collect()
    }

    #[test]
    fn the_risen_stroke_lifts_over_the_floor_and_lands_back_on_it() {
        let points = pixels(&[
            0.0, 0.0, 0.0, 0.6, 1.0, 0.3, 0.0, 0.0, 0.0, 0.0, 0.8, 0.0, 0.0,
        ]);
        let floor = format!("{:.2}", HEIGHT - 0.5);
        let risen = curve_path(&points, HEIGHT, Stroke::Risen);
        let runs = subpaths(&risen);
        assert_eq!(runs.len(), 2, "{risen}");
        for run in &runs {
            let (start, rest) = run.split_once(" C").expect("a run has segments");
            let end = rest.rsplit(' ').next().expect("a segment ends on a point");
            assert!(start.ends_with(&format!(",{floor}")), "{run}");
            assert!(end.ends_with(&format!(",{floor}")), "{run}");
        }
        // The second run rises from sample 9 and lands on sample 11, not 12.
        assert!(runs[1].starts_with("180.00,"), "{}", runs[1]);
        assert!(
            runs[1].ends_with(&format!(" 220.00,{floor}")),
            "{}",
            runs[1]
        );

        let whole = curve_path(&points, HEIGHT, Stroke::Whole);
        assert_eq!(subpaths(&whole).len(), 1, "{whole}");
        assert_eq!(whole.matches(" C").count(), points.len() - 1);
    }

    #[test]
    fn an_idle_curve_strokes_nothing_but_still_outlines_its_fill() {
        let points = pixels(&[0.0; 8]);
        assert_eq!(curve_path(&points, HEIGHT, Stroke::Risen), "");
        assert!(curve_path(&points, HEIGHT, Stroke::Whole).starts_with("M0.00,31.50 C"));
    }

    #[test]
    fn the_window_ends_now_and_drops_older_activity() {
        let placed = trace(
            &[
                at(AgentActivityKind::Tool, 601),
                at(AgentActivityKind::Turn, 601),
                at(AgentActivityKind::Turn, 150),
                at(AgentActivityKind::Reasoning, 300),
            ],
            NOW,
            SAMPLES,
        );
        assert_eq!(
            placed
                .curves
                .iter()
                .map(|curve| curve.kind)
                .collect::<Vec<_>>(),
            [TraceKind::Reasoning]
        );
        assert_eq!(placed.markers.len(), 1);
        assert!((placed.markers[0].x - 0.75).abs() < 1e-9);
    }

    #[test]
    fn one_fixed_rate_scale_is_shared_by_every_browser_sample_count() {
        let mut midpoint = None;
        let mut edge = None;
        for samples in 5..=9 {
            let middle = trace(&[at(AgentActivityKind::Tool, 300)], NOW, samples);
            let middle = levels(&middle, TraceKind::Tool);
            let current = trace(&[at(AgentActivityKind::Tool, 0)], NOW, samples);
            let current = levels(&current, TraceKind::Tool);
            let observed = (middle[middle.len() / 2], current[current.len() - 1]);
            let expected = (
                *midpoint.get_or_insert(observed.0),
                *edge.get_or_insert(observed.1),
            );
            assert!((observed.0 - expected.0).abs() < 1e-9, "{samples}");
            assert!((observed.1 - expected.1).abs() < 1e-9, "{samples}");
        }
    }

    #[test]
    fn pooled_activity_adds_rates_on_the_shared_scale() {
        let activity = at(AgentActivityKind::Reasoning, 300);
        let single = trace(&[activity], NOW, SAMPLES);
        let pooled = trace(&[activity, activity], NOW, SAMPLES);
        let single = levels(&single, TraceKind::Reasoning)[SAMPLES / 2];
        let pooled = levels(&pooled, TraceKind::Reasoning)[SAMPLES / 2];
        assert!((pooled - single * 2.0).abs() < 1e-9);
    }

    #[test]
    fn the_tool_curve_reads_returned_output_not_how_many_calls_returned_it() {
        let result = |context_bytes: usize| TraceActivity {
            context_bytes,
            ..at(AgentActivityKind::Tool, 300)
        };
        let middle = |activities: &[TraceActivity]| {
            levels(&trace(activities, NOW, SAMPLES), TraceKind::Tool)[SAMPLES / 2]
        };
        // A call, or a result that returned nothing, draws no curve however
        // long its preview.
        assert!(trace(&[result(0)], NOW, SAMPLES).is_empty());
        let whole = middle(&[result(8_192)]);
        assert!((middle(&[result(1_024); 8]) - whole).abs() < 1e-9);
        assert!(middle(&[result(64); 8]) < whole);
        assert!((middle(&[result(32_768)]) - whole * 2.0).abs() < 1e-9);
    }

    #[test]
    fn a_curve_peaks_at_its_activity_and_stays_inside_the_scale() {
        let placed = trace(&[at(AgentActivityKind::Tool, 300)], NOW, SAMPLES);
        let tool = levels(&placed, TraceKind::Tool);
        let peak = tool
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(index, _)| index);
        assert_eq!(peak, Some(tool.len() / 2));
        assert!(tool[0].abs() < f64::EPSILON && tool[tool.len() - 1].abs() < f64::EPSILON);

        let flood = (0..600)
            .map(|second| TraceActivity {
                context_bytes: 4_096,
                ..at(AgentActivityKind::Tool, second)
            })
            .collect::<Vec<_>>();
        for samples in 5..=9 {
            let flooded = trace(&flood, NOW, samples);
            assert!(
                levels(&flooded, TraceKind::Tool)
                    .iter()
                    .all(|level| (level - 1.0).abs() < f64::EPSILON),
                "{samples}",
            );
        }
    }

    #[test]
    fn a_steady_rate_reads_the_same_at_now_as_mid_window_at_every_density() {
        // One reasoning step every twenty seconds: three a minute.
        let steady = (0..30)
            .map(|step| at(AgentActivityKind::Reasoning, step * 20 + 5))
            .collect::<Vec<_>>();
        for samples in 5..=9 {
            let placed = trace(&steady, NOW, samples);
            let reasoning = levels(&placed, TraceKind::Reasoning);
            let middle = reasoning[reasoning.len() / 2];
            assert!(middle > 0.0 && middle < 1.0);
            assert!(
                (reasoning[reasoning.len() - 1] - middle).abs() / middle < 0.2,
                "{samples}",
            );
        }
    }

    #[test]
    fn occupancy_transitions_pair_each_vacancy_with_the_next_occupant() {
        use Occupancy::{Renewed, Started, Vacant};
        let intervals = off_intervals(
            [
                (10, Renewed),
                (20, Vacant),
                (25, Vacant),
                (30, Started),
                (40, Renewed),
                (50, Vacant),
            ],
            false,
        );
        assert_eq!(
            intervals,
            [
                TraceOff {
                    from: Some(20),
                    until: Some(30)
                },
                TraceOff {
                    from: Some(50),
                    until: None
                },
            ]
        );
    }

    #[test]
    fn a_lane_is_off_before_its_first_retained_transition_only_when_that_is_a_start() {
        use Occupancy::{Renewed, Started, Vacant};
        assert_eq!(
            off_intervals([(30, Started), (50, Vacant)], false),
            [
                TraceOff {
                    from: None,
                    until: Some(30)
                },
                TraceOff {
                    from: Some(50),
                    until: None
                },
            ]
        );
        assert_eq!(off_intervals([(30, Renewed)], true), []);
        assert_eq!(
            off_intervals([(30, Vacant)], false),
            [TraceOff {
                from: Some(30),
                until: None
            }]
        );
        // A start before the window leaves only the time since it drawn.
        let started = off_intervals([(NOW - 200_000, Started)], true);
        let spans = drawn_spans(&started, NOW, SAMPLES);
        assert_eq!(spans.len(), 1);
        assert!((spans[0].from * 600.0 - 400.0).abs() < 1e-6 && spans[0].to == 1.0);
    }

    #[test]
    fn a_lane_without_a_retained_transition_is_off_throughout_only_when_vacant_now() {
        let none = std::iter::empty::<(i64, Occupancy)>;
        assert_eq!(off_intervals(none(), true), []);
        let vacant = off_intervals(none(), false);
        assert_eq!(
            vacant,
            [TraceOff {
                from: None,
                until: None
            }]
        );
        assert_eq!(drawn_spans(&vacant, NOW, SAMPLES), []);
    }

    #[test]
    fn activity_moves_left_and_leaves_exactly_a_window_after_it_happened() {
        let activity = [
            at(AgentActivityKind::Tool, 0),
            at(AgentActivityKind::Turn, 0),
        ];
        let fresh = trace(&activity, NOW, SAMPLES);
        assert!((fresh.markers[0].x - 1.0).abs() < 1e-9);
        let halfway = trace(&activity, NOW + TRACE_WINDOW_MILLIS / 2, SAMPLES);
        assert!((halfway.markers[0].x - 0.5).abs() < 1e-9);
        let last = trace(&activity, NOW + TRACE_WINDOW_MILLIS - 1, SAMPLES);
        assert_eq!(last.markers.len(), 1);
        assert_eq!(last.curves.len(), 1);
        assert!(trace(&activity, NOW + TRACE_WINDOW_MILLIS, SAMPLES).is_empty());
    }

    #[test]
    fn three_runs_in_the_window_draw_three_areas_split_by_two_gaps() {
        use Occupancy::{Started, Vacant};
        let ago = |seconds: i64| NOW - seconds * 1_000;
        let off = off_intervals(
            [
                (ago(600), Started),
                (ago(480), Vacant),
                (ago(360), Started),
                (ago(240), Vacant),
                (ago(120), Started),
            ],
            true,
        );
        let spans = drawn_spans(&off, NOW, SAMPLES)
            .iter()
            .map(|span| ((span.from * 600.0).round(), (span.to * 600.0).round()))
            .collect::<Vec<_>>();
        assert_eq!(spans, [(0.0, 120.0), (240.0, 360.0), (480.0, 600.0)]);
    }

    #[test]
    fn completed_gaps_need_one_sample_spacing_but_a_current_gap_cuts_immediately() {
        let off = |from_ago: i64, until_ago: Option<i64>| TraceOff {
            from: Some(NOW - from_ago * 1_000),
            until: until_ago.map(|seconds| NOW - seconds * 1_000),
        };
        // Seconds into the window through which the curves draw.
        let seconds = |off: &[TraceOff]| {
            drawn_spans(off, NOW, SAMPLES)
                .iter()
                .map(|span| ((span.from * 600.0).round(), (span.to * 600.0).round()))
                .collect::<Vec<_>>()
        };
        assert_eq!(seconds(&[]), [(0.0, 600.0)]);
        // Nine samples space the ten-minute window every seventy-five seconds.
        assert_eq!(seconds(&[off(30, Some(0))]), [(0.0, 600.0)]);
        assert_eq!(seconds(&[off(30, None)]), [(0.0, 570.0)]);
        assert_eq!(
            seconds(&[
                off(900, Some(650)),
                off(700, Some(540)),
                off(400, Some(325)),
                off(200, Some(126)),
                off(100, None),
            ]),
            [(60.0, 200.0), (275.0, 500.0)]
        );
    }
}

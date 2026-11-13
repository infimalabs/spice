//! Raster-coordinate marks and editing history; no browser or wire-format state.

use std::rc::Rc;

#[cfg(any(target_arch = "wasm32", rust_analyzer, test))]
mod gesture;
#[cfg(any(target_arch = "wasm32", rust_analyzer))]
pub(crate) use gesture::{Edit, Held};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Point {
    pub(crate) x: i32,
    pub(crate) y: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Rectangle,
    Line,
    Arrow,
    Text,
}

#[cfg(any(target_arch = "wasm32", rust_analyzer, test))]
impl Kind {
    /// Lines and arrows run between their endpoints along a route.
    pub(crate) const fn routed(self) -> bool {
        matches!(self, Self::Line | Self::Arrow)
    }
}

#[cfg(any(target_arch = "wasm32", rust_analyzer))]
impl Kind {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Rectangle => "Rectangle",
            Self::Line => "Line",
            Self::Arrow => "Arrow",
            Self::Text => "Text",
        }
    }
}

/// How a line or arrow travels from its start to its end.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Route {
    #[default]
    Straight,
    /// Axis-aligned segments turning at the midpoint of the longer axis, or of
    /// the shorter axis when mirrored.
    Elbow,
    /// One quadratic bend bowing a quarter of the span off the chord, to its
    /// other side when mirrored.
    Curve,
}

#[cfg(any(target_arch = "wasm32", rust_analyzer, test))]
impl Route {
    /// Whether this route bends, so that mirroring it changes its course.
    pub(crate) const fn bends(self) -> bool {
        !matches!(self, Self::Straight)
    }
}

#[cfg(any(target_arch = "wasm32", rust_analyzer))]
impl Route {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Straight => "Straight",
            Self::Elbow => "Elbow",
            Self::Curve => "Curve",
        }
    }

    pub(crate) const fn next(self) -> Self {
        match self {
            Self::Straight => Self::Elbow,
            Self::Elbow => Self::Curve,
            Self::Curve => Self::Straight,
        }
    }
}

/// The drawn course of a mark in image pixels, shared by SVG and PNG export.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Course {
    Polyline(Vec<(f64, f64)>),
    Quadratic {
        start: (f64, f64),
        control: (f64, f64),
        end: (f64, f64),
    },
}

impl Course {
    /// SVG path data for this course.
    #[cfg(any(target_arch = "wasm32", rust_analyzer))]
    pub(crate) fn path_data(&self) -> String {
        match self {
            Self::Polyline(points) => points
                .iter()
                .enumerate()
                .map(|(index, (x, y))| format!("{}{x} {y}", if index == 0 { 'M' } else { 'L' }))
                .collect::<Vec<_>>()
                .join(" "),
            Self::Quadratic {
                start,
                control,
                end,
            } => format!(
                "M{} {} Q{} {} {} {}",
                start.0, start.1, control.0, control.1, end.0, end.1
            ),
        }
    }

    /// The point the course arrives at its end from, which aims an arrowhead.
    fn approach(&self) -> (f64, f64) {
        match self {
            Self::Polyline(points) => {
                let end = points[points.len() - 1];
                points
                    .iter()
                    .rev()
                    .copied()
                    .find(|point| *point != end)
                    .unwrap_or(end)
            }
            Self::Quadratic { control, .. } => *control,
        }
    }
}

/// The colours a mark can take. Each has a contrasting halo that keeps a text
/// label legible on light and dark images alike.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Color {
    #[default]
    Red,
    Yellow,
    Green,
    Blue,
    Black,
    White,
}

impl Color {
    pub(crate) const ALL: [Self; 6] = [
        Self::Red,
        Self::Yellow,
        Self::Green,
        Self::Blue,
        Self::Black,
        Self::White,
    ];

    #[cfg(any(target_arch = "wasm32", rust_analyzer))]
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Red => "Red",
            Self::Yellow => "Yellow",
            Self::Green => "Green",
            Self::Blue => "Blue",
            Self::Black => "Black",
            Self::White => "White",
        }
    }

    pub(crate) const fn ink(self) -> &'static str {
        match self {
            Self::Red => "#ef4444",
            Self::Yellow => "#facc15",
            Self::Green => "#22c55e",
            Self::Blue => "#3b82f6",
            Self::Black => "#111111",
            Self::White => "#ffffff",
        }
    }

    /// The outline drawn under a label so its ink always meets a contrasting
    /// edge, whatever the image beneath.
    pub(crate) const fn halo(self) -> &'static str {
        match self {
            Self::Yellow | Self::Green | Self::White => "#111111",
            Self::Red | Self::Blue | Self::Black => "#ffffff",
        }
    }
}

/// One mark. Rectangles span their two corners and routed marks run between
/// their endpoints; a text label is anchored at `start`, and `end` is the
/// opposite corner of its measured box so it moves and clamps like any mark.
/// A mirrored mark bends to the other side of its route.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Shape {
    pub(crate) kind: Kind,
    pub(crate) route: Route,
    pub(crate) mirrored: bool,
    pub(crate) color: Color,
    pub(crate) start: Point,
    pub(crate) end: Point,
    pub(crate) text: Rc<str>,
}

impl Shape {
    pub(crate) fn new(kind: Kind, route: Route, color: Color, start: Point, end: Point) -> Self {
        Self {
            kind,
            route,
            mirrored: false,
            color,
            start,
            end,
            text: Rc::from(""),
        }
    }

    /// An empty label anchored at its top-left corner, one text size tall.
    pub(crate) fn label(anchor: Point, color: Color, size: i32) -> Self {
        let mut label = Self::new(Kind::Text, Route::Straight, color, anchor, anchor);
        label.relabel(Rc::from(""), 0, size);
        label
    }

    /// Give a label new text whose box is `width` wide and `size` tall.
    pub(crate) fn relabel(&mut self, text: Rc<str>, width: i32, size: i32) {
        self.end = Point {
            x: self.start.x + width.max(1),
            y: self.start.y + size,
        };
        self.text = text;
    }

    pub(crate) fn bounds(&self) -> (Point, Point) {
        (
            Point {
                x: self.start.x.min(self.end.x),
                y: self.start.y.min(self.end.y),
            },
            Point {
                x: self.start.x.max(self.end.x),
                y: self.start.y.max(self.end.y),
            },
        )
    }

    pub(crate) fn translated(&self, dx: i32, dy: i32, width: i32, height: i32) -> Self {
        let (low, high) = self.bounds();
        let dx = dx.clamp(-low.x, (width - high.x).max(-low.x));
        let dy = dy.clamp(-low.y, (height - high.y).max(-low.y));
        Self {
            start: Point {
                x: self.start.x + dx,
                y: self.start.y + dy,
            },
            end: Point {
                x: self.end.x + dx,
                y: self.end.y + dy,
            },
            ..self.clone()
        }
    }

    pub(crate) fn visible(&self) -> bool {
        match self.kind {
            Kind::Rectangle => self.start.x != self.end.x && self.start.y != self.end.y,
            Kind::Line | Kind::Arrow => self.start != self.end,
            Kind::Text => !self.text.trim().is_empty(),
        }
    }

    /// The routed course from start to end.
    pub(crate) fn course(&self) -> Course {
        let start = (f64::from(self.start.x), f64::from(self.start.y));
        let end = (f64::from(self.end.x), f64::from(self.end.y));
        let (dx, dy) = (end.0 - start.0, end.1 - start.1);
        match self.route {
            Route::Straight => Course::Polyline(vec![start, end]),
            Route::Elbow if (dx.abs() >= dy.abs()) != self.mirrored => {
                let middle = start.0 + dx / 2.0;
                Course::Polyline(vec![start, (middle, start.1), (middle, end.1), end])
            }
            Route::Elbow => {
                let middle = start.1 + dy / 2.0;
                Course::Polyline(vec![start, (start.0, middle), (end.0, middle), end])
            }
            Route::Curve => {
                let side = if self.mirrored { -1.0 } else { 1.0 };
                Course::Quadratic {
                    start,
                    control: (
                        start.0 + dx / 2.0 - side * dy / 4.0,
                        start.1 + dy / 2.0 + side * dx / 4.0,
                    ),
                    end,
                }
            }
        }
    }

    /// The same arrowhead geometry is used by SVG presentation and PNG export;
    /// it points along the direction the course arrives at its end.
    pub(crate) fn arrowhead(&self, stroke: f64) -> [(f64, f64); 3] {
        let (x, y) = (f64::from(self.end.x), f64::from(self.end.y));
        let (from_x, from_y) = self.course().approach();
        let dx = x - from_x;
        let dy = y - from_y;
        let distance = dx.hypot(dy);
        let length = (stroke * 4.0).min(distance);
        let angle = dy.atan2(dx);
        [
            (x, y),
            (
                x - length * (angle - 0.45).cos(),
                y - length * (angle - 0.45).sin(),
            ),
            (
                x - length * (angle + 0.45).cos(),
                y - length * (angle + 0.45).sin(),
            ),
        ]
    }
}

/// The pointer position projected onto the nearest 45-degree ray from `start`.
pub(crate) fn snapped(start: Point, pointer: Point) -> Point {
    let dx = f64::from(pointer.x - start.x);
    let dy = f64::from(pointer.y - start.y);
    let step = std::f64::consts::FRAC_PI_4;
    let angle = (dy.atan2(dx) / step).round() * step;
    let (sin, cos) = angle.sin_cos();
    let reach = dx * cos + dy * sin;
    Point {
        x: start.x + (reach * cos).round() as i32,
        y: start.y + (reach * sin).round() as i32,
    }
}

pub(crate) fn stroke_width(width: u32, height: u32) -> f64 {
    (f64::from(width.max(height)) / 250.0).clamp(3.0, 14.0)
}

/// Label text size in image pixels, proportional to the stroke.
#[cfg(any(target_arch = "wasm32", rust_analyzer))]
pub(crate) fn text_size(width: u32, height: u32) -> i32 {
    (stroke_width(width, height) * 7.0).round() as i32
}

#[derive(Default, PartialEq, Eq)]
pub(crate) struct Editor {
    pub(crate) shapes: Vec<Shape>,
    pub(crate) selected: Option<usize>,
    undo: Vec<Vec<Shape>>,
    redo: Vec<Vec<Shape>>,
}

impl Editor {
    pub(crate) fn new(shapes: Vec<Shape>) -> Self {
        Self {
            shapes,
            ..Self::default()
        }
    }

    pub(crate) fn commit(&mut self, before: Vec<Shape>) {
        if self.shapes != before {
            self.undo.push(before);
            self.redo.clear();
        }
    }

    pub(crate) fn differs_from(&self, baseline: &[Shape]) -> bool {
        self.shapes != baseline
    }

    pub(crate) fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub(crate) fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub(crate) fn undo(&mut self) {
        if let Some(previous) = self.undo.pop() {
            self.redo
                .push(std::mem::replace(&mut self.shapes, previous));
            self.selected = None;
        }
    }

    pub(crate) fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.shapes, next));
            self.selected = None;
        }
    }

    pub(crate) fn delete(&mut self) {
        if let Some(index) = self.selected.take() {
            let before = self.shapes.clone();
            self.shapes.remove(index);
            self.commit(before);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark(kind: Kind, start: (i32, i32), end: (i32, i32)) -> Shape {
        Shape::new(
            kind,
            Route::Straight,
            Color::Red,
            Point {
                x: start.0,
                y: start.1,
            },
            Point { x: end.0, y: end.1 },
        )
    }

    #[test]
    fn reversed_marks_move_without_crossing_image_edges() {
        for kind in [Kind::Rectangle, Kind::Line, Kind::Arrow] {
            let shape = mark(kind, (90, 70), (20, 10));
            let moved = shape.translated(100, -100, 120, 80);
            assert_eq!(moved.start, Point { x: 120, y: 60 });
            assert_eq!(moved.end, Point { x: 50, y: 0 });
            assert_eq!(moved.end.x - moved.start.x, shape.end.x - shape.start.x);
            assert_eq!(moved.end.y - moved.start.y, shape.end.y - shape.start.y);
        }
        // A label wider than the image pins to the left edge instead of
        // failing to clamp.
        let mut wide = Shape::label(Point { x: 30, y: 10 }, Color::Red, 20);
        wide.relabel(Rc::from("wide"), 200, 20);
        assert_eq!(wide.translated(5, 0, 120, 80).start.x, 0);
    }

    #[test]
    fn gesture_history_delete_and_new_edit_preserve_only_reachable_states() {
        let rectangle = mark(Kind::Rectangle, (10, 10), (50, 40));
        let arrow = mark(Kind::Arrow, (60, 20), (80, 50));
        let baseline = vec![rectangle.clone()];
        let mut editor = Editor::new(baseline.clone());
        assert!(!editor.differs_from(&baseline));
        let before = editor.shapes.clone();
        editor.shapes.push(arrow.clone());
        editor.commit(before);
        assert!(editor.can_undo());
        assert!(editor.differs_from(&baseline));
        editor.undo();
        assert_eq!(editor.shapes, std::slice::from_ref(&rectangle));
        assert!(!editor.differs_from(&baseline));
        editor.redo();
        assert_eq!(editor.shapes, [rectangle.clone(), arrow.clone()]);
        assert!(editor.differs_from(&baseline));
        editor.selected = Some(0);
        editor.delete();
        assert_eq!(editor.shapes, std::slice::from_ref(&arrow));
        editor.undo();
        assert_eq!(editor.shapes, [rectangle.clone(), arrow.clone()]);
        let before = editor.shapes.clone();
        editor.shapes[1].end = Point { x: 100, y: 70 };
        editor.commit(before);
        assert!(!editor.can_redo());
        editor.undo();
        assert_eq!(editor.shapes, [rectangle, arrow]);
    }
}

#[cfg(test)]
mod boundaries {
    use super::*;

    #[test]
    fn degenerate_marks_are_discarded_and_short_arrows_have_bounded_heads() {
        let point = Point { x: 10, y: 20 };
        for kind in [Kind::Rectangle, Kind::Line, Kind::Arrow] {
            assert!(!Shape::new(kind, Route::Straight, Color::Red, point, point).visible());
        }
        assert!(
            !Shape::new(
                Kind::Rectangle,
                Route::Straight,
                Color::Red,
                point,
                Point { x: 10, y: 80 }
            )
            .visible()
        );
        let mut blank = Shape::label(point, Color::Red, 10);
        blank.relabel(Rc::from("  "), 10, 10);
        assert!(!blank.visible());
        let arrow = Shape::new(
            Kind::Arrow,
            Route::Straight,
            Color::Red,
            point,
            Point { x: 12, y: 22 },
        );
        assert!(arrow.visible());
        let head = arrow.arrowhead(stroke_width(1200, 800));
        assert_eq!(head[0], (12.0, 22.0));
        for (x, y) in head {
            assert!((x - 12.0).hypot(y - 22.0) <= 2.0_f64.sqrt() * 2.0 + f64::EPSILON * 16.0);
        }
    }

    /// Elbows run only along the axes and arrive along their final segment,
    /// and every route aims its arrowhead the way the course arrives.
    #[test]
    fn routes_stay_on_their_geometry_and_heads_follow_the_arrival() {
        let start = Point { x: 10, y: 10 };
        for end in [
            Point { x: 110, y: 40 },
            Point { x: 30, y: 210 },
            Point { x: 10, y: 90 },
        ] {
            let elbow = Shape::new(Kind::Arrow, Route::Elbow, Color::Red, start, end);
            let Course::Polyline(points) = elbow.course() else {
                panic!("an elbow is a polyline");
            };
            assert_eq!(points.first(), Some(&(10.0, 10.0)));
            assert_eq!(points.last(), Some(&(f64::from(end.x), f64::from(end.y))));
            for pair in points.windows(2) {
                assert!(pair[0].0 == pair[1].0 || pair[0].1 == pair[1].1);
            }
            let head = elbow.arrowhead(4.0);
            let tail = points
                .iter()
                .rev()
                .find(|point| **point != points[points.len() - 1])
                .unwrap();
            let arrival = (f64::from(end.x) - tail.0, f64::from(end.y) - tail.1);
            let barbs = (
                (head[1].0 + head[2].0) / 2.0 - head[0].0,
                (head[1].1 + head[2].1) / 2.0 - head[0].1,
            );
            // The barbs sit behind the tip, against the direction of arrival.
            assert!(arrival.0 * barbs.0 + arrival.1 * barbs.1 < 0.0);
            assert!((arrival.0 * barbs.1 - arrival.1 * barbs.0).abs() < 1e-9);
        }
        let curve = Shape::new(
            Kind::Arrow,
            Route::Curve,
            Color::Red,
            start,
            Point { x: 110, y: 10 },
        );
        let Course::Quadratic { control, .. } = curve.course() else {
            panic!("a curve is quadratic");
        };
        assert_eq!(control, (60.0, 35.0));
    }

    #[test]
    fn shift_snaps_to_the_nearest_eighth_turn() {
        let start = Point { x: 100, y: 100 };
        assert_eq!(
            snapped(start, Point { x: 180, y: 108 }),
            Point { x: 180, y: 100 }
        );
        assert_eq!(
            snapped(start, Point { x: 104, y: 20 }),
            Point { x: 100, y: 20 }
        );
        let diagonal = snapped(start, Point { x: 150, y: 160 });
        assert_eq!(diagonal.x - start.x, diagonal.y - start.y);
        assert_eq!(snapped(start, start), start);
    }

    /// Every palette ink meets its label halo at least at the WCAG contrast
    /// for large text, so a label reads on any image; and every ink is a
    /// distinct colour.
    #[test]
    fn every_ink_contrasts_with_its_label_halo() {
        fn luminance(hex: &str) -> f64 {
            let channel = |at: usize| {
                let value = f64::from(u8::from_str_radix(&hex[at..at + 2], 16).unwrap()) / 255.0;
                if value <= 0.04045 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(1) + 0.7152 * channel(3) + 0.0722 * channel(5)
        }
        for color in Color::ALL {
            let (ink, halo) = (luminance(color.ink()), luminance(color.halo()));
            let ratio = (ink.max(halo) + 0.05) / (ink.min(halo) + 0.05);
            assert!(ratio >= 3.0, "{color:?} ink meets its halo at {ratio:.2}:1");
        }
        let mut inks = Color::ALL.map(Color::ink).to_vec();
        inks.sort_unstable();
        inks.dedup();
        assert_eq!(inks.len(), Color::ALL.len());
    }
}

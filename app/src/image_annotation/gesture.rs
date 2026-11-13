//! One pointer gesture on the annotation canvas, replayed from the marks before
//! it for where the pointer is and which modifier keys are held now, so a
//! modifier pressed or released mid-gesture changes the mark at once.

use super::{Color, Kind, Point, Route, Shape, snapped};

/// The modifier keys held during a gesture. `command` is Command or Control:
/// platforms without a Command key give Control the same role.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Held {
    pub(crate) shift: bool,
    pub(crate) option: bool,
    pub(crate) command: bool,
}

/// What a gesture changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Edit {
    /// A new mark from the press, starting on the toolbar's route.
    Draw {
        kind: Kind,
        route: Route,
        color: Color,
    },
    /// Drag one mark's body.
    Move(usize),
    /// Drag one mark's start.
    Start(usize),
    /// Drag one mark's end.
    End(usize),
}

impl Edit {
    /// Make `shapes` the marks `before` this gesture as changed by a press at
    /// `press` now at `point`, inside an image of `size`, and return the mark
    /// the gesture leaves selected.
    ///
    /// A line or arrow takes an elbow under Option and a curve under Command.
    /// Shift snaps a straight one to 45 degrees and mirrors a bent one from how
    /// it bent before the gesture. Shift squares a rectangle, and Option draws
    /// a new one from its centre. A moved mark travels only along its longer
    /// axis under Shift, and under Option a copy moves and the original stays.
    pub(crate) fn apply(
        self,
        before: &[Shape],
        shapes: &mut Vec<Shape>,
        press: Point,
        point: Point,
        held: Held,
        size: (i32, i32),
    ) -> usize {
        shapes.clear();
        shapes.extend_from_slice(before);
        match self {
            Self::Draw { kind, route, color } => {
                let mut shape = Shape::new(kind, route, color, press, press);
                if kind.routed() {
                    shape.route = routed(held, route);
                    shape.mirrored = held.shift && shape.route.bends();
                    shape.end = steered(&shape, held, press, point);
                } else {
                    (shape.start, shape.end) = framed(press, point, held, size);
                }
                shapes.push(shape);
                before.len()
            }
            Self::Start(index) | Self::End(index) => {
                let original = &before[index];
                let start = matches!(self, Self::Start(_));
                let anchor = if start { original.end } else { original.start };
                let shape = &mut shapes[index];
                let at = if shape.kind.routed() {
                    shape.route = routed(held, original.route);
                    shape.mirrored = original.mirrored != (held.shift && shape.route.bends());
                    steered(shape, held, anchor, point)
                } else if shape.kind == Kind::Rectangle {
                    let square = Held {
                        option: false,
                        ..held
                    };
                    framed(anchor, point, square, size).1
                } else {
                    point
                };
                if start {
                    shape.start = at;
                } else {
                    shape.end = at;
                }
                index
            }
            Self::Move(index) => {
                let (mut dx, mut dy) = (point.x - press.x, point.y - press.y);
                if held.shift {
                    if dx.abs() >= dy.abs() {
                        dy = 0;
                    } else {
                        dx = 0;
                    }
                }
                let moved = before[index].translated(dx, dy, size.0, size.1);
                if held.option && moved != before[index] {
                    shapes.push(moved);
                    before.len()
                } else {
                    shapes[index] = moved;
                    index
                }
            }
        }
    }
}

/// Option turns elbows and Command bends a curve; otherwise `chosen` applies.
fn routed(held: Held, chosen: Route) -> Route {
    if held.option {
        Route::Elbow
    } else if held.command {
        Route::Curve
    } else {
        chosen
    }
}

/// Shift holds a straight line or arrow to 45-degree steps from its fixed end.
fn steered(shape: &Shape, held: Held, anchor: Point, point: Point) -> Point {
    if held.shift && !shape.route.bends() {
        snapped(anchor, point)
    } else {
        point
    }
}

/// The corners of a rectangle pulled from `anchor` toward `point`: square
/// under Shift, centred on `anchor` under Option, and inside the image.
fn framed(anchor: Point, point: Point, held: Held, size: (i32, i32)) -> (Point, Point) {
    let room = |delta: i32, at: i32, extent: i32| {
        let ahead = if delta >= 0 { extent - at } else { at };
        let behind = extent - ahead;
        if held.option {
            ahead.min(behind)
        } else {
            ahead
        }
        .max(0)
    };
    let (dx, dy) = (point.x - anchor.x, point.y - anchor.y);
    let (room_x, room_y) = (room(dx, anchor.x, size.0), room(dy, anchor.y, size.1));
    let (width, height) = if held.shift {
        let side = dx.abs().max(dy.abs()).min(room_x).min(room_y);
        (side, side)
    } else {
        (dx.abs().min(room_x), dy.abs().min(room_y))
    };
    let (dx, dy) = (
        if dx < 0 { -width } else { width },
        if dy < 0 { -height } else { height },
    );
    let start = if held.option {
        Point {
            x: anchor.x - dx,
            y: anchor.y - dy,
        }
    } else {
        anchor
    };
    (
        start,
        Point {
            x: anchor.x + dx,
            y: anchor.y + dy,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::super::Course;
    use super::*;

    const SIZE: (i32, i32) = (200, 100);

    fn at(x: i32, y: i32) -> Point {
        Point { x, y }
    }

    fn held(shift: bool, option: bool, command: bool) -> Held {
        Held {
            shift,
            option,
            command,
        }
    }

    fn draw(kind: Kind, route: Route, press: Point, point: Point, keys: Held) -> Shape {
        let mut shapes = Vec::new();
        let edit = Edit::Draw {
            kind,
            route,
            color: Color::Red,
        };
        let index = edit.apply(&[], &mut shapes, press, point, keys, SIZE);
        shapes.swap_remove(index)
    }

    fn control(shape: &Shape) -> (f64, f64) {
        let Course::Quadratic { control, .. } = shape.course() else {
            panic!("a curve is quadratic");
        };
        control
    }

    #[test]
    fn shift_mirrors_a_bent_mark_and_snaps_a_straight_one() {
        let (press, point) = (at(10, 10), at(110, 14));
        // Command curves below this chord; Shift bows it the same distance above.
        let curve = draw(
            Kind::Arrow,
            Route::Straight,
            press,
            point,
            held(false, false, true),
        );
        let mirrored = draw(
            Kind::Arrow,
            Route::Straight,
            press,
            point,
            held(true, false, true),
        );
        assert_eq!(mirrored.route, Route::Curve);
        let (below, above) = (control(&curve), control(&mirrored));
        let middle = (
            f64::from(press.x + point.x) / 2.0,
            f64::from(press.y + point.y) / 2.0,
        );
        assert!(below.1 > middle.1 && above.1 < middle.1);
        // The two control points sit symmetrically about the chord's midpoint.
        assert!(
            ((below.0 + above.0) / 2.0 - middle.0).abs() < 1e-9
                && ((below.1 + above.1) / 2.0 - middle.1).abs() < 1e-9
        );
        // Along this wide chord an elbow leaves horizontally; mirrored, it
        // leaves vertically and turns at the middle of the short axis.
        let elbow = draw(
            Kind::Line,
            Route::Elbow,
            press,
            point,
            held(true, false, false),
        );
        let Course::Polyline(points) = elbow.course() else {
            panic!("an elbow is a polyline");
        };
        assert_eq!(points[1], (10.0, 12.0));
        // A straight mark ignores mirroring and snaps.
        let straight = draw(
            Kind::Line,
            Route::Straight,
            press,
            point,
            held(true, false, false),
        );
        assert!(!straight.mirrored);
        assert_eq!(straight.end, at(110, 10));

        // Shift while dragging an end flips the mark from how it already bent,
        // and releasing Shift mid-drag restores that bend.
        let before = [mirrored];
        let mut shapes = Vec::new();
        let flipped = Edit::End(0).apply(
            &before,
            &mut shapes,
            point,
            at(120, 20),
            held(true, false, false),
            SIZE,
        );
        assert!(!shapes[flipped].mirrored);
        Edit::End(0).apply(
            &before,
            &mut shapes,
            point,
            at(120, 20),
            Held::default(),
            SIZE,
        );
        assert!(shapes[0].mirrored && shapes[0].route == Route::Curve);
        assert_eq!(shapes[0].end, at(120, 20));
    }

    #[test]
    fn rectangles_square_and_centre_without_leaving_the_image() {
        let square = draw(
            Kind::Rectangle,
            Route::Straight,
            at(20, 20),
            at(60, 30),
            held(true, false, false),
        );
        assert_eq!((square.start, square.end), (at(20, 20), at(60, 60)));
        let centred = draw(
            Kind::Rectangle,
            Route::Straight,
            at(50, 50),
            at(70, 60),
            held(false, true, false),
        );
        assert_eq!((centred.start, centred.end), (at(30, 40), at(70, 60)));
        // Centred near an edge, the mirrored corner stops at the image and the
        // square shrinks to fit both rooms.
        let edge = draw(
            Kind::Rectangle,
            Route::Straight,
            at(10, 50),
            at(60, 90),
            held(true, true, false),
        );
        assert_eq!((edge.start, edge.end), (at(0, 40), at(20, 60)));
        // Shift squares a resize against the opposite corner, within the image.
        let before = [square];
        let mut shapes = Vec::new();
        Edit::Start(0).apply(
            &before,
            &mut shapes,
            at(20, 20),
            at(-30, 50),
            held(true, false, false),
            SIZE,
        );
        assert_eq!((shapes[0].start, shapes[0].end), (at(0, 0), at(60, 60)));
    }

    #[test]
    fn moves_lock_to_one_axis_and_option_moves_a_copy() {
        let before = [Shape::new(
            Kind::Rectangle,
            Route::Straight,
            Color::Red,
            at(10, 10),
            at(30, 30),
        )];
        let mut shapes = Vec::new();
        let locked = Edit::Move(0).apply(
            &before,
            &mut shapes,
            at(20, 20),
            at(50, 28),
            held(true, false, false),
            SIZE,
        );
        assert_eq!((locked, shapes[0].start), (0, at(40, 10)));
        let copy = Edit::Move(0).apply(
            &before,
            &mut shapes,
            at(20, 20),
            at(50, 28),
            held(false, true, false),
            SIZE,
        );
        assert_eq!(copy, 1);
        assert_eq!(shapes[0], before[0]);
        assert_eq!(shapes[1].start, at(40, 18));
        // Releasing Option mid-drag drops the copy and moves the original.
        Edit::Move(0).apply(
            &before,
            &mut shapes,
            at(20, 20),
            at(50, 28),
            Held::default(),
            SIZE,
        );
        assert_eq!(shapes.len(), 1);
        // An Option press that never moves adds no copy.
        Edit::Move(0).apply(
            &before,
            &mut shapes,
            at(20, 20),
            at(20, 20),
            held(false, true, false),
            SIZE,
        );
        assert_eq!(shapes, before);
    }
}

//! Product-branded browser title and favicon state.
//!
//! The icon keeps the spice pepper mark in its upper-left corner and
//! gives the changing state one large lower-right badge, so a count stays
//! readable at the 16px tab size. The badge counts one kind of work, never
//! both: open execution work on the execution rail's green while there is any,
//! else open Design work on the Design rail's plum, so a glance at the tab
//! tells running from planning and by how much. Counts are exact through 99;
//! larger values use `99+` in the tiny icon while the title retains the exact
//! total. A current connection with nothing open shows the pepper alone, with
//! no badge.

use crate::work::{WorkCounts, WorkMark};

/// The compact brand: short, lowercase, and last, so the tab's few visible
/// characters carry the changing state first.
const BRAND: &str = "spice";
const MAX_EXACT_BADGE_COUNT: usize = 99;
/// Badge rectangle width in icon units; every label must fit inside it.
const BADGE_WIDTH: usize = 49;
/// Font size and baseline in icon units for one-, two-, and three-character labels.
const LABEL_METRICS: [(usize, usize); 3] = [(42, 57), (34, 54), (24, 51)];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConnectionState {
    Starting,
    Connecting,
    Online,
    Offline,
}

/// The journal cursor the connection label's hover names while that connection
/// proves the retained app runtime is current: the integer position first,
/// then its condensed sequence code in parentheses, the one place the console
/// leads with the integer because it is what resident and CLI readers report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct JournalCursor {
    pub(crate) value: Option<String>,
    pub(crate) accessible: String,
}

impl JournalCursor {
    #[must_use]
    pub(crate) fn resolve(connection: ConnectionState, cursor: Option<wire::Seq>) -> Option<Self> {
        if connection != ConnectionState::Online {
            return None;
        }
        let value = cursor.map(|cursor| {
            format!(
                "{} ({})",
                cursor.get(),
                wire::SequenceCode::new(cursor.get())
            )
        });
        Some(Self {
            accessible: value.as_ref().map_or_else(
                || "journal cursor not yet admitted".to_owned(),
                |cursor| format!("journal cursor {cursor}"),
            ),
            value,
        })
    }
}

/// One global Work rail's total as its command-bar tab shows it: the open
/// count and the mark dot, if any.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RailTotal {
    pub(crate) open: usize,
    pub(crate) mark: Option<WorkMark>,
}

impl From<WorkCounts> for RailTotal {
    fn from(counts: WorkCounts) -> Self {
        Self {
            open: counts.open(),
            mark: counts.mark().map(|(mark, _)| mark),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkKind {
    Execution,
    Design,
}

/// The small dot riding the badge's upper-right rim: the shown rail's own
/// mark, or the Spice button's waiting-message dot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Dot {
    Mark(WorkMark),
    Inbox,
}

impl Dot {
    /// The dark theme's colour for the same dot on the rail tab or the Spice
    /// button; the icon always draws on the dark ground.
    #[cfg(any(target_arch = "wasm32", rust_analyzer))]
    const fn color(self) -> &'static str {
        match self {
            Self::Mark(WorkMark::Blocked) => "#ee8f88",
            Self::Mark(WorkMark::Decision | WorkMark::Deferred) => "#e6b44e",
            Self::Mark(WorkMark::Pass) => "#74c69d",
            Self::Inbox => "#a8b4a8",
        }
    }
}

/// What the tab counts: one kind of work and its dot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TabWork {
    kind: WorkKind,
    count: usize,
    dot: Option<Dot>,
}

impl TabWork {
    /// Execution work while any is open, else Design work, even when that is
    /// none; the shown rail's mark outranks a waiting message for the dot.
    #[must_use]
    pub(crate) fn select(execution: RailTotal, design: RailTotal, inbox_waiting: bool) -> Self {
        let (kind, rail) = if execution.open > 0 {
            (WorkKind::Execution, execution)
        } else {
            (WorkKind::Design, design)
        };
        Self {
            kind,
            count: rail.open,
            dot: rail
                .mark
                .map(Dot::Mark)
                .or(inbox_waiting.then_some(Dot::Inbox)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CountState {
    Loading,
    Current(TabWork),
    Stale(TabWork),
    Disconnected,
}

impl CountState {
    #[must_use]
    pub(crate) const fn resolve(connection: ConnectionState, work: Option<TabWork>) -> Self {
        match (connection, work) {
            (ConnectionState::Online, Some(work)) => Self::Current(work),
            (ConnectionState::Online, None)
            | (ConnectionState::Starting, None)
            | (ConnectionState::Connecting, None) => Self::Loading,
            (ConnectionState::Offline, None) => Self::Disconnected,
            (
                ConnectionState::Starting | ConnectionState::Connecting | ConnectionState::Offline,
                Some(work),
            ) => Self::Stale(work),
        }
    }
}

/// Badge colour: the shown kind's rail tint while current, amber stale, red
/// disconnected, grey loading.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Tone {
    Loading,
    Execution,
    Design,
    Stale,
    Disconnected,
}

impl Tone {
    #[cfg(any(target_arch = "wasm32", rust_analyzer))]
    const fn color(self) -> &'static str {
        match self {
            Self::Loading => "#9aa29b",
            // The dark theme's execution `--accent` green and Design
            // `--accent-5` plum, as the global Work rails wear them.
            Self::Execution => "#74c69d",
            Self::Design => "#d1a3ff",
            Self::Stale => "#e8b84e",
            Self::Disconnected => "#ef7358",
        }
    }
}

/// The lower-right badge: its label and colour.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Badge {
    label: String,
    tone: Tone,
}

impl Badge {
    fn new(label: impl Into<String>, tone: Tone) -> Self {
        Self {
            label: label.into(),
            tone,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BrowserBranding {
    title: String,
    /// Absent when a current connection shows no open work.
    badge: Option<Badge>,
    dot: Option<Dot>,
}

impl BrowserBranding {
    #[must_use]
    pub(crate) fn for_state(state: CountState) -> Self {
        let (title, badge, dot) = match state {
            CountState::Loading => (
                format!("Loading - {BRAND}"),
                Some(Badge::new("…", Tone::Loading)),
                None,
            ),
            CountState::Disconnected => (
                format!("Disconnected - {BRAND}"),
                Some(Badge::new("!", Tone::Disconnected)),
                None,
            ),
            CountState::Current(work) if work.count == 0 => {
                (format!("No open work - {BRAND}"), None, work.dot)
            }
            CountState::Current(work) => (
                remaining_title(work, false),
                Some(Badge::new(
                    badge_count(work.count),
                    match work.kind {
                        WorkKind::Execution => Tone::Execution,
                        WorkKind::Design => Tone::Design,
                    },
                )),
                work.dot,
            ),
            CountState::Stale(work) => (
                remaining_title(work, true),
                Some(Badge::new(badge_count(work.count), Tone::Stale)),
                work.dot,
            ),
        };
        Self { title, badge, dot }
    }

    #[must_use]
    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    /// Standalone SVG document for the favicon.
    #[cfg(any(target_arch = "wasm32", rust_analyzer))]
    #[must_use]
    pub(crate) fn icon(&self) -> String {
        let Some(badge) = &self.badge else {
            // Nothing open: the pepper alone, large and centred, with the
            // waiting-message dot in the upper-right corner.
            let dot = self.dot.map_or_else(String::new, |dot| {
                format!(
                    "<circle cx=\"52\" cy=\"12\" r=\"7.5\" fill=\"{}\" stroke=\"#101411\" stroke-width=\"3\"/>",
                    dot.color()
                )
            });
            return format!(
                concat!(
                    "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 64 64\">",
                    "<rect width=\"64\" height=\"64\" rx=\"14\" fill=\"#101411\"/>",
                    "<text x=\"32\" y=\"47\" text-anchor=\"middle\" font-family=\"Apple Color Emoji,Segoe UI Emoji,sans-serif\" font-size=\"44\">🌶️</text>",
                    "{dot}",
                    "</svg>"
                ),
                dot = dot
            );
        };
        let (font_size, baseline) = label_metrics(&badge.label);
        // The dot sits on the badge's upper-right rim, ringed in the ground
        // colour so it reads apart from the badge at 16px.
        let dot = self.dot.map_or_else(String::new, |dot| {
            format!(
                "<circle cx=\"55\" cy=\"24\" r=\"7.5\" fill=\"{}\" stroke=\"#101411\" stroke-width=\"3\"/>",
                dot.color()
            )
        });
        format!(
            concat!(
                "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 64 64\">",
                "<rect width=\"64\" height=\"64\" rx=\"14\" fill=\"#101411\"/>",
                "<text x=\"16\" y=\"29\" text-anchor=\"middle\" font-family=\"Apple Color Emoji,Segoe UI Emoji,sans-serif\" font-size=\"28\">🌶️</text>",
                "<rect x=\"13.5\" y=\"21.5\" width=\"{width}\" height=\"41\" rx=\"13\" fill=\"{color}\" stroke=\"#101411\" stroke-width=\"3\"/>",
                "<text x=\"38\" y=\"{baseline}\" text-anchor=\"middle\" font-family=\"ui-monospace,SFMono-Regular,Menlo,monospace\" font-size=\"{font_size}\" font-weight=\"800\" fill=\"#101411\">{label}</text>",
                "{dot}",
                "</svg>"
            ),
            width = BADGE_WIDTH,
            color = badge.tone.color(),
            baseline = baseline,
            font_size = font_size,
            label = badge.label,
            dot = dot
        )
    }
}

/// The shown kind and its exact count, so the tab never rests on colour.
fn remaining_title(work: TabWork, stale: bool) -> String {
    let count = work.count;
    let noun = match (work.kind, count) {
        (WorkKind::Execution, 1) => "task",
        (WorkKind::Execution, _) => "tasks",
        (WorkKind::Design, 1) => "design",
        (WorkKind::Design, _) => "designs",
    };
    let suffix = if stale { " (stale)" } else { "" };
    format!("{count} {noun}{suffix} - {BRAND}")
}

fn badge_count(count: usize) -> String {
    if count > MAX_EXACT_BADGE_COUNT {
        format!("{MAX_EXACT_BADGE_COUNT}+")
    } else {
        count.to_string()
    }
}

/// Largest font and its baseline for a badge label of this many characters.
fn label_metrics(label: &str) -> (usize, usize) {
    let characters = label.chars().count().clamp(1, LABEL_METRICS.len());
    LABEL_METRICS[characters - 1]
}

#[cfg(any(target_arch = "wasm32", rust_analyzer))]
#[yew::hook]
pub(crate) fn use_browser_branding(state: CountState) {
    use yew::prelude::use_effect_with;

    use_effect_with(state, |state| apply(&BrowserBranding::for_state(*state)));
}

#[cfg(any(target_arch = "wasm32", rust_analyzer))]
fn apply(branding: &BrowserBranding) {
    use base64::Engine as _;

    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    document.set_title(branding.title());
    let icon = document
        .query_selector("link[data-spice-app-icon]")
        .ok()
        .flatten()
        .or_else(|| {
            let icon = document.create_element("link").ok()?;
            icon.set_attribute("data-spice-app-icon", "").ok()?;
            document
                .query_selector("head")
                .ok()
                .flatten()?
                .append_child(&icon)
                .ok()?;
            Some(icon)
        });
    let Some(icon) = icon else {
        return;
    };
    let encoded = base64::engine::general_purpose::STANDARD.encode(branding.icon().as_bytes());
    let _ = icon.set_attribute("rel", "icon");
    let _ = icon.set_attribute("type", "image/svg+xml");
    let _ = icon.set_attribute("href", &format!("data:image/svg+xml;base64,{encoded}"));
}

#[cfg(test)]
mod tests {
    use super::*;
    const KNOWN_COUNT: usize = 7;
    const LARGE_TITLE_COUNT: usize = 1_234;
    /// Monospace advance as a percentage of the font size.
    const MONOSPACE_ADVANCE_PERCENT: usize = 60;
    const PERCENT: usize = 100;

    const fn rail(open: usize, mark: Option<WorkMark>) -> RailTotal {
        RailTotal { open, mark }
    }

    const fn execution(count: usize) -> TabWork {
        TabWork {
            kind: WorkKind::Execution,
            count,
            dot: None,
        }
    }

    const fn design(count: usize) -> TabWork {
        TabWork {
            kind: WorkKind::Design,
            count,
            dot: None,
        }
    }

    fn badge(branding: &BrowserBranding) -> Option<(&str, Tone)> {
        branding
            .badge
            .as_ref()
            .map(|badge| (badge.label.as_str(), badge.tone))
    }

    #[test]
    fn connection_and_snapshot_state_define_freshness() {
        let work = execution(KNOWN_COUNT);
        assert_eq!(
            CountState::resolve(ConnectionState::Starting, None),
            CountState::Loading
        );
        assert_eq!(
            CountState::resolve(ConnectionState::Connecting, Some(work)),
            CountState::Stale(work)
        );
        assert_eq!(
            CountState::resolve(ConnectionState::Online, Some(work)),
            CountState::Current(work)
        );
        assert_eq!(
            CountState::resolve(ConnectionState::Offline, Some(work)),
            CountState::Stale(work)
        );
        assert_eq!(
            CountState::resolve(ConnectionState::Offline, None),
            CountState::Disconnected
        );
    }

    #[test]
    fn the_tab_counts_open_execution_work_and_else_design_work_never_their_sum() {
        let running = TabWork::select(rail(3, None), rail(5, None), false);
        assert_eq!(running, execution(3), "execution wins; Design is not added");
        let branding = BrowserBranding::for_state(CountState::Current(running));
        assert_eq!(badge(&branding), Some(("3", Tone::Execution)));

        let planning = TabWork::select(rail(0, None), rail(5, None), false);
        assert_eq!(planning, design(5));
        let branding = BrowserBranding::for_state(CountState::Current(planning));
        assert_eq!(badge(&branding), Some(("5", Tone::Design)));
    }

    #[test]
    fn only_a_current_empty_board_drops_the_badge() {
        let idle = TabWork::select(rail(0, None), rail(0, None), true);
        let branding = BrowserBranding::for_state(CountState::Current(idle));
        assert_eq!(badge(&branding), None);
        assert_eq!(
            branding.dot,
            Some(Dot::Inbox),
            "a waiting message still shows"
        );
        assert_eq!(
            badge(&BrowserBranding::for_state(CountState::Stale(idle))),
            Some(("0", Tone::Stale)),
            "a stale empty count keeps reporting its freshness"
        );
    }

    #[test]
    fn the_badge_dot_follows_the_shown_rails_mark_before_a_waiting_message() {
        let blocked_execution = rail(2, Some(WorkMark::Blocked));
        let deciding_design = rail(4, Some(WorkMark::Decision));
        for (execution, design, inbox, dot) in [
            (
                blocked_execution,
                deciding_design,
                true,
                Some(Dot::Mark(WorkMark::Blocked)),
            ),
            // A hidden rail's mark never leaks onto the shown count.
            (rail(2, None), deciding_design, false, None),
            (rail(2, None), deciding_design, true, Some(Dot::Inbox)),
            (
                rail(0, None),
                deciding_design,
                true,
                Some(Dot::Mark(WorkMark::Decision)),
            ),
            (rail(0, None), rail(0, None), true, Some(Dot::Inbox)),
            (rail(0, None), rail(0, None), false, None),
        ] {
            let work = TabWork::select(execution, design, inbox);
            assert_eq!(work.dot, dot, "{work:?}");
            assert_eq!(
                BrowserBranding::for_state(CountState::Current(work)).dot,
                dot
            );
            assert_eq!(BrowserBranding::for_state(CountState::Stale(work)).dot, dot);
        }
    }

    #[test]
    fn journal_cursor_is_current_only_on_an_online_connection() {
        let cursor = wire::Seq::new(42).expect("positive cursor");
        assert_eq!(
            JournalCursor::resolve(ConnectionState::Online, Some(cursor)),
            Some(JournalCursor {
                value: Some("42 (p)".to_owned()),
                accessible: "journal cursor 42 (p)".to_owned(),
            })
        );
        assert_eq!(
            JournalCursor::resolve(ConnectionState::Online, None),
            Some(JournalCursor {
                value: None,
                accessible: "journal cursor not yet admitted".to_owned(),
            })
        );
        for connection in [
            ConnectionState::Starting,
            ConnectionState::Connecting,
            ConnectionState::Offline,
        ] {
            assert_eq!(JournalCursor::resolve(connection, Some(cursor)), None);
        }
    }

    #[test]
    fn titles_name_the_shown_kind_and_its_exact_count() {
        for (state, expected) in [
            (CountState::Loading, "Loading - spice"),
            (CountState::Disconnected, "Disconnected - spice"),
            (CountState::Current(design(2)), "2 designs - spice"),
            (CountState::Current(design(1)), "1 design - spice"),
            (CountState::Current(execution(1)), "1 task - spice"),
            (
                CountState::Current(execution(LARGE_TITLE_COUNT)),
                "1234 tasks - spice",
            ),
            (
                CountState::Stale(design(KNOWN_COUNT)),
                "7 designs (stale) - spice",
            ),
        ] {
            assert_eq!(BrowserBranding::for_state(state).title(), expected);
        }
    }

    #[test]
    fn badges_bound_large_counts_mark_freshness_and_fit_the_icon() {
        for (state, badge_label, tone) in [
            (CountState::Loading, "…", Tone::Loading),
            (CountState::Disconnected, "!", Tone::Disconnected),
            (CountState::Stale(execution(1)), "1", Tone::Stale),
            (
                CountState::Current(execution(MAX_EXACT_BADGE_COUNT)),
                "99",
                Tone::Execution,
            ),
            (
                CountState::Current(design(MAX_EXACT_BADGE_COUNT + 1)),
                "99+",
                Tone::Design,
            ),
            (
                CountState::Current(execution(LARGE_TITLE_COUNT)),
                "99+",
                Tone::Execution,
            ),
        ] {
            let branding = BrowserBranding::for_state(state);
            assert_eq!(badge(&branding), Some((badge_label, tone)));
            let (font_size, _) = label_metrics(badge_label);
            let advance =
                badge_label.chars().count() * font_size * MONOSPACE_ADVANCE_PERCENT / PERCENT;
            assert!(advance < BADGE_WIDTH, "{badge_label:?} overflows the badge");
        }
    }
}

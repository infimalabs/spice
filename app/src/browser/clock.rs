//! Deadline-driven relative ages for the browser app.
//!
//! An age belongs to the thing being aged, not to the frame that rendered it:
//! arithmetic done only during render would leave a lane the resident has
//! stopped talking about showing the span it had when its last frame arrived.
//!
//! Each relative label schedules only the next instant at which its visible
//! spelling can change. A seconds-old message wakes each second, while a lease
//! countdown wakes at the decreasing boundary for its displayed unit. Keeping
//! that timer in the leaf avoids reconciling the complete app VDOM for every
//! relative label. The callback writes only a changed text node; any later Yew
//! render computes that same text from the current clock. Dropping the effect
//! cancels the pending browser timer and prevents the callback from rearming.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gloo_timers::callback::Timeout;
use js_sys::Intl::DateTimeFormat;
use js_sys::{Array, Date, Object, Reflect};
use wasm_bindgen::JsValue;
use yew::prelude::*;

use crate::watchband::{
    AGE_SLOT_MIN_WIDTH, elapsed_label, elapsed_label_deadline_millis, remaining_label,
    remaining_label_deadline_millis, task_elapsed_label, task_elapsed_label_deadline_millis,
    task_remaining_label, task_remaining_label_deadline_millis,
};

#[derive(Properties, PartialEq)]
pub(super) struct AgeProps {
    /// Epoch milliseconds selected by the caller's display chronology.
    ///
    /// Provenance remains with the caller; this component only renders elapsed
    /// time and does not classify the instant as authored or observed.
    pub(super) authored: i64,
    /// Element class, so a caller keeps its own placement and colour.
    pub(super) class: Classes,
    /// Hover text, which carries the exact instants the label rounds away.
    #[prop_or_default]
    pub(super) title: AttrValue,
    /// Canonical card permalink. Absent ages remain plain text.
    #[prop_or_default]
    pub(super) href: Option<AttrValue>,
    /// Accessible action label for a linked age.
    #[prop_or_default]
    pub(super) label: AttrValue,
}

#[derive(Properties, PartialEq)]
pub(super) struct CountdownProps {
    /// Authoritative epoch millisecond at which the lease expires.
    pub(super) expiry: i64,
    /// Element class, so a caller keeps its own placement and colour.
    pub(super) class: Classes,
    /// Hover text, which carries exact context the label rounds away.
    #[prop_or_default]
    pub(super) title: AttrValue,
}

/// Which way a task timestamp counts: since a past instant or toward a deadline.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum TaskTimePresentation {
    Elapsed,
    Remaining,
}

impl TaskTimePresentation {
    const fn clock(self) -> RelativeClock {
        match self {
            Self::Elapsed => RelativeClock::Elapsed,
            Self::Remaining => RelativeClock::Remaining,
        }
    }

    const fn key(self) -> &'static str {
        match self {
            Self::Elapsed => "elapsed",
            Self::Remaining => "remaining",
        }
    }
}

#[derive(Properties, PartialEq)]
struct TaskTimestampProps {
    instant: i64,
    canonical: AttrValue,
    presentation: TaskTimePresentation,
}

#[derive(Clone)]
enum ClockLabels {
    Relative,
    /// The local date a task timestamp reads once it ages out of relative range.
    Task {
        compact: AttrValue,
    },
}

impl ClockLabels {
    fn render(&self, clock: RelativeClock, relative: i64) -> String {
        match self {
            Self::Relative => relative_label_from_millis(clock, relative),
            Self::Task { compact } => {
                task_relative_label(clock, relative).unwrap_or_else(|| compact.to_string())
            }
        }
    }

    fn deadline(&self, clock: RelativeClock, relative: i64) -> Option<u32> {
        match (self, clock) {
            (Self::Relative, RelativeClock::Elapsed) => elapsed_label_deadline_millis(relative),
            (Self::Relative, RelativeClock::Remaining) => remaining_label_deadline_millis(relative),
            (Self::Task { .. }, RelativeClock::Elapsed) => {
                task_elapsed_label_deadline_millis(relative)
            }
            (Self::Task { .. }, RelativeClock::Remaining) => {
                task_remaining_label_deadline_millis(relative)
            }
        }
    }
}

#[derive(Clone, Copy)]
enum RelativeClock {
    Elapsed,
    Remaining,
}

/// One relative age, advancing at its next visible deadline.
///
/// `data-age-slot` carries the minimum reservation and the stylesheet owns its
/// geometry. Ordinary ticks replace only the text node within that reservation;
/// an exceptional longer spelling expands the slot rather than overlapping it.
#[function_component(Age)]
pub(super) fn age(props: &AgeProps) -> Html {
    let slot = use_node_ref();
    {
        let slot = slot.clone();
        use_effect_with(props.authored, move |authored| {
            let active = Rc::new(Cell::new(true));
            let pending = Rc::new(RefCell::new(None));
            schedule(
                slot,
                *authored,
                RelativeClock::Elapsed,
                ClockLabels::Relative,
                Rc::clone(&active),
                Rc::clone(&pending),
            );
            move || {
                active.set(false);
                pending.borrow_mut().take();
            }
        });
    }
    let now = now_millis();
    let text = elapsed_label(now.saturating_sub(props.authored));
    if let Some(href) = &props.href {
        html! {
            <a
                ref={slot}
                class={props.class.clone()}
                title={props.title.clone()}
                href={href.clone()}
                aria-label={props.label.clone()}
                data-age-slot={AGE_SLOT_MIN_WIDTH.to_string()}
                data-age-authored={props.authored.to_string()}
                data-age-clock="deadline"
            >{text}</a>
        }
    } else {
        html! {
            <span
                ref={slot}
                class={props.class.clone()}
                title={props.title.clone()}
                data-age-slot={AGE_SLOT_MIN_WIDTH.to_string()}
                data-age-authored={props.authored.to_string()}
                data-age-clock="deadline"
            >{text}</span>
        }
    }
}

/// One relative countdown, advancing toward an authoritative expiry deadline.
#[function_component(Countdown)]
pub(super) fn countdown(props: &CountdownProps) -> Html {
    let slot = use_node_ref();
    {
        let slot = slot.clone();
        use_effect_with(props.expiry, move |expiry| {
            let active = Rc::new(Cell::new(true));
            let pending = Rc::new(RefCell::new(None));
            schedule(
                slot,
                *expiry,
                RelativeClock::Remaining,
                ClockLabels::Relative,
                Rc::clone(&active),
                Rc::clone(&pending),
            );
            move || {
                active.set(false);
                pending.borrow_mut().take();
            }
        });
    }
    let now = now_millis();
    html! {
        <span
            ref={slot}
            class={props.class.clone()}
            title={props.title.clone()}
            data-age-slot={AGE_SLOT_MIN_WIDTH.to_string()}
            data-expiry={props.expiry.to_string()}
            data-age-clock="countdown"
        >{remaining_label(props.expiry.saturating_sub(now))}</span>
    }
}

/// One task timestamp: its relative age or countdown, the local date once
/// that leaves relative range, and the full local time on hover.
#[function_component(TaskTimestamp)]
fn task_timestamp(props: &TaskTimestampProps) -> Html {
    let compact = local_timestamp(props.instant, &props.canonical, LocalStyle::Compact);
    let full = local_timestamp(props.instant, &props.canonical, LocalStyle::Full);
    let labels = ClockLabels::Task { compact };
    let clock = props.presentation.clock();
    let slot = use_node_ref();
    {
        let slot = slot.clone();
        let labels = labels.clone();
        let effect = (props.instant, props.presentation);
        use_effect_with(effect, move |(instant, _)| {
            let active = Rc::new(Cell::new(true));
            let pending = Rc::new(RefCell::new(None));
            schedule(
                slot,
                *instant,
                clock,
                labels,
                Rc::clone(&active),
                Rc::clone(&pending),
            );
            move || {
                active.set(false);
                pending.borrow_mut().take();
            }
        });
    }
    let relative = relative_millis(clock, props.instant);
    html! {
        <time
            ref={slot}
            class="task-time"
            datetime={props.canonical.clone()}
            title={full.clone()}
            aria-label={full}
            data-task-time={props.presentation.key()}
            data-task-time-instant={props.instant.to_string()}
        >{labels.render(clock, relative)}</time>
    }
}

pub(super) fn task_time(instant: &wire::Instant, presentation: TaskTimePresentation) -> Html {
    html! {
        <TaskTimestamp
            instant={instant.get().as_millisecond()}
            canonical={instant.to_string()}
            {presentation}
        />
    }
}

/// A task timestamp for an epoch millisecond no wire instant carries, such as
/// the birth a task stamp encodes.
pub(super) fn task_time_millis(millis: i64, presentation: TaskTimePresentation) -> Html {
    let canonical = String::from(Date::new(&JsValue::from_f64(millis as f64)).to_iso_string());
    html! {
        <TaskTimestamp instant={millis} canonical={canonical} {presentation} />
    }
}

fn schedule(
    slot: NodeRef,
    instant: i64,
    clock: RelativeClock,
    labels: ClockLabels,
    active: Rc<Cell<bool>>,
    pending: Rc<RefCell<Option<Timeout>>>,
) {
    let relative = relative_millis(clock, instant);
    let Some(delay) = labels.deadline(clock, relative) else {
        return;
    };
    let next_pending = Rc::clone(&pending);
    let timeout = Timeout::new(delay, move || {
        next_pending.borrow_mut().take();
        if !active.get() {
            return;
        }
        let Some(element) = slot.cast::<web_sys::Element>() else {
            return;
        };
        let label = labels.render(clock, relative_millis(clock, instant));
        if element.text_content().as_deref() != Some(&label) {
            element.set_text_content(Some(&label));
        }
        schedule(slot, instant, clock, labels, active, next_pending);
    });
    pending.borrow_mut().replace(timeout);
}

fn relative_millis(clock: RelativeClock, instant: i64) -> i64 {
    match clock {
        RelativeClock::Elapsed => now_millis().saturating_sub(instant),
        RelativeClock::Remaining => instant.saturating_sub(now_millis()),
    }
}

fn relative_label_from_millis(clock: RelativeClock, relative: i64) -> String {
    match clock {
        RelativeClock::Elapsed => elapsed_label(relative),
        RelativeClock::Remaining => remaining_label(relative),
    }
}

fn task_relative_label(clock: RelativeClock, relative: i64) -> Option<String> {
    match clock {
        RelativeClock::Elapsed => task_elapsed_label(relative),
        RelativeClock::Remaining => task_remaining_label(relative),
    }
}

#[derive(Clone, Copy)]
enum LocalStyle {
    Compact,
    Full,
}

thread_local! {
    static COMPACT_TIME_FORMATTER: DateTimeFormat = local_formatter("short", Some("short"));
    static FULL_TIME_FORMATTER: DateTimeFormat = local_formatter("medium", Some("long"));
}

fn local_formatter(date_style: &str, time_style: Option<&str>) -> DateTimeFormat {
    let options = Object::new();
    Reflect::set(
        options.as_ref(),
        &JsValue::from_str("dateStyle"),
        &JsValue::from_str(date_style),
    )
    .expect("the date formatter accepts dateStyle");
    if let Some(time_style) = time_style {
        Reflect::set(
            options.as_ref(),
            &JsValue::from_str("timeStyle"),
            &JsValue::from_str(time_style),
        )
        .expect("the date formatter accepts timeStyle");
    }
    DateTimeFormat::new(&Array::new(), &options)
}

fn local_timestamp(instant: i64, canonical: &str, style: LocalStyle) -> AttrValue {
    let date = Date::new(&JsValue::from_f64(instant as f64));
    let rendered = match style {
        LocalStyle::Compact => COMPACT_TIME_FORMATTER.with(|formatter| {
            formatter
                .format()
                .call1(&JsValue::UNDEFINED, date.as_ref())
                .ok()
                .and_then(|value| value.as_string())
        }),
        LocalStyle::Full => FULL_TIME_FORMATTER.with(|formatter| {
            formatter
                .format()
                .call1(&JsValue::UNDEFINED, date.as_ref())
                .ok()
                .and_then(|value| value.as_string())
        }),
    };
    rendered
        .filter(|value| !value.is_empty() && value != "Invalid Date")
        .unwrap_or_else(|| canonical.to_owned())
        .into()
}

/// An instant in the viewer's local time for static diagram text.
pub(super) fn local_compact(instant: &wire::Instant) -> String {
    local_timestamp(
        instant.get().as_millisecond(),
        &instant.to_string(),
        LocalStyle::Compact,
    )
    .to_string()
}

/// An instant's full local time, for hover text beside a relative label.
pub(super) fn local_full(instant: &wire::Instant) -> AttrValue {
    local_timestamp(
        instant.get().as_millisecond(),
        &instant.to_string(),
        LocalStyle::Full,
    )
}

/// [`local_full`] for an epoch millisecond no wire instant carries.
pub(super) fn local_full_millis(millis: i64) -> AttrValue {
    local_timestamp(
        millis,
        &format!("{millis} Unix milliseconds"),
        LocalStyle::Full,
    )
}

/// The browser's current instant in epoch milliseconds.
pub(super) fn now_millis() -> i64 {
    js_sys::Date::now() as i64
}

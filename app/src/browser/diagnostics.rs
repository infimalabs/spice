//! Opt-in bounded User Timing diagnostics for the browser app.

use std::cell::RefCell;
use std::collections::VecDeque;

use wasm_bindgen::prelude::*;

use super::APP_SOURCE_IDENTITY;

const STORAGE_KEY: &str = "spice:app-trace";
const RETAINED_RECORDS: usize = 256;
const CONTROL_NAME: &str = "__spiceAppTrace";

thread_local! {
    static TRACE: RefCell<Option<Trace>> = const { RefCell::new(None) };
}

#[derive(Default)]
pub(in crate::browser) struct Metadata {
    pub(in crate::browser) frame_bytes: Option<usize>,
    pub(in crate::browser) cards: Option<usize>,
    pub(in crate::browser) event: Option<&'static str>,
    pub(in crate::browser) items: Option<usize>,
    pub(in crate::browser) width_px: Option<i32>,
    pub(in crate::browser) instance: Option<u64>,
    pub(in crate::browser) kind: Option<&'static str>,
    pub(in crate::browser) focused: Option<bool>,
    pub(in crate::browser) composing: Option<bool>,
    pub(in crate::browser) selection_start: Option<u32>,
    pub(in crate::browser) selection_end: Option<u32>,
}

pub(in crate::browser) struct Span {
    open: Option<OpenSpan>,
}

impl Drop for Span {
    fn drop(&mut self) {
        let Some(open) = self.open.take() else {
            return;
        };
        TRACE.with_borrow_mut(|slot| {
            if let Some(trace) = slot.as_mut() {
                trace.finish(open);
            }
        });
    }
}

struct OpenSpan {
    start: String,
    end: String,
    measure: String,
}

struct Retained {
    marks: Vec<String>,
    measure: Option<String>,
}

struct Trace {
    performance: web_sys::Performance,
    next: u64,
    retained: VecDeque<Retained>,
}

impl Trace {
    fn open() -> Option<Self> {
        let window = web_sys::window()?;
        let storage = window.session_storage().ok().flatten()?;
        if storage.get_item(STORAGE_KEY).ok().flatten().as_deref() != Some("1") {
            return None;
        }
        Some(Self {
            performance: window.performance()?,
            next: 0,
            retained: VecDeque::with_capacity(RETAINED_RECORDS),
        })
    }

    fn label(&mut self, phase: &str, metadata: Metadata) -> String {
        self.next = self.next.wrapping_add(1);
        let mut label = format!(
            "spice.app.{phase}#{:06} build={}",
            self.next, APP_SOURCE_IDENTITY
        );
        metadata.append_to(&mut label);
        label
    }

    fn instant(&mut self, phase: &str, metadata: Metadata) {
        let mark = self.label(phase, metadata);
        if self.performance.mark(&mark).is_ok() {
            self.retain(Retained {
                marks: vec![mark],
                measure: None,
            });
        }
    }

    fn start(&mut self, phase: &str, metadata: Metadata) -> Option<OpenSpan> {
        let label = self.label(phase, metadata);
        let start = format!("{label}:start");
        self.performance.mark(&start).ok()?;
        Some(OpenSpan {
            start,
            end: format!("{label}:end"),
            measure: label,
        })
    }

    fn finish(&mut self, open: OpenSpan) {
        if self.performance.mark(&open.end).is_err() {
            self.performance.clear_marks_with_mark_name(&open.start);
            return;
        }
        let measured = self
            .performance
            .measure_with_start_mark_and_end_mark(&open.measure, &open.start, &open.end)
            .is_ok();
        self.retain(Retained {
            marks: vec![open.start, open.end],
            measure: measured.then_some(open.measure),
        });
    }

    fn retain(&mut self, record: Retained) {
        self.retained.push_back(record);
        while self.retained.len() > RETAINED_RECORDS {
            if let Some(expired) = self.retained.pop_front() {
                self.clear_record(expired);
            }
        }
    }

    fn clear(&mut self) {
        while let Some(record) = self.retained.pop_front() {
            self.clear_record(record);
        }
    }

    fn clear_record(&self, record: Retained) {
        for mark in record.marks {
            self.performance.clear_marks_with_mark_name(&mark);
        }
        if let Some(measure) = record.measure {
            self.performance.clear_measures_with_measure_name(&measure);
        }
    }
}

impl Metadata {
    fn append_to(self, label: &mut String) {
        append(label, "bytes", self.frame_bytes);
        append(label, "cards", self.cards);
        append(label, "items", self.items);
        append(label, "width", self.width_px);
        append(label, "instance", self.instance);
        if let Some(kind) = self.kind {
            label.push_str(" kind=");
            label.push_str(kind);
        }
        if let Some(event) = self.event {
            label.push_str(" event=");
            label.push_str(event);
        }
        append(label, "composing", self.composing.map(u8::from));
        append(label, "selection_start", self.selection_start);
        append(label, "selection_end", self.selection_end);
        append(label, "focused", self.focused.map(u8::from));
    }
}

fn append<T: std::fmt::Display>(label: &mut String, field: &str, value: Option<T>) {
    if let Some(value) = value {
        label.push(' ');
        label.push_str(field);
        label.push('=');
        use std::fmt::Write as _;
        let _ = write!(label, "{value}");
    }
}

pub(in crate::browser) fn install() {
    TRACE.with_borrow_mut(|slot| *slot = Trace::open());
    let enabled = TRACE.with_borrow(Option::is_some);
    if !enabled {
        return;
    }
    let Some(window) = web_sys::window() else {
        return;
    };
    let control = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&control, &JsValue::from_str("enabled"), &JsValue::TRUE);
    let _ = js_sys::Reflect::set(
        &control,
        &JsValue::from_str("limit"),
        &JsValue::from_f64(RETAINED_RECORDS as f64),
    );
    let _ = js_sys::Reflect::set(
        &control,
        &JsValue::from_str("build"),
        &JsValue::from_str(APP_SOURCE_IDENTITY),
    );
    let clear = Closure::<dyn FnMut()>::new(clear);
    let _ = js_sys::Reflect::set(
        &control,
        &JsValue::from_str("clear"),
        clear.as_ref().unchecked_ref(),
    );
    clear.forget();
    let _ = js_sys::Reflect::set(window.as_ref(), &JsValue::from_str(CONTROL_NAME), &control);
    instant("trace.start", Metadata::default);
}

pub(in crate::browser) fn instant(phase: &'static str, metadata: impl FnOnce() -> Metadata) {
    TRACE.with_borrow_mut(|slot| {
        if let Some(trace) = slot.as_mut() {
            trace.instant(phase, metadata());
        }
    });
}

pub(in crate::browser) fn span(phase: &'static str, metadata: impl FnOnce() -> Metadata) -> Span {
    let open = TRACE.with_borrow_mut(|slot| {
        slot.as_mut()
            .and_then(|trace| trace.start(phase, metadata()))
    });
    Span { open }
}

pub(in crate::browser) fn instance() -> u64 {
    TRACE.with_borrow_mut(|slot| {
        slot.as_mut().map_or(0, |trace| {
            trace.next = trace.next.wrapping_add(1);
            trace.next
        })
    })
}

fn clear() {
    TRACE.with_borrow_mut(|slot| {
        if let Some(trace) = slot.as_mut() {
            trace.clear();
        }
    });
}

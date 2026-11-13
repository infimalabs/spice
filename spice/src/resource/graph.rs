//! Native graph continuations retain validated diagrams in a bounded seat cache.
//! A cursor names one immutable snapshot and the first unseen UTF-8 byte.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::ops::Range;
use std::sync::{Arc, Mutex, PoisonError};

use serde::Serialize;
use wire::output::{Budget, ClippedText, INTERACTIVE, Request};
use wire::{Digest, Domain, Seq};

use super::{Query, Shape};

#[cfg(test)]
mod tests;

/// Maximum incomplete diagrams retained by one seat.
const MAX_RETAINED_SNAPSHOTS: usize = 16;
/// Maximum graph snapshot payload retained by one seat.
const MAX_RETAINED_BYTES: usize = 8 * 1024 * 1024;

/// Incomplete diagrams remain readable across board changes until deterministic eviction.
pub(crate) struct Pages {
    cache: Mutex<Cache>,
    retention: Retention,
}

#[derive(Default)]
struct Cache {
    snapshots: VecDeque<Arc<Snapshot>>,
    bytes: usize,
}

#[derive(Clone, Copy)]
struct Retention {
    snapshots: usize,
    bytes: usize,
}

impl Default for Pages {
    fn default() -> Self {
        Self {
            cache: Mutex::default(),
            retention: Retention {
                snapshots: MAX_RETAINED_SNAPSHOTS,
                bytes: MAX_RETAINED_BYTES,
            },
        }
    }
}

struct Snapshot {
    key: String,
    view: String,
    at: Option<String>,
    all: bool,
    barrier: Seq,
    text: String,
    lines: Vec<Range<usize>>,
}

impl Snapshot {
    fn new(view: &str, at: Option<String>, all: bool, barrier: Seq, text: String) -> Self {
        let scope = if all { "all" } else { "live" };
        let key = format!(
            "{barrier}:{scope}:{}",
            Digest::compute(Domain::CliExport, text.as_bytes())
        );
        let mut offset = 0;
        let lines = text
            .split_inclusive('\n')
            .map(|line| {
                let content = line
                    .strip_suffix("\r\n")
                    .or_else(|| line.strip_suffix('\n'))
                    .unwrap_or(line);
                let end = offset + content.len();
                let range = offset..end;
                offset += line.len();
                range
            })
            .collect();
        Self {
            key,
            view: view.to_owned(),
            at,
            all,
            barrier,
            text,
            lines,
        }
    }

    fn line(&self, line: usize) -> &str {
        &self.text[self.lines[line].clone()]
    }

    fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.key.capacity()
            + self.view.capacity()
            + self.at.as_ref().map_or(0, String::capacity)
            + self.text.capacity()
            + self.lines.capacity() * std::mem::size_of::<Range<usize>>()
    }
}

impl Cache {
    fn retain(&mut self, snapshot: Arc<Snapshot>, retention: Retention) -> Result<(), String> {
        if self
            .snapshots
            .iter()
            .any(|retained| retained.key == snapshot.key)
        {
            return Ok(());
        }
        let bytes = snapshot.retained_bytes();
        if bytes > retention.bytes {
            return Err(format!(
                "board::graph-snapshot-too-large: diagram needs {bytes} retained bytes, above this seat's {}-byte native continuation limit; use `spice board graph {} --output board-graph-{}.txt`",
                retention.bytes, snapshot.view, snapshot.view
            ));
        }
        while self.snapshots.len() >= retention.snapshots
            || self.bytes.saturating_add(bytes) > retention.bytes
        {
            let evicted = self
                .snapshots
                .pop_front()
                .expect("a cache over its bound is non-empty");
            self.bytes -= evicted.retained_bytes();
        }
        self.bytes += bytes;
        self.snapshots.push_back(snapshot);
        Ok(())
    }
}

struct Cursor {
    key: String,
    line: usize,
    byte: usize,
}

impl Cursor {
    fn parse(written: &str) -> Result<Self, String> {
        let parse = || {
            let (rest, byte) = written.rsplit_once(':')?;
            let (key, line) = rest.rsplit_once(':')?;
            Some(Self {
                key: key.to_owned(),
                line: line.parse().ok()?,
                byte: byte.parse().ok()?,
            })
        };
        parse().ok_or_else(|| {
            "board::cursor-invalid: pass the graph continuation exactly as read".to_owned()
        })
    }

    fn written(&self) -> String {
        format!("{}:{}:{}", self.key, self.line, self.byte)
    }
}

#[derive(Serialize)]
struct Fragment {
    line: usize,
    offset: usize,
    text: ClippedText,
}

struct Page {
    rows: Vec<Fragment>,
    next: Option<Cursor>,
    widened: Request,
    exhausted: &'static str,
}

impl Pages {
    #[cfg(test)]
    fn with_retention(snapshots: usize, bytes: usize) -> Self {
        assert!(snapshots > 0 && bytes > 0);
        Self {
            cache: Mutex::default(),
            retention: Retention { snapshots, bytes },
        }
    }

    pub(super) fn read(
        &self,
        query: &Query,
        load: impl FnOnce() -> Result<(Seq, String), String>,
    ) -> Result<String, String> {
        let Shape::Graph { view } = &query.shape else {
            unreachable!("graph resource")
        };
        let bound = |key| {
            option(query, key)
                .map(|value| {
                    value.parse::<usize>().map_err(|_| {
                        format!("board::limit-invalid: {key} takes a non-negative integer")
                    })
                })
                .transpose()
        };
        let budget = INTERACTIVE
            .budget(Request {
                rows: bound("limit")?,
                bytes: bound("max-bytes")?,
                text: bound("max-text")?,
            })
            .map_err(|error| format!("board::limit-invalid: {error}"))?;
        let at = option(query, "at");
        let all = query.options.iter().any(|(name, _)| name == "all");
        let (snapshot, cursor, fresh) = match option(query, "after") {
            Some(after) => {
                let cursor = Cursor::parse(after)?;
                let snapshot = self
                    .cache
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .snapshots
                    .iter()
                    .find(|snapshot| snapshot.key == cursor.key)
                    .cloned()
                    .ok_or_else(|| {
                        "board::cursor-expired: graph snapshot is no longer retained by this seat; read the graph without after"
                            .to_owned()
                    })?;
                if snapshot.view != *view || snapshot.at.as_deref() != at || snapshot.all != all {
                    return Err(
                        "board::cursor-invalid: continuation names another graph read".to_owned(),
                    );
                }
                (snapshot, cursor, false)
            }
            None => {
                let (barrier, text) = load()?;
                let snapshot = Arc::new(Snapshot::new(
                    view,
                    at.map(str::to_owned),
                    all,
                    barrier,
                    text,
                ));
                let cursor = Cursor {
                    key: snapshot.key.clone(),
                    line: 0,
                    byte: 0,
                };
                (snapshot, cursor, true)
            }
        };
        let page = page(&snapshot, cursor, budget)?;
        if fresh && page.next.is_some() {
            self.cache
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .retain(Arc::clone(&snapshot), self.retention)?;
        }
        let mut document = format!(
            "graph {} snapshot {}; {} lines",
            snapshot.view,
            snapshot.barrier,
            snapshot.lines.len()
        );
        for fragment in &page.rows {
            write!(
                document,
                "\n{}:{} {}",
                fragment.line, fragment.offset, fragment.text
            )
            .expect("String writes are infallible");
        }
        if let Some(next) = page.next {
            let mut continuation = query.clone();
            set(&mut continuation, "after", next.written());
            if let Some(rows) = page.widened.rows {
                set(&mut continuation, "limit", rows.to_string());
            }
            if let Some(bytes) = page.widened.bytes {
                set(&mut continuation, "max-bytes", bytes.to_string());
            }
            if let Some(text) = page.widened.text {
                set(&mut continuation, "max-text", text.to_string());
            }
            write!(
                document,
                "\nwithheld {} lines from line {} byte {} ({} limit); continue: {}",
                snapshot.lines.len() - next.line,
                next.line + 1,
                next.byte,
                page.exhausted,
                continuation.url()
            )
            .expect("String writes are infallible");
        }
        Ok(document)
    }
}

fn option<'a>(query: &'a Query, key: &str) -> Option<&'a str> {
    query
        .options
        .iter()
        .find(|(name, _)| name == key)
        .and_then(|(_, value)| value.as_deref())
}

fn set(query: &mut Query, key: &str, value: String) {
    if let Some((_, previous)) = query.options.iter_mut().find(|(name, _)| name == key) {
        *previous = Some(value);
    } else {
        query.options.push((key.to_owned(), Some(value)));
    }
}

fn page(snapshot: &Snapshot, mut cursor: Cursor, budget: Budget) -> Result<Page, String> {
    if cursor.line > snapshot.lines.len()
        || (cursor.line == snapshot.lines.len() && cursor.byte != 0)
        || (cursor.line < snapshot.lines.len()
            && !snapshot.line(cursor.line).is_char_boundary(cursor.byte))
    {
        return Err("board::cursor-invalid: graph cursor names no UTF-8 boundary".to_owned());
    }
    let limits = budget.limits();
    let mut result = Page {
        rows: Vec::new(),
        next: None,
        widened: Request::default(),
        exhausted: "rows",
    };
    let mut bytes = 0;
    while cursor.line < snapshot.lines.len() {
        if result.rows.len() == limits.rows {
            if result.rows.is_empty() {
                result.widened.rows = Some(1);
            }
            break;
        }
        let remaining = &snapshot.line(cursor.line)[cursor.byte..];
        let clipped = budget.clip(remaining);
        if clipped.text.is_empty() && !remaining.is_empty() {
            result.exhausted = "text";
            result.widened.text = remaining.chars().next().map(char::len_utf8);
            break;
        }
        let fragment = Fragment {
            line: cursor.line + 1,
            offset: cursor.byte,
            text: clipped,
        };
        let needed = wire::encoded_len(&fragment)
            .map_err(|error| format!("board::row-unencodable: {error}"))?;
        if needed > limits.bytes - bytes {
            result.exhausted = "bytes";
            if result.rows.is_empty() {
                result.widened.bytes = Some(needed);
            }
            break;
        }
        bytes += needed;
        cursor.byte += fragment.text.text.len();
        let partial = fragment.text.withheld_bytes.is_some();
        result.rows.push(fragment);
        if partial {
            result.exhausted = "text";
            break;
        }
        cursor.line += 1;
        cursor.byte = 0;
    }
    if cursor.line < snapshot.lines.len() {
        result.next = Some(cursor);
    }
    Ok(result)
}

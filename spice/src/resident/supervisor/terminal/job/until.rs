//! The until watcher: a shell command started with `until` has its keeper
//! read the job's log line by line, as `job wait until` reads it, for the
//! first line the pattern matches. That line, clipped to [`LINE_BYTES`], is
//! published once and the watcher disarms; the command keeps running and its
//! ending is told on its own. The keeper completes the search before it
//! publishes the ending, so a match always precedes it; output past the log's
//! cap was never kept, so it is never searched. The lane records hearing the
//! match, so a successor seat hears it only if no seat did.

use std::fs::File;
use std::io::{self, ErrorKind};
use std::path::Path;
use std::sync::mpsc;
use std::thread;

use serde::{Deserialize, Serialize};
use vendor::omp::OutputPattern;

use super::super::super::{publish_json, read_json};
use super::keeper::watch;
use super::{DELIVERED, LOG, LogLines};

/// Bytes of the matched line its notice carries; the rest stays in the log.
const LINE_BYTES: usize = 1024;

/// Bound on the match record: one clipped line with room for JSON escaping.
const MATCH_BYTES: u64 = 16 * 1024;

const MATCH: &str = "match.json";
const HEARD: &str = "match-heard";

#[derive(Deserialize, Serialize)]
struct Matched {
    line: String,
}

/// The line the watcher matched, if it has.
pub(super) fn matched(directory: &Path) -> io::Result<Option<String>> {
    match read_json::<Matched>(&directory.join(MATCH), MATCH_BYTES) {
        Ok(matched) => Ok(Some(matched.line)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Publish `line` as the job's match, clipped to [`LINE_BYTES`] and marked
/// when clipped.
pub(super) fn publish(directory: &Path, line: &str) -> io::Result<()> {
    let line = if line.len() > LINE_BYTES {
        let mut end = LINE_BYTES;
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        format!("{} …", &line[..end])
    } else {
        line.to_owned()
    };
    publish_json(&directory.join(MATCH), &Matched { line })
}

/// Record that the lane heard the match.
pub(super) fn hear(directory: &Path) -> io::Result<()> {
    File::create(directory.join(HEARD)).map(drop)
}

/// Whether the lane owes no telling of the match: it heard the match, or the
/// ending, which follows it.
pub(super) fn heard(directory: &Path) -> bool {
    directory.join(HEARD).exists() || directory.join(DELIVERED).exists()
}

/// What reaches the watcher.
enum Look {
    /// The log was written.
    Wrote,
    /// The log can no longer be watched; only completion wakes a search.
    Unwatched,
    /// The log is complete.
    Complete,
}

/// The keeper's end of a running watcher.
pub(super) struct Watcher {
    complete: mpsc::Sender<Look>,
    searched: mpsc::Receiver<()>,
}

impl Watcher {
    /// Tell the watcher the log is complete and wait for its last search,
    /// which takes an unterminated last line too.
    pub(super) fn complete(self) {
        let _ = self.complete.send(Look::Complete);
        let _ = self.searched.recv();
    }
}

/// Search the log beneath `directory` for `pattern` on a thread of its own:
/// the first matching line is published, then `published` runs.
///
/// # Errors
///
/// Returns the failure of starting the thread.
pub(super) fn arm(
    directory: &Path,
    pattern: OutputPattern,
    published: impl FnOnce() + Send + 'static,
) -> io::Result<Watcher> {
    let (sender, receiver) = mpsc::channel();
    let (searching, searched) = mpsc::channel();
    let complete = sender.clone();
    let directory = directory.to_path_buf();
    thread::Builder::new()
        .name("spice-job-until".to_owned())
        .spawn(move || {
            let found = search(&directory, &pattern, &sender, &receiver)
                .is_some_and(|line| publish(&directory, &line).is_ok());
            drop(searching);
            if found {
                published();
            }
        })?;
    Ok(Watcher { complete, searched })
}

/// The first line of the log `pattern` matches, read on each write to the
/// log until one matches or the log is complete.
fn search(
    directory: &Path,
    pattern: &OutputPattern,
    sender: &mpsc::Sender<Look>,
    receiver: &mpsc::Receiver<Look>,
) -> Option<String> {
    let log = directory.join(LOG);
    let mut lines = LogLines::new(directory.to_path_buf());
    let look = |lines: &mut LogLines| {
        let mut found = None;
        let _ = lines.read(&mut |line| {
            if found.is_none() && pattern.is_match(line) {
                found = Some(line.to_owned());
            }
        });
        found
    };
    let mut watchable = true;
    let mut armed = None;
    loop {
        if watchable && armed.is_none() {
            armed = watch(&log, sender, |wrote| match wrote {
                Ok(()) => Look::Wrote,
                Err(_) => Look::Unwatched,
            })
            .ok();
            watchable = armed.is_some();
        }
        if let Some(line) = look(&mut lines) {
            return Some(line);
        }
        match receiver.recv() {
            Ok(Look::Wrote) => armed = None,
            Ok(Look::Unwatched) => (watchable, armed) = (false, None),
            Ok(Look::Complete) | Err(_) => break,
        }
    }
    look(&mut lines).or_else(|| lines.finish().filter(|line| pattern.is_match(line)))
}

//! Git as the build's source: one `cat-file --batch` for objects and, under a
//! tracked major, one first-parent `log` streamed until the newest raise.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use super::SourceIdentity;
use super::history::{self, Change, Commit, Floor, HistoryError, Parent};
use super::precedence::History;
use crate::{COUNTERS_FILE, TrackedVersion, VERSION_FILE};

const ABSENT_MODE: &str = "000000";
const FILE_MODES: [&str; 2] = ["100644", "100755"];

/// HEAD and its history in the repository at one root.
pub(super) struct GitHistory<'a> {
    root: &'a Path,
    batch: Option<Batch>,
    head: Option<SourceIdentity>,
}

impl<'a> GitHistory<'a> {
    pub(super) fn new(root: &'a Path) -> Self {
        Self {
            root,
            batch: None,
            head: None,
        }
    }

    fn batch(&mut self) -> Result<&mut Batch, String> {
        if self.batch.is_none() {
            self.batch = Some(Batch::spawn(self.root)?);
        }
        Ok(self.batch.as_mut().expect("the batch was just spawned"))
    }

    fn object(&mut self, name: &str) -> Result<Object, String> {
        self.batch()?
            .get(name)?
            .ok_or_else(|| format!("Git has no object {name}"))
    }

    fn counter(&mut self, oid: Option<&str>) -> Result<Option<u64>, String> {
        let Some(oid) = oid else {
            return Ok(None);
        };
        let object = self.object(oid)?;
        if object.kind != "blob" {
            return Err(format!("{COUNTERS_FILE} at {oid} is not a file"));
        }
        let text = String::from_utf8(object.body)
            .map_err(|_| format!("{COUNTERS_FILE} at {oid} is not UTF-8"))?;
        crate::counter_sum(&text)
            .map(Some)
            .map_err(|error| format!("{error} at {oid}"))
    }

    /// [`VERSION_FILE`] as HEAD tracks it, ignoring the worktree.
    pub(super) fn tracked_version(&mut self) -> Result<TrackedVersion, String> {
        let commit = self.head()?.commit().to_owned();
        let object = self.object(&format!("{commit}:{VERSION_FILE}"))?;
        if object.kind != "blob" {
            return Err(format!("{VERSION_FILE} at {commit} is not a file"));
        }
        let text = String::from_utf8(object.body)
            .map_err(|_| format!("{VERSION_FILE} at {commit} is not UTF-8"))?;
        crate::admit_tracked_version(&text)
            .map_err(|error| format!("{VERSION_FILE} at {commit}: {error}"))
    }

    fn head_sum(&mut self, head: &SourceIdentity) -> Result<u64, String> {
        let commit = head.commit();
        let object = self.object(&format!("{commit}:{COUNTERS_FILE}"))?;
        if object.kind != "blob" {
            return Err(format!("{COUNTERS_FILE} at {commit} is not a file"));
        }
        let text = String::from_utf8(object.body)
            .map_err(|_| format!("{COUNTERS_FILE} at {commit} is not UTF-8"))?;
        crate::counter_sum(&text).map_err(|error| format!("{error} at {commit}"))
    }

    fn resolve(&mut self, logged: Logged) -> Result<Commit, String> {
        let parent = if logged.has_parent {
            Parent::First
        } else if parse_commit(&self.object(&logged.commit)?.body)?.has_parent {
            Parent::Shallow
        } else {
            Parent::Root
        };
        let entry = logged.entry.ok_or_else(|| {
            format!(
                "git log listed no {COUNTERS_FILE} change at {}",
                logged.commit
            )
        })?;
        let change = Change {
            old: self.counter(entry.old.as_deref())?,
            new: self.counter(entry.new.as_deref())?,
        };
        Ok(Commit {
            commit: logged.commit,
            seconds: logged.seconds,
            parent,
            change,
        })
    }
}

impl History for GitHistory<'_> {
    fn head(&mut self) -> Result<SourceIdentity, String> {
        if let Some(head) = &self.head {
            return Ok(head.clone());
        }
        let object = self.object("HEAD")?;
        if object.kind != "commit" {
            return Err("Git HEAD is not a commit".to_owned());
        }
        let seconds = parse_commit(&object.body)?.seconds;
        let head = SourceIdentity::parse(&object.oid, &seconds)
            .map_err(|detail| format!("Git HEAD is malformed: {detail}"))?;
        self.head = Some(head.clone());
        Ok(head)
    }

    fn floor(&mut self) -> Result<(u64, Floor), HistoryError> {
        let head = self.head().map_err(HistoryError::Source)?;
        let sum = self.head_sum(&head).map_err(HistoryError::Source)?;
        let mut log = Log::spawn(self.root, head.commit()).map_err(HistoryError::Source)?;
        let floor = history::floor(
            sum,
            std::iter::from_fn(|| log.next())
                .map(|logged| logged.and_then(|logged| self.resolve(logged))),
        );
        // A floor found early abandons the rest: `Log`'s drop kills git.
        let floor = match floor {
            Err(HistoryError::Unraised) => {
                log.finish().map_err(HistoryError::Source)?;
                Err(HistoryError::Unraised)
            }
            floor => floor,
        }?;
        Ok((sum, floor))
    }
}

/// One object `cat-file --batch` returned.
struct Object {
    oid: String,
    kind: String,
    body: Vec<u8>,
}

/// One long-lived `git cat-file --batch`, answering one request at a time.
struct Batch {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl Batch {
    fn spawn(root: &Path) -> Result<Self, String> {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("cannot run git cat-file: {error}"))?;
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().expect("stdout is piped"));
        Ok(Self {
            child,
            stdin,
            stdout,
        })
    }

    fn get(&mut self, name: &str) -> Result<Option<Object>, String> {
        if name.contains('\n') {
            return Err(format!("Git object name {name:?} spans lines"));
        }
        let failed = |error: std::io::Error| format!("git cat-file failed: {error}");
        let stdin = self.stdin.as_mut().expect("stdin stays open until drop");
        writeln!(stdin, "{name}")
            .and_then(|()| stdin.flush())
            .map_err(failed)?;
        let mut header = String::new();
        self.stdout.read_line(&mut header).map_err(failed)?;
        let header = header
            .strip_suffix('\n')
            .ok_or_else(|| "git cat-file ended early".to_owned())?;
        if header.ends_with(" missing") || header.ends_with(" ambiguous") {
            return Ok(None);
        }
        let mut fields = header.split(' ');
        let (Some(oid), Some(kind), Some(size), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(format!("git cat-file answered {header:?}"));
        };
        let size = size
            .parse::<usize>()
            .map_err(|_| format!("git cat-file answered {header:?}"))?;
        let mut body = vec![0; size + 1];
        self.stdout.read_exact(&mut body).map_err(failed)?;
        if body.pop() != Some(b'\n') {
            return Err("git cat-file framed an object without its LF".to_owned());
        }
        Ok(Some(Object {
            oid: oid.to_owned(),
            kind: kind.to_owned(),
            body,
        }))
    }
}

impl Drop for Batch {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.wait();
    }
}

/// One commit `git log --raw` listed, its counter blobs unread.
struct Logged {
    commit: String,
    seconds: i64,
    has_parent: bool,
    entry: Option<Entry>,
}

impl Logged {
    fn admit(&mut self, entry: Entry) -> Result<(), String> {
        if self.entry.is_some() {
            return Err(format!(
                "git log listed more than one {COUNTERS_FILE} change at {}",
                self.commit
            ));
        }
        self.entry = Some(entry);
        Ok(())
    }

    fn finish(self) -> Result<Self, String> {
        if self.entry.is_some() {
            Ok(self)
        } else {
            Err(format!(
                "git log listed no {COUNTERS_FILE} change at {}",
                self.commit
            ))
        }
    }
}

struct Entry {
    old: Option<String>,
    new: Option<String>,
}

/// One streaming first-parent log of counter-registry changes, newest first.
/// Drop kills git, so a walk that stops early reads no further history.
struct Log {
    child: Child,
    stdout: BufReader<ChildStdout>,
    pending: Option<String>,
    done: bool,
}

impl Log {
    fn spawn(root: &Path, head: &str) -> Result<Self, String> {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(root)
            .args([
                "log",
                "--first-parent",
                "--diff-merges=first-parent",
                "--no-renames",
                "--no-abbrev",
                "--raw",
                "--root",
                "--format=%x01%H %ct %P",
                head,
                "--",
                COUNTERS_FILE,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("cannot run git log: {error}"))?;
        let stdout = BufReader::new(child.stdout.take().expect("stdout is piped"));
        Ok(Self {
            child,
            stdout,
            pending: None,
            done: false,
        })
    }

    fn next(&mut self) -> Option<Result<Logged, String>> {
        if self.done {
            return None;
        }
        let result = self.read();
        if !matches!(result, Ok(Some(_))) {
            self.done = true;
        }
        result.transpose()
    }

    fn read(&mut self) -> Result<Option<Logged>, String> {
        let header = match self.pending.take() {
            Some(header) => header,
            None => match self.line()? {
                Some(line) => line,
                None => return Ok(None),
            },
        };
        let mut logged = parse_header(&header)?;
        while let Some(line) = self.line()? {
            if line.starts_with('\u{1}') {
                self.pending = Some(line);
                break;
            }
            if !line.is_empty() {
                logged.admit(parse_raw(&line)?)?;
            }
        }
        logged.finish().map(Some)
    }

    fn line(&mut self) -> Result<Option<String>, String> {
        let mut line = String::new();
        let read = self
            .stdout
            .read_line(&mut line)
            .map_err(|error| format!("git log failed: {error}"))?;
        if read == 0 {
            return Ok(None);
        }
        Ok(Some(line.strip_suffix('\n').unwrap_or(&line).to_owned()))
    }

    /// Wait for a log read to its end, refusing a failed git.
    fn finish(mut self) -> Result<(), String> {
        let mut stderr = String::new();
        if let Some(mut pipe) = self.child.stderr.take() {
            let _ = pipe.read_to_string(&mut stderr);
        }
        let status = self
            .child
            .wait()
            .map_err(|error| format!("git log failed: {error}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("git log failed: {}", stderr.trim()))
        }
    }
}

impl Drop for Log {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

/// `\u{1}COMMIT SECONDS [PARENT...]`.
fn parse_header(line: &str) -> Result<Logged, String> {
    let malformed = || format!("git log listed {line:?}");
    let mut fields = line
        .strip_prefix('\u{1}')
        .ok_or_else(malformed)?
        .split(' ')
        .filter(|field| !field.is_empty());
    let (Some(commit), Some(seconds)) = (fields.next(), fields.next()) else {
        return Err(malformed());
    };
    let seconds = seconds.parse::<i64>().map_err(|_| malformed())?;
    Ok(Logged {
        commit: commit.to_owned(),
        seconds,
        has_parent: fields.next().is_some(),
        entry: None,
    })
}

/// `:OLDMODE NEWMODE OLD NEW STATUS\tPATH`.
fn parse_raw(line: &str) -> Result<Entry, String> {
    let malformed = || format!("git log listed {line:?}");
    let (fields, path) = line
        .strip_prefix(':')
        .and_then(|line| line.split_once('\t'))
        .ok_or_else(malformed)?;
    if path != COUNTERS_FILE {
        return Err(malformed());
    }
    let fields = fields.split(' ').collect::<Vec<_>>();
    let [old_mode, new_mode, old, new, _status] = fields.as_slice() else {
        return Err(malformed());
    };
    let side = |mode: &str, oid: &str| match mode {
        ABSENT_MODE => Ok(None),
        mode if FILE_MODES.contains(&mode) => Ok(Some(oid.to_owned())),
        _ => Err(format!("{COUNTERS_FILE} is not a regular counter file")),
    };
    Ok(Entry {
        old: side(old_mode, old)?,
        new: side(new_mode, new)?,
    })
}

/// A commit's committer second and whether its object names any parent.
struct CommitHeader {
    seconds: String,
    has_parent: bool,
}

fn parse_commit(body: &[u8]) -> Result<CommitHeader, String> {
    let mut seconds = None;
    let mut has_parent = false;
    for line in body.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            break;
        }
        if line.starts_with(b"parent ") {
            has_parent = true;
        } else if let Some(committer) = line.strip_prefix(b"committer ") {
            let committer = std::str::from_utf8(committer)
                .map_err(|_| "Git commit names a non-UTF-8 committer".to_owned())?;
            seconds = committer.rsplit(' ').nth(1).map(str::to_owned);
        }
    }
    Ok(CommitHeader {
        seconds: seconds.ok_or_else(|| "Git commit names no committer second".to_owned())?,
        has_parent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = "0123456789abcdef0123456789abcdef01234567";
    const NEW: &str = "89abcdef0123456789abcdef0123456789abcdef";

    fn raw(path: &str, old_mode: &str, new_mode: &str) -> String {
        format!(":{old_mode} {new_mode} {OLD} {NEW} M\t{path}")
    }

    #[test]
    fn raw_counter_changes_require_the_canonical_regular_file() {
        let entry =
            parse_raw(&raw(COUNTERS_FILE, "100644", "100755")).expect("canonical counter change");
        assert_eq!(entry.old.as_deref(), Some(OLD));
        assert_eq!(entry.new.as_deref(), Some(NEW));

        assert!(parse_raw(&raw("dist/version/other", "100644", "100644")).is_err());
        assert!(parse_raw(&raw(COUNTERS_FILE, "040000", "100644")).is_err());
    }

    #[test]
    fn a_logged_commit_requires_exactly_one_counter_change() {
        let header = format!("\u{1}{NEW} 1");
        assert!(parse_header(&header).expect("header").finish().is_err());

        let mut logged = parse_header(&header).expect("header");
        logged
            .admit(parse_raw(&raw(COUNTERS_FILE, "100644", "100644")).expect("change"))
            .expect("first change");
        assert!(
            logged
                .admit(parse_raw(&raw(COUNTERS_FILE, "100644", "100644")).expect("change"))
                .is_err()
        );
    }
}

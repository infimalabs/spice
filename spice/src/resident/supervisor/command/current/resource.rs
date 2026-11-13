//! A lane's stateful `spice://` server in the currently installed executable.
//!
//! One helper process owns one [`crate::resource::Seat`], including its bounded
//! graph continuation cache. The supervisor checks the executable image before
//! every request and replaces the helper when an install replaces that image.
//! Requests already sent to the prior helper retain it until they finish.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command as ProcessCommand, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use spice::ambient::Ambient;
use wire::CanonicalUuid;

use super::super::Terminal;
use crate::resource::Access;

/// The hidden entry that keeps one current-release resource seat alive.
pub(in crate::resident::supervisor) const ENTRY: &str = "__lane-resource";

/// One request on the stable supervisor-to-current-release boundary.
#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: u64,
    url: String,
    content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    carrier: Option<wire::outbox::Carrier>,
}

/// One correlated answer from the current release.
#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Response {
    id: u64,
    answer: Answer,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "disposition", content = "content", rename_all = "snake_case")]
enum Answer {
    Served(String),
    Refused(String),
}

/// Identity of the executable bytes a helper opened. Install publication
/// replaces the path atomically; the Unix file identity catches that change,
/// while size and modification time retain a portable fallback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Image {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    changed_seconds: i64,
    #[cfg(unix)]
    changed_nanoseconds: i64,
    bytes: u64,
    modified: Option<SystemTime>,
}

impl Image {
    fn read(path: &Path) -> io::Result<Self> {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.file_type().is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} is not a regular executable", path.display()),
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
            if metadata.permissions().mode() & 0o111 == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("{} is not executable", path.display()),
                ));
            }
            Ok(Self {
                device: metadata.dev(),
                inode: metadata.ino(),
                changed_seconds: metadata.ctime(),
                changed_nanoseconds: metadata.ctime_nsec(),
                bytes: metadata.len(),
                modified: metadata.modified().ok(),
            })
        }
        #[cfg(not(unix))]
        Ok(Self {
            bytes: metadata.len(),
            modified: metadata.modified().ok(),
        })
    }
}

/// A value selected by the image that constructed it.
#[derive(Debug)]
struct Generation<T> {
    image: Image,
    value: T,
}

#[derive(Debug)]
struct Current<T> {
    generation: Option<Generation<T>>,
}

impl<T> Default for Current<T> {
    fn default() -> Self {
        Self { generation: None }
    }
}

impl<T> Current<T> {
    fn select(&mut self, image: Image, create: impl FnOnce() -> io::Result<T>) -> io::Result<&T> {
        if self
            .generation
            .as_ref()
            .is_none_or(|generation| generation.image != image)
        {
            let value = create()?;
            self.generation = Some(Generation { image, value });
        }
        Ok(&self.generation.as_ref().expect("selected generation").value)
    }
}

/// One occupant's resource reader. The executable path is stable across
/// installs; its image is not.
pub(super) struct Reader {
    executable: PathBuf,
    worktree: PathBuf,
    lane: CanonicalUuid,
    terminal: Option<Terminal>,
    current: Mutex<Current<Arc<Service>>>,
}

impl Reader {
    pub(super) fn new(
        lane: CanonicalUuid,
        worktree: PathBuf,
        terminal: Option<Terminal>,
    ) -> io::Result<Arc<Self>> {
        Ok(Arc::new(Self {
            executable: std::env::current_exe()?,
            worktree,
            lane,
            terminal,
            current: Mutex::new(Current::default()),
        }))
    }

    /// Serve through the helper opened from the current executable image. A
    /// broken helper is replaced once; a native resource refusal is final.
    pub(super) fn serve(&self, url: &str, access: Access<'_>) -> Result<String, String> {
        let read = matches!(access, Access::Read | Access::CapturedRead(_));
        let (content, carrier) = match access {
            Access::Read => (None, None),
            Access::CapturedRead(carrier) => (None, Some(carrier)),
            Access::Write(content) => (Some(content.to_owned()), None),
            Access::Authored { text, carrier } => (Some(text.to_owned()), Some(carrier)),
        };
        let mut last_failure = None;
        for _attempt in 0..2 {
            let image = Image::read(&self.executable)
                .map_err(|error| format!("current Spice executable is unavailable: {error}"))?;
            let service = {
                let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
                Arc::clone(current.select(image, || self.start()).map_err(|error| {
                    format!("current Spice resource reader did not start: {error}")
                })?)
            };
            match service.request(url, content.as_deref(), carrier) {
                Ok(answer) => {
                    if read {
                        let answered_by = Image::read(&self.executable).map_err(|error| {
                            format!("current Spice executable is unavailable: {error}")
                        })?;
                        if answered_by != image {
                            last_failure = Some(
                                "installed Spice executable changed during the read".to_owned(),
                            );
                            self.invalidate(image, &service);
                            continue;
                        }
                    }
                    return answer;
                }
                Err(failure) => {
                    last_failure = Some(failure);
                    self.invalidate(image, &service);
                }
            }
        }
        Err(format!(
            "current Spice resource reader failed: {}",
            last_failure.unwrap_or_else(|| "no helper answered".to_owned())
        ))
    }

    fn invalidate(&self, image: Image, service: &Arc<Service>) {
        let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        if current.generation.as_ref().is_some_and(|generation| {
            generation.image == image && Arc::ptr_eq(&generation.value, service)
        }) {
            current.generation = None;
        }
    }

    fn start(&self) -> io::Result<Arc<Service>> {
        let mut command = ProcessCommand::new(&self.executable);
        command
            .arg(ENTRY)
            .arg(self.lane.to_string())
            .arg(&self.worktree)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if let Some(terminal) = &self.terminal {
            terminal.configure_process(&mut command);
        } else {
            command.current_dir(&self.worktree);
        }
        Service::start(command)
    }
}

#[derive(Default)]
struct Replies {
    waiting: HashMap<u64, SyncSender<Result<String, String>>>,
    failure: Option<String>,
}

/// One running current-release helper and its correlated requests.
struct Service {
    stdin: Mutex<Option<ChildStdin>>,
    child: Mutex<Option<std::process::Child>>,
    replies: Arc<Mutex<Replies>>,
    sequence: AtomicU64,
}

impl Service {
    fn start(mut command: ProcessCommand) -> io::Result<Arc<Self>> {
        let mut child = command.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("resource reader has no input pipe"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("resource reader has no output pipe"))?;
        let replies = Arc::new(Mutex::new(Replies::default()));
        let service = Arc::new(Self {
            stdin: Mutex::new(Some(stdin)),
            child: Mutex::new(Some(child)),
            replies: Arc::clone(&replies),
            sequence: AtomicU64::new(0),
        });
        thread::Builder::new()
            .name("spice-resource-reader".to_owned())
            .spawn(move || read_responses(stdout, &replies))?;
        Ok(service)
    }

    fn request(
        &self,
        url: &str,
        content: Option<&str>,
        carrier: Option<wire::outbox::Carrier>,
    ) -> Result<Result<String, String>, String> {
        let id = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let request = Request {
            id,
            url: url.to_owned(),
            content: content.map(str::to_owned),
            carrier,
        };
        let (sender, receiver) = mpsc::sync_channel(1);
        {
            let mut replies = self.replies.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(failure) = &replies.failure {
                return Err(failure.clone());
            }
            replies.waiting.insert(id, sender);
        }
        let written = (|| {
            let mut input = self.stdin.lock().unwrap_or_else(PoisonError::into_inner);
            let input = input
                .as_mut()
                .ok_or_else(|| io::Error::other("resource reader input is closed"))?;
            serde_json::to_writer(&mut *input, &request).map_err(io::Error::other)?;
            input.write_all(b"\n")?;
            input.flush()
        })();
        if let Err(error) = written {
            let failure = error.to_string();
            fail(&self.replies, failure.clone());
            return Err(failure);
        }
        receiver
            .recv()
            .map_err(|_| "current Spice resource reader ended without an answer".to_owned())
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.stdin
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(mut child) = self
            .child
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            let _ = child.wait();
        }
    }
}

fn read_responses(stdout: impl io::Read, replies: &Arc<Mutex<Replies>>) {
    for line in BufReader::new(stdout).lines() {
        let response = match line {
            Ok(line) => serde_json::from_str::<Response>(&line)
                .map_err(|error| format!("invalid resource reader answer: {error}")),
            Err(error) => Err(format!("resource reader output failed: {error}")),
        };
        let response = match response {
            Ok(response) => response,
            Err(failure) => {
                fail(replies, failure);
                return;
            }
        };
        let sender = replies
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .waiting
            .remove(&response.id);
        if let Some(sender) = sender {
            let answer = match response.answer {
                Answer::Served(content) => Ok(content),
                Answer::Refused(reason) => Err(reason),
            };
            let _ = sender.send(answer);
        }
    }
    fail(replies, "current Spice resource reader exited".to_owned());
}

fn fail(replies: &Arc<Mutex<Replies>>, failure: String) {
    let waiting = {
        let mut replies = replies.lock().unwrap_or_else(PoisonError::into_inner);
        replies.failure = Some(failure.clone());
        std::mem::take(&mut replies.waiting)
    };
    for sender in waiting.into_values() {
        let _ = sender.send(Err(failure.clone()));
    }
}

/// Serve requests until the old supervisor closes this helper's input. The
/// current executable owns parsing, command dispatch, and the stateful graph
/// cache; the old supervisor understands only this small framed contract.
pub(in crate::resident::supervisor) fn run(
    lane: &OsString,
    worktree: &OsString,
    ambient: &Ambient,
) -> io::Result<()> {
    let lane = lane
        .to_str()
        .and_then(|lane| lane.parse::<CanonicalUuid>().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "lane is not a UUID"))?;
    let worktree = PathBuf::from(worktree);
    let repository =
        spice::repository::Repository::discover(&worktree).map_err(io::Error::other)?;
    let paths = super::super::super::RuntimePaths::from_repository(&repository)
        .map_err(|outcome| io::Error::other(format!("{outcome:?}")))?;
    let server = crate::resource::lane_server(
        paths.serve,
        paths.journal,
        lane,
        repository.worktree_root().to_path_buf(),
        ambient.clone(),
    );
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    for line in stdin.lock().lines() {
        let request: Request = serde_json::from_str(&line?).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid resource request: {error}"),
            )
        })?;
        let access = match (request.content.as_deref(), request.carrier) {
            (Some(text), Some(carrier)) => Access::Authored { text, carrier },
            (Some(text), None) => Access::Write(text),
            (None, Some(carrier)) => Access::CapturedRead(carrier),
            (None, None) => Access::Read,
        };
        let answer = match server(&request.url, access) {
            Ok(content) => Answer::Served(content),
            Err(reason) => Answer::Refused(reason),
        };
        serde_json::to_writer(
            &mut stdout,
            &Response {
                id: request.id,
                answer,
            },
        )
        .map_err(io::Error::other)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(bytes: u64) -> Image {
        Image {
            #[cfg(unix)]
            device: 1,
            #[cfg(unix)]
            inode: 1,
            #[cfg(unix)]
            changed_seconds: 1,
            #[cfg(unix)]
            changed_nanoseconds: 0,
            bytes,
            modified: None,
        }
    }

    #[test]
    fn current_generation_reuses_one_image_and_replaces_another() {
        let mut current = Current::default();
        let mut starts = 0;
        let first = *current
            .select(image(1), || {
                starts += 1;
                Ok(starts)
            })
            .expect("first generation");
        let same = *current
            .select(image(1), || {
                starts += 1;
                Ok(starts)
            })
            .expect("same generation");
        let replacement = *current
            .select(image(2), || {
                starts += 1;
                Ok(starts)
            })
            .expect("replacement generation");
        assert_eq!((first, same, replacement, starts), (1, 1, 2, 2));
    }

    #[test]
    fn responses_are_correlated_and_preserve_native_outcomes() {
        let (first_sender, first_receiver) = mpsc::sync_channel(1);
        let (second_sender, second_receiver) = mpsc::sync_channel(1);
        let replies = Arc::new(Mutex::new(Replies {
            waiting: HashMap::from([(1, first_sender), (2, second_sender)]),
            failure: None,
        }));
        let encoded = [
            Response {
                id: 2,
                answer: Answer::Refused("stale digest\nread again".to_owned()),
            },
            Response {
                id: 1,
                answer: Answer::Served("first\nsecond\n".to_owned()),
            },
        ]
        .map(|response| serde_json::to_string(&response).expect("response encodes"))
        .join("\n");

        read_responses(encoded.as_bytes(), &replies);

        assert_eq!(
            first_receiver.recv().expect("first response"),
            Ok("first\nsecond\n".to_owned())
        );
        assert_eq!(
            second_receiver.recv().expect("second response"),
            Err("stale digest\nread again".to_owned())
        );
    }
}

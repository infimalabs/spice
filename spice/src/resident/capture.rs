//! Bounded bridge from lane capture workers into the resident event loop.

use std::collections::{BTreeSet, VecDeque};
#[cfg(unix)]
use std::fs;
use std::io;
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::Arc;

use lane::capture::{
    CaptureExit, CaptureFailure, CaptureRecord, CaptureReportError, CaptureReporter,
};
use tokio::sync::mpsc;
use wire::CanonicalUuid;

use super::supervisor::Notice;

const RECORD_CAPACITY: usize = 64;
const NOTICE_BYTES: usize = 4 * 1024;
/// Kernel queue the notice socket keeps while the resident is mid-transaction.
///
/// A supervisor sends one datagram per tape record and cannot wait: when the
/// queue is full the kernel refuses the send outright. Darwin's default receive
/// space is 4 KiB, seventeen record notices, and one journal commit outlasts
/// that on a busy lane, so most notices were refused before this queue grew.
#[cfg(unix)]
const NOTICE_QUEUE_BYTES: usize = 1024 * 1024;
/// Notices one loop turn takes from the drained queue before it sweeps.
pub(super) const NOTICE_BATCH: usize = 1024;

/// Resident-generation endpoint for tiny supervisor tape notices.
///
/// The socket is read by its own task so the kernel queue empties whenever the
/// event loop yields at all, not only when the loop reaches the notice arm.
/// Everything read waits in an unbounded in-process queue until the loop takes
/// a batch, so a burst that arrives during one journal commit is never lost.
///
/// A seat sweep judges a supervisor by whether its process is gone, and a
/// supervisor sends its ending notice and exits right after, so at the moment
/// a sweep could speak for it that notice is in the kernel queue or already
/// drained. [`Self::pull`] empties both queues without a scheduler turn, and
/// [`Self::noticed_endings`] stages what it pulls, so the sweep asks the door
/// and leaves every seat whose ending is waiting there to the notice arm.
///
/// On Windows the door acknowledges a notice only once it waits in the
/// in-process queue, so the supervisor's send returning already means the
/// notice is there and no kernel queue sits behind it.
pub(super) struct SupervisorReports {
    notices: mpsc::UnboundedReceiver<io::Result<Notice>>,
    staged: VecDeque<io::Result<Notice>>,
    #[cfg(unix)]
    socket: Arc<tokio::net::UnixDatagram>,
    #[cfg(unix)]
    drain: tokio::task::JoinHandle<()>,
    #[cfg(unix)]
    path: PathBuf,
    #[cfg(windows)]
    _door: door::Listener,
}

impl SupervisorReports {
    /// Bind the notice door and start draining it. Requires a live runtime.
    pub(super) fn bind(paths: &super::RuntimePaths) -> super::CommandResult<Self> {
        #[cfg(windows)]
        {
            let path = super::supervisor::capture_path(paths);
            let (sender, notices) = mpsc::unbounded_channel();
            let door = door::Listener::bind(&path, NOTICE_BYTES, move |message| {
                let _ = sender.send(decode(&message));
            })
            .map_err(|error| {
                let code = if error.kind() == io::ErrorKind::AlreadyExists {
                    "lane::supervisor-capture-invalid"
                } else {
                    "lane::supervisor-capture-unavailable"
                };
                super::environment_fault(code, &error.to_string())
            })?;
            Ok(Self {
                notices,
                staged: VecDeque::new(),
                _door: door,
            })
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _};

            let path = super::supervisor::capture_path(paths);
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_socket() => {
                    fs::remove_file(&path).map_err(|error| {
                        super::environment_fault(
                            "lane::supervisor-capture-unavailable",
                            &error.to_string(),
                        )
                    })?;
                }
                Ok(_) => {
                    return Err(super::environment_fault(
                        "lane::supervisor-capture-invalid",
                        &format!("{} is not a Unix socket", path.display()),
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(super::environment_fault(
                        "lane::supervisor-capture-unavailable",
                        &error.to_string(),
                    ));
                }
            }
            let socket = tokio::net::UnixDatagram::bind(&path).map_err(|error| {
                super::environment_fault("lane::supervisor-capture-unavailable", &error.to_string())
            })?;
            socket2::SockRef::from(&socket)
                .set_recv_buffer_size(NOTICE_QUEUE_BYTES)
                .map_err(|error| {
                    super::environment_fault(
                        "lane::supervisor-capture-unavailable",
                        &error.to_string(),
                    )
                })?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|error| {
                super::environment_fault("lane::supervisor-capture-unavailable", &error.to_string())
            })?;
            let socket = Arc::new(socket);
            let (sender, notices) = mpsc::unbounded_channel();
            let drain = tokio::spawn(drain_notices(Arc::clone(&socket), sender));
            Ok(Self {
                notices,
                staged: VecDeque::new(),
                socket,
                drain,
                path,
            })
        }
    }

    /// Everything staged, waiting in the in-process queue, and behind it in
    /// the kernel queue, in arrival order, without waiting on the drain task.
    ///
    /// The kernel queue is read through the descriptor itself: the runtime's
    /// own non-blocking read answers from readiness its I/O driver last
    /// recorded, which is exactly the scheduler turn this read must not wait
    /// for. The drain task keeps reading after this; a datagram is taken by
    /// exactly one reader, so nothing crosses twice.
    pub(super) fn pull(&mut self) -> Vec<io::Result<Notice>> {
        let mut notices: Vec<_> = self.staged.drain(..).collect();
        while let Ok(notice) = self.notices.try_recv() {
            notices.push(notice);
        }
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd as _;

            let mut buffer = vec![0_u8; NOTICE_BYTES];
            let socket = self.socket.as_raw_fd();
            loop {
                // SAFETY: the descriptor is open and non-blocking for as long
                // as `self.socket` lives, and the write is bounded by the
                // buffer's own length.
                let received =
                    unsafe { libc::recv(socket, buffer.as_mut_ptr().cast(), buffer.len(), 0) };
                match usize::try_from(received) {
                    Ok(received) => notices.push(decode(&buffer[..received])),
                    Err(_) => {
                        let error = io::Error::last_os_error();
                        if error.kind() != io::ErrorKind::WouldBlock {
                            notices.push(Err(error));
                        }
                        break;
                    }
                }
            }
        }
        coalesce(notices)
    }

    /// The seats whose ending notice is already at the door.
    ///
    /// A sweep judges a supervisor by its exit, and a supervisor sends its
    /// ending before it exits, so an ending the sweep could see is either
    /// accounted already or waiting here. Everything pulled stays staged in
    /// arrival order for the notice arm, which accepts the ending after the
    /// records that came before it.
    pub(super) fn noticed_endings(&mut self) -> BTreeSet<(CanonicalUuid, CanonicalUuid)> {
        let pulled = self.pull();
        let noticed = pulled
            .iter()
            .filter_map(|notice| match notice {
                Ok(Notice::Exited { lane, occupant } | Notice::Failed { lane, occupant, .. }) => {
                    Some((*lane, *occupant))
                }
                _ => None,
            })
            .collect();
        self.staged.extend(pulled);
        noticed
    }

    /// The next notice: one staged by a sweep, else one read off the socket.
    pub(super) async fn recv(&mut self) -> io::Result<Notice> {
        if let Some(staged) = self.staged.pop_front() {
            return staged;
        }
        self.notices.recv().await.unwrap_or_else(|| {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the supervisor notice drain ended",
            ))
        })
    }

    /// One received notice plus everything already queued behind it, with the
    /// record notices a later record notice for the same occupant supersedes
    /// left out.
    ///
    /// The sweep a record notice triggers accounts and presents every record
    /// on that lane's tape up to its head, so of a run of record notices for
    /// one occupant only the last one needs to cross. Exit and failure notices
    /// keep their place in the order.
    pub(super) fn batch(&mut self, first: io::Result<Notice>) -> Vec<io::Result<Notice>> {
        let mut notices = vec![first];
        notices.extend(self.staged.drain(..));
        while notices.len() < NOTICE_BATCH {
            match self.notices.try_recv() {
                Ok(notice) => notices.push(notice),
                Err(_) => break,
            }
        }
        coalesce(notices)
    }
}

#[cfg(unix)]
impl Drop for SupervisorReports {
    fn drop(&mut self) {
        self.drain.abort();
        let _ = fs::remove_file(&self.path);
    }
}

/// Read datagrams for as long as the resident loop keeps its receiver.
#[cfg(unix)]
async fn drain_notices(
    socket: Arc<tokio::net::UnixDatagram>,
    notices: mpsc::UnboundedSender<io::Result<Notice>>,
) {
    let mut buffer = vec![0_u8; NOTICE_BYTES];
    loop {
        let notice = match socket.recv(&mut buffer).await {
            Ok(received) => decode(&buffer[..received]),
            Err(error) => Err(error),
        };
        if notices.send(notice).is_err() {
            return;
        }
    }
}

fn decode(datagram: &[u8]) -> io::Result<Notice> {
    serde_json::from_slice(datagram)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Keep only the last record notice per occupant, and every other notice.
fn coalesce(notices: Vec<io::Result<Notice>>) -> Vec<io::Result<Notice>> {
    let mut kept = Vec::with_capacity(notices.len());
    let mut occupants: BTreeSet<(CanonicalUuid, CanonicalUuid)> = BTreeSet::new();
    for notice in notices.into_iter().rev() {
        if let Ok(Notice::Record { lane, occupant, .. }) = &notice
            && !occupants.insert((*lane, *occupant))
        {
            continue;
        }
        kept.push(notice);
    }
    kept.reverse();
    kept
}

/// The capture record one record notice names, read back from the tape.
///
/// A notice from an occupant the lane no longer seats is refused out loud:
/// the closing sweep at its exit already presented that tape, and a silent
/// refusal here is indistinguishable from a lost record.
pub(super) fn materialize(
    paths: &super::RuntimePaths,
    residency: &wire::ResidencyHead,
    notice: Notice,
    diagnostic: &mut dyn io::Write,
) -> super::CommandResult<Option<CaptureRecord>> {
    let Notice::Record {
        lane,
        occupant,
        stream,
        boundary,
        locus,
    } = notice
    else {
        return Ok(None);
    };
    let Some(row) = residency.lanes.iter().find(|row| row.lane == lane) else {
        return Ok(None);
    };
    if row.occupant != Some(occupant) {
        let _ = writeln!(
            diagnostic,
            "lane {lane} record notice from occupant {occupant} after its seat closed; \
             the closing sweep presented that tape (seated now: {})",
            row.occupant
                .map_or_else(|| "nobody".to_owned(), |seated| seated.to_string())
        );
        return Ok(None);
    }
    let Some(vendor) = super::ingest_vendor(&row.vendor) else {
        return Ok(None);
    };
    let stream = match stream.as_str() {
        "stdout" => vendor::RecordStream::Stdout,
        "stderr" => vendor::RecordStream::Stderr,
        other => {
            return Err(super::store_fault(
                "lane::supervisor-capture-invalid",
                &format!("unknown record stream {other:?}"),
            ));
        }
    };
    let boundary = match boundary.as_str() {
        "complete" => sill::RecordBoundary::Complete,
        "terminal" => sill::RecordBoundary::Terminal,
        "truncated" => sill::RecordBoundary::Truncated,
        other => {
            return Err(super::store_fault(
                "lane::supervisor-capture-invalid",
                &format!("unknown record boundary {other:?}"),
            ));
        }
    };
    let locus = tape::Locus::new(locus.member, locus.offset, locus.length);
    let tape = tape::Tape::open_existing(paths.lane_tape(lane).join(row.vendor.as_str())).map_err(
        |error| super::store_fault("lane::supervisor-capture-unavailable", &error.to_string()),
    )?;
    let bytes = tape.read(locus).map_err(|error| {
        super::store_fault("lane::supervisor-capture-unavailable", &error.to_string())
    })?;
    let decoded = listen::decode(vendor.dialect(), &bytes, locus).map_err(|error| {
        super::store_fault("lane::supervisor-capture-invalid", &error.to_string())
    })?;
    Ok(Some(CaptureRecord {
        lane,
        occupant,
        stream,
        boundary,
        locus,
        declaration: None,
        decoded,
    }))
}

pub(super) struct Channels {
    pub(super) reporter: Arc<dyn CaptureReporter>,
    pub(super) records: mpsc::Receiver<CaptureRecord>,
    pub(super) failures: mpsc::UnboundedReceiver<CaptureFailure>,
    pub(super) exits: mpsc::UnboundedReceiver<CaptureExit>,
}

pub(super) fn channels() -> Channels {
    let (records, record_receiver) = mpsc::channel(RECORD_CAPACITY);
    let (failures, failure_receiver) = mpsc::unbounded_channel();
    let (exits, exit_receiver) = mpsc::unbounded_channel();
    Channels {
        reporter: Arc::new(Reporter {
            records,
            failures,
            exits,
        }),
        records: record_receiver,
        failures: failure_receiver,
        exits: exit_receiver,
    }
}

struct Reporter {
    records: mpsc::Sender<CaptureRecord>,
    failures: mpsc::UnboundedSender<CaptureFailure>,
    exits: mpsc::UnboundedSender<CaptureExit>,
}

impl CaptureReporter for Reporter {
    fn record(&self, record: CaptureRecord) -> Result<(), CaptureReportError> {
        self.records.try_send(record).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => {
                CaptureReportError::new("resident capture report queue is full")
            }
            mpsc::error::TrySendError::Closed(_) => {
                CaptureReportError::new("resident capture report receiver is closed")
            }
        })
    }

    fn failed(&self, failure: CaptureFailure) {
        let _sent = self.failures.send(failure);
    }

    fn exited(&self, exit: CaptureExit) {
        let _sent = self.exits.send(exit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(lane: u8, occupant: u8, offset: u64) -> io::Result<Notice> {
        Ok(Notice::Record {
            lane: uuid(lane),
            occupant: uuid(occupant),
            stream: "stdout".to_owned(),
            boundary: "complete".to_owned(),
            locus: wire::TapeLocus {
                member: 1,
                offset,
                length: 1,
            },
        })
    }

    fn uuid(byte: u8) -> CanonicalUuid {
        format!("00000000-0000-4000-8000-0000000000{byte:02x}")
            .parse()
            .expect("fixture uuid")
    }

    fn spelled(notices: &[io::Result<Notice>]) -> Vec<String> {
        notices
            .iter()
            .map(|notice| match notice {
                Ok(Notice::Record {
                    occupant, locus, ..
                }) => {
                    format!("record {occupant} @{}", locus.offset)
                }
                Ok(Notice::Idle { occupant, .. }) => format!("idle {occupant}"),
                Ok(Notice::Active { occupant, .. }) => format!("active {occupant}"),
                Ok(Notice::Exited { occupant, .. }) => format!("exited {occupant}"),
                Ok(Notice::Failed { occupant, .. }) => format!("failed {occupant}"),
                Ok(Notice::Asked { request, .. }) => format!("asked {request}"),
                Ok(Notice::FastRefused { occupant, .. }) => format!("fast refused {occupant}"),
                Err(error) => format!("refused {error}"),
            })
            .collect()
    }

    #[test]
    fn a_batch_keeps_the_last_record_notice_per_occupant_and_every_other_notice() {
        let exited = Ok(Notice::Exited {
            lane: uuid(1),
            occupant: uuid(2),
        });
        let refused = Err(io::Error::new(io::ErrorKind::InvalidData, "garbled"));
        let batch = vec![
            record(1, 2, 0),
            record(1, 2, 10),
            record(3, 4, 0),
            refused,
            record(1, 2, 20),
            exited,
            record(3, 4, 10),
        ];

        assert_eq!(
            spelled(&coalesce(batch)),
            vec![
                "refused garbled".to_owned(),
                format!("record {} @20", uuid(2)),
                format!("exited {}", uuid(2)),
                format!("record {} @10", uuid(4)),
            ]
        );
    }
}

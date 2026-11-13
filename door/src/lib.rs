//! Bounded one-message doors between local processes of one user.
//!
//! A door is an endpoint at a filesystem path. Its owner binds it, and any
//! process of the same user sends it one whole message at a time; a message
//! larger than the door's capacity never reaches the owner. Binding replaces a
//! stale endpoint left at the path, and dropping the door removes the endpoint
//! only while it is still this door's, so a successor bound at the same path
//! keeps receiving.
//!
//! On Unix the endpoint is an owner-only datagram socket. Darwin queues a
//! megabyte for the owner, so a burst waits for a busy owner rather than
//! failing at the platform's few-kilobyte default; Linux bounds a door's
//! unread datagrams by count (`net.unix.max_dgram_qlen`, ten by default) and
//! holds a sender at that bound until the owner reads. On Windows it is a
//! file naming an owner-only, local-only, message-mode named pipe; the door
//! queues each message before acknowledging it, so a returned [`send`] means
//! the message is already waiting for the owner.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as platform;
#[cfg(windows)]
pub use windows::{Listener, current_user_sid, handoff};

/// The bound end of one door.
pub struct Door {
    inner: platform::Door,
    path: PathBuf,
}

impl Door {
    /// Bind the door at `path`, admitting messages of at most `capacity`
    /// bytes. A stale endpoint at `path` is replaced; anything else there is
    /// refused.
    ///
    /// # Errors
    ///
    /// Returns the error that prevents binding, or `AlreadyExists` when `path`
    /// names something other than a door endpoint.
    pub fn bind(path: &Path, capacity: usize) -> io::Result<Self> {
        Ok(Self {
            inner: platform::Door::bind(path, capacity)?,
            path: path.to_owned(),
        })
    }

    /// Receive the next whole message. `None` waits indefinitely, and a zero
    /// timeout only takes a message already waiting.
    ///
    /// # Errors
    ///
    /// Returns `TimedOut` when no message arrives in time, or the error that
    /// ended the door.
    pub fn recv(&self, timeout: Option<Duration>) -> io::Result<Vec<u8>> {
        self.inner.recv(timeout)
    }

    /// Withdraw the endpoint while it is still this door's, so no new message
    /// reaches the door. Closing again changes nothing; dropping closes.
    pub fn close(&self) {
        self.inner.close(&self.path);
    }
}

impl Drop for Door {
    fn drop(&mut self) {
        self.close();
    }
}

/// Send one whole message to the door bound at `path`.
///
/// # Errors
///
/// Returns the error when no door is bound at `path` or the door does not take
/// the message.
pub fn send(path: &Path, message: &[u8]) -> io::Result<()> {
    Sender::new()?.send(path, message)
}

/// A sending end reused across many messages.
pub struct Sender {
    inner: platform::Sender,
}

impl Sender {
    /// Open a sending end.
    ///
    /// # Errors
    ///
    /// Returns the error that prevents opening it.
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            inner: platform::Sender::new()?,
        })
    }

    /// Send one whole message to the door bound at `path`.
    ///
    /// # Errors
    ///
    /// Returns the error when no door is bound at `path` or the door does not
    /// take the message.
    pub fn send(&self, path: &Path, message: &[u8]) -> io::Result<()> {
        self.inner.send(path, message)
    }
}

fn timed_out() -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "no message arrived at the door in time",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    use super::*;

    const CAPACITY: usize = 64;
    const WAIT: Duration = Duration::from_secs(5);
    /// A pass's burst of round-update kicks, each a kilobyte.
    const BURST: usize = 64;
    const KICK: usize = 1024;

    fn kick(index: usize) -> [u8; KICK] {
        [u8::try_from(index).expect("index fits"); KICK]
    }

    /// A short unique door path: Darwin socket paths are bounded to 104 bytes.
    fn door_path() -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let index = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir()
            .canonicalize()
            .expect("the temporary directory resolves")
            .join(format!("door-{}-{index}", std::process::id()))
    }

    #[test]
    fn each_receive_takes_exactly_one_whole_message() {
        let path = door_path();
        let door = Door::bind(&path, CAPACITY).expect("door binds");

        send(&path, b"first").expect("first message sends");
        send(&path, b"").expect("empty message sends");
        send(&path, &[7; CAPACITY]).expect("a message at capacity sends");

        assert_eq!(door.recv(Some(WAIT)).expect("first arrives"), b"first");
        assert_eq!(door.recv(Some(WAIT)).expect("empty arrives"), b"");
        assert_eq!(door.recv(Some(WAIT)).expect("full arrives"), [7; CAPACITY]);
    }

    /// Darwin sizes the door's queue by its receive buffer and Windows queues
    /// every message, so a whole burst waits unread; Linux would hold this
    /// sender at its unread bound before the owner ever reads.
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn a_burst_waits_whole_and_in_order_for_an_owner_not_yet_reading() {
        let path = door_path();
        let door = Door::bind(&path, KICK).expect("door binds");

        // Far past the few kilobytes a platform default queues for a
        // datagram socket, so a busy owner loses nothing to a pass's burst.
        let sender = Sender::new().expect("sender opens");
        for index in 0..BURST {
            sender
                .send(&path, &kick(index))
                .expect("the door queues every message of the burst");
        }
        for index in 0..BURST {
            assert_eq!(
                door.recv(Some(WAIT)).expect("queued message arrives"),
                kick(index)
            );
        }
    }

    #[test]
    fn a_burst_past_the_unread_bound_arrives_whole_and_in_order_as_the_owner_reads() {
        let path = door_path();
        let door = Door::bind(&path, KICK).expect("door binds");

        // Past Linux's default of ten unread datagrams, so its sender waits on
        // the owner. The sender runs apart and every receive is bounded, so a
        // sender that never resumes fails the check instead of stalling it.
        let target = path.clone();
        let sending = std::thread::spawn(move || {
            let sender = Sender::new().expect("sender opens");
            for index in 0..BURST {
                sender
                    .send(&target, &kick(index))
                    .expect("the door takes every message of the burst");
            }
        });
        for index in 0..BURST {
            assert_eq!(
                door.recv(Some(WAIT)).expect("burst message arrives"),
                kick(index)
            );
        }
        sending.join().expect("the sender finishes");
    }

    #[test]
    fn an_oversized_message_never_reaches_the_owner() {
        let path = door_path();
        let door = Door::bind(&path, CAPACITY).expect("door binds");

        // Unix accepts the datagram and the door discards it; Windows refuses
        // it at the door. Either way the owner sees only what fits.
        let _ = send(&path, &[1; CAPACITY + 1]);
        send(&path, b"fits").expect("a fitting message sends");

        assert_eq!(
            door.recv(Some(WAIT)).expect("fitting message arrives"),
            b"fits"
        );
    }

    #[test]
    fn receive_waits_no_longer_than_its_timeout() {
        let path = door_path();
        let door = Door::bind(&path, CAPACITY).expect("door binds");

        let polled = door
            .recv(Some(Duration::ZERO))
            .expect_err("nothing is waiting");
        assert_eq!(polled.kind(), io::ErrorKind::TimedOut);

        let started = Instant::now();
        let waited = door
            .recv(Some(Duration::from_millis(50)))
            .expect_err("nothing arrives");
        assert_eq!(waited.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < WAIT, "the timeout bounds the wait");

        send(&path, b"late").expect("message sends");
        assert_eq!(
            door.recv(Some(Duration::ZERO)).expect("waiting message"),
            b"late"
        );
    }

    #[test]
    fn rebinding_replaces_the_endpoint_and_the_old_door_leaves_it_alone() {
        let path = door_path();
        let stale = Door::bind(&path, CAPACITY).expect("first door binds");
        let current = Door::bind(&path, CAPACITY).expect("a rebind replaces the endpoint");

        send(&path, b"to the successor").expect("message sends");
        assert_eq!(
            current.recv(Some(WAIT)).expect("successor receives"),
            b"to the successor"
        );

        drop(stale);
        send(&path, b"still here").expect("the successor's endpoint survives");
        assert_eq!(
            current.recv(Some(WAIT)).expect("successor receives"),
            b"still here"
        );
    }

    #[test]
    fn dropping_the_door_removes_its_endpoint() {
        let path = door_path();
        let door = Door::bind(&path, CAPACITY).expect("door binds");
        drop(door);

        assert!(!path.exists(), "the endpoint is gone");
        send(&path, b"nobody").expect_err("nothing listens");
    }

    #[test]
    fn binding_refuses_a_path_that_is_not_a_door() {
        let path = door_path();
        std::fs::write(&path, b"ordinary file").expect("file writes");

        let refused = Door::bind(&path, CAPACITY).err().expect("binding refuses");
        assert_eq!(refused.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read(&path).expect("file survives"),
            b"ordinary file"
        );
        std::fs::remove_file(&path).expect("file removes");
    }

    #[test]
    fn only_the_owning_user_may_reach_the_door() {
        let path = door_path();
        let door = Door::bind(&path, CAPACITY).expect("door binds");

        platform::assert_owner_only(&door.inner, &path);
    }
}

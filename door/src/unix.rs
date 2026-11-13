//! Doors as owner-only Unix datagram sockets.

use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _};
use std::os::unix::net::UnixDatagram;
use std::path::Path;
use std::time::{Duration, Instant};

/// Bytes a door queues for its owner where the platform sizes a local
/// datagram queue by its receive buffer, so a burst behind a busy owner waits
/// instead of failing against the few-kilobyte default. Linux bounds unread
/// datagrams by count instead and holds the sender until the owner reads.
#[cfg(not(target_os = "linux"))]
const QUEUE_BYTES: usize = 1024 * 1024;

pub(crate) struct Door {
    socket: UnixDatagram,
    capacity: usize,
    /// Device and inode of the socket this door bound, so cleanup never
    /// removes a successor's endpoint.
    identity: (u64, u64),
}

impl Door {
    pub(crate) fn bind(path: &Path, capacity: usize) -> io::Result<Self> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_socket() => fs::remove_file(path)?,
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} is not a door endpoint", path.display()),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let socket = UnixDatagram::bind(path)?;
        #[cfg(not(target_os = "linux"))]
        socket2::SockRef::from(&socket).set_recv_buffer_size(QUEUE_BYTES)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        let metadata = fs::symlink_metadata(path)?;
        Ok(Self {
            socket,
            capacity,
            identity: (metadata.dev(), metadata.ino()),
        })
    }

    pub(crate) fn recv(&self, timeout: Option<Duration>) -> io::Result<Vec<u8>> {
        let deadline = timeout.map(|timeout| Instant::now() + timeout);
        // One byte past capacity tells an oversized datagram from a full one.
        let mut buffer = vec![0; self.capacity + 1];
        loop {
            let received = match deadline {
                None => {
                    self.socket.set_read_timeout(None)?;
                    self.socket.recv(&mut buffer)
                }
                Some(deadline) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        self.socket.set_nonblocking(true)?;
                        let received = self.socket.recv(&mut buffer);
                        self.socket.set_nonblocking(false)?;
                        received
                    } else {
                        self.socket.set_read_timeout(Some(remaining))?;
                        self.socket.recv(&mut buffer)
                    }
                }
            };
            match received {
                Ok(length) if length <= self.capacity => {
                    buffer.truncate(length);
                    return Ok(buffer);
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return Err(super::timed_out());
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub(crate) fn close(&self, path: &Path) {
        if fs::symlink_metadata(path)
            .is_ok_and(|metadata| (metadata.dev(), metadata.ino()) == self.identity)
        {
            let _ = fs::remove_file(path);
        }
    }
}

pub(crate) struct Sender {
    socket: UnixDatagram,
}

impl Sender {
    pub(crate) fn new() -> io::Result<Self> {
        Ok(Self {
            socket: UnixDatagram::unbound()?,
        })
    }

    pub(crate) fn send(&self, path: &Path, message: &[u8]) -> io::Result<()> {
        self.socket.send_to(message, path).map(|_| ())
    }
}

#[cfg(test)]
pub(crate) fn assert_owner_only(_door: &Door, path: &Path) {
    let mode = fs::symlink_metadata(path)
        .expect("endpoint exists")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600, "only the owner may write the socket");
}

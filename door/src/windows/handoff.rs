//! A one-shot launch channel from a parent to the exact child it spawns.
//!
//! The parent creates the only instance of a fresh owner-only, local-only
//! pipe before it spawns, names it to the child through one environment
//! variable, and accepts the connection only from the child's process id; a
//! connection from any other process is refused. The pipe's handles are never
//! inheritable, so nothing but its name crosses to the child.

use std::fs::File;
use std::io;
use std::os::windows::io::{FromRawHandle as _, OwnedHandle};
use std::process::Command;
use std::ptr::null_mut;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Foundation::{ERROR_PIPE_CONNECTED, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};

use super::{CONNECT_BOUND, Handle, connect, owner_only, unique, wide};

const PIPE_PREFIX: &str = r"\\.\pipe\spice-handoff-";
const BUFFER_BYTES: u32 = 64 * 1024;

/// The parent's end, open before the child exists.
pub struct Offer {
    name: String,
    pipe: Handle,
}

impl Offer {
    /// Create the channel's only pipe instance.
    ///
    /// # Errors
    ///
    /// Returns the error that prevents creating the pipe.
    pub fn open() -> io::Result<Self> {
        let descriptor = owner_only()?;
        let name = format!("{PIPE_PREFIX}{}", unique());
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: 0,
        };
        let wide_name = wide(&name);
        // SAFETY: `wide_name` is NUL-terminated and `attributes` outlives the call.
        let pipe = unsafe {
            CreateNamedPipeW(
                wide_name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                BUFFER_BYTES,
                BUFFER_BYTES,
                0,
                &attributes,
            )
        };
        if pipe == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            name,
            pipe: Handle(pipe),
        })
    }

    /// Name the channel to the child `command` spawns through `variable`.
    pub fn grant(&self, command: &mut Command, variable: &str) {
        command.env(variable, &self.name);
    }

    /// The channel to process `child` once it connects, within `bound`.
    ///
    /// # Errors
    ///
    /// Returns `TimedOut` when nothing connects in time, `PermissionDenied`
    /// when a process other than `child` connected, or the pipe's error.
    pub fn accept(self, child: u32, bound: Duration) -> io::Result<File> {
        let Self { name, pipe } = self;
        let (connected, arrival) = mpsc::channel();
        let waiter = thread::spawn(move || {
            // SAFETY: `pipe` is the open server instance this thread owns.
            let joined = unsafe { ConnectNamedPipe(pipe.0, null_mut()) } != 0;
            let outcome = if joined {
                Ok(())
            } else {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32) {
                    Ok(())
                } else {
                    Err(error)
                }
            };
            let _ = connected.send(());
            (pipe, outcome)
        });
        let timed_out = arrival.recv_timeout(bound).is_err();
        if timed_out {
            // Taking the only instance releases the waiter; a refusal means a
            // connection already released it.
            drop(connect(&wide(&name), Duration::ZERO));
        }
        let (pipe, outcome) = waiter
            .join()
            .map_err(|_| io::Error::other("the launch channel waiter panicked"))?;
        if timed_out {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the child did not take its launch channel in time",
            ));
        }
        outcome?;
        let mut client = 0_u32;
        // SAFETY: `pipe` is a connected server instance; `client` is writable.
        if unsafe { GetNamedPipeClientProcessId(pipe.0, &mut client) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if client != child {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "a process other than the child took its launch channel",
            ));
        }
        Ok(file(pipe))
    }
}

/// Take the launch channel a parent granted this process under `name`.
///
/// # Errors
///
/// Returns `InvalidData` when `name` is no launch channel, or the error
/// connecting to it.
pub fn join(name: &str) -> io::Result<File> {
    if !name.starts_with(PIPE_PREFIX) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{name} does not name a launch channel"),
        ));
    }
    Ok(file(connect(&wide(name), CONNECT_BOUND)?))
}

fn file(pipe: Handle) -> File {
    // SAFETY: `into_raw` transfers sole ownership of the open handle.
    File::from(unsafe { OwnedHandle::from_raw_handle(pipe.into_raw()) })
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};

    use super::*;

    const BOUND: Duration = Duration::from_secs(5);

    #[test]
    fn the_exact_child_gets_a_two_way_channel() {
        let offer = Offer::open().expect("channel opens");
        let name = offer.name.clone();
        let child = thread::spawn(move || {
            let mut channel = join(&name).expect("child joins its channel");
            channel.write_all(b"ready").expect("child writes");
            let mut answer = [0_u8; 2];
            channel.read_exact(&mut answer).expect("child reads");
            answer
        });

        let mut channel = offer
            .accept(std::process::id(), BOUND)
            .expect("the expected process is accepted");
        let mut status = [0_u8; 5];
        channel.read_exact(&mut status).expect("parent reads");
        assert_eq!(&status, b"ready");
        channel.write_all(b"ok").expect("parent writes");
        assert_eq!(&child.join().expect("child joins"), b"ok");
    }

    #[test]
    fn another_process_is_refused() {
        let offer = Offer::open().expect("channel opens");
        let name = offer.name.clone();
        let intruder = thread::spawn(move || join(&name).map(drop));

        let refused = offer
            .accept(std::process::id().wrapping_add(1), BOUND)
            .expect_err("a different process id is refused");
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
        intruder
            .join()
            .expect("intruder joins")
            .expect("intruder connected");
    }

    #[test]
    fn a_child_that_never_connects_times_out() {
        let offer = Offer::open().expect("channel opens");
        let missed = offer
            .accept(std::process::id(), Duration::from_millis(50))
            .expect_err("nobody connects");
        assert_eq!(missed.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn a_second_channel_cannot_take_an_existing_name() {
        let offer = Offer::open().expect("channel opens");
        let name = wide(&offer.name);
        let descriptor = owner_only().expect("descriptor builds");
        let squatter = super::super::instance(&name, true, 1, &descriptor);
        assert!(squatter.is_err(), "the first instance owns the name");
    }

    #[test]
    fn joining_refuses_a_name_that_is_no_launch_channel() {
        let refused = join(r"\\.\pipe\unrelated").expect_err("foreign names are refused");
        assert_eq!(refused.kind(), io::ErrorKind::InvalidData);
    }
}

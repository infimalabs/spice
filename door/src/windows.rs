//! Doors as endpoint files naming owner-only, local-only, message-mode named
//! pipes.
//!
//! Binding creates the first instance of a fresh pipe name, so no other
//! process can already hold it, and only then publishes the name at the door
//! path. One server thread keeps a listening instance ahead of the connection
//! it serves, reads one bounded message per connection, hands it to the sink,
//! and acknowledges it. A sender returns only after that acknowledgement.

use std::fs;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE,
    INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile,
    SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT, WriteFile,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_MESSAGE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_MESSAGE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    SetNamedPipeHandleState, WaitNamedPipeW,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

pub mod handoff;

const PIPE_PREFIX: &str = r"\\.\pipe\spice-door-";
/// An endpoint file holds one pipe name, far shorter than this.
const ENDPOINT_BYTES: u64 = 128;
/// How long a sender waits for a busy door to offer a free instance.
const CONNECT_BOUND: Duration = Duration::from_secs(5);
const ACKNOWLEDGED: u8 = 1;

/// The bound end of one Windows door that hands each admitted message to a
/// sink before acknowledging its sender.
pub struct Listener {
    name: Vec<u16>,
    endpoint: String,
    path: PathBuf,
    stopping: Arc<AtomicBool>,
}

impl Listener {
    /// Bind the door at `path`, admitting messages of at most `capacity`
    /// bytes and handing each to `sink` before its sender is acknowledged.
    ///
    /// # Errors
    ///
    /// Returns the error that prevents binding, or `AlreadyExists` when `path`
    /// names something other than a door endpoint.
    pub fn bind(
        path: &Path,
        capacity: usize,
        sink: impl FnMut(Vec<u8>) + Send + 'static,
    ) -> io::Result<Self> {
        match read_endpoint(path) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} is not a door endpoint", path.display()),
                ));
            }
            Err(error) => return Err(error),
        }
        let capacity = u32::try_from(capacity).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "door capacity exceeds a pipe message",
            )
        })?;
        let descriptor = owner_only()?;
        let endpoint = format!("{PIPE_PREFIX}{}", unique());
        let name = wide(&endpoint);
        let first = instance(&name, true, capacity, &descriptor)?;
        publish(path, &endpoint)?;
        let stopping = Arc::new(AtomicBool::new(false));
        let server = Server {
            name: name.clone(),
            capacity,
            descriptor,
            stopping: Arc::clone(&stopping),
        };
        thread::Builder::new()
            .name("spice-door".to_owned())
            .spawn(move || server.serve(first, sink))?;
        Ok(Self {
            name,
            endpoint,
            path: path.to_owned(),
            stopping,
        })
    }

    /// Stop taking messages and withdraw the endpoint while it is still this
    /// listener's. Closing again changes nothing; dropping closes.
    pub fn close(&self) {
        if self.stopping.swap(true, Ordering::AcqRel) {
            return;
        }
        // Connecting wakes the server so it observes the stop and closes the
        // pipe; a server already gone refuses the connection.
        drop(connect(&self.name, CONNECT_BOUND));
        if read_endpoint(&self.path).is_ok_and(|endpoint| endpoint == self.endpoint) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.close();
    }
}

pub(crate) struct Door {
    listener: Listener,
    messages: Mutex<mpsc::Receiver<Vec<u8>>>,
}

impl Door {
    pub(crate) fn bind(path: &Path, capacity: usize) -> io::Result<Self> {
        let (sender, messages) = mpsc::channel();
        let listener = Listener::bind(path, capacity, move |message| {
            let _ = sender.send(message);
        })?;
        Ok(Self {
            listener,
            messages: Mutex::new(messages),
        })
    }

    pub(crate) fn recv(&self, timeout: Option<Duration>) -> io::Result<Vec<u8>> {
        let messages = self.messages.lock().unwrap_or_else(PoisonError::into_inner);
        match timeout {
            None => messages.recv().map_err(|_| ended()),
            Some(timeout) => messages.recv_timeout(timeout).map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => super::timed_out(),
                mpsc::RecvTimeoutError::Disconnected => ended(),
            }),
        }
    }

    pub(crate) fn close(&self, _path: &Path) {
        self.listener.close();
    }
}

/// Each message rides its own pipe connection, so nothing persists between
/// sends.
pub(crate) struct Sender;

impl Sender {
    pub(crate) fn new() -> io::Result<Self> {
        Ok(Self)
    }

    pub(crate) fn send(&self, path: &Path, message: &[u8]) -> io::Result<()> {
        send(path, message)
    }
}

fn send(path: &Path, message: &[u8]) -> io::Result<()> {
    let endpoint = read_endpoint(path)?;
    let pipe = connect(&wide(&endpoint), CONNECT_BOUND)?;
    let mode = PIPE_READMODE_MESSAGE;
    // SAFETY: `pipe` is an open pipe client handle and `mode` outlives the call.
    if unsafe { SetNamedPipeHandleState(pipe.0, &mode, null(), null()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let length = u32::try_from(message.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "message exceeds a pipe message",
        )
    })?;
    let mut written = 0_u32;
    // SAFETY: `message` holds `length` readable bytes for the call.
    if unsafe { WriteFile(pipe.0, message.as_ptr(), length, &mut written, null_mut()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut acknowledgement = 0_u8;
    let mut read = 0_u32;
    // SAFETY: `acknowledgement` is one writable byte for the call.
    let answered = unsafe { ReadFile(pipe.0, &mut acknowledgement, 1, &mut read, null_mut()) } != 0;
    if answered && read == 1 && acknowledgement == ACKNOWLEDGED {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            "the door refused the message",
        ))
    }
}

struct Server {
    name: Vec<u16>,
    capacity: u32,
    descriptor: Descriptor,
    stopping: Arc<AtomicBool>,
}

impl Server {
    fn serve(self, first: Handle, mut sink: impl FnMut(Vec<u8>)) {
        let mut listening = first;
        loop {
            // SAFETY: `listening` is an open server instance.
            let connected = unsafe { ConnectNamedPipe(listening.0, null_mut()) } != 0
                || io::Error::last_os_error().raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32);
            if self.stopping.load(Ordering::Acquire) {
                return;
            }
            let Ok(next) = instance(&self.name, false, self.capacity, &self.descriptor) else {
                return;
            };
            if connected {
                self.exchange(&listening, &mut sink);
            }
            // SAFETY: `listening` is an open server instance.
            unsafe { DisconnectNamedPipe(listening.0) };
            listening = next;
        }
    }

    fn exchange(&self, pipe: &Handle, sink: &mut impl FnMut(Vec<u8>)) {
        let mut message = vec![0_u8; self.capacity as usize];
        let mut read = 0_u32;
        // A message past capacity fails with `ERROR_MORE_DATA` and goes
        // unacknowledged, which the sender reports as a refusal.
        // SAFETY: `message` holds `capacity` writable bytes for the call.
        if unsafe {
            ReadFile(
                pipe.0,
                message.as_mut_ptr(),
                self.capacity,
                &mut read,
                null_mut(),
            )
        } == 0
        {
            return;
        }
        message.truncate(read as usize);
        sink(message);
        let mut written = 0_u32;
        // SAFETY: the acknowledgement is one readable byte for the call.
        if unsafe { WriteFile(pipe.0, &ACKNOWLEDGED, 1, &mut written, null_mut()) } == 0 {
            return;
        }
        // Disconnecting discards unread bytes, so hold the instance until the
        // sender has taken the acknowledgement and closed its end.
        let mut end = 0_u8;
        // SAFETY: `end` is one writable byte for the call.
        unsafe { ReadFile(pipe.0, &mut end, 1, &mut read, null_mut()) };
    }
}

struct Handle(HANDLE);

// SAFETY: a kernel handle may be used and closed from any thread.
unsafe impl Send for Handle {}

impl Handle {
    /// Give up ownership without closing.
    fn into_raw(self) -> HANDLE {
        std::mem::ManuallyDrop::new(self).0
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle is open and owned by this value.
        unsafe { CloseHandle(self.0) };
    }
}

/// A security descriptor allocated by the system.
struct Descriptor(PSECURITY_DESCRIPTOR);

// SAFETY: the descriptor is immutable memory owned by this value.
unsafe impl Send for Descriptor {}

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: the system allocated the descriptor with `LocalAlloc`.
        unsafe { LocalFree(self.0) };
    }
}

/// A protected DACL granting the current user alone full access.
fn owner_only() -> io::Result<Descriptor> {
    let sddl = wide(&format!("D:P(A;;GA;;;{})", current_user_sid()?));
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: `sddl` is NUL-terminated and `descriptor` receives the allocation.
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        )
    };
    if converted == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Descriptor(descriptor))
}

/// The string SID of the principal this process runs as.
///
/// # Errors
///
/// Returns the operating-system refusal opening or reading the process token.
pub fn current_user_sid() -> io::Result<String> {
    let mut token: HANDLE = null_mut();
    // SAFETY: the current process pseudo-handle needs no closing; `token`
    // receives an owned handle.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Handle(token);
    let mut length = 0_u32;
    // SAFETY: a null buffer asks only for the required length.
    unsafe { GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut length) };
    if length == 0 {
        return Err(io::Error::last_os_error());
    }
    // `u64` storage aligns the `TOKEN_USER` header.
    let mut buffer = vec![0_u64; (length as usize).div_ceil(8)];
    // SAFETY: `buffer` holds at least `length` writable bytes.
    let queried = unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            length,
            &mut length,
        )
    };
    if queried == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call filled the aligned buffer with a `TOKEN_USER`.
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    sid_text(user.User.Sid)
}

fn sid_text(sid: PSID) -> io::Result<String> {
    let mut text = null_mut();
    // SAFETY: `sid` is a valid SID and `text` receives a `LocalAlloc` string.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the system returned a NUL-terminated wide string.
    let length = (0..)
        .take_while(|&index| unsafe { *text.add(index) } != 0)
        .count();
    // SAFETY: `length` units precede the terminator.
    let sid = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) });
    // SAFETY: the system allocated `text` with `LocalAlloc`.
    unsafe { LocalFree(text.cast()) };
    Ok(sid)
}

fn instance(
    name: &[u16],
    first: bool,
    capacity: u32,
    descriptor: &Descriptor,
) -> io::Result<Handle> {
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let open = PIPE_ACCESS_DUPLEX
        | if first {
            FILE_FLAG_FIRST_PIPE_INSTANCE
        } else {
            0
        };
    // SAFETY: `name` is NUL-terminated and `attributes` outlives the call.
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            open,
            PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            1,
            capacity,
            0,
            &attributes,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(Handle(handle))
}

fn connect(name: &[u16], bound: Duration) -> io::Result<Handle> {
    let deadline = Instant::now() + bound;
    loop {
        // Identification-level quality of service keeps the door from
        // impersonating its sender.
        // SAFETY: `name` is NUL-terminated.
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                null_mut(),
            )
        };
        if handle != INVALID_HANDLE_VALUE {
            return Ok(Handle(handle));
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_PIPE_BUSY as i32) {
            return Err(error);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the door stayed busy",
            ));
        }
        let wait = u32::try_from(remaining.as_millis())
            .unwrap_or(u32::MAX)
            .max(1);
        // SAFETY: `name` is NUL-terminated.
        unsafe { WaitNamedPipeW(name.as_ptr(), wait) };
    }
}

/// Replace whatever endpoint sits at `path` with one naming `endpoint`.
fn publish(path: &Path, endpoint: &str) -> io::Result<()> {
    let mut staged = path.as_os_str().to_owned();
    staged.push(format!(".{}", unique()));
    let staged = PathBuf::from(staged);
    fs::write(&staged, endpoint)?;
    fs::rename(&staged, path).inspect_err(|_| {
        let _ = fs::remove_file(&staged);
    })
}

fn read_endpoint(path: &Path) -> io::Result<String> {
    let mut endpoint = String::new();
    fs::File::open(path)?
        .take(ENDPOINT_BYTES)
        .read_to_string(&mut endpoint)?;
    let named = endpoint.len() < ENDPOINT_BYTES as usize
        && endpoint.starts_with(PIPE_PREFIX)
        && !endpoint.contains('\0');
    if named {
        Ok(endpoint)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} does not name a door pipe", path.display()),
        ))
    }
}

/// A name no other door of this host holds: the first-instance flag refuses
/// any collision outright.
fn unique() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    format!(
        "{}-{nanos:x}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn ended() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "the door stopped listening")
}

#[cfg(test)]
pub(crate) fn assert_owner_only(_door: &Door, path: &Path) {
    use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT};
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
        DACL_SECURITY_INFORMATION, GetAce, GetAclInformation,
    };
    use windows_sys::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;

    let endpoint = read_endpoint(path).expect("the endpoint names a pipe");
    let pipe = connect(&wide(&endpoint), CONNECT_BOUND).expect("the owner connects");
    let mut dacl: *mut ACL = null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: `pipe` is open with read-control access; outputs are writable.
    let status = unsafe {
        GetSecurityInfo(
            pipe.0,
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut descriptor,
        )
    };
    assert_eq!(status, 0, "the pipe's DACL reads");
    let _descriptor = Descriptor(descriptor);
    let mut size = ACL_SIZE_INFORMATION::default();
    // SAFETY: `dacl` lives in `descriptor`; `size` is writable for its length.
    let sized = unsafe {
        GetAclInformation(
            dacl,
            (&raw mut size).cast(),
            size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    };
    assert_ne!(sized, 0, "the DACL describes itself");
    assert_eq!(size.AceCount, 1, "exactly one principal holds access");
    let mut ace = null_mut();
    // SAFETY: index 0 exists; `ace` receives a pointer into the DACL.
    assert_ne!(unsafe { GetAce(dacl, 0, &mut ace) }, 0, "the entry reads");
    // SAFETY: the entry type is checked before its SID is read.
    let ace = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
    assert_eq!(u32::from(ace.Header.AceType), ACCESS_ALLOWED_ACE_TYPE);
    let holder = sid_text((&raw const ace.SidStart).cast_mut().cast()).expect("SID renders");
    assert_eq!(holder, current_user_sid().expect("current user resolves"));
}

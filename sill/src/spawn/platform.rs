use std::os::raw::{c_int, c_short};

pub const SIGKILL: c_int = 9;
pub const SIGTERM: c_int = 15;
pub const EPERM: c_int = 1;
pub const ESRCH: c_int = 3;
pub const P_PID: c_int = 1;
pub const WEXITED: c_int = 0x4;
const POLLIN: c_short = 0x1;

#[cfg(target_os = "macos")]
pub const WNOWAIT: c_int = 0x20;
#[cfg(target_os = "linux")]
pub const WNOWAIT: c_int = 0x01_00_00_00;

#[cfg(target_os = "linux")]
pub type Nfds = std::os::raw::c_ulong;
#[cfg(target_os = "macos")]
pub type Nfds = u32;

#[repr(C)]
pub struct PollFd {
    descriptor: c_int,
    events: c_short,
    returned_events: c_short,
}

#[repr(C, align(8))]
pub struct SignalInfo([u8; 128]);

impl PollFd {
    pub const fn read(descriptor: c_int) -> Self {
        Self {
            descriptor,
            events: POLLIN,
            returned_events: 0,
        }
    }

    pub const fn ready(&self) -> bool {
        self.returned_events != 0
    }
}

unsafe extern "C" {
    pub fn kill(pid: c_int, signal: c_int) -> c_int;
    pub fn poll(descriptors: *mut PollFd, count: Nfds, timeout: c_int) -> c_int;
    pub fn waitid(kind: c_int, id: u32, information: *mut SignalInfo, options: c_int) -> c_int;
}

//! Locale-independent process topology with native start-generation identity.

#[cfg(target_os = "macos")]
use std::io;

use super::{Process, ProcessGeneration};

#[cfg(target_os = "linux")]
pub(super) use linux::process_snapshot;

#[cfg(target_os = "macos")]
pub(super) fn process_snapshot() -> io::Result<Vec<Process>> {
    use std::mem::{MaybeUninit, size_of, size_of_val};

    // A null destination asks libproc how many process identifiers to reserve.
    // Reading the same native table here avoids launching the setuid `ps`
    // executable, which macOS deliberately refuses inside `sandbox-exec`.
    // SAFETY: a null destination requests a size without writing memory.
    let required = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    let required = usize::try_from(required).map_err(|_| {
        io::Error::other(format!(
            "libproc could not size the process table: {}",
            io::Error::last_os_error()
        ))
    })?;
    let capacity = required.checked_mul(2).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "process table size overflowed")
    })?;
    let mut identifiers = vec![0 as libc::pid_t; capacity];
    let bytes = libc::c_int::try_from(size_of_val(identifiers.as_slice())).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "process table exceeds libproc width",
        )
    })?;
    // SAFETY: the PID destination is writable for its complete byte capacity.
    let found = unsafe { libc::proc_listallpids(identifiers.as_mut_ptr().cast(), bytes) };
    let found = usize::try_from(found).map_err(|_| {
        io::Error::other(format!(
            "libproc could not list the process table: {}",
            io::Error::last_os_error()
        ))
    })?;
    if found >= capacity {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "process table changed while libproc filled its snapshot",
        ));
    }

    let mut processes = Vec::new();
    for pid in identifiers.into_iter().take(found).filter(|pid| *pid > 0) {
        let mut information = MaybeUninit::<libc::proc_bsdinfo>::uninit();
        let information_size =
            libc::c_int::try_from(size_of::<libc::proc_bsdinfo>()).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "process record exceeds libproc width",
                )
            })?;
        // SAFETY: the process-information destination is writable for its full size.
        let read = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                information.as_mut_ptr().cast(),
                information_size,
            )
        };
        let error = io::Error::last_os_error();
        if read == 0 {
            match error.raw_os_error() {
                Some(libc::ESRCH) => continue,
                Some(libc::EPERM) => continue,
                _ => {
                    return Err(io::Error::other(format!(
                        "libproc could not read process {pid}: {error}"
                    )));
                }
            }
        }
        if read != information_size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("libproc returned {read} of {information_size} bytes for process {pid}"),
            ));
        }
        // SAFETY: the exact-size result initialized the complete record.
        let information = unsafe { information.assume_init() };
        if information.pbi_start_tvusec >= 1_000_000 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("libproc returned an invalid generation for process {pid}"),
            ));
        }
        let process = Process {
            pid: information.pbi_pid,
            parent: information.pbi_ppid,
            group: information.pbi_pgid,
            terminal: information.pbi_status == libc::SZOMB,
            generation: Some(ProcessGeneration {
                seconds: information.pbi_start_tvsec,
                subsecond: information.pbi_start_tvusec,
            }),
        };
        if process.pid != u32::try_from(pid).expect("positive pid_t fits u32") {
            continue;
        }
        if process.group == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("libproc returned a zero process group for process {pid}"),
            ));
        }
        processes.push(process);
    }
    Ok(processes)
}

#[cfg(any(target_os = "linux", test))]
mod linux {
    use std::io;

    #[cfg(target_os = "linux")]
    use std::{fs, path::Path};

    use super::{Process, ProcessGeneration};

    #[cfg(target_os = "linux")]
    pub(in crate::process_tree) fn process_snapshot() -> io::Result<Vec<Process>> {
        // SAFETY: sysconf reads one immutable kernel clock-frequency value.
        let ticks_per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        let ticks_per_second = u64::try_from(ticks_per_second).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "kernel process clock has no positive tick frequency",
            )
        })?;
        let mut processes = Vec::new();
        for entry in fs::read_dir(Path::new("/proc"))? {
            let entry = entry?;
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            let path = entry.path().join("stat");
            let stat = match fs::read_to_string(&path) {
                Ok(stat) => stat,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(io::Error::new(
                        error.kind(),
                        format!("could not read {}: {error}", path.display()),
                    ));
                }
            };
            let process = parse_stat(&stat, ticks_per_second).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!(
                        "{} is not a valid native process record: {error}",
                        path.display()
                    ),
                )
            })?;
            if process.pid != pid {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} reports process {}", path.display(), process.pid),
                ));
            }
            processes.push(process);
        }
        Ok(processes)
    }

    fn parse_stat(stat: &str, ticks_per_second: u64) -> io::Result<Process> {
        if ticks_per_second == 0 {
            return Err(invalid_stat("zero clock tick frequency"));
        }
        let (pid, remainder) = stat
            .split_once(' ')
            .ok_or_else(|| invalid_stat("PID boundary"))?;
        let pid = positive_u32(pid, "PID")?;
        let (_, fields) = remainder
            .rsplit_once(") ")
            .ok_or_else(|| invalid_stat("command boundary"))?;
        let fields = fields.split_whitespace().collect::<Vec<_>>();
        let state = fields
            .first()
            .and_then(|state| state.as_bytes().first())
            .copied()
            .ok_or_else(|| invalid_stat("process state"))?;
        let parent = fields
            .get(1)
            .and_then(|parent| parent.parse::<u32>().ok())
            .ok_or_else(|| invalid_stat("parent PID"))?;
        let group = positive_u32(
            fields
                .get(2)
                .copied()
                .ok_or_else(|| invalid_stat("process group"))?,
            "process group",
        )?;
        let start_ticks = fields
            .get(19)
            .and_then(|ticks| ticks.parse::<u64>().ok())
            .ok_or_else(|| invalid_stat("start ticks"))?;
        Ok(Process {
            pid,
            parent,
            group,
            terminal: matches!(state, b'Z' | b'X' | b'x'),
            generation: Some(ProcessGeneration {
                seconds: start_ticks / ticks_per_second,
                subsecond: start_ticks % ticks_per_second,
            }),
        })
    }

    fn positive_u32(value: &str, name: &str) -> io::Result<u32> {
        value
            .parse::<u32>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| invalid_stat(name))
    }

    fn invalid_stat(expected: &str) -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("process stat has no valid {expected}"),
        )
    }

    #[test]
    fn stat_parser_preserves_subsecond_native_generation() {
        let mut fields = vec!["S", "10", "12"];
        fields.extend(std::iter::repeat_n("0", 16));
        fields.push("12345");
        let stat = format!("12 (fixture name (nested)) {}", fields.join(" "));

        let process = parse_stat(&stat, 100).expect("native stat parses");

        assert_eq!(process.pid, 12);
        assert_eq!(process.parent, 10);
        assert_eq!(process.group, 12);
        assert!(!process.terminal);
        assert_eq!(
            process.generation,
            Some(ProcessGeneration {
                seconds: 123,
                subsecond: 45,
            })
        );
    }

    #[test]
    fn stat_parser_refuses_a_missing_generation() {
        let error =
            parse_stat("12 (fixture) S 10 12", 100).expect_err("truncated native stat refuses");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn native_snapshot_carries_the_current_process_generation() {
        let pid = std::process::id();
        let process = process_snapshot()
            .expect("native process snapshot succeeds")
            .into_iter()
            .find(|process| process.pid == pid)
            .expect("native snapshot contains the test process");

        let generation = process
            .generation
            .expect("current process generation is readable");
        assert!(generation.seconds > 0);
        assert!(generation.subsecond < 1_000_000);
    }
}

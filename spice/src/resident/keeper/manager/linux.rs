//! systemd user-manager adapter for one repository keeper.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use super::{Manager, Status};

/// Native systemd manager reached through the current user's bus.
pub(crate) struct SystemdUser<R = Systemctl> {
    units: PathBuf,
    runner: R,
}

/// One captured `systemctl --user` answer.
pub(crate) struct Answer {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

pub(crate) trait Runner {
    fn invoke(&mut self, arguments: &[&str]) -> io::Result<Answer>;
}

/// Production command runner. No command is ever interpreted by a shell.
pub(crate) struct Systemctl;

impl Runner for Systemctl {
    fn invoke(&mut self, arguments: &[&str]) -> io::Result<Answer> {
        let output = Command::new("systemctl")
            .arg("--user")
            .args(arguments)
            .stdin(Stdio::null())
            .output()?;
        Ok(Answer {
            success: output.status.success(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

#[cfg(target_os = "linux")]
impl SystemdUser<Systemctl> {
    /// Resolve the user unit search path and prove the user manager and bus are
    /// reachable without mutating either one.
    pub(crate) fn connect() -> io::Result<Self> {
        Self::connect_at(user_unit_directory()?, Systemctl)
    }
}

impl<R: Runner> SystemdUser<R> {
    fn connect_at(units: PathBuf, runner: R) -> io::Result<Self> {
        let mut manager = Self { units, runner };
        manager.checked(&["show-environment"])?;
        Ok(manager)
    }

    fn checked(&mut self, arguments: &[&str]) -> io::Result<Answer> {
        let answer = self.runner.invoke(arguments)?;
        if answer.success {
            Ok(answer)
        } else {
            Err(command_error(arguments, &answer.stderr))
        }
    }

    fn unit_path(&self, registration: &str) -> io::Result<PathBuf> {
        if !registration.ends_with(".service")
            || registration.is_empty()
            || !registration
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid systemd user service name {registration:?}"),
            ));
        }
        Ok(self.units.join(registration))
    }

    fn reload(&mut self) -> io::Result<()> {
        self.checked(&["daemon-reload"]).map(|_| ())
    }
}

impl<R: Runner> Manager for SystemdUser<R> {
    fn register(&mut self, registration: &str, definition: &str) -> io::Result<()> {
        let path = self.unit_path(registration)?;
        sill::authority::create_private_directories(&self.units)?;
        super::super::record::replace_private(&path, definition.as_bytes())?;
        self.reload()
    }

    fn status(&mut self, registration: &str) -> io::Result<Option<Status>> {
        self.unit_path(registration)?;
        let arguments = [
            "show",
            registration,
            "--property=LoadState",
            "--property=UnitFileState",
            "--property=ActiveState",
        ];
        let answer = self.runner.invoke(&arguments)?;
        let status = parse_status(&answer.stdout);
        if answer.success || matches!(status, Ok(None)) {
            status
        } else {
            Err(command_error(&arguments, &answer.stderr))
        }
    }

    fn set_enabled(&mut self, registration: &str, enabled: bool) -> io::Result<()> {
        self.unit_path(registration)?;
        let action = if enabled { "enable" } else { "disable" };
        self.checked(&[action, registration]).map(|_| ())
    }

    fn run(&mut self, registration: &str) -> io::Result<()> {
        self.unit_path(registration)?;
        self.checked(&["start", registration]).map(|_| ())
    }

    fn stop(&mut self, registration: &str) -> io::Result<()> {
        if self.status(registration)?.is_none() {
            return Ok(());
        }
        self.checked(&["stop", registration]).map(|_| ())
    }

    fn delete(&mut self, registration: &str) -> io::Result<()> {
        let path = self.unit_path(registration)?;
        if self.status(registration)?.is_some() {
            self.checked(&["disable", registration])?;
        }
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        self.reload()
    }
}

#[cfg(target_os = "linux")]
fn user_unit_directory() -> io::Result<PathBuf> {
    if let Some(config) = std::env::var_os("XDG_CONFIG_HOME") {
        let config = PathBuf::from(config);
        if config.is_absolute() {
            return Ok(config.join("systemd/user"));
        }
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "HOME does not name an absolute user configuration root",
            )
        })?;
    Ok(home.join(".config/systemd/user"))
}

fn parse_status(bytes: &[u8]) -> io::Result<Option<Status>> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let properties = text
        .lines()
        .filter_map(|line| line.split_once('='))
        .collect::<BTreeMap<_, _>>();
    // Every state but not-found names an installed unit: a unit systemd
    // reports as bad-setting, error, or masked must still stop, disable, and
    // be removed rather than strand `down --keeper`.
    match properties.get("LoadState").copied() {
        Some("not-found") => return Ok(None),
        Some(_) => {}
        None => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "systemd omitted LoadState",
            ));
        }
    }
    let enabled = matches!(
        properties.get("UnitFileState").copied(),
        Some("enabled" | "enabled-runtime")
    );
    let running = matches!(
        properties.get("ActiveState").copied(),
        Some("active" | "activating" | "reloading")
    );
    Ok(Some(Status { enabled, running }))
}

fn command_error(arguments: &[&str], stderr: &[u8]) -> io::Error {
    let detail = String::from_utf8_lossy(stderr);
    let detail = detail.trim();
    io::Error::other(if detail.is_empty() {
        format!("systemctl --user {} refused", arguments.join(" "))
    } else {
        format!("systemctl --user {} refused: {detail}", arguments.join(" "))
    })
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[derive(Default)]
    struct Script {
        answers: VecDeque<io::Result<Answer>>,
        calls: Vec<Vec<String>>,
    }

    impl Runner for Script {
        fn invoke(&mut self, arguments: &[&str]) -> io::Result<Answer> {
            self.calls
                .push(arguments.iter().map(|value| (*value).to_owned()).collect());
            self.answers
                .pop_front()
                .expect("the script covers every manager call")
        }
    }

    fn answer(stdout: &str) -> io::Result<Answer> {
        Ok(Answer {
            success: true,
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        })
    }

    fn refusal(stdout: &str, stderr: &str) -> io::Result<Answer> {
        Ok(Answer {
            success: false,
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        })
    }

    fn scratch(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock follows epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "spice-systemd-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn unavailable_manager_refuses_before_creating_the_unit_directory() {
        let units = scratch("unavailable");
        let script = Script {
            answers: VecDeque::from([Ok(Answer {
                success: false,
                stdout: Vec::new(),
                stderr: b"Failed to connect to bus".to_vec(),
            })]),
            calls: Vec::new(),
        };
        let error = SystemdUser::connect_at(units.clone(), script)
            .err()
            .expect("an absent user bus refuses");
        assert!(error.to_string().contains("Failed to connect to bus"));
        assert!(!units.exists());
    }

    #[test]
    fn registration_is_atomic_then_reloaded_enabled_and_started() {
        let units = scratch("apply");
        let script = Script {
            answers: VecDeque::from([answer(""), answer(""), answer(""), answer("")]),
            calls: Vec::new(),
        };
        let mut manager = SystemdUser::connect_at(units.clone(), script).expect("manager connects");
        super::super::super::owner::apply(
            &mut manager,
            "spice-keeper-0123456789abcdef.service",
            "[Service]\nExecStart=/usr/bin/spice\n",
        )
        .expect("owner applies");
        assert_eq!(
            fs::read_to_string(units.join("spice-keeper-0123456789abcdef.service"))
                .expect("unit reads"),
            "[Service]\nExecStart=/usr/bin/spice\n",
        );
        assert_eq!(
            manager.runner.calls,
            [
                vec!["show-environment"],
                vec!["daemon-reload"],
                vec!["enable", "spice-keeper-0123456789abcdef.service"],
                vec!["start", "spice-keeper-0123456789abcdef.service"],
            ]
        );
        fs::remove_dir_all(&units).expect("scratch removes");
    }

    #[test]
    fn status_and_removal_follow_native_manager_state() {
        let units = scratch("remove");
        fs::create_dir_all(&units).expect("unit directory creates");
        let name = "spice-keeper-0123456789abcdef.service";
        fs::write(units.join(name), "unit").expect("unit writes");
        let loaded = "LoadState=loaded\nUnitFileState=enabled\nActiveState=active\n";
        let script = Script {
            answers: VecDeque::from([
                answer(""),
                answer(loaded),
                answer(loaded),
                answer(""),
                answer(""),
            ]),
            calls: Vec::new(),
        };
        let mut manager = SystemdUser::connect_at(units.clone(), script).expect("manager connects");
        assert_eq!(
            manager.status(name).expect("status reads"),
            Some(Status {
                enabled: true,
                running: true,
            })
        );
        manager.delete(name).expect("registration deletes");
        assert!(!units.join(name).exists());
        assert_eq!(
            manager.runner.calls,
            [
                vec!["show-environment"],
                vec![
                    "show",
                    name,
                    "--property=LoadState",
                    "--property=UnitFileState",
                    "--property=ActiveState",
                ],
                vec![
                    "show",
                    name,
                    "--property=LoadState",
                    "--property=UnitFileState",
                    "--property=ActiveState",
                ],
                vec!["disable", name],
                vec!["daemon-reload"],
            ]
        );
        fs::remove_dir_all(&units).expect("scratch removes");
    }

    #[test]
    fn absent_native_registration_is_idempotent_at_stop_and_delete() {
        let units = scratch("absent");
        let absent = "LoadState=not-found\nUnitFileState=\nActiveState=inactive\n";
        let script = Script {
            answers: VecDeque::from([
                answer(""),
                refusal(absent, "Unit not found"),
                refusal(absent, "Unit not found"),
                answer(""),
            ]),
            calls: Vec::new(),
        };
        let mut manager = SystemdUser::connect_at(units.clone(), script).expect("manager connects");
        let name = "spice-keeper-0123456789abcdef.service";
        manager.stop(name).expect("absent stop is complete");
        manager.delete(name).expect("absent delete is complete");
        assert!(!units.exists());
        assert_eq!(
            manager.runner.calls.last(),
            Some(&vec!["daemon-reload".to_owned()])
        );
    }

    #[test]
    fn removal_retires_units_systemd_reports_as_broken() {
        let name = "spice-keeper-0123456789abcdef.service";
        for load_state in ["bad-setting", "error", "masked"] {
            let units = scratch(load_state);
            fs::create_dir_all(&units).expect("unit directory creates");
            fs::write(units.join(name), "unit").expect("unit writes");
            let broken =
                format!("LoadState={load_state}\nUnitFileState=enabled\nActiveState=inactive\n");
            let script = Script {
                answers: VecDeque::from([
                    answer(""),
                    answer(&broken),
                    answer(""),
                    answer(&broken),
                    answer(""),
                    answer(&broken),
                    answer(""),
                    answer(""),
                ]),
                calls: Vec::new(),
            };
            let mut manager =
                SystemdUser::connect_at(units.clone(), script).expect("manager connects");
            super::super::super::owner::hold(&mut manager, name).expect("a broken unit is held");
            super::super::super::owner::release(&mut manager, name)
                .expect("a broken unit is released");
            assert!(!units.join(name).exists(), "{load_state} unit is removed");
            fs::remove_dir_all(&units).expect("scratch removes");
        }
    }
}

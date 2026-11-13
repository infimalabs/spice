//! The LaunchAgent adapter's mapping of the shared owner transitions onto
//! launchd, against a `launchctl` that answers as launchd does.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::super::super::owner;
use super::super::{Manager, Status};
use super::{Answer, LaunchAgents, Launchctl, disabled, running};

const LABEL: &str = "dev.spice.keeper-0123456789abcdef";
const SERVICE: &str = "gui/501/dev.spice.keeper-0123456789abcdef";

/// launchd's state for one service in user 501's GUI domain.
#[derive(Default)]
struct Launchd {
    session: bool,
    loaded: Option<String>,
    running: bool,
    disabled: bool,
    calls: Vec<String>,
}

impl Launchd {
    fn answer(code: i32, stdout: &str) -> Answer {
        Answer {
            code,
            stdout: stdout.to_owned(),
            stderr: String::new(),
        }
    }

    /// Logout ends the session's jobs; the next login loads every enabled
    /// agent whose property list remains.
    fn log_out_and_in(&mut self, agents: &Path) {
        self.loaded = None;
        self.running = false;
        let agent = agents.join(format!("{LABEL}.plist"));
        if !self.disabled
            && let Ok(definition) = fs::read_to_string(agent)
        {
            self.loaded = Some(definition);
            self.running = true;
        }
    }
}

impl Launchctl for Launchd {
    fn call(&mut self, arguments: &[&str]) -> io::Result<Answer> {
        self.calls.push(arguments[0].to_owned());
        Ok(match arguments {
            ["print", "gui/501"] => Self::answer(if self.session { 0 } else { 112 }, ""),
            ["print", SERVICE] => match (&self.loaded, self.running) {
                (None, _) => Self::answer(113, ""),
                (Some(_), true) => Self::answer(0, "\tstate = running\n\tpid = 4242\n"),
                (Some(_), false) => Self::answer(0, "\tstate = not running\n"),
            },
            ["print-disabled", "gui/501"] => {
                let state = if self.disabled { "disabled" } else { "enabled" };
                Self::answer(
                    0,
                    &format!("\tdisabled services = {{\n\t\t\"{LABEL}\" => {state}\n\t}}\n"),
                )
            }
            ["enable", SERVICE] => {
                self.disabled = false;
                Self::answer(0, "")
            }
            ["disable", SERVICE] => {
                self.disabled = true;
                Self::answer(0, "")
            }
            ["bootstrap", "gui/501", path] => {
                if self.loaded.is_some() || self.disabled {
                    Self::answer(5, "")
                } else {
                    self.loaded = Some(fs::read_to_string(path)?);
                    self.running = true;
                    Self::answer(0, "")
                }
            }
            ["bootout", SERVICE] => {
                if self.loaded.take().is_none() {
                    Self::answer(3, "")
                } else {
                    self.running = false;
                    Self::answer(0, "")
                }
            }
            ["kickstart", SERVICE] => {
                if self.loaded.is_none() {
                    Self::answer(113, "")
                } else {
                    self.running = true;
                    Self::answer(0, "")
                }
            }
            other => panic!("unexpected launchctl call {other:?}"),
        })
    }
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "spice-keeper-launchd-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        Self(root)
    }

    fn agents(&self) -> PathBuf {
        self.0.join("Library").join("LaunchAgents")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn agents(scratch: &Scratch, launchd: Launchd) -> LaunchAgents<Launchd> {
    LaunchAgents::new(launchd, 501, scratch.agents())
}

fn session() -> Launchd {
    Launchd {
        session: true,
        ..Launchd::default()
    }
}

#[test]
fn applying_writes_one_agent_and_reapplying_it_unchanged_keeps_the_running_keeper() {
    let scratch = Scratch::new("apply");
    let mut agents = agents(&scratch, session());
    owner::apply(&mut agents, LABEL, "first").expect("first apply");
    let agent = scratch.agents().join(format!("{LABEL}.plist"));
    assert_eq!(fs::read_to_string(&agent).expect("agent written"), "first");
    assert_eq!(
        fs::read_dir(scratch.agents()).expect("agents").count(),
        1,
        "exactly one property list, no scratch left behind"
    );
    assert_eq!(agents.launchctl.loaded.as_deref(), Some("first"));
    assert!(agents.launchctl.running);

    agents.launchctl.calls.clear();
    owner::apply(&mut agents, LABEL, "first").expect("converged apply");
    assert!(
        !agents
            .launchctl
            .calls
            .iter()
            .any(|call| call == "bootout" || call == "bootstrap"),
        "an unchanged definition never restarts the keeper: {:?}",
        agents.launchctl.calls
    );

    owner::apply(&mut agents, LABEL, "second").expect("changed apply");
    assert_eq!(
        agents.launchctl.loaded.as_deref(),
        Some("second"),
        "a changed definition is reloaded"
    );
    assert!(agents.launchctl.running);
}

#[test]
fn down_holds_the_agent_across_login_and_up_restores_it() {
    let scratch = Scratch::new("hold");
    let mut agents = agents(&scratch, session());
    owner::apply(&mut agents, LABEL, "definition").expect("apply");

    owner::hold(&mut agents, LABEL).expect("hold");
    assert_eq!(
        agents.status(LABEL).expect("status"),
        Some(Status {
            enabled: false,
            running: true,
        }),
        "holding forbids relaunch and leaves the draining keeper to observe it"
    );

    let directory = scratch.agents();
    agents.launchctl.log_out_and_in(&directory);
    assert_eq!(
        agents.status(LABEL).expect("status"),
        Some(Status {
            enabled: false,
            running: false,
        }),
        "a held agent stays stopped at the next login"
    );

    owner::apply(&mut agents, LABEL, "definition").expect("apply after login");
    assert_eq!(
        agents.status(LABEL).expect("status"),
        Some(Status {
            enabled: true,
            running: true,
        })
    );
}

#[test]
fn login_restores_an_enabled_agent_whose_property_list_remains() {
    let scratch = Scratch::new("login");
    let mut agents = agents(&scratch, session());
    owner::apply(&mut agents, LABEL, "definition").expect("apply");
    let directory = scratch.agents();
    agents.launchctl.log_out_and_in(&directory);
    assert_eq!(
        agents.status(LABEL).expect("status"),
        Some(Status {
            enabled: true,
            running: true,
        })
    );
}

#[test]
fn removal_unloads_and_deletes_and_tolerates_a_job_already_gone() {
    let scratch = Scratch::new("release");
    let mut agents = agents(&scratch, session());
    owner::release(&mut agents, LABEL).expect("releasing nothing is fine");

    owner::apply(&mut agents, LABEL, "definition").expect("apply");
    owner::hold(&mut agents, LABEL).expect("hold");
    owner::release(&mut agents, LABEL).expect("release");
    assert_eq!(agents.launchctl.loaded, None);
    assert_eq!(agents.status(LABEL).expect("status"), None);
    owner::release(&mut agents, LABEL).expect("releasing again is fine");
}

#[test]
fn an_unloaded_agent_starts_by_loading_it() {
    let scratch = Scratch::new("run");
    let mut agents = agents(&scratch, session());
    owner::apply(&mut agents, LABEL, "definition").expect("apply");
    agents.stop(LABEL).expect("stop");
    agents.run(LABEL).expect("run");
    assert!(agents.launchctl.running);
}

#[test]
fn only_a_login_session_offers_the_gui_domain() {
    let scratch = Scratch::new("domain");
    assert!(
        agents(&scratch, session())
            .domain_available()
            .expect("domain")
    );
    assert!(
        !agents(&scratch, Launchd::default())
            .domain_available()
            .expect("domain")
    );
}

#[test]
fn the_override_list_disables_only_the_exact_label() {
    let overrides = concat!(
        "\tdisabled services = {\n",
        "\t\t\"dev.spice.keeper-0123456789abcdef0\" => disabled\n",
        "\t\t\"dev.spice.keeper-0123456789abcdef\" => enabled\n",
        "\t\t\"dev.spice.keeper-fedcba9876543210\" => true\n",
        "\t}\n",
    );
    assert!(!disabled(overrides, LABEL));
    assert!(disabled(overrides, "dev.spice.keeper-0123456789abcdef0"));
    assert!(
        disabled(overrides, "dev.spice.keeper-fedcba9876543210"),
        "earlier releases print true for a disabled service"
    );
    assert!(!disabled(overrides, "dev.spice.keeper-absent"));
}

#[test]
fn only_a_top_level_process_marks_the_job_running() {
    assert!(running("\tstate = running\n\tpid = 4242\n"));
    assert!(!running("\tstate = not running\n\tlast exit code = 0\n"));
    assert!(
        !running("\tstate = not running\n\tendpoints = {\n\t\tpid = 7\n\t}\n"),
        "a nested property is not the job's process"
    );
}

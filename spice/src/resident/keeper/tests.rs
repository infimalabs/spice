//! Host-independent keeper contracts: native registration ordering, private
//! entry points, record policy, relaunch behavior, and ownership.

use std::collections::VecDeque;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(target_os = "linux")]
use super::linux as platform;
#[cfg(target_os = "macos")]
use super::macos as platform;
use super::macos_definition;
use super::manager::{Manager, Status};
use super::owner;
use super::ownership::{self, Mismatch, Owners};
use super::policy::{self, Attempt, End, Exit, MAX_PAUSE, RELAUNCH_PAUSE, STABLE_RUN};
use super::record::{self, KeeperRecord};
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
use super::unsupported as platform;
#[cfg(windows)]
use super::windows as platform;
use super::windows_definition::{self, Definition};

#[test]
fn private_worker_entry_requires_its_exact_argument_vector() {
    assert!(super::worker_selected(
        ["spice", super::WORKER_ARGUMENT]
            .into_iter()
            .map(Into::into)
    ));
    for arguments in [
        vec!["spice"],
        vec!["spice", "--keeper"],
        vec!["spice", super::WORKER_ARGUMENT, "extra"],
    ] {
        assert!(
            !super::worker_selected(arguments.iter().copied().map(Into::into)),
            "{arguments:?} must remain on the public parser path"
        );
    }
    assert!(super::package_default_selected(
        ["spice", super::PACKAGE_DEFAULT_ARGUMENT]
            .into_iter()
            .map(Into::into)
    ));
    assert!(!super::package_default_selected(
        ["spice", super::PACKAGE_DEFAULT_ARGUMENT, "extra"]
            .into_iter()
            .map(Into::into)
    ));
}

/// A scheduler that records what it was asked, in order.
#[derive(Default)]
struct RecordingManager {
    registered: bool,
    calls: Vec<String>,
}

impl Manager for RecordingManager {
    fn register(&mut self, _path: &str, definition: &str) -> io::Result<()> {
        self.registered = true;
        self.calls.push(format!("register:{}", definition.len()));
        Ok(())
    }

    fn status(&mut self, _path: &str) -> io::Result<Option<Status>> {
        self.calls.push("status".to_owned());
        Ok(self.registered.then_some(Status {
            enabled: true,
            running: false,
        }))
    }

    fn set_enabled(&mut self, _path: &str, enabled: bool) -> io::Result<()> {
        self.calls.push(format!("enabled:{enabled}"));
        Ok(())
    }

    fn run(&mut self, _path: &str) -> io::Result<()> {
        self.calls.push("run".to_owned());
        Ok(())
    }

    fn stop(&mut self, _path: &str) -> io::Result<()> {
        self.calls.push("stop".to_owned());
        Ok(())
    }

    fn delete(&mut self, _path: &str) -> io::Result<()> {
        self.registered = false;
        self.calls.push("delete".to_owned());
        Ok(())
    }
}

#[test]
fn apply_registers_before_it_allows_and_starts_and_hold_skips_an_absent_task() {
    let mut scheduler = RecordingManager::default();
    owner::hold(&mut scheduler, "\\Spice\\keeper").expect("holding nothing is fine");
    assert_eq!(
        scheduler.calls,
        ["status"],
        "an absent task is never disabled"
    );
    owner::apply(&mut scheduler, "\\Spice\\keeper", "<Task/>").expect("apply");
    owner::hold(&mut scheduler, "\\Spice\\keeper").expect("hold");
    owner::release(&mut scheduler, "\\Spice\\keeper").expect("release");
    assert_eq!(
        scheduler.calls,
        [
            "status",
            "register:7",
            "enabled:true",
            "run",
            "status",
            "enabled:false",
            "stop",
            "delete"
        ]
    );
}

#[test]
fn removing_artifacts_tolerates_entries_already_gone() {
    let root = std::env::temp_dir().join(format!("spice-keeper-artifacts-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a scratch directory");
    let present = root.join("keeper.log");
    std::fs::write(&present, b"log").expect("a log to remove");
    owner::remove_artifacts(&[&present, &root.join("keeper.json")]).expect("removal");
    assert!(!present.exists());
    std::fs::remove_dir_all(&root).expect("scratch cleanup");
}

/// A scheduler whose answers are scripted per `status` call.
struct ScriptedManager {
    statuses: VecDeque<Option<Status>>,
}

impl Manager for ScriptedManager {
    fn register(&mut self, _path: &str, _definition: &str) -> io::Result<()> {
        Ok(())
    }

    fn status(&mut self, _path: &str) -> io::Result<Option<Status>> {
        Ok(self
            .statuses
            .pop_front()
            .expect("the script covers every status call"))
    }

    fn set_enabled(&mut self, _path: &str, _enabled: bool) -> io::Result<()> {
        Ok(())
    }

    fn run(&mut self, _path: &str) -> io::Result<()> {
        Ok(())
    }

    fn stop(&mut self, _path: &str) -> io::Result<()> {
        Ok(())
    }

    fn delete(&mut self, _path: &str) -> io::Result<()> {
        Ok(())
    }
}

const ENABLED: Option<Status> = Some(Status {
    enabled: true,
    running: true,
});
const DISABLED: Option<Status> = Some(Status {
    enabled: false,
    running: true,
});

fn served(ran_for: Duration) -> Attempt {
    Attempt::Ready {
        endpoint: "http://127.0.0.1:4242".to_owned(),
        wait: Box::new(move || Exit {
            detail: "exit status: 0".to_owned(),
            ran_for,
        }),
    }
}

fn record() -> KeeperRecord {
    KeeperRecord {
        registration: "\\Spice\\keeper-0123456789abcdef".to_owned(),
        executable: PathBuf::from(r"C:\Spice\spice.exe"),
        binary: wire::Digest::compute(wire::Domain::DistArtifact, b"fixture spice"),
        working_directory: PathBuf::from(r"C:\Users\c\devel\repo"),
        listen: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
        allow_insecure_bind: true,
        auth_token: Some("0123456789abcdef".to_owned()),
        managed_default: false,
    }
}

#[test]
fn the_keeper_relaunches_after_every_exit_until_the_task_is_disabled() {
    let mut scheduler = ScriptedManager {
        statuses: VecDeque::from([ENABLED, ENABLED, ENABLED, DISABLED]),
    };
    let mut attempts = VecDeque::from([
        served(STABLE_RUN * 2),
        Attempt::Failed("port in use".to_owned()),
        served(Duration::from_secs(1)),
    ]);
    let mut launches = 0;
    let mut attempt = || {
        launches += 1;
        attempts.pop_front().expect("no attempt after the disable")
    };
    let mut pauses = Vec::new();
    let mut pause = |delay| pauses.push(delay);
    let mut log = Vec::new();

    let end = policy::keep(
        &mut scheduler,
        "\\Spice\\keeper",
        &mut attempt,
        &mut pause,
        &mut log,
    )
    .expect("the scripted scheduler answers");

    assert_eq!(end, End::Disabled);
    assert_eq!(launches, 3, "clean exit, refusal, and crash each relaunch");
    assert_eq!(
        pauses,
        vec![
            policy::pause_after(0),
            policy::pause_after(1),
            policy::pause_after(2)
        ],
        "a stable run resets the pace; early failures lengthen it"
    );
    let log = String::from_utf8(log).expect("the log is text");
    assert!(log.contains("port in use"), "refusals are durable: {log}");
    assert!(log.contains("is disabled; keeper exits"), "{log}");
}

#[test]
fn a_deleted_task_ends_the_keeper_without_another_launch() {
    let mut scheduler = ScriptedManager {
        statuses: VecDeque::from([None]),
    };
    let mut attempt = || panic!("an unregistered owner launches nothing");
    let mut pause = |_| panic!("an unregistered owner waits for nothing");
    let end = policy::keep(
        &mut scheduler,
        "\\Spice\\keeper",
        &mut attempt,
        &mut pause,
        &mut io::sink(),
    )
    .expect("the scripted scheduler answers");
    assert_eq!(end, End::Unregistered);
}

#[test]
fn relaunch_pacing_doubles_from_the_base_and_caps() {
    assert_eq!(policy::pause_after(0), RELAUNCH_PAUSE);
    assert_eq!(policy::pause_after(1), RELAUNCH_PAUSE * 2);
    assert_eq!(policy::pause_after(3), RELAUNCH_PAUSE * 8);
    assert_eq!(policy::pause_after(10), MAX_PAUSE);
    assert_eq!(policy::pause_after(u32::MAX), MAX_PAUSE);
}

#[test]
fn the_record_relaunches_with_its_listen_flags_and_keeps_the_token_off_argv() {
    let record = record();
    let argv = record.launch_argv();
    assert_eq!(
        argv,
        [
            "spice",
            "--json",
            "up",
            "--host",
            "0.0.0.0",
            "--port",
            "0",
            "--allow-insecure-bind"
        ]
    );
    let matches = crate::cli::command()
        .try_get_matches_from(&argv)
        .expect("the synthesized argv parses");
    let leaf = crate::cli::selected_matches(&matches);
    assert!(leaf.get_flag("allow-insecure-bind"));
    assert_eq!(leaf.get_one::<String>("auth-token"), None);
    assert!(!leaf.get_flag("foreground"), "the keeper launches detached");
}

#[test]
fn an_ephemeral_install_pins_the_port_the_first_resident_bound() {
    let mut record = record();
    assert!(record.pin_port("http://127.0.0.1:51234"));
    assert_eq!(record.listen.port(), 51234);
    assert!(
        !record.pin_port("http://127.0.0.1:60000"),
        "a pinned port is never moved"
    );
    assert_eq!(record.listen.port(), 51234);
    assert_eq!(record::endpoint_port("http://[::1]:8080/"), Some(8080));
    assert_eq!(record::endpoint_port("http://localhost"), None);
}

#[test]
fn the_record_round_trips_privately_and_reads_absent_as_uninstalled() {
    let root = std::env::temp_dir().join(format!("spice-keeper-record-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a scratch directory");
    let path = root.join("keeper.json");
    assert_eq!(record::read(&path).expect("absent reads"), None);
    let written = record();
    record::write(&path, &written).expect("the record writes");
    record::write(&path, &written).expect("rewriting replaces in place");
    assert_eq!(
        record::read(&path).expect("the record reads"),
        Some(written)
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "the token never leaves the owner");
    }
    std::fs::remove_dir_all(&root).expect("scratch cleanup");
}

#[test]
fn the_task_runs_this_executable_as_the_user_at_logon_without_a_password() {
    let xml = windows_definition::render(&Definition {
        user_sid: "S-1-5-21-1-2-3-1001",
        executable: Path::new(r"C:\Program Files\Spice & Co\spice.exe"),
        working_directory: Path::new(r"C:\Users\c\devel\repo"),
        repository: Path::new(r"C:\Users\c\devel\repo\.git"),
    });
    assert!(
        xml.contains("<LogonTrigger><Enabled>true</Enabled><UserId>S-1-5-21-1-2-3-1001</UserId>")
    );
    assert!(xml.contains("<LogonType>InteractiveToken</LogonType>"));
    assert!(xml.contains("<RunLevel>LeastPrivilege</RunLevel>"));
    assert!(xml.contains("<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>"));
    assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
    assert!(xml.contains("<StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>"));
    assert!(xml.contains(r"<Command>C:\Program Files\Spice &amp; Co\spice.exe</Command>"));
    assert!(xml.contains("<Arguments>--spice-keeper-worker</Arguments>"));
    assert!(xml.contains(r"<WorkingDirectory>C:\Users\c\devel\repo</WorkingDirectory>"));
    assert!(
        !xml.contains("Password"),
        "no credential is ever registered"
    );
}

#[test]
fn task_paths_are_stable_per_repository_and_distinct_between_them() {
    let one = windows_definition::task_path(Path::new(r"C:\Users\c\devel\one\.git"));
    let other = windows_definition::task_path(Path::new(r"C:\Users\c\devel\other\.git"));
    assert_eq!(
        one,
        windows_definition::task_path(Path::new(r"C:\Users\c\devel\one\.git"))
    );
    assert_ne!(one, other);
    assert!(one.starts_with(&format!("{}\\keeper-", windows_definition::FOLDER)));
}

#[test]
fn the_agent_runs_only_this_executable_as_the_keeper_in_the_login_session() {
    let plist = macos_definition::render(&macos_definition::Definition {
        label: "dev.spice.keeper-0123456789abcdef",
        executable: Path::new("/Applications/Spice & Co/spice"),
        working_directory: Path::new("/srv/repo"),
    });
    assert!(
        plist.contains("<key>Label</key>\n\t<string>dev.spice.keeper-0123456789abcdef</string>")
    );
    assert!(plist.contains(concat!(
        "<key>ProgramArguments</key>\n\t<array>\n",
        "\t\t<string>/Applications/Spice &amp; Co/spice</string>\n",
        "\t\t<string>--spice-keeper-worker</string>\n\t</array>",
    )));
    assert!(plist.contains("<key>WorkingDirectory</key>\n\t<string>/srv/repo</string>"));
    assert!(plist.contains("<key>RunAtLoad</key>\n\t<true/>"));
    assert!(
        plist.contains(
            "<key>KeepAlive</key>\n\t<dict>\n\t\t<key>SuccessfulExit</key>\n\t\t<false/>"
        ),
        "launchd relaunches only a keeper that exits unsuccessfully"
    );
    assert!(plist.contains("<key>LimitLoadToSessionType</key>\n\t<string>Aqua</string>"));
    for absent in ["UserName", "EnvironmentVariables", "Password", "token"] {
        assert!(!plist.contains(absent), "{absent} never enters the agent");
    }
}

#[test]
fn agent_labels_are_stable_per_repository_and_distinct_between_them() {
    let one = macos_definition::label(Path::new("/srv/one/.git"));
    let other = macos_definition::label(Path::new("/srv/other/.git"));
    assert_eq!(one, macos_definition::label(Path::new("/srv/one/.git")));
    assert_ne!(one, other);
    assert!(one.starts_with(macos_definition::LABEL_PREFIX));
}

#[test]
fn an_unchanged_reinstall_keeps_the_pinned_port_and_a_changed_one_reconfigures() {
    let mut installed = record();
    installed.listen.set_port(47777);
    let mut same = record();
    same.inherit_port(&installed);
    assert_eq!(
        same.listen.port(),
        47777,
        "no port asked keeps the pinned one"
    );
    assert!(!installed.reconfigured_by(&same));

    let mut moved = record();
    moved.listen.set_port(47778);
    moved.inherit_port(&installed);
    assert_eq!(
        moved.listen.port(),
        47778,
        "an asked port is never overridden"
    );
    assert!(installed.reconfigured_by(&moved));

    let mut rekeyed = record();
    rekeyed.auth_token = Some("fedcba9876543210".to_owned());
    rekeyed.inherit_port(&installed);
    assert!(installed.reconfigured_by(&rekeyed));

    let mut rebuilt = installed.clone();
    rebuilt.binary = wire::Digest::compute(wire::Domain::DistArtifact, b"next fixture spice");
    assert!(
        installed.reconfigured_by(&rebuilt),
        "replacing the appliance at the same path retires the old keeper"
    );

    let mut upgraded = installed.clone();
    upgraded.executable = PathBuf::from(r"C:\Spice\next\spice.exe");
    assert!(
        installed.reconfigured_by(&upgraded),
        "a running keeper can launch only its own executable"
    );

    let mut re_registered = installed.clone();
    re_registered.registration = "native-owner-b".to_owned();
    assert!(
        installed.reconfigured_by(&re_registered),
        "changing native registration retires the old owner"
    );

    let mut moved_worktree = installed.clone();
    moved_worktree.working_directory = PathBuf::from(r"C:\Users\c\devel\repo-b");
    assert!(
        installed.reconfigured_by(&moved_worktree),
        "the native owner and keeper must agree on their repository working directory"
    );
}

#[test]
fn install_standing_hides_policy_and_ignores_appliance_replacement() {
    let initial = record();
    let standing = initial.standing().expect("policy standing derives");
    let mut rebuilt = initial.clone();
    rebuilt.binary = wire::Digest::compute(wire::Domain::DistArtifact, b"replacement appliance");
    rebuilt.executable = PathBuf::from(r"C:\Spice\next\spice.exe");
    assert_eq!(
        standing,
        rebuilt.standing().expect("rebuilt policy standing derives"),
        "package replacement is not a private policy change"
    );

    rebuilt.auth_token = Some("another secret".to_owned());
    assert_ne!(
        standing,
        rebuilt.standing().expect("changed policy standing derives"),
        "bearer changes move the opaque standing without publishing the bearer"
    );
}

#[test]
fn package_default_standing_survives_system_port_pinning() {
    let mut managed = record();
    managed.managed_default = true;
    managed.listen = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    assert_eq!(
        managed.standing().expect("default standing derives"),
        spice::install::LifecycleOwnerStanding::Default
    );
    managed.listen.set_port(47_778);
    assert_eq!(
        managed.standing().expect("pinned standing derives"),
        spice::install::LifecycleOwnerStanding::Default
    );
}

#[test]
fn package_default_standing_is_platform_policy() {
    #[cfg(windows)]
    assert_eq!(
        platform::package_default(),
        Some(spice::install::LifecycleOwnerStanding::Default),
    );
    #[cfg(not(windows))]
    assert_eq!(platform::package_default(), None);
}

const USER: &str = "S-1-5-21-1-2-3-1001";
const OTHER_USER: &str = "S-1-5-21-1-2-3-1002";
const ADMINISTRATORS: &str = "S-1-5-32-544";

#[test]
fn lifecycle_owners_admit_only_state_the_process_user_owns() {
    let owners = |default_owner, runtime| Owners {
        user: USER,
        default_owner,
        repository: Some(USER),
        runtime,
    };
    // An unelevated administrator or ordinary user creates objects it owns,
    // whether or not the runtime root exists yet.
    for runtime in [None, Some(USER)] {
        assert_eq!(ownership::mismatch(&owners(USER, runtime)), None);
    }
    // An elevated token defaulting to Administrators is refused before it can
    // create anything, even though it could take the repository over.
    for runtime in [None, Some(USER), Some(ADMINISTRATORS)] {
        assert_eq!(
            ownership::mismatch(&owners(ADMINISTRATORS, runtime)),
            Some(Mismatch::DefaultOwner(ADMINISTRATORS)),
            "{runtime:?}"
        );
    }
    assert_eq!(
        ownership::mismatch(&Owners {
            user: USER,
            default_owner: USER,
            repository: Some(OTHER_USER),
            runtime: None,
        }),
        Some(Mismatch::Repository(OTHER_USER)),
    );
    // A matching process never reuses a runtime root another principal owns.
    for foreign in [ADMINISTRATORS, OTHER_USER] {
        assert_eq!(
            ownership::mismatch(&owners(USER, Some(foreign))),
            Some(Mismatch::Runtime(foreign))
        );
    }
}

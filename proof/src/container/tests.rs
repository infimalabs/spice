use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use sill::{Deadlines, Spec, StdinPolicy, StreamPolicy};

use super::{Attestation, Container, ContainerError, Limits, OciRuntime, RuntimeKind};
use crate::container::Execution;
use crate::launch::{Launch, LaunchError};
use crate::test_support::{arguments, exited};
const FIXTURE_LOCK: &str = "version = 4\n";
const IMAGE: &str = "sha256:4444444444444444444444444444444444444444444444444444444444444444";

/// What the runtime's image store answers for the vendor-set tag.
#[derive(Clone, Copy, Debug)]
enum Store {
    /// Holds the requested vendor set for the named architecture.
    Holds(&'static str),
    /// Resolves the vendor-set tag to a malformed image ID.
    Malformed,
    /// Has never seen the sealed appliance.
    Empty,
}

/// How `exec` answers the attestation exchange inside the scratch owner.
#[derive(Clone, Copy, Debug)]
enum Exec {
    /// Runs the command without the attestation script, so no marker appears.
    Unattested,
    /// Writes one public marker, whatever challenge it was handed.
    FixedMarker,
    /// Runs the attestation script it was handed and exits with this code.
    Attests(i32),
}

/// An OCI runtime as the sealed container contract observes it: one bounded
/// process per question, answered from the arguments it was given.
#[derive(Debug)]
struct Runtime {
    version: &'static str,
    store: Store,
    starts: bool,
    exec: Exec,
    vendor_set: String,
    /// Named volumes the runtime holds, with the label each was created with.
    volumes: Mutex<Vec<(String, String)>>,
    /// Whether `volume create` refuses.
    refuses_volumes: bool,
    events: Mutex<Vec<String>>,
}

impl Runtime {
    fn docker() -> Self {
        Self {
            version: "Docker version 1.0.0, build fixture\n",
            store: Store::Holds("amd64"),
            starts: true,
            exec: Exec::Unattested,
            vendor_set: crate::VendorSet::parse(std::path::Path::new("Cargo.lock"), FIXTURE_LOCK)
                .expect("fixture vendor set")
                .to_string(),
            volumes: Mutex::default(),
            refuses_volumes: false,
            events: Mutex::default(),
        }
    }

    fn events(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn record(&self, event: String) {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(event);
    }

    fn hold_volume(&self, name: &str, purpose: &str) {
        self.volumes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((
                name.to_owned(),
                format!("org.spice.release-target={purpose}"),
            ));
    }

    fn volume(&self, arguments: &[String]) -> Execution {
        let argument = |index: usize| arguments.get(index).map_or("", String::as_str);
        let mut volumes = self.volumes.lock().unwrap_or_else(PoisonError::into_inner);
        match argument(1) {
            "ls" => {
                let label = argument(4).strip_prefix("label=").unwrap_or_default();
                let names: String = volumes
                    .iter()
                    .filter(|(_, held)| held == label)
                    .map(|(name, _)| format!("{name}\n"))
                    .collect();
                exited(0, names, "")
            }
            "rm" => {
                volumes.retain(|(name, _)| name != argument(2));
                drop(volumes);
                self.record(format!("volume rm {}", argument(2)));
                exited(0, format!("{}\n", argument(2)), "")
            }
            "create" if self.refuses_volumes => exited(1, "", "Error: volume store refused\n"),
            "create" => {
                volumes.push((argument(4).to_owned(), argument(3).to_owned()));
                drop(volumes);
                self.record(format!("volume create {}", argument(4)));
                exited(0, format!("{}\n", argument(4)), "")
            }
            _ => exited(1, "", ""),
        }
    }
}

const OWNER: &str = "0000000000000000000000000000000000000000000000000000000000000000";

impl Launch for Runtime {
    fn run(&self, spec: Spec) -> Result<Execution, LaunchError> {
        let arguments = arguments(&spec);
        let argument = |index: usize| arguments.get(index).map_or("", String::as_str);
        Ok(match argument(0) {
            "--version" => exited(0, self.version, ""),
            "image" => {
                let expected_tag =
                    crate::VendorSet::parse(std::path::Path::new("Cargo.lock"), FIXTURE_LOCK)
                        .expect("fixture vendor set")
                        .image_tag();
                if argument(4) != expected_tag {
                    exited(1, "", "Error: unexpected image tag\n")
                } else {
                    match self.store {
                        Store::Holds(architecture) => exited(
                            0,
                            format!("{IMAGE} {architecture} linux {}\n", self.vendor_set),
                            "",
                        ),
                        Store::Malformed => exited(
                            0,
                            format!("not-an-image amd64 linux {}\n", self.vendor_set),
                            "",
                        ),
                        Store::Empty => exited(1, "", "Error: no such image\n"),
                    }
                }
            }
            "volume" => self.volume(&arguments),
            // A sealed release run: record the Cargo target it mounts.
            "run" if argument(1) == "--rm" => {
                let target = arguments
                    .iter()
                    .find(|argument| argument.ends_with("destination=/cargo-target"))
                    .map_or("", String::as_str);
                self.record(format!("sealed {target}"));
                exited(0, "", "")
            }
            "run" if self.starts => {
                self.record(format!("run {} {OWNER}", argument(3)));
                exited(0, format!("{OWNER}\n"), "")
            }
            "run" => {
                self.record(format!("run {}", argument(3)));
                exited(125, "", "")
            }
            "exec" => {
                self.record(format!("exec {}", argument(1)));
                match self.exec {
                    Exec::Unattested => exited(0, "", ""),
                    Exec::FixedMarker => {
                        exited(0, "", "spice-proof-attestation-v1:docker:fixed-challenge\n")
                    }
                    Exec::Attests(code) => exited(
                        code,
                        "",
                        format!(
                            "spice-proof-attestation-v1:{}:{}\n",
                            argument(6),
                            argument(7)
                        ),
                    ),
                }
            }
            "rm" => {
                self.record(format!("rm {}", argument(2)));
                exited(0, "", "")
            }
            _ => exited(1, "", ""),
        })
    }
}

#[test]
fn one_battery_scope_disables_network_and_mounts_fresh_empty_caches() {
    let fixture = Fixture::new();
    let container = fixture.container();
    let spec = container
        .battery_spec("spice-proof-fixture")
        .expect("sealed battery scope");
    let arguments = spec
        .argv
        .arguments()
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    assert_eq!(
        arguments,
        [
            "run".to_owned(),
            "--detach".to_owned(),
            "--name".to_owned(),
            "spice-proof-fixture".to_owned(),
            // Named here rather than inferred, so a host whose own
            // architecture differs cannot have a runtime warning written
            // across the attestation stream.
            "--platform".to_owned(),
            "linux/amd64".to_owned(),
            "--network=none".to_owned(),
            "--read-only".to_owned(),
            "--cap-drop=ALL".to_owned(),
            "--security-opt=no-new-privileges".to_owned(),
            "--mount".to_owned(),
            format!(
                "type=bind,source={},destination=/workspace,readonly",
                fixture
                    .workspace
                    .canonicalize()
                    .expect("canonical workspace")
                    .display()
            ),
            "--tmpfs".to_owned(),
            "/cargo-home:rw,noexec,mode=0700".to_owned(),
            "--tmpfs".to_owned(),
            "/cargo-target:rw,exec,mode=0700,size=4g".to_owned(),
            "--tmpfs".to_owned(),
            "/tmp:rw,nosuid,nodev,noexec,mode=1777".to_owned(),
            "--workdir".to_owned(),
            "/workspace".to_owned(),
            "--env".to_owned(),
            "CARGO_HOME=/cargo-home".to_owned(),
            "--env".to_owned(),
            "CARGO_TARGET_DIR=/cargo-target".to_owned(),
            "--env".to_owned(),
            "CARGO_NET_OFFLINE=true".to_owned(),
            "--env".to_owned(),
            "CARGO_PROFILE_TEST_DEBUG=0".to_owned(),
            "--env".to_owned(),
            "CARGO_INCREMENTAL=0".to_owned(),
            "--env".to_owned(),
            "RUST_TEST_THREADS=1".to_owned(),
            IMAGE.to_owned(),
            "/bin/sh".to_owned(),
            "-ceu".to_owned(),
            super::BATTERY_KEEPALIVE_SCRIPT.to_owned(),
        ]
    );
    assert!(spec.environment.entries().is_empty());
    assert_eq!(spec.output.stdin(), StdinPolicy::Null);
    assert!(matches!(
        spec.output.stdout(),
        StreamPolicy::Capture { limit: 4096 }
    ));
}

#[test]
fn every_member_enters_the_named_battery_scope() {
    let fixture = Fixture::new();
    let container = fixture.container();
    let attestation = Attestation::fixture("fixture-challenge");
    let spec = container
        .exec_spec(
            "spice-proof-fixture",
            super::MEMBER_SCRIPT,
            "cargo",
            &["test".to_owned(), "--locked".to_owned()],
            &attestation,
            "fixture member",
        )
        .expect("sealed member command");
    let arguments = spec
        .argv
        .arguments()
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    assert_eq!(
        arguments,
        [
            "exec",
            "spice-proof-fixture",
            "/bin/sh",
            "-ceu",
            super::MEMBER_SCRIPT,
            "spice-proof-attestation",
            "docker",
            "fixture-challenge",
            "cargo",
            "test",
            "--locked",
        ]
    );
}

#[test]
fn one_scratch_owner_is_reused_and_removed_for_the_whole_battery() {
    let fixture = Fixture::with(Runtime {
        exec: Exec::Attests(0),
        ..Runtime::docker()
    });
    let container = fixture.container();
    let battery = container.battery().expect("battery scratch starts empty");

    battery
        .execute(
            "cargo",
            &["test".to_owned(), "-p".to_owned(), "gate".to_owned()],
        )
        .expect("first member enters the scratch owner");
    battery
        .execute(
            "cargo",
            &["test".to_owned(), "-p".to_owned(), "dist".to_owned()],
        )
        .expect("later member reuses the scratch owner");
    battery.close().expect("battery scratch is removed");

    let events = fixture.runtime.events();
    assert_eq!(
        events.len(),
        5,
        "one start, one preflight, two members, one removal"
    );
    let name = started_name(&events[0]);
    assert!(name.starts_with("spice-proof-"));
    assert_eq!(
        events,
        [
            format!("run {name} {OWNER}"),
            format!("exec {OWNER}"),
            format!("exec {OWNER}"),
            format!("exec {OWNER}"),
            format!("rm {OWNER}"),
        ],
        "preflight and every member must enter the one owner that teardown removes"
    );
}

#[test]
fn a_refused_preflight_removes_the_scratch_owner_before_returning() {
    let fixture = Fixture::with(Runtime {
        exec: Exec::Attests(70),
        ..Runtime::docker()
    });
    let container = fixture.container();

    container
        .battery()
        .expect_err("a refused preflight cannot expose its scratch");
    let events = fixture.runtime.events();
    let name = started_name(&events[0]);
    assert_eq!(
        events,
        [
            format!("run {name} {OWNER}"),
            format!("exec {OWNER}"),
            format!("rm {OWNER}"),
        ],
        "the owner guard must remove a scratch whose preflight refuses"
    );
}

#[test]
fn a_refused_start_never_removes_a_container_it_did_not_create() {
    let fixture = Fixture::with(Runtime {
        starts: false,
        ..Runtime::docker()
    });
    let container = fixture.container();

    container
        .battery()
        .expect_err("a refused start creates no owned scratch");
    let events = fixture.runtime.events();
    assert_eq!(events.len(), 1, "a failed start has nothing safe to remove");
    assert!(events[0].starts_with("run spice-proof-"));
}

/// The scratch owner name a recorded start used.
fn started_name(event: &str) -> &str {
    let mut start = event.split_whitespace();
    assert_eq!(start.next(), Some("run"));
    start.next().expect("the start names the scratch owner")
}

#[test]
fn the_vendor_set_tag_resolves_to_an_exact_image_id() {
    let fixture = Fixture::new();
    let container = fixture.container();

    assert_eq!(container.image(), IMAGE);
}

#[test]
fn a_runtime_that_cannot_produce_the_vendor_set_image_refuses_before_any_work() {
    for (store, expected) in [
        (Store::Malformed, "exact SHA-256 image ID"),
        (Store::Holds("arm64"), "arm64"),
        (Store::Empty, "does not hold proof image"),
    ] {
        let fixture = Fixture::with(Runtime {
            store,
            ..Runtime::docker()
        });
        let error = Container::new(fixture.oci(), &fixture.workspace, limits())
            .expect_err("a runtime that cannot produce the vendor-set image is not sealed");

        assert!(
            error.to_string().contains(expected),
            "the refusal did not name {expected}: {error}",
        );
    }
}

/// A prune is the usual reason the image is gone, and it takes the image
/// from every lane at once. The refusal therefore has to carry the whole
/// recovery, not just the loader: a lane told only to run the load action
/// still has to guess which archive, and the guess it makes under time
/// pressure is the build action, which is the emulated build this refusal
/// exists to avoid.
#[test]
fn a_pruned_image_names_the_loader_and_the_archive_it_takes() {
    let fixture = Fixture::with(Runtime {
        store: Store::Empty,
        ..Runtime::docker()
    });
    let error = Container::new(fixture.oci(), &fixture.workspace, limits())
        .expect_err("an empty image store is not sealed");

    let refusal = error.to_string();
    for named in [
        "spice dev xtask -- proof-image load",
        "target/proof-image/spice-proof-image.tar",
        "spice dev xtask -- proof-image build",
        "prune",
    ] {
        assert!(
            refusal.contains(named),
            "the refusal did not name {named}: {refusal}",
        );
    }
}
#[test]
fn a_stale_vendor_set_refuses_before_any_container_work() {
    let fixture = Fixture::with(Runtime {
        vendor_set: format!("sha256:{}", "9".repeat(64)),
        ..Runtime::docker()
    });
    let error = Container::new(fixture.oci(), &fixture.workspace, limits())
        .expect_err("a stale package set is not a sealed environment");

    let refusal = error.to_string();
    assert!(matches!(error, ContainerError::ImageVendorSetDrift { .. }));
    assert!(refusal.contains("stale vendored package set"), "{refusal}");
    assert!(
        refusal.contains("spice dev xtask -- proof-image build"),
        "{refusal}"
    );
    assert!(
        fixture.runtime.events().is_empty(),
        "image admission must refuse before a container starts"
    );
}

#[test]
fn an_executable_that_ignores_the_oci_contract_cannot_produce_a_replay_verdict() {
    let fixture = Fixture::new();
    let environment = crate::environment::SealedEnvironment::fixture_oci(fixture.container());
    let error = crate::replay::replay(&environment)
        .expect_err("an unattested runtime cannot produce any verdict");

    assert_eq!(error.check(), crate::replay::CheckKind::Gate);
    assert!(
        error
            .evidence()
            .contains("did not prove its OCI isolation contract"),
        "the refusal did not name the missing attestation: {error:?}",
    );
}

#[test]
fn a_replayed_public_marker_cannot_produce_a_replay_verdict() {
    let fixture = Fixture::with(Runtime {
        exec: Exec::FixedMarker,
        ..Runtime::docker()
    });
    let environment = crate::environment::SealedEnvironment::fixture_oci(fixture.container());
    let error = crate::replay::replay(&environment)
        .expect_err("a fixed marker cannot answer a fresh challenge");

    assert_eq!(error.check(), crate::replay::CheckKind::Gate);
    assert!(
        error
            .evidence()
            .contains("did not prove its OCI isolation contract"),
        "the refusal did not name the stale attestation: {error:?}",
    );
}

#[test]
fn runtime_dialect_is_observed_from_the_executable() {
    let docker = Fixture::new();
    let podman = Fixture::with(Runtime {
        version: "podman version 1.0.0\n",
        ..Runtime::docker()
    });

    assert_eq!(docker.oci().kind, RuntimeKind::Docker);
    assert_eq!(podman.oci().kind, RuntimeKind::Podman);
}

#[test]
fn attestation_is_required_once_and_removed_from_command_evidence() {
    let fixture = Fixture::new();
    let container = fixture.container();
    let attestation = Attestation::fixture("fresh-challenge");
    let mut stderr = super::Captured {
        bytes: b"spice-proof-attestation-v1:docker:fresh-challenge\ncompiler evidence".to_vec(),
        truncated: false,
        complete: true,
    };

    container
        .verify_attestation(&mut stderr, &attestation)
        .expect("the declared dialect attests");

    assert_eq!(stderr.bytes, b"compiler evidence");
}

#[test]
fn attestation_must_name_the_declared_runtime_dialect() {
    let fixture = Fixture::new();
    let container = fixture.container();
    let attestation = Attestation::fixture("fresh-challenge");
    let mut stderr = super::Captured {
        bytes: b"spice-proof-attestation-v1:podman:fresh-challenge\n".to_vec(),
        truncated: false,
        complete: true,
    };

    let error = container
        .verify_attestation(&mut stderr, &attestation)
        .expect_err("a Podman attestation cannot answer for Docker");

    assert!(matches!(error, ContainerError::RuntimeNotAttested { .. }));
}

#[test]
fn attestation_must_answer_the_current_execution_challenge() {
    let fixture = Fixture::new();
    let container = fixture.container();
    let attestation = Attestation::fixture("fresh-challenge");
    let mut stderr = super::Captured {
        bytes: b"spice-proof-attestation-v1:docker:stale-challenge\n".to_vec(),
        truncated: false,
        complete: true,
    };

    let error = container
        .verify_attestation(&mut stderr, &attestation)
        .expect_err("a marker from another execution cannot be replayed");

    assert!(matches!(error, ContainerError::RuntimeNotAttested { .. }));
}

const CROSS_IMAGE: &str = "sha256:5555555555555555555555555555555555555555555555555555555555555555";

fn sealed_volume(name: &str) -> String {
    format!("sealed type=volume,source={name},destination=/cargo-target")
}

fn linux_volume() -> String {
    format!(
        "spice-release-target-linux-stage-{}",
        IMAGE.trim_start_matches("sha256:")
    )
}

#[test]
fn sealed_runs_reuse_one_persistent_target_per_purpose_and_image() {
    let fixture = Fixture::new();
    fixture
        .runtime
        .hold_volume("spice-release-target-linux-stage-superseded", "linux-stage");
    fixture.runtime.hold_volume(
        "spice-release-target-windows-cross-superseded",
        "windows-cross",
    );
    let container = fixture.container();
    let git_common = fixture.workspace.join(".git");
    fs::create_dir_all(&git_common).expect("fixture Git common directory");
    let out = fixture.root.join("windows-cross-out");

    for _ in 0..2 {
        let stage = container
            .stage_release(&git_common, None, None, None)
            .expect("sealed stage");
        assert!(stage.success);
    }
    container
        .cross_build_windows(super::WindowsCross {
            cross_image: CROSS_IMAGE,
            msvc_generation: "fixture",
            git_common: &git_common,
            out: &out,
            delivered: None,
            canvas: None,
        })
        .expect("sealed cross build");

    let cross_volume = format!(
        "spice-release-target-windows-cross-{}",
        CROSS_IMAGE.trim_start_matches("sha256:")
    );
    assert_eq!(
        fixture.runtime.events(),
        [
            "volume rm spice-release-target-linux-stage-superseded".to_owned(),
            format!("volume create {}", linux_volume()),
            sealed_volume(&linux_volume()),
            sealed_volume(&linux_volume()),
            "volume rm spice-release-target-windows-cross-superseded".to_owned(),
            format!("volume create {cross_volume}"),
            sealed_volume(&cross_volume),
        ]
    );
}

#[cfg(unix)]
#[test]
fn windows_cross_output_is_writable_to_a_remapped_container_user() {
    let fixture = Fixture::new();
    let container = fixture.container();
    let git_common = fixture.workspace.join(".git");
    fs::create_dir_all(&git_common).expect("fixture Git common directory");
    let out = fixture.root.join("windows-cross-out");
    let stale_root = out.join("root/proof");
    fs::create_dir_all(&stale_root).expect("fixture stale cross output");
    for path in [&out, &out.join("root"), &stale_root] {
        let mut permissions = fs::metadata(path)
            .expect("cross output metadata")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(path, permissions).expect("restrict cross output");
    }

    container
        .cross_build_windows(super::WindowsCross {
            cross_image: CROSS_IMAGE,
            msvc_generation: "fixture",
            git_common: &git_common,
            out: &out,
            delivered: None,
            canvas: None,
        })
        .expect("sealed cross build");

    for path in [out, stale_root] {
        assert_eq!(
            fs::metadata(path)
                .expect("cross output metadata")
                .permissions()
                .mode()
                & 0o777,
            0o777
        );
    }
}

#[test]
fn an_unavailable_persistent_target_compiles_in_fresh_scratch() {
    let fixture = Fixture::with(Runtime {
        refuses_volumes: true,
        ..Runtime::docker()
    });
    let container = fixture.container();
    let git_common = fixture.workspace.join(".git");
    fs::create_dir_all(&git_common).expect("fixture Git common directory");

    let stage = container
        .stage_release(&git_common, None, None, None)
        .expect("a refused volume never refuses the stage");

    assert!(stage.success);
    assert_eq!(
        fixture.runtime.events(),
        ["sealed type=volume,destination=/cargo-target"]
    );
}

#[test]
fn a_named_host_target_replaces_the_persistent_volume() {
    let fixture = Fixture::new();
    let container = fixture.container();
    let git_common = fixture.workspace.join(".git");
    fs::create_dir_all(&git_common).expect("fixture Git common directory");
    let target = fixture.root.join("sealed-target");
    fs::create_dir_all(&target).expect("fixture host target");

    container
        .stage_release(&git_common, None, None, Some(&target))
        .expect("sealed stage");

    let target = fs::canonicalize(&target).expect("canonical host target");
    assert_eq!(
        fixture.runtime.events(),
        [format!(
            "sealed type=bind,source={},destination=/cargo-target",
            target.display()
        )]
    );
}

struct Fixture {
    root: std::path::PathBuf,
    executable: std::path::PathBuf,
    workspace: std::path::PathBuf,
    runtime: Arc<Runtime>,
}

impl Fixture {
    fn new() -> Self {
        Self::with(Runtime::docker())
    }

    fn with(runtime: Runtime) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "spice-proof-container-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let workspace = root.join("workspace");
        let executable = root.join("docker");
        fs::create_dir_all(&workspace).expect("fixture workspace");
        fs::write(workspace.join("Cargo.lock"), FIXTURE_LOCK).expect("fixture lockfile");
        crate::test_support::executable(&executable, b"fixture runtime");
        Self {
            root,
            executable,
            workspace,
            runtime: Arc::new(runtime),
        }
    }

    fn oci(&self) -> OciRuntime {
        OciRuntime::attest_with(self.runtime.clone(), &self.executable, limits())
            .expect("fixture runtime")
    }

    fn container(&self) -> Container {
        Container::new(self.oci(), &self.workspace, limits()).expect("sealed fixture")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("remove fixture");
    }
}

fn limits() -> Limits {
    Limits::new(
        Deadlines::new(
            Duration::from_secs(30),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .expect("deadlines"),
        Duration::from_secs(1),
        4096,
    )
    .expect("limits")
}

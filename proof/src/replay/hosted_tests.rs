//! The hosted Windows battery runs and binds the cross-built bundle's exact
//! proof executables.

use std::collections::BTreeMap;

use windows_execution::hosted::bundle;
use windows_execution::hosted::client::{HostedExecution, HostedOutput};
use windows_execution::hosted::{
    GITHUB_PROVIDER, Identity, ImagePin, Isolation, PROTOCOL_VERSION, PreparedBundlePin, Request,
    ResultDocument, ToolchainPins, WORKFLOW,
};
use windows_execution::protocol::{
    BUNDLE_PROGRAM, BundleProgram, Capture, Command, CommandResult, Completion, Limits,
};
use wire::manifest::{ProofEnvironment, ProofStanding};
use wire::{Digest, Domain};

use super::{CheckKind, CheckStanding, HostedBattery, HostedBatteryError, replay_hosted_windows};

const PASSED: &[u8] =
    b"test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n";

/// The proof files of one cross-built bundle beside the files every bundle
/// also carries, with `gate`'s suite executable digest `suite`.
fn bundle_files(suite: &str) -> Vec<(&'static str, String)> {
    vec![
        ("candidate/spice.exe", "a".repeat(64)),
        ("proof/dialect-refusals/test-omp.exe", "b".repeat(64)),
        ("proof/gate/lib-gate.exe", "c".repeat(64)),
        ("proof/gate/test-suite.exe", suite.to_owned()),
        ("proof/supply-rebuild/test-pipeline.exe", "d".repeat(64)),
        ("proof/wait-and-process/lib-sill.exe", "e".repeat(64)),
        ("runner/windows-hosted.exe", "f".repeat(64)),
    ]
}

fn battery(suite: &str) -> HostedBattery {
    let files = bundle_files(suite);
    HostedBattery::from_bundle(files.iter().map(|(path, sha)| (*path, sha.as_str())))
        .expect("bundle carries the current battery")
}

fn pins() -> ToolchainPins {
    let pin = |value: &str| value.to_owned();
    ToolchainPins {
        rust: pin("rustc 1.97.1"),
        cargo: pin("cargo 1.97.1"),
        git: pin("git"),
        trunk: pin("trunk"),
        msvc: pin("19.44"),
        sdk: pin("10.0.26100.0"),
        rustc_path: pin("C:/rustc.exe"),
        cargo_path: pin("C:/cargo.exe"),
        git_path: pin("C:/git.exe"),
        trunk_path: pin("C:/trunk.exe"),
        cl_path: pin("C:/cl.exe"),
        rc_path: pin("C:/rc.exe"),
        link_path: pin("C:/link.exe"),
        path: pin("C:/tools"),
        include: pin("C:/include"),
        lib: pin("C:/lib"),
    }
}

/// The bundled publisher's stage command a release run sends before its
/// battery.
fn stage() -> Command {
    Command {
        program: format!("{BUNDLE_PROGRAM}runner/xtask.exe"),
        arguments: vec!["stage-target".to_owned()],
        bundle: Some(BundleProgram {
            sha256: "0".repeat(64),
            directory: bundle::SOURCE_ROOT_DIRECTORY.to_owned(),
        }),
    }
}

/// One provider run that staged with the bundled publisher, then ran
/// `requested` and verified `verified`, each command exiting with the
/// matching `codes` entry.
fn execution(requested: &[Command], verified: &[Command], codes: &[i32]) -> HostedExecution {
    let mut commands = vec![stage()];
    commands.extend_from_slice(requested);
    let mut verified_commands = vec![stage()];
    verified_commands.extend_from_slice(verified);
    let image = ImagePin {
        label: "windows-2025".to_owned(),
        provider_os: "win25".to_owned(),
        version: "20261013.288.2".to_owned(),
        os_version: "10.0.26100.33438".to_owned(),
        architecture: "X64".to_owned(),
    };
    let request = Request {
        version: PROTOCOL_VERSION,
        challenge: "1".repeat(64),
        provider: GITHUB_PROVIDER.to_owned(),
        repository: "infimalabs/spice".to_owned(),
        workflow_ref: "refs/heads/main".to_owned(),
        source_commit: "0".repeat(40),
        source: "2".repeat(64),
        archive_sha256: "4".repeat(64),
        transport_asset: None,
        release_inputs: None,
        tree: "3".repeat(64),
        image: image.clone(),
        toolchain: pins(),
        limits: Limits {
            runtime_seconds: 60,
            output_bytes: 1024,
        },
        commands,
        canvas: None,
        prepared_bundle: PreparedBundlePin {
            build_key: format!("sha256:{}", "5".repeat(64)),
            inputs: BTreeMap::new(),
        },
        cross_bundle: None,
    };
    let outputs = std::iter::once(0)
        .chain(codes.iter().copied())
        .map(|code| HostedOutput {
            completion: Completion::Exited { code },
            stdout: PASSED.to_vec(),
            stderr: Vec::new(),
        })
        .collect::<Vec<_>>();
    let result = ResultDocument {
        version: PROTOCOL_VERSION,
        challenge: request.challenge.clone(),
        identity: Identity {
            provider: GITHUB_PROVIDER.to_owned(),
            repository: request.repository.clone(),
            workflow: WORKFLOW.to_owned(),
            run_id: 7,
            run_attempt: 1,
            image,
            runner: Digest::compute(Domain::ProofNativeRunner, b"runner"),
            source: request.source.clone(),
            toolchain: Some(pins()),
            executables: bundle::executables(&verified_commands),
        },
        outputs: outputs
            .iter()
            .map(|output| CommandResult {
                completion: output.completion.clone(),
                stdout: Capture::from_bytes(&output.stdout, false),
                stderr: Capture::from_bytes(&output.stderr, false),
            })
            .collect(),
        request,
        artifacts: Vec::new(),
        isolation: Isolation {
            source_read_only: true,
            fresh_cargo_home: true,
            fresh_target: true,
            loopback_only: true,
            process_tree_bounded: true,
        },
    };
    HostedExecution { result, outputs }
}

#[test]
fn every_member_runs_its_bundle_executables_from_its_package() {
    let battery = battery(&"9".repeat(64));
    let runs = battery
        .commands()
        .iter()
        .map(|command| {
            let bound = command.bundle.as_ref().expect("bundle executable");
            (
                command.program.as_str(),
                bound.sha256.as_str(),
                bound.directory.as_str(),
                command.arguments.is_empty(),
            )
        })
        .collect::<Vec<_>>();
    let c = "c".repeat(64);
    let nine = "9".repeat(64);
    let b = "b".repeat(64);
    let e = "e".repeat(64);
    let d = "d".repeat(64);
    assert_eq!(
        runs,
        [
            ("@bundle/proof/gate/lib-gate.exe", c.as_str(), "gate", true),
            (
                "@bundle/proof/gate/test-suite.exe",
                nine.as_str(),
                "gate",
                true
            ),
            (
                "@bundle/proof/dialect-refusals/test-omp.exe",
                b.as_str(),
                "listen",
                true
            ),
            (
                "@bundle/proof/wait-and-process/lib-sill.exe",
                e.as_str(),
                "sill",
                true
            ),
            (
                "@bundle/proof/supply-rebuild/test-pipeline.exe",
                d.as_str(),
                "dist",
                true
            ),
        ]
    );
}

#[test]
fn a_bundle_without_the_current_battery_refuses() {
    let digest = "a".repeat(64);
    let missing = bundle_files(&digest)
        .into_iter()
        .filter(|(path, _)| !path.starts_with("proof/wait-and-process/"))
        .collect::<Vec<_>>();
    assert_eq!(
        HostedBattery::from_bundle(missing.iter().map(|(path, sha)| (*path, sha.as_str()))),
        Err(HostedBatteryError::Empty(CheckKind::WaitAndProcess))
    );
    let mut foreign = bundle_files(&digest);
    foreign.push(("proof/other/test.exe", digest.clone()));
    assert_eq!(
        HostedBattery::from_bundle(foreign.iter().map(|(path, sha)| (*path, sha.as_str()))),
        Err(HostedBatteryError::Member(
            "proof/other/test.exe".to_owned()
        ))
    );
}

#[test]
fn the_replay_environment_binds_the_verified_executable_digests() {
    let battery = battery(&"9".repeat(64));
    let codes = vec![0; battery.commands().len()];
    let run = execution(battery.commands(), battery.commands(), &codes);
    let verdict = replay_hosted_windows(&run, 1, &battery).expect("exact bundle replays");
    assert_eq!(verdict.standing(), ProofStanding::Clean);
    let ProofEnvironment::HostedWindows { identity } = verdict.environment() else {
        panic!("hosted Windows replay names a hosted environment");
    };
    let bound = identity
        .executables
        .iter()
        .map(|executable| (executable.path.as_str(), executable.sha256.as_str()))
        .collect::<Vec<_>>();
    let nine = "9".repeat(64);
    assert!(bound.contains(&("proof/gate/test-suite.exe", nine.as_str())));
    assert_eq!(bound.len(), 1 + battery.commands().len());
}

#[test]
fn a_run_naming_other_executable_bytes_refuses() {
    let expected = battery(&"9".repeat(64));
    let other = battery(&"8".repeat(64));
    let codes = vec![0; expected.commands().len()];

    let requested_other = execution(other.commands(), other.commands(), &codes);
    assert!(replay_hosted_windows(&requested_other, 1, &expected).is_err());

    let verified_other = execution(expected.commands(), other.commands(), &codes);
    assert!(replay_hosted_windows(&verified_other, 1, &expected).is_err());
}

#[test]
fn any_failing_executable_fails_its_member() {
    let battery = battery(&"9".repeat(64));
    let mut codes = vec![0; battery.commands().len()];
    codes[1] = 101;
    let run = execution(battery.commands(), battery.commands(), &codes);
    let verdict = replay_hosted_windows(&run, 1, &battery).expect("complete run replays");
    assert_eq!(verdict.standing(), ProofStanding::Refused);
    let standings = verdict
        .checks()
        .iter()
        .map(|check| (check.kind(), check.standing()))
        .collect::<Vec<_>>();
    assert_eq!(
        standings[0],
        (CheckKind::Gate, CheckStanding::Failed { code: Some(101) })
    );
    assert!(
        standings[1..]
            .iter()
            .all(|(_, standing)| *standing == CheckStanding::Clean)
    );
}

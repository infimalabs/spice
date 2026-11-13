use super::*;
use wire::Domain;

fn preview() -> InstallPreview {
    let digest = Digest::compute(Domain::InstallPlan, b"install guidance fixture");
    wire::decode(
        serde_json::json!({
            "digest": digest,
            "plan": {
                "version": 1,
                "operation_count": 0,
                "operations": [],
                "retirements": [],
            },
        })
        .to_string()
        .as_bytes(),
    )
    .expect("a minimal preview decodes")
}

#[cfg(unix)]
fn apply_line(text: &str) -> &str {
    text.lines()
        .last()
        .and_then(|line| line.strip_prefix("apply: "))
        .expect("a preview ends with its apply command")
}

#[cfg(unix)]
const PROGRAMS: [&str; 3] = [
    "spice",
    "./spice",
    "/tmp/My Repo/.git/.spice-install/bin/spice",
];

#[cfg(unix)]
#[test]
fn source_preview_replays_the_invoked_program_with_the_digest() {
    for program in PROGRAMS {
        let preview = preview();
        let digest = preview.digest().to_string();

        let text = PlanPreview::source(preview, program).to_string();

        assert_eq!(
            shell_words::split(apply_line(&text)).expect("the replay command parses"),
            [program, "install", &format!("--apply={digest}")],
            "replay for {program:?}",
        );
    }
}

#[cfg(unix)]
#[test]
fn package_preview_replays_the_program_and_original_source_as_single_arguments() {
    for program in PROGRAMS {
        for package in [
            "dist/spice-0.3.0",
            "/tmp/My Packages/spice 0.3.0",
            "./pkg; rm -rf ~ $(touch x) `id` 'quoted' \"double\" *?[a]",
        ] {
            let preview = preview();
            let digest = preview.digest().to_string();

            let text = PlanPreview::package(preview, program, package).to_string();

            assert_eq!(
                shell_words::split(apply_line(&text)).expect("the replay command parses"),
                [
                    program,
                    "install",
                    "--package",
                    package,
                    &format!("--apply={digest}"),
                ],
                "replay for {program:?} {package:?}",
            );
        }
    }
}

#[test]
fn windows_replay_quotes_every_program_as_one_powershell_literal() {
    let digest = preview().digest().clone();
    let package = r"C:\Packages\Spice Release";
    for (program, rendered) in [
        ("spice.exe", "& 'spice.exe'"),
        (r".\spice.exe", r"& '.\spice.exe'"),
        (
            r"C:\Program Files\Spice\spice.exe",
            r"& 'C:\Program Files\Spice\spice.exe'",
        ),
        (
            r"C:\Tools&Stuff;(x)\$bin`\spice.exe",
            r"& 'C:\Tools&Stuff;(x)\$bin`\spice.exe'",
        ),
        (r"C:\O'Brien\spice.exe", r"& 'C:\O''Brien\spice.exe'"),
    ] {
        assert_eq!(
            windows_apply_command(program, None, &digest),
            format!("{rendered} install --apply={digest}"),
            "source replay for {program:?}",
        );
        assert_eq!(
            windows_apply_command(program, Some(package), &digest),
            format!("{rendered} install --package \"{package}\" --apply={digest}"),
            "package replay for {program:?}",
        );
    }
}

/// Replay guidance never reaches the structured preview: an installed
/// front-end decodes it with unknown fields denied, and the guidance is this
/// executable's rendering concern.
#[test]
fn guidance_never_changes_the_structured_preview() {
    let preview = preview();
    let expected = serde_json::to_value(&preview).expect("preview encodes");

    for guided in [
        PlanPreview::source(preview.clone(), "spice"),
        PlanPreview::package(preview, "spice", "/tmp/My Packages/spice"),
    ] {
        assert_eq!(
            serde_json::to_value(&guided).expect("guided preview encodes"),
            expected,
        );
    }
}

#[test]
fn the_owner_step_follows_the_receipt_boundary() {
    let failed = InstallError::RecordMissing;
    assert_eq!(
        owner_step(Ok(InstallStatus::Establishing), false),
        OwnerStep::Establish,
        "a publication that stopped at the lifecycle boundary is established"
    );
    assert_eq!(
        owner_step(Ok(InstallStatus::Establishing), true),
        OwnerStep::Establish,
        "an upgrade that quiesced the prior owner establishes the new one"
    );
    assert_eq!(
        owner_step(Ok(InstallStatus::Complete), false),
        OwnerStep::Leave,
        "a converged or non-Windows plan recreates no removed owner"
    );
    assert_eq!(
        owner_step(Err(&failed), true),
        OwnerStep::Restore,
        "a failed replacement restores the owner it quiesced"
    );
    assert_eq!(
        owner_step(Err(&failed), false),
        OwnerStep::Leave,
        "a failed first publication installs nothing"
    );
}

#[test]
fn package_reversal_owns_only_package_default_standing() {
    let configured = spice::install::LifecycleOwnerStanding::Configured(Digest::compute(
        Domain::InstallPlan,
        b"explicit keeper policy",
    ));
    assert_eq!(reversal_owner_step(None), ReversalOwnerStep::Leave);
    assert_eq!(
        reversal_owner_step(Some(&spice::install::LifecycleOwnerStanding::Default)),
        ReversalOwnerStep::Remove,
    );
    assert_eq!(
        reversal_owner_step(Some(&configured)),
        ReversalOwnerStep::Refuse,
    );
}

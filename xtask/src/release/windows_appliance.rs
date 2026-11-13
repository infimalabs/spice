use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

use sill::Environment;
use wire::manifest::{BuildInfo, ReleaseTarget};

use super::{
    CommandSpec, Executor as _, ReleaseEnvironment, SystemExecutor, TargetBuildError, build_error,
    find_program, native, read_regular, successful, verify_checkout, workspace_root,
};

/// Cross-build the complete Windows appliance with the current staged canvas,
/// without staging a release payload or receipt.
///
/// A clean checkout builds the release archive's exported tree into the same
/// target directory as `stage-target`; a dirty checkout remains the developer
/// guest loop and builds the worktree as `-dev`.
pub fn build_windows_appliance(
    release: ReleaseEnvironment,
    ambient: Environment,
    output: &mut impl Write,
) -> Result<PathBuf, TargetBuildError> {
    let target = ReleaseTarget::X86_64PcWindowsMsvc;
    let workspace = workspace_root()?;
    let path = release
        .path
        .as_deref()
        .ok_or(TargetBuildError::MissingProgram("PATH"))?;
    let checkout_executor = SystemExecutor::ambient(&workspace, ambient.clone());
    let git = find_program(path, "git")?;
    let kind = match verify_checkout(&checkout_executor, &git) {
        Ok(()) => dist::Build::Staged,
        Err(TargetBuildError::SourceDirty(reason)) => {
            writeln!(
                output,
                "[windows-appliance] {reason}; building the -dev version"
            )
            .map_err(TargetBuildError::Output)?;
            dist::Build::Dev
        }
        Err(error) => return Err(error),
    };
    let WindowsSource {
        root,
        target_dir,
        build_info,
        archive,
    } = select_windows_source(&workspace, &release, kind)?;
    crate::app::app_from(&root, &release, output).map_err(TargetBuildError::Application)?;
    let canvas = crate::app::StagedCanvas::read(&root, &build_info)
        .map_err(TargetBuildError::Application)?;
    let build =
        native::cargo_build(target, &target_dir, &canvas).map_err(TargetBuildError::Application)?;
    let spice = find_program(path, "spice")?;
    let mut wrapped_arguments = vec![
        OsString::from("dev"),
        OsString::from("cargo"),
        OsString::from("--"),
    ];
    wrapped_arguments.extend(build.arguments);
    let native_executor = SystemExecutor::ambient(&root, ambient);
    let native = successful(native_executor.execute(CommandSpec::new(
        "native-build",
        &spice,
        wrapped_arguments,
    ))?)?;
    let cargo = crate::native_link::cargo_output(&native.stdout, &build.link.object)
        .map_err(build_error)
        .map_err(TargetBuildError::Build)?;
    let artifacts = cargo.artifacts;
    let rebuilt = cargo.rebuilt;
    let emitted = cargo.emitted.is_some();
    if let Some(plan) = cargo.emitted {
        plan.publish(&build.link.plan)
            .map_err(build_error)
            .map_err(TargetBuildError::Build)?;
    }
    let executable = cargo.executable;
    let bytes = read_regular(&executable)?;
    if canvas.canvas().verify_linked(&bytes).is_err() {
        if emitted {
            return Err(TargetBuildError::Build(build_error(
                "rustc completed a final link that did not carry the staged canvas",
            )));
        }
        relink_windows(
            &build.link,
            &executable,
            artifacts,
            rebuilt,
            &native_executor,
            output,
            &canvas,
        )?;
    }
    if let Some(archive) = &archive {
        archive.verify()?;
    }
    let archive = archive.as_ref().map_or_else(
        || "development".to_owned(),
        |archive| archive.digest.to_string(),
    );
    writeln!(
        output,
        "[windows-appliance] executable={} version={} archive={} canvas={}",
        executable.display(),
        build_info.product,
        archive,
        canvas.canvas().generation()
    )
    .map_err(TargetBuildError::Output)?;
    Ok(executable)
}

struct WindowsSource {
    root: PathBuf,
    target_dir: PathBuf,
    build_info: BuildInfo,
    archive: Option<super::SourceArchive>,
}

fn select_windows_source(
    workspace: &Path,
    release: &ReleaseEnvironment,
    kind: dist::Build,
) -> Result<WindowsSource, TargetBuildError> {
    if kind == dist::Build::Dev {
        let build_info =
            spice::version::build_info(workspace, kind).map_err(TargetBuildError::SourceVersion)?;
        return Ok(WindowsSource {
            root: workspace.to_owned(),
            target_dir: workspace.join("target"),
            build_info,
            archive: None,
        });
    }
    let head = dist::head_build_info(workspace, dist::Build::Staged)
        .map_err(TargetBuildError::SourceVersion)?;
    let expected = head
        .source
        .ok_or_else(|| {
            TargetBuildError::SourceVersion("HEAD build identity names no source commit".to_owned())
        })?
        .commit;
    let (archive, root) = super::local_source_tree(workspace, release)?;
    let source = archive.source(&root)?;
    require_archive_head(&expected, &source.commit)?;
    let build_info = spice::version::build_info(&root, dist::Build::Staged)
        .map_err(TargetBuildError::SourceVersion)?;
    Ok(WindowsSource {
        root,
        target_dir: release.build_root(workspace).join("release-appliance"),
        build_info,
        archive: Some(archive),
    })
}

fn require_archive_head(expected: &str, observed: &str) -> Result<(), TargetBuildError> {
    if observed == expected {
        Ok(())
    } else {
        Err(TargetBuildError::SourceMoved {
            expected: expected.to_owned(),
            observed: observed.to_owned(),
        })
    }
}

fn relink_windows(
    link: &crate::app::NativeLink,
    executable: &Path,
    artifacts: usize,
    rebuilt: usize,
    executor: &SystemExecutor,
    output: &mut impl Write,
    canvas: &crate::app::StagedCanvas,
) -> Result<(), TargetBuildError> {
    let plan = crate::native_link::LinkPlan::read(&link.plan, &link.object)
        .map_err(build_error)
        .map_err(TargetBuildError::Build)?;
    writeln!(
        output,
        "[windows-appliance] Cargo reported {artifacts} compiler artifacts and {rebuilt} rebuilt; relinking {} with {}",
        executable.display(),
        plan.program().display(),
    )
    .map_err(TargetBuildError::Output)?;
    let (set, unset) = plan.environment();
    successful(
        executor.execute(
            CommandSpec::new(
                "native-relink",
                plan.program(),
                plan.arguments().iter().cloned(),
            )
            .with_environment(set, unset),
        )?,
    )?;
    plan.publish_output(executable)
        .map_err(build_error)
        .map_err(TargetBuildError::Build)?;
    let bytes = read_regular(executable)?;
    canvas
        .canvas()
        .verify_linked(&bytes)
        .map_err(build_error)
        .map_err(TargetBuildError::Build)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_archive_commit_must_equal_head() {
        let head = "1111111111111111111111111111111111111111";
        assert!(require_archive_head(head, head).is_ok());
        let archive = "2222222222222222222222222222222222222222";
        assert!(matches!(
            require_archive_head(head, archive),
            Err(TargetBuildError::SourceMoved { expected, observed })
                if expected == head && observed == archive
        ));
    }
}

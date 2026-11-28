use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use super::{USAGE_EXIT, help};

#[derive(Default)]
struct ProofImageBuildArguments {
    archive: Option<PathBuf>,
    cache_from: Option<OsString>,
    cache_to: Option<OsString>,
    build_key: Option<OsString>,
}

fn proof_image_build_arguments(arguments: &[OsString]) -> Option<ProofImageBuildArguments> {
    let [operation, arguments @ ..] = arguments else {
        return None;
    };
    if operation != OsStr::new("build") {
        return None;
    }
    let mut request = ProofImageBuildArguments::default();
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--cache-from") => {
                let value = arguments.next()?.clone();
                if value.is_empty() || request.cache_from.replace(value).is_some() {
                    return None;
                }
            }
            Some("--cache-to") => {
                let value = arguments.next()?.clone();
                if value.is_empty() || request.cache_to.replace(value).is_some() {
                    return None;
                }
            }
            Some("--build-key") => {
                let value = arguments.next()?.clone();
                if value.is_empty() || request.build_key.replace(value).is_some() {
                    return None;
                }
            }
            Some(value) if value.starts_with('-') => return None,
            _ if request.archive.is_none() => request.archive = Some(PathBuf::from(argument)),
            _ => return None,
        }
    }
    Some(request)
}

pub(super) fn run_proof_image(arguments: Vec<OsString>) -> ExitCode {
    let (operation, archive, cache_from, cache_to, build_key) = if let Some(request) =
        proof_image_build_arguments(&arguments)
    {
        (
            "build",
            request.archive,
            request.cache_from,
            request.cache_to,
            request.build_key,
        )
    } else {
        match arguments.as_slice() {
            [operation] if operation == OsStr::new("verify") => ("verify", None, None, None, None),
            [operation, archive] if operation == OsStr::new("load") => {
                ("load", Some(PathBuf::from(archive)), None, None, None)
            }
            [operation, archive] if operation == OsStr::new("verify") => {
                ("verify", Some(PathBuf::from(archive)), None, None, None)
            }
            _ => {
                eprintln!("{}", help());
                return ExitCode::from(USAGE_EXIT);
            }
        }
    };
    let repository = match spice::repository::Repository::discover(Path::new(".")) {
        Ok(repository) => repository,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    let archive = archive.unwrap_or_else(|| {
        repository
            .worktree_root()
            .join("target/proof-image/spice-proof-image.tar")
    });
    let result = match operation {
        "build" => xtask::build_proof_image(
            repository.worktree_root(),
            &archive,
            cache_from.as_deref(),
            cache_to.as_deref(),
            build_key.as_deref(),
            &mut stdout,
            &mut stderr,
        ),
        "load" => xtask::load_proof_image(
            repository.worktree_root(),
            &archive,
            &mut stdout,
            &mut stderr,
        ),
        "verify" => xtask::build_proof_image(
            repository.worktree_root(),
            &archive,
            None,
            None,
            None,
            &mut stdout,
            &mut stderr,
        )
        .and_then(|()| {
            xtask::load_proof_image(
                repository.worktree_root(),
                &archive,
                &mut stdout,
                &mut stderr,
            )
        }),
        _ => unreachable!("proof-image operation was admitted above"),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(error.exit_code())
        }
    }
}

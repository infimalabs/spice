use std::ffi::OsString;
use std::io;
use std::path::Path;

use spice::ambient::Ambient;

use super::{CommandResult, help, release_environment, usage_error};

pub(super) fn run_dispatch_hosted_release(
    arguments: Vec<OsString>,
    output: &mut impl io::Write,
) -> CommandResult {
    let arguments = arguments
        .iter()
        .map(|argument| argument.to_str())
        .collect::<Option<Vec<_>>>()
        .ok_or_else(usage_error)?;
    let (notes, stage_only, release_symbols) = match arguments.as_slice() {
        ["--notes", notes] => (notes, false, false),
        ["--notes", notes, "--stage-only"] => (notes, true, false),
        ["--notes", notes, "--symbols"] => (notes, false, true),
        ["--notes", notes, "--stage-only", "--symbols"]
        | ["--notes", notes, "--symbols", "--stage-only"] => (notes, true, true),
        _ => {
            eprintln!("{}", help());
            return Err(usage_error().into());
        }
    };
    xtask::dispatch_hosted_release(Path::new(notes), stage_only, release_symbols, output)
        .map(|_| ())
        .map_err(Into::into)
}

pub(super) fn run_rebuild_release_symbols(
    arguments: Vec<OsString>,
    ambient: &Ambient,
    output: &mut impl io::Write,
) -> CommandResult {
    let [recipe, released_binary, destination] = arguments.as_slice() else {
        eprintln!("{}", help());
        return Err(usage_error().into());
    };
    xtask::rebuild_release_symbols(
        Path::new(recipe),
        Path::new(released_binary),
        Path::new(destination),
        release_environment(ambient),
        output,
    )
    .map_err(Into::into)
}

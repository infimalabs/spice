//! Shell-safe lifecycle recovery receipts for preserved demo roots.

use std::path::Path;

use spice::shell_quote;
use wire::{DemoAnswer, Outcome};

use super::{DemoResult, Environment, protocol_fault, refusal};

pub(super) fn preserve_environment_failure(
    environment: &Environment,
    failure: Outcome<DemoAnswer>,
) -> Outcome<DemoAnswer> {
    append_recovery(
        failure,
        &environment.root,
        &environment.launch_command,
        &environment.remove_command,
    )
}

pub(super) fn preserve_failure(
    root: &Path,
    executable: &Path,
    failure: Outcome<DemoAnswer>,
) -> Outcome<DemoAnswer> {
    let root_text = root.to_string_lossy();
    let executable = executable.to_string_lossy();
    let quoted_root = shell_quote(&root_text);
    let launch = format!(
        "cd {quoted_root} && {} demo --reuse {quoted_root}",
        shell_quote(&executable)
    );
    let remove = format!("rm -rf -- {quoted_root}");
    append_recovery(failure, root, &launch, &remove)
}

fn append_recovery(
    failure: Outcome<DemoAnswer>,
    root: &Path,
    launch: &str,
    remove: &str,
) -> Outcome<DemoAnswer> {
    let recovery = format!(
        "demo root preserved at {}; retry with {launch}; remove with {remove}",
        root.display()
    );
    match failure {
        Outcome::Refusal(mut refusal) => {
            let diagnostic = refusal
                .diagnostic_mut()
                .expect("demo recovery augments only diagnostic refusals");
            let observed = diagnostic.replace_observed(None);
            let _ = diagnostic.replace_observed(Some(match observed {
                Some(observed) => format!("{observed}; {recovery}"),
                None => recovery,
            }));
            Outcome::Refusal(refusal)
        }
        Outcome::Fault(mut fault) => {
            fault.detail = format!("{}; {recovery}", fault.detail);
            Outcome::Fault(fault)
        }
        Outcome::Reply(_) => protocol_fault("demo::failure-shape-invalid", recovery),
    }
}

pub(super) fn require_utf8<'a>(path: &'a Path, label: &str) -> DemoResult<&'a str> {
    Ok(path.to_str().ok_or_else(|| {
        refusal(
            "demo::path-refused",
            Some("one UTF-8 path that can be reproduced in a shell recovery command"),
            Some(label),
            "choose a UTF-8 demo root and executable path",
        )
    })?)
}

//! Canonical compiler environment shared by Cargo-launched and installed rims.

use std::ffi::{OsStr, OsString};
use std::path::Path;

pub(super) const SHELL_LAST_COMMAND: &str = "_";
const SHELL_OLD_DIRECTORY: &str = "OLDPWD";
const SHELL_LEVEL: &str = "SHLVL";
const HOME: &str = "HOME";
const CARGO_HOME: &str = "CARGO_HOME";
const RUSTUP_HOME: &str = "RUSTUP_HOME";
pub(super) const SHELL_HOOK_PREFIX: &[u8] = b"SPICE_SHELL_HOOK_"; // env-policy: allow

/// Names that route a command to its producer, lane, or agent tooling. The
/// RTK history database is one per lane and the multiplexer opt-out is omp's
/// alone, so neither may partition receipt identity across lanes.
const OPERATIONAL_ROUTING: [&str; 9] = [
    wire::SpiceVariable::AgentVendor.as_str(),
    wire::SpiceVariable::Lane.as_str(),
    wire::SpiceVariable::PreCommitPaths.as_str(),
    wire::SpiceVariable::PreCommitSource.as_str(),
    wire::SpiceVariable::RtkExecutable.as_str(),
    wire::SpiceVariable::Session.as_str(),
    wire::SpiceVariable::WrapperSpice.as_str(),
    crate::rtk::HISTORY_VARIABLE,
    crate::agent_launch::OMP_DISABLE_LSPMUX,
];

const GIT_LOCAL_ENVIRONMENT: [&str; 15] = [
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
];

/// Per-login agent sockets and terminal session identifiers: they name the
/// shell a command ran in, never a build or test input, so a commit made from
/// another terminal still reuses the receipts its prove produced.
const SESSION_IDENTITY: [&str; 9] = [
    "ITERM_SESSION_ID",
    "SSH_AGENT_PID",
    "SSH_AUTH_SOCK",
    "SSH_CLIENT",
    "SSH_CONNECTION",
    "SSH_TTY",
    "TERM_SESSION_ID",
    "TMUX",
    "TMUX_PANE",
];

pub(super) fn compiler_environment(ambient: &crate::ambient::Ambient) -> Vec<(OsString, OsString)> {
    compiler_environment_from(ambient.variables().iter().cloned())
}

#[cfg(test)]
pub(super) fn compiler_environment_from_with_cargo_home(
    environment: impl IntoIterator<Item = (OsString, OsString)>,
    cargo_home: &Path,
) -> Vec<(OsString, OsString)> {
    let mut environment = environment.into_iter().collect::<Vec<_>>();
    set_environment_value(&mut environment, CARGO_HOME, cargo_home);
    compiler_environment_from(environment)
}

pub(super) fn compiler_environment_from(
    environment: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    compiler_environment_for_platform(environment, cfg!(windows))
}

fn compiler_environment_for_platform(
    environment: impl IntoIterator<Item = (OsString, OsString)>,
    windows: bool,
) -> Vec<(OsString, OsString)> {
    let mut environment = canonical_platform_environment(environment, windows)
        .into_iter()
        .filter(|(name, _)| retained(name))
        .collect::<Vec<_>>();
    bind_implicit_home(&mut environment, CARGO_HOME, ".cargo");
    bind_implicit_home(&mut environment, RUSTUP_HOME, ".rustup");
    environment.sort();
    environment
}

fn canonical_platform_environment(
    environment: impl IntoIterator<Item = (OsString, OsString)>,
    windows: bool,
) -> Vec<(OsString, OsString)> {
    let mut environment = environment
        .into_iter()
        .map(|(name, value)| {
            let name = if windows {
                name.to_str()
                    .map(|name| OsString::from(name.to_ascii_uppercase()))
                    .unwrap_or(name)
            } else {
                name
            };
            (name, value)
        })
        .collect::<Vec<_>>();
    if !windows {
        return environment;
    }
    let value = |name: &str| {
        environment
            .iter()
            .find(|(candidate, _)| candidate == OsStr::new(name))
            .map(|(_, value)| value.clone())
    };
    let git_shell = value("MSYSTEM").is_some();
    let user_profile = value("USERPROFILE");
    let temp = value("TEMP").or_else(|| value("TMP"));
    environment.retain(|(name, value)| {
        if name == OsStr::new("HOME")
            && user_profile
                .as_ref()
                .is_some_and(|profile| windows_value_eq(value, profile))
        {
            return false;
        }
        if name == OsStr::new("TMPDIR")
            && temp
                .as_ref()
                .is_some_and(|temp| windows_value_eq(value, temp))
        {
            return false;
        }
        if !git_shell {
            return true;
        }
        !matches!(
            name.to_str(),
            Some("MSYSTEM") | Some("COLORTERM") | Some("TERM") | Some("LC_CTYPE")
        )
    });
    environment
}

fn windows_value_eq(left: &OsStr, right: &OsStr) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

fn retained(name: &OsStr) -> bool {
    let bytes = name.as_encoded_bytes();
    bytes != SHELL_LAST_COMMAND.as_bytes()
        && bytes != SHELL_OLD_DIRECTORY.as_bytes()
        && bytes != SHELL_LEVEL.as_bytes()
        && bytes != b"PS1"
        && bytes != b"CARGO"
        && bytes != b"CARGO_MANIFEST_DIR"
        && bytes != b"CARGO_MANIFEST_PATH"
        && !bytes.starts_with(b"CARGO_PKG_")
        && bytes != b"DYLD_FALLBACK_LIBRARY_PATH"
        && bytes != b"RUSTUP_TOOLCHAIN"
        && bytes != b"RUSTUP_TOOLCHAIN_SOURCE"
        && bytes != b"RUST_RECURSION_COUNT"
        && bytes != b"GIT_AUTHOR_DATE"
        && bytes != b"GIT_AUTHOR_EMAIL"
        && bytes != b"GIT_AUTHOR_NAME"
        && bytes != b"GIT_COMMITTER_DATE"
        && bytes != b"GIT_COMMITTER_EMAIL"
        && bytes != b"GIT_COMMITTER_NAME"
        && bytes != b"GIT_EDITOR"
        && bytes != b"GIT_EXEC_PATH"
        && bytes != b"GIT_OPTIONAL_LOCKS"
        && bytes != b"GIT_PAGER"
        && bytes != b"PLINK_PROTOCOL"
        && !SESSION_IDENTITY
            .iter()
            .any(|candidate| name == OsStr::new(candidate))
        && !operational_routing(name)
        && !git_local_environment(name)
}

/// Whether one environment name carries only producer or command-routing state.
#[must_use]
pub fn operational_routing(name: &OsStr) -> bool {
    let bytes = name.as_encoded_bytes();
    bytes.starts_with(SHELL_HOOK_PREFIX)
        || OPERATIONAL_ROUTING
            .iter()
            .any(|candidate| name == OsStr::new(candidate))
}

/// Whether Git declares an environment entry local to one repository command.
///
/// The installed pre-commit rim removes this authority before configured child
/// execution. Direct compiler execution uses the same classifier so Git's
/// transient index, object, or command-line configuration cannot become an
/// implicit build input.
pub fn git_local_environment(name: &OsStr) -> bool {
    GIT_LOCAL_ENVIRONMENT
        .iter()
        .any(|candidate| name == OsStr::new(candidate))
        || name.to_str().is_some_and(|name| {
            name.starts_with("GIT_CONFIG_KEY_") || name.starts_with("GIT_CONFIG_VALUE_")
        })
}

#[cfg(test)]
fn set_environment_value(environment: &mut Vec<(OsString, OsString)>, name: &str, value: &Path) {
    match environment
        .iter_mut()
        .find(|(candidate, _)| candidate == OsStr::new(name))
    {
        Some((_, current)) => *current = value.as_os_str().to_owned(),
        None => environment.push((OsString::from(name), value.as_os_str().to_owned())),
    }
}

fn bind_implicit_home(environment: &mut Vec<(OsString, OsString)>, name: &str, suffix: &str) {
    if environment
        .iter()
        .any(|(candidate, _)| candidate == OsStr::new(name))
    {
        return;
    }
    let Some((_, home)) = environment
        .iter()
        .find(|(candidate, _)| candidate == OsStr::new(HOME))
    else {
        return;
    };
    environment.push((
        OsString::from(name),
        Path::new(home).join(suffix).into_os_string(),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_for_windows_shell_routing_matches_the_parent_environment() {
        let direct = compiler_environment_for_platform(
            [
                ("ComSpec", r"C:\WINDOWS\system32\cmd.exe"),
                ("PATH", r"C:\Program Files\Git\cmd;C:\Windows\System32"),
                ("TEMP", r"C:\spice\tmp"),
                ("TMP", r"C:\spice\tmp"),
                ("USERPROFILE", r"C:\Users\spice"),
            ]
            .map(|(name, value)| (OsString::from(name), OsString::from(value))),
            true,
        );
        let hook = compiler_environment_for_platform(
            [
                ("COMSPEC", r"C:\WINDOWS\system32\cmd.exe"),
                ("COLORTERM", "truecolor"),
                ("HOME", r"C:\Users\spice"),
                ("LC_CTYPE", "C.UTF-8"),
                ("MSYSTEM", "CLANGARM64"),
                ("PATH", r"C:\Program Files\Git\cmd;C:\Windows\System32"),
                ("TEMP", r"C:\spice\tmp"),
                ("TERM", "xterm-256color"),
                ("TMP", r"C:\spice\tmp"),
                ("TMPDIR", r"C:\spice\tmp"),
                ("USERPROFILE", r"C:\Users\spice"),
            ]
            .map(|(name, value)| (OsString::from(name), OsString::from(value))),
            true,
        );

        assert_eq!(direct, hook);
    }
}

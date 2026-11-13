//! systemd user-unit definition for one repository keeper.

use std::fmt::{self, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;

/// Prefix shared by every repository keeper unit.
const SERVICE_PREFIX: &str = "spice-keeper-";
/// Native unit suffix required by systemd.
const SERVICE_SUFFIX: &str = ".service";
/// Delay before systemd relaunches a keeper that itself failed.
const RESTART_DELAY: &str = "60s";

/// Inputs to one repository's complete systemd user-unit definition.
pub(crate) struct Definition<'a> {
    /// Exact installed appliance that runs the private worker.
    pub(crate) executable: &'a Path,
    /// Repository worktree in which the worker resolves its authority.
    pub(crate) working_directory: &'a Path,
    /// Git common directory named in the operator-facing description.
    pub(crate) repository: &'a Path,
}

/// Stable service name for one exact Git common directory.
pub(crate) fn service_name(common_dir: &Path) -> String {
    format!(
        "{SERVICE_PREFIX}{}{SERVICE_SUFFIX}",
        super::manager::repository_key(common_dir)
    )
}

/// A value one systemd unit line cannot carry byte for byte.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Unrepresentable {
    /// Unit directive that would hold the value.
    pub(crate) directive: &'static str,
    /// The refused value, lossily decoded for display.
    pub(crate) value: String,
    /// What makes the value inexpressible.
    pub(crate) reason: &'static str,
}

impl fmt::Display for Unrepresentable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}={:?} {}",
            self.directive, self.value, self.reason
        )
    }
}

/// Render the complete user unit without a shell or bearer material.
pub(crate) fn render(definition: &Definition<'_>) -> Result<String, Unrepresentable> {
    let description = scalar(
        "Description",
        &format!(
            "Spice lifecycle owner for the repository at {}",
            definition.repository.display()
        ),
    )?;
    let working_directory = working_directory(definition.working_directory)?;
    let mut unit = String::new();
    // Every write targets a String and therefore cannot fail.
    let _ = write!(
        unit,
        concat!(
            "[Unit]\n",
            "Description={}\n",
            "\n",
            "[Service]\n",
            "Type=simple\n",
            "ExecStart={} {}\n",
            "WorkingDirectory={}\n",
            "Restart=on-failure\n",
            "RestartSec={}\n",
            "\n",
            "[Install]\n",
            "WantedBy=default.target\n",
        ),
        description,
        exec_word(definition.executable),
        super::WORKER_ARGUMENT,
        working_directory,
        RESTART_DELAY,
    );
    Ok(unit)
}

/// Quote one systemd command word, escaping bytes and `%` specifiers so the
/// manager passes exactly one admitted path to the private worker.
fn exec_word(value: &Path) -> String {
    let mut quoted = String::with_capacity(value.as_os_str().len() + 2);
    quoted.push('"');
    for byte in value.as_os_str().as_bytes() {
        match *byte {
            b'%' => quoted.push_str("%%"),
            b'"' => quoted.push_str("\\\""),
            b'\\' => quoted.push_str("\\\\"),
            b' '..=b'~' => quoted.push(char::from(*byte)),
            byte => {
                let _ = write!(quoted, "\\x{byte:02x}");
            }
        }
    }
    quoted.push('"');
    quoted
}

/// Render the worktree for `WorkingDirectory=`, which systemd reads as an
/// absolute path after specifier expansion alone.
fn working_directory(path: &Path) -> Result<String, Unrepresentable> {
    let refuse = |reason| Unrepresentable {
        directive: "WorkingDirectory",
        value: path.to_string_lossy().into_owned(),
        reason,
    };
    let text = path.to_str().ok_or_else(|| refuse("is not UTF-8"))?;
    if !text.starts_with('/') {
        return Err(refuse("is not absolute"));
    }
    scalar("WorkingDirectory", text)
}

/// Render one scalar directive value. systemd takes it verbatim after
/// stripping surrounding whitespace and expanding `%` specifiers; it never
/// unescapes or unquotes it, and a trailing backslash continues the line.
fn scalar(directive: &'static str, value: &str) -> Result<String, Unrepresentable> {
    let refuse = |reason| Unrepresentable {
        directive,
        value: value.to_owned(),
        reason,
    };
    if value.contains(['\n', '\r']) {
        return Err(refuse("contains a line break"));
    }
    if value.starts_with([' ', '\t']) || value.ends_with([' ', '\t']) {
        return Err(refuse("has surrounding whitespace that systemd strips"));
    }
    if value.ends_with('\\') {
        return Err(refuse("ends with a backslash that continues the unit line"));
    }
    Ok(value.replace('%', "%%"))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    use super::*;
    use std::path::PathBuf;

    #[test]
    fn repository_units_are_stable_distinct_and_enable_under_default_target() {
        let repository = Path::new("/home/c/repo/.git");
        let name = service_name(repository);
        assert_eq!(name, service_name(repository));
        assert_ne!(name, service_name(Path::new("/home/c/other/.git")));
        assert!(name.starts_with(SERVICE_PREFIX));
        assert!(name.ends_with(SERVICE_SUFFIX));

        let unit = render(&Definition {
            executable: Path::new("/home/c/.local/bin/spice"),
            working_directory: Path::new("/home/c/repo"),
            repository,
        })
        .expect("an ordinary worktree renders");
        assert!(unit.contains("ExecStart=\"/home/c/.local/bin/spice\" --spice-keeper-worker\n"));
        assert!(unit.contains("WorkingDirectory=/home/c/repo\n"));
        assert!(unit.contains("Restart=on-failure\nRestartSec=60s\n"));
        assert!(unit.ends_with("[Install]\nWantedBy=default.target\n"));
        assert!(!unit.contains("auth-token"));
    }

    #[test]
    fn exec_words_preserve_paths_without_interpolation_or_a_shell() {
        let path = PathBuf::from(OsString::from_vec(
            b"/tmp/a % specifier \" quote \\ slash \n nonutf8-\xff".to_vec(),
        ));
        assert_eq!(
            exec_word(&path),
            "\"/tmp/a %% specifier \\\" quote \\\\ slash \\x0a nonutf8-\\xff\"",
        );
    }

    fn rendered(working_directory: &Path, repository: &Path) -> Result<String, Unrepresentable> {
        render(&Definition {
            executable: Path::new("/usr/bin/spice"),
            working_directory,
            repository,
        })
    }

    #[test]
    fn scalar_directives_carry_paths_verbatim_apart_from_specifiers() {
        let unit = rendered(
            Path::new("/home/c/My Work/100% \"done\" \\ repo"),
            Path::new("/home/c/My Work/.git"),
        )
        .expect("a path systemd can read verbatim renders");
        assert!(unit.contains("\nWorkingDirectory=/home/c/My Work/100%% \"done\" \\ repo\n"));
        assert!(unit.contains(
            "\nDescription=Spice lifecycle owner for the repository at /home/c/My Work/.git\n"
        ));
    }

    #[test]
    fn worktrees_a_unit_line_cannot_carry_are_refused_by_directive() {
        let repository = Path::new("/home/c/repo/.git");
        for (working_directory, reason) in [
            (b"/home/c/two\nlines".as_slice(), "contains a line break"),
            (
                b"/home/c/trailing ",
                "has surrounding whitespace that systemd strips",
            ),
            (
                b"/home/c/continued\\",
                "ends with a backslash that continues the unit line",
            ),
            (b"/home/c/nonutf8-\xff", "is not UTF-8"),
            (b"relative/repo", "is not absolute"),
        ] {
            let path = PathBuf::from(OsString::from_vec(working_directory.to_vec()));
            let refusal = rendered(&path, repository).expect_err("the worktree is refused");
            assert_eq!(refusal.directive, "WorkingDirectory");
            assert_eq!(refusal.reason, reason);
        }
        let refusal = rendered(
            Path::new("/home/c/repo"),
            Path::new("/home/c/two\nlines/.git"),
        )
        .expect_err("a repository path with a line break is refused");
        assert_eq!(refusal.directive, "Description");
    }
}

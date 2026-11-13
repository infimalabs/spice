//! Host paths shared by session pinning and the supervised process.

use super::*;

/// Only these captured names cross the occupant boundary. Windows session
/// roots are resolved separately so their native and Unix spellings agree.
#[cfg(not(windows))]
const ADMITTED_AMBIENT: [&str; 4] = ["HOME", "TMPDIR", "USER", "SSH_AUTH_SOCK"];
#[cfg(windows)]
const ADMITTED_AMBIENT: [&str; 9] = [
    "USER",
    "SSH_AUTH_SOCK",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "SystemRoot",
    "PATHEXT",
    "ComSpec",
];

pub(super) fn ambient(probes: &dyn LaunchProbes) -> CommandResult<Vec<(OsString, OsString)>> {
    let values: Vec<_> = ADMITTED_AMBIENT
        .into_iter()
        .filter_map(|key| probes.variable(key).map(|value| (key.into(), value)))
        .collect();
    #[cfg(windows)]
    {
        let home = session_home(probes)?;
        let temp = session_temp(probes)?;
        // Node's Windows os.homedir()/os.tmpdir() use USERPROFILE and TEMP,
        // not HOME/TMPDIR. Match even explicit Unix-style overrides.
        let mut values = values;
        values.extend([
            ("HOME".into(), home.clone().into_os_string()),
            ("USERPROFILE".into(), home.into_os_string()),
            ("TMPDIR".into(), temp.clone().into_os_string()),
            ("TEMP".into(), temp.clone().into_os_string()),
            ("TMP".into(), temp.into_os_string()),
        ]);
        Ok(values)
    }
    #[cfg(not(windows))]
    {
        Ok(values)
    }
}

pub(super) fn session_home(probes: &dyn LaunchProbes) -> CommandResult<PathBuf> {
    spice::ambient::session::home(|name| probes.variable(name))
        .map(PathBuf::from)
        .ok_or_else(|| {
            store_fault(
                "lane::session-home-unavailable",
                if cfg!(windows) {
                    "the supervised environment has no HOME, USERPROFILE, or complete HOMEDRIVE/HOMEPATH pair for oh-my-pi sessions"
                } else {
                    "the supervised environment has no HOME to keep oh-my-pi sessions beneath"
                },
            )
        })
}

pub(super) fn session_temp(probes: &dyn LaunchProbes) -> CommandResult<PathBuf> {
    spice::ambient::session::temp(|name| probes.variable(name))
        .map(PathBuf::from)
        .ok_or_else(|| {
            store_fault(
                "lane::session-temp-unavailable",
                "the supervised environment has no TMPDIR, TEMP, or TMP for oh-my-pi sessions",
            )
        })
}

pub(super) fn native_shell(probes: &dyn LaunchProbes, directory: &Path) -> CommandResult<OsString> {
    if let Some(shell) = probes
        .variable("SHELL")
        .filter(|value| !cfg!(windows) || !value.is_empty())
    {
        return Ok(shell);
    }
    #[cfg(windows)]
    {
        if let Some(shell) = probes.git(directory).and_then(|git| git_bash(&git)) {
            return Ok(shell);
        }
    }
    let _ = directory;
    Err(store_fault(
        "lane::native-shell-unavailable",
        if cfg!(windows) {
            "SHELL is absent and the admitted Git installation has no regular git.exe and adjacent bin/bash.exe"
        } else {
            "the supervised environment has no native SHELL for command execution"
        },
    ))
}

#[cfg(windows)]
fn git_bash(git: &Path) -> Option<OsString> {
    if !fs::symlink_metadata(git).ok()?.is_file() {
        return None;
    }
    let shell = git.parent()?.parent()?.join(r"bin\bash.exe");
    fs::symlink_metadata(&shell)
        .ok()?
        .is_file()
        .then(|| shell.into_os_string())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn git_bash_requires_both_regular_files_in_the_admitted_installation() {
        let fixture = super::super::test_support::LaunchFixture::new();
        let installation = fixture.root.join("Program Files").join("Git");
        let git = installation.join(r"cmd\git.exe");
        let bash = installation.join(r"bin\bash.exe");
        fs::create_dir_all(git.parent().unwrap()).unwrap();
        fs::create_dir_all(bash.parent().unwrap()).unwrap();
        fs::write(&git, b"git").unwrap();
        assert_eq!(git_bash(&git), None);
        fs::create_dir(&bash).unwrap();
        assert_eq!(git_bash(&git), None);
        fs::remove_dir(&bash).unwrap();
        fs::write(&bash, b"bash").unwrap();
        assert_eq!(git_bash(&git), Some(bash.into_os_string()));
        fs::remove_file(&git).unwrap();
        assert_eq!(git_bash(&git), None);
    }
}

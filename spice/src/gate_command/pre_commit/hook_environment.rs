//! Git-hook routing removed before gate authority is captured.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use spice::ambient::Ambient;

const GIT_EXEC_PATH: &str = "GIT_EXEC_PATH";
const GIT_LOCAL: [&str; 3] = ["GIT_DIR", GIT_EXEC_PATH, "GIT_PREFIX"];
const COMMAND_LINE_TOOLS: &str = "/Library/Developer/CommandLineTools";
const XCODE_DEVELOPER: &str = "/Applications/Xcode.app/Contents/Developer";

/// Remove Git's hook-only routing before any proof environment or child is
/// captured. Authored values survive unless they exactly match Git's defaults.
pub(super) fn normalize(ambient: &Ambient) -> Ambient {
    let git_exec_path = ambient.get(GIT_EXEC_PATH).map(PathBuf::from);
    let developer_root = git_exec_path.as_deref().and_then(developer_root_for_git);
    let normalized_path = git_exec_path.as_deref().and_then(|git_exec_path| {
        ambient
            .get("PATH")
            .map(|path| strip_leading_git_exec_path(path, git_exec_path))
    });

    Ambient::new(ambient.variables().iter().filter_map(|(name, value)| {
        if GIT_LOCAL.iter().any(|removed| same_name(name, removed))
            || developer_root.is_some_and(|root| injected_developer_value(name, value, root))
        {
            return None;
        }
        if same_name(name, "PATH") {
            return Some((
                name.clone(),
                normalized_path.clone().unwrap_or_else(|| value.clone()),
            ));
        }
        Some((name.clone(), value.clone()))
    }))
}

fn strip_leading_git_exec_path(path: &OsStr, git_exec_path: &Path) -> OsString {
    let entries = std::env::split_paths(path).collect::<Vec<_>>();
    let first_retained = entries
        .iter()
        .position(|entry| !same_git_path(entry, git_exec_path, cfg!(windows)))
        .unwrap_or(entries.len());
    if first_retained == 0 {
        return path.to_owned();
    }
    std::env::join_paths(&entries[first_retained..]).unwrap_or_else(|_| path.to_owned())
}

fn same_git_path(left: &Path, right: &Path, windows: bool) -> bool {
    if left == right {
        return true;
    }
    if !windows {
        return false;
    }
    let left = left.as_os_str().to_string_lossy();
    let right = right.as_os_str().to_string_lossy();
    windows_path_eq(&left, &right)
        || msys_drive_matches(&left, &right)
        || msys_drive_matches(&right, &left)
        || msys_distribution_spelling(&left)
            .is_some_and(|left| windows_path_ends_with(&right, left))
        || msys_distribution_spelling(&right)
            .is_some_and(|right| windows_path_ends_with(&left, right))
}

fn windows_path_eq(left: &str, right: &str) -> bool {
    left.len() == right.len()
        && left
            .bytes()
            .zip(right.bytes())
            .all(|(left, right)| path_byte_eq(left, right))
}

fn windows_path_ends_with(path: &str, suffix: &str) -> bool {
    path.len() >= suffix.len()
        && path
            .as_bytes()
            .get(path.len() - suffix.len()..)
            .is_some_and(|tail| {
                tail.iter()
                    .copied()
                    .zip(suffix.bytes())
                    .all(|(left, right)| path_byte_eq(left, right))
            })
}

fn path_byte_eq(left: u8, right: u8) -> bool {
    (matches!(left, b'/' | b'\\') && matches!(right, b'/' | b'\\'))
        || left.eq_ignore_ascii_case(&right)
}

fn msys_drive_matches(native: &str, msys: &str) -> bool {
    let native = native.as_bytes();
    let msys = msys.as_bytes();
    native.len() == msys.len()
        && native.len() >= 3
        && native[0].is_ascii_alphabetic()
        && native[1] == b':'
        && matches!(native[2], b'/' | b'\\')
        && msys[0] == b'/'
        && native[0].eq_ignore_ascii_case(&msys[1])
        && msys[2] == b'/'
        && native[3..]
            .iter()
            .copied()
            .zip(msys[3..].iter().copied())
            .all(|(left, right)| path_byte_eq(left, right))
}

fn msys_distribution_spelling(path: &str) -> Option<&str> {
    [b"/mingw64/".as_slice(), b"/usr/".as_slice()]
        .into_iter()
        .find_map(|boundary| {
            path.as_bytes()
                .windows(boundary.len())
                .position(|window| {
                    window
                        .iter()
                        .copied()
                        .zip(boundary.iter().copied())
                        .all(|(left, right)| path_byte_eq(left, right))
                })
                .map(|at| &path[at..])
        })
}

fn developer_root_for_git(git_exec_path: &Path) -> Option<&'static str> {
    let path = git_exec_path.to_str()?;
    [COMMAND_LINE_TOOLS, XCODE_DEVELOPER]
        .into_iter()
        .find(|root| under(path, root))
}

fn under(path: &str, root: &str) -> bool {
    path.strip_prefix(root)
        .is_some_and(|remainder| remainder.starts_with('/'))
}

fn injected_developer_value(name: &OsStr, value: &OsStr, root: &str) -> bool {
    let Some(value) = value.to_str() else {
        return false;
    };
    (same_name(name, "CPATH") && value == "/usr/local/include")
        || (same_name(name, "LIBRARY_PATH") && value == "/usr/local/lib")
        || (same_name(name, "SDKROOT") && under(value, root))
        || (same_name(name, "MANPATH") && value.contains(root))
}

#[cfg(windows)]
fn same_name(candidate: &OsStr, expected: &str) -> bool {
    candidate.eq_ignore_ascii_case(expected)
}

#[cfg(not(windows))]
fn same_name(candidate: &OsStr, expected: &str) -> bool {
    candidate == OsStr::new(expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_routing_is_removed_without_erasing_authored_inputs() {
        let root = COMMAND_LINE_TOOLS;
        let git_exec_path = format!("{root}/usr/libexec/git-core");
        let path = std::env::join_paths([
            Path::new(&git_exec_path),
            Path::new(&git_exec_path),
            Path::new("/authored/bin"),
        ])
        .expect("compose search path");
        let ambient = Ambient::new([
            (OsString::from("PATH"), path),
            (
                OsString::from(GIT_EXEC_PATH),
                OsString::from(&git_exec_path),
            ),
            (OsString::from("GIT_DIR"), OsString::from(".git")),
            (OsString::from("GIT_PREFIX"), OsString::from("nested/")),
            (
                OsString::from("CPATH"),
                OsString::from("/usr/local/include"),
            ),
            (
                OsString::from("LIBRARY_PATH"),
                OsString::from("/custom/lib"),
            ),
            (
                OsString::from("SDKROOT"),
                OsString::from(format!("{root}/SDKs/MacOSX.sdk")),
            ),
            (
                OsString::from("MANPATH"),
                OsString::from(format!("/custom/man:{root}/usr/share/man")),
            ),
            (OsString::from("AUTHORED"), OsString::from("kept")),
        ]);

        let normalized = normalize(&ambient);
        assert_eq!(
            normalized.get("PATH"),
            Some(
                std::env::join_paths([Path::new("/authored/bin")])
                    .unwrap()
                    .as_os_str()
            )
        );
        for removed in [
            GIT_EXEC_PATH,
            "GIT_DIR",
            "GIT_PREFIX",
            "CPATH",
            "SDKROOT",
            "MANPATH",
        ] {
            assert_eq!(normalized.get(removed), None, "{removed}");
        }
        assert_eq!(
            normalized.get("LIBRARY_PATH"),
            Some(OsStr::new("/custom/lib"))
        );
        assert_eq!(normalized.get("AUTHORED"), Some(OsStr::new("kept")));
    }

    #[test]
    fn native_and_msys_spellings_name_the_same_windows_git_directory() {
        let native = Path::new(r"C:\Program Files\Git\mingw64\libexec\git-core");
        for spelling in [
            Path::new("C:/Program Files/Git/mingw64/libexec/git-core"),
            Path::new("/c/Program Files/Git/mingw64/libexec/git-core"),
            Path::new("/mingw64/libexec/git-core"),
        ] {
            assert!(same_git_path(spelling, native, true), "{spelling:?}");
        }
        assert!(!same_git_path(Path::new("/authored/bin"), native, true));
    }

    #[test]
    fn nondefault_developer_values_and_unrelated_git_exec_paths_remain_inputs() {
        let ambient = Ambient::new([
            ("PATH", "/authored/bin"),
            (GIT_EXEC_PATH, "/custom/git-core"),
            ("CPATH", "/custom/include"),
            ("LIBRARY_PATH", "/custom/lib"),
            ("SDKROOT", "/custom/sdk"),
            ("MANPATH", "/custom/man"),
        ]);

        let normalized = normalize(&ambient);
        assert_eq!(normalized.get("PATH"), Some(OsStr::new("/authored/bin")));
        assert_eq!(normalized.get(GIT_EXEC_PATH), None);
        for retained in ["CPATH", "LIBRARY_PATH", "SDKROOT", "MANPATH"] {
            assert_eq!(
                normalized.get(retained),
                ambient.get(retained),
                "{retained}"
            );
        }
    }
}

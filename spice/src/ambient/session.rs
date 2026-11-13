//! Session paths resolved from the captured host environment, never a later
//! process-global read. The supervised vendor receives these same paths.

use std::ffi::OsString;

/// The explicit HOME, or the Windows profile location when HOME is absent.
#[must_use]
pub fn home(variable: impl Fn(&str) -> Option<OsString>) -> Option<OsString> {
    #[cfg(windows)]
    {
        let value = |name: &str| variable(name).filter(|value| !value.is_empty());
        value("HOME").or_else(|| value("USERPROFILE")).or_else(|| {
            let mut drive = value("HOMEDRIVE")?;
            drive.push(value("HOMEPATH")?);
            Some(drive)
        })
    }
    #[cfg(not(windows))]
    {
        variable("HOME")
    }
}

/// The session temp root: explicit TMPDIR, then Windows TEMP/TMP, or /tmp on
/// Unix. Windows refuses an absent temp location rather than using a Unix path.
#[must_use]
pub fn temp(variable: impl Fn(&str) -> Option<OsString>) -> Option<OsString> {
    #[cfg(windows)]
    {
        ["TMPDIR", "TEMP", "TMP"]
            .into_iter()
            .find_map(|name| variable(name).filter(|value| !value.is_empty()))
    }
    #[cfg(not(windows))]
    {
        Some(variable("TMPDIR").unwrap_or_else(|| "/tmp".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_session_paths_obey_explicit_then_native_precedence() {
        let values = [
            ("HOME", r"C:\explicit home"),
            ("USERPROFILE", r"C:\Users\operator"),
            ("HOMEDRIVE", "D:"),
            ("HOMEPATH", r"\profile"),
            ("TMPDIR", r"C:\explicit temp"),
            ("TEMP", r"C:\native temp"),
            ("TMP", r"C:\other temp"),
        ];
        let lookup = |omit: &[&str], name: &str| {
            values
                .iter()
                .find(|(key, _)| *key == name && !omit.contains(key))
                .map(|(_, value)| OsString::from(value))
        };
        assert_eq!(home(|name| lookup(&[], name)), Some(values[0].1.into()));
        assert_eq!(
            home(|name| lookup(&["HOME"], name)),
            Some(values[1].1.into())
        );
        assert_eq!(
            home(|name| lookup(&["HOME", "USERPROFILE"], name)),
            Some(r"D:\profile".into())
        );
        assert_eq!(
            home(|name| lookup(&["HOME", "USERPROFILE", "HOMEDRIVE"], name)),
            None
        );
        assert_eq!(temp(|name| lookup(&[], name)), Some(values[4].1.into()));
        assert_eq!(
            temp(|name| lookup(&["TMPDIR"], name)),
            Some(values[5].1.into())
        );
        assert_eq!(
            temp(|name| lookup(&["TMPDIR", "TEMP"], name)),
            Some(values[6].1.into())
        );
        assert_eq!(temp(|_| None), None);
        assert_eq!(home(|_| Some(OsString::new())), None);
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_session_paths_do_not_adopt_windows_variables() {
        let native = |name: &str| match name {
            "USERPROFILE" | "TEMP" | "TMP" => Some("/windows-only".into()),
            _ => None,
        };
        assert_eq!(home(native), None);
        assert_eq!(temp(native), Some("/tmp".into()));
        assert_eq!(home(|_| Some("/explicit".into())), Some("/explicit".into()));
        assert_eq!(temp(|_| Some("/explicit".into())), Some("/explicit".into()));
    }
}

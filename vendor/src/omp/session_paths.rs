//! OMP's directory spelling shared by session publication and discovery.

use std::path::Path;

/// Encode canonical paths as OMP does: relative to the temp root first, then
/// the home, otherwise an absolute path bracketed by `--`. Temp takes
/// precedence on every platform because OMP checks it first, which matters
/// wherever the temp root lies beneath the home.
#[must_use]
pub fn session_directory(home: Option<&Path>, temp: Option<&Path>, cwd: &Path) -> String {
    let roots = [("-tmp", temp), ("-", home)];
    for (prefix, root) in roots {
        if let Some(relative) = root.and_then(|root| cwd.strip_prefix(root).ok()) {
            let relative = dashed(&relative.to_string_lossy());
            let separator = if prefix.ends_with('-') { "" } else { "-" };
            return if relative.is_empty() {
                prefix.to_owned()
            } else {
                format!("{prefix}{separator}{relative}")
            };
        }
    }
    let absolute = cwd.to_string_lossy();
    // Rust canonicalization emits verbatim paths; Node's ordinary Windows
    // realpath spelling does not include this namespace marker in the slug.
    #[cfg(windows)]
    let absolute = absolute.strip_prefix(r"\\?\").unwrap_or(&absolute);
    #[cfg(windows)]
    if let Some(unc) = absolute.strip_prefix(r"UNC\") {
        return format!("---{}--", dashed(unc));
    }
    let absolute = absolute.strip_prefix(['/', '\\']).unwrap_or(&absolute);
    format!("--{}--", dashed(absolute))
}

fn dashed(path: &str) -> String {
    path.replace(['/', '\\', ':'], "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_temp_root_beneath_the_home_keeps_the_omp_temp_scope() {
        let home = Path::new("/home/operator");
        let temp = home.join("tmp");
        assert_eq!(
            session_directory(Some(home), Some(&temp), &temp.join("project")),
            "-tmp-project"
        );
        assert_eq!(session_directory(Some(home), Some(&temp), &temp), "-tmp");
        assert_eq!(session_directory(Some(home), Some(&temp), home), "-");
        assert_eq!(
            session_directory(Some(home), Some(&temp), Path::new("/srv/project")),
            "--srv-project--"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_absolute_sessions_exclude_the_verbatim_namespace_marker() {
        for cwd in [r"C:\work\project", r"\\?\C:\work\project"] {
            assert_eq!(
                session_directory(None, None, Path::new(cwd)),
                "--C--work-project--"
            );
        }
        for cwd in [r"\\server\share\project", r"\\?\UNC\server\share\project"] {
            assert_eq!(
                session_directory(None, None, Path::new(cwd)),
                "---server-share-project--"
            );
        }
    }
}

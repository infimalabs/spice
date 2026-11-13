//! The LaunchAgent definition of one repository's lifecycle owner.
//!
//! The agent lives in the repository user's own launchd GUI domain, so it runs
//! as that user with no stored password and everything it creates belongs to
//! them. launchd loads it at that user's login, the earliest boundary at which
//! the login session and per-user discovery exist, and logout ends it; no
//! service is promised while the user is logged out. The program is this
//! executable running as the keeper, which relaunches the resident itself, so
//! launchd restarts only a keeper that exits unsuccessfully.

use std::fmt::Write as _;
use std::path::Path;

use super::manager::xml_escape as escape;

/// Reverse-DNS prefix of every owner's launchd label.
pub(crate) const LABEL_PREFIX: &str = "dev.spice.keeper-";

/// What one owner's definition is rendered from.
pub(crate) struct Definition<'a> {
    /// The owner's launchd label.
    pub(crate) label: &'a str,
    /// The executable to run; the exact appliance installed at the time.
    pub(crate) executable: &'a Path,
    /// Directory the keeper resolves the repository from.
    pub(crate) working_directory: &'a Path,
}

/// The launchd label for the repository whose Git common directory is
/// `common_dir`: stable across installs of the same repository and distinct
/// between repositories.
pub(crate) fn label(common_dir: &Path) -> String {
    format!(
        "{LABEL_PREFIX}{}",
        super::manager::repository_key(common_dir)
    )
}

/// Render the definition as a launchd property list.
///
/// The agent loads only into the user's Aqua login session. It runs at load,
/// and launchd relaunches it only after an unsuccessful exit: the keeper exits
/// successfully once its registration is disabled or removed. Its process type
/// is interactive because the resident it keeps serves the user's app and lane
/// work, which background throttling would starve.
pub(crate) fn render(definition: &Definition<'_>) -> String {
    let mut plist = String::new();
    // Every push below writes to a String, which cannot fail.
    let _ = write!(
        plist,
        concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
            "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" ",
            "\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n",
            "<plist version=\"1.0\">\n<dict>\n",
            "\t<key>Label</key>\n\t<string>{label}</string>\n",
            "\t<key>ProgramArguments</key>\n\t<array>\n",
            "\t\t<string>{executable}</string>\n\t\t<string>{argument}</string>\n\t</array>\n",
            "\t<key>WorkingDirectory</key>\n\t<string>{working_directory}</string>\n",
            "\t<key>RunAtLoad</key>\n\t<true/>\n",
            "\t<key>KeepAlive</key>\n\t<dict>\n",
            "\t\t<key>SuccessfulExit</key>\n\t\t<false/>\n\t</dict>\n",
            "\t<key>LimitLoadToSessionType</key>\n\t<string>Aqua</string>\n",
            "\t<key>ProcessType</key>\n\t<string>Interactive</string>\n",
            "</dict>\n</plist>\n"
        ),
        label = escape(definition.label),
        executable = escape(&definition.executable.to_string_lossy()),
        argument = super::WORKER_ARGUMENT,
        working_directory = escape(&definition.working_directory.to_string_lossy()),
    );
    plist
}

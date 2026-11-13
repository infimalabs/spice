use super::test_support::{InertProbes, LaunchFixture};
use super::*;

struct Variables(Vec<(&'static str, OsString)>, Option<PathBuf>);

impl LaunchProbes for Variables {
    fn git(&self, _directory: &Path) -> Option<PathBuf> {
        self.1.clone()
    }
    fn advance(&self, repository: &Repository) -> String {
        InertProbes::default().advance(repository)
    }

    fn shadow(&self, repository: &Repository, base: Environment) -> CommandResult<Environment> {
        InertProbes::default().shadow(repository, base)
    }

    fn rtk(
        &self,
        executable: &std::ffi::OsStr,
        working: &Path,
        environment: &Environment,
    ) -> wire::body::RtkHealth {
        InertProbes::default().rtk(executable, working, environment)
    }

    fn cargo(
        &self,
        repository: &Repository,
        lane: CanonicalUuid,
    ) -> CommandResult<Vec<(OsString, OsString)>> {
        InertProbes::default().cargo(repository, lane)
    }

    fn session(&self, worktree: PathBuf) -> crate::session_command::Environment {
        crate::main_tail::session_environment_from(|name| self.variable(name), Some(worktree))
            .unglanced()
    }

    fn variable(&self, name: &str) -> Option<OsString> {
        self.0
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.clone())
    }
}

#[test]
fn windows_pin_and_launch_share_the_native_profile_and_temp() {
    let fixture = LaunchFixture::new();
    let repository = Repository::discover(&fixture.root).unwrap();
    let temp = fixture.root.join("native temp");
    fs::create_dir(&temp).unwrap();
    let explicit = fixture.root.join("explicit home");
    fs::create_dir(&explicit).unwrap();
    let git = fixture.root.join(r"Program Files\Git\cmd\git.exe");
    let bash = fixture.root.join(r"Program Files\Git\bin\bash.exe");
    fs::create_dir_all(git.parent().unwrap()).unwrap();
    fs::create_dir_all(bash.parent().unwrap()).unwrap();
    fs::write(&git, b"git").unwrap();
    fs::write(&bash, b"bash").unwrap();
    let mut probes = Variables(
        vec![
            ("USERPROFILE", fixture.root.clone().into_os_string()),
            ("TEMP", temp.clone().into_os_string()),
            ("SystemRoot", r"C:\Windows".into()),
            (
                "APPDATA",
                fixture.root.join(r"AppData\Roaming").into_os_string(),
            ),
            ("UNADMITTED_SECRET", "must not reach the occupant".into()),
        ],
        Some(git),
    );
    for home in [&fixture.root, &explicit] {
        if home == &explicit {
            probes.0.push(("HOME", home.clone().into_os_string()));
            probes
                .0
                .push(("SHELL", r"C:\explicit shell\bash.exe".into()));
        }
        pin_session(&repository, &probes).unwrap();
        let (environment, _, shell) = launch_environment(
            &repository,
            &fixture.root.join("spice.exe"),
            "rtk",
            fixture.lane,
            &probes,
        )
        .unwrap();
        let child = Variables(
            environment
                .entries()
                .iter()
                .filter_map(|(key, value)| {
                    ["HOME", "USERPROFILE", "TMPDIR", "TEMP", "TMP"]
                        .into_iter()
                        .find(|name| key == name)
                        .map(|name| (name, value.clone()))
                })
                .collect(),
            None,
        );
        assert_eq!(
            child.variable("USERPROFILE"),
            Some(home.clone().into_os_string())
        );
        assert_eq!(
            child.variable("TMPDIR"),
            Some(temp.clone().into_os_string())
        );
        assert_eq!(child.variable("TEMP"), Some(temp.clone().into_os_string()));
        assert_eq!(child.variable("TMP"), Some(temp.clone().into_os_string()));
        let value = |name: &str| {
            environment
                .entries()
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value)
        };
        assert_eq!(
            shell,
            probes
                .variable("SHELL")
                .unwrap_or_else(|| bash.clone().into_os_string())
        );
        assert!(value("SHELL").is_none());
        assert!(value("UNADMITTED_SECRET").is_none());
        assert_eq!(value("SystemRoot"), Some(&OsString::from(r"C:\Windows")));
    }
}

#[test]
fn windows_missing_profile_refuses_before_creating_a_session() {
    let fixture = LaunchFixture::new();
    let repository = Repository::discover(&fixture.root).unwrap();
    let probes = Variables(vec![("APPDATA", "private unrelated value".into())], None);
    let Err(Outcome::Fault(fault)) = pin_session(&repository, &probes) else {
        panic!("a missing profile must refuse session pinning");
    };
    assert_eq!(fault.reason, "lane::session-home-unavailable");
    assert!(!fault.detail.contains("private unrelated value"));
    assert!(!fixture.root.join(".omp").exists());
}

#[test]
fn windows_missing_posix_shell_does_not_select_comspec_or_disclose_environment() {
    let probes = Variables(
        vec![("ComSpec", r"C:\private location\cmd.exe".into())],
        None,
    );
    let Err(Outcome::Fault(fault)) = native_shell(&probes, Path::new(r"C:\work")) else {
        panic!("no supported shell must refuse");
    };
    assert_eq!(fault.reason, "lane::native-shell-unavailable");
    assert!(!fault.detail.contains("private location"));
}

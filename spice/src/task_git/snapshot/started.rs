use super::*;

/// Intent published before Git can install a delta or synthetic controls.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::task_git) struct Started {
    commit: String,
    head: String,
    configuration: String,
}

impl Started {
    pub(super) fn begin(root: Worktree<'_>, commit: &str, head: &str) -> Result<(), Error> {
        require_clean_or(
            root,
            "begin parked snapshot restoration",
            "commit or park claimant changes before restoring a different snapshot",
        )?;
        let replay = Self {
            commit: commit.to_owned(),
            head: head.to_owned(),
            configuration: configuration(root)?,
        };
        write_pending(
            &transaction_path(root)?,
            &Pending::SnapshotStarted { replay },
        )
    }

    pub(in crate::task_git) fn recover(
        &self,
        root: Worktree<'_>,
        marker: &Path,
    ) -> Result<(), Error> {
        let pending = Pending::SnapshotCleanup {
            cleanup: self.seal(root)?,
        };
        write_pending(marker, &pending)?;
        let Pending::SnapshotCleanup { cleanup } = pending else {
            unreachable!("constructed snapshot cleanup")
        };
        cleanup.finish(root, marker)
    }

    /// Reconstruct only the controls, never replay onto the claimant's surface.
    pub(in crate::task_git) fn seal(&self, root: Worktree<'_>) -> Result<Cleanup, Error> {
        self.verify_inputs(root)?;
        require_foreign_controls_absent(root)?;
        let current = Controls::read(root)?;
        if current == Controls::default() {
            return Err(snapshot_state_refusal(
                "interrupted snapshot has no returned-operation controls to corroborate",
            ));
        }
        let sandbox = Sandbox::new(root, &self.configuration)?;
        sandbox.checked(
            root,
            &["read-tree", "--reset", "-u", &self.head],
            "reconstruct snapshot starting tree",
        )?;
        fs::write(sandbox.git.join("HEAD"), format!("{}\n", self.head)).map_err(|error| {
            filesystem_fault("pin snapshot reconstruction HEAD", &sandbox.git, error)
        })?;
        let picked = sandbox.run(root, &["cherry-pick", "--no-commit", &self.commit])?;
        let conflicts = sandbox.checked(
            root,
            &["diff", "--name-only", "--diff-filter=U"],
            "inspect reconstructed snapshot conflicts",
        )?;
        if !picked.success() && conflicts.is_empty() {
            return Err(Error::fault(
                "git::snapshot-reconstruction-failed",
                picked.detail("corroborate interrupted snapshot"),
            ));
        }
        let expected = Controls::read(Worktree::new(&sandbox.path, &sandbox.ambient))?;
        if expected != current {
            return Err(snapshot_state_refusal(
                "interrupted snapshot controls do not match its pinned replay",
            ));
        }
        self.verify_inputs(root)?;
        Ok(Cleanup {
            commit: self.commit.clone(),
            head: self.head.clone(),
            controls: expected,
            failure: None,
        })
    }

    fn verify_inputs(&self, root: Worktree<'_>) -> Result<(), Error> {
        let head = required_read(
            root,
            &["rev-parse", "HEAD"],
            "check interrupted snapshot HEAD",
        )?;
        if head != self.head || configuration(root)? != self.configuration {
            return Err(snapshot_state_refusal(
                "interrupted snapshot HEAD or Git configuration changed",
            ));
        }
        Ok(())
    }
}

fn configuration(root: Worktree<'_>) -> Result<String, Error> {
    required_read(
        root,
        &["config", "--null", "--list"],
        "capture snapshot Git configuration",
    )
}

/// Recovery-only checkout with its own HEAD, index, controls and object writes.
/// The original repository is an object alternate, never this checkout's gitdir.
struct Sandbox {
    path: PathBuf,
    git: PathBuf,
    ambient: Ambient,
    options: Vec<String>,
}

impl Sandbox {
    fn new(root: Worktree<'_>, configuration: &str) -> Result<Self, Error> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = git_state_path(
            root,
            &format!(
                "spice-snapshot-recovery-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ),
        )?;
        fs::create_dir(&path)
            .map_err(|error| filesystem_fault("create snapshot reconstruction", &path, error))?;
        let mut sandbox = Self {
            git: path.join(".git"),
            path,
            ambient: Ambient::new(
                root.ambient
                    .variables()
                    .iter()
                    .filter(|(key, _)| !key.to_string_lossy().starts_with("GIT_"))
                    .cloned(),
            ),
            options: Vec::new(),
        };
        fs::write(sandbox.path.join("empty-config"), []).map_err(|error| {
            filesystem_fault("isolate snapshot configuration", &sandbox.path, error)
        })?;
        let format = required_read(
            root,
            &["rev-parse", "--show-object-format"],
            "read snapshot object format",
        )?;
        sandbox.checked(
            root,
            &["init", "--quiet", &format!("--object-format={format}")],
            "initialize snapshot reconstruction",
        )?;
        let objects = fs::canonicalize(git_state_path(root, "objects")?)
            .map_err(|error| filesystem_fault("resolve snapshot objects", &sandbox.path, error))?;
        let spelling = transaction::git_index_spelling(&objects, "git::snapshot-object-path")?;
        fs::write(
            sandbox.git.join("objects/info/alternates"),
            format!("{spelling}\n"),
        )
        .map_err(|error| filesystem_fault("attach snapshot source objects", &sandbox.git, error))?;
        if let Some(attributes) = read_control(root, "info/attributes")? {
            fs::write(sandbox.git.join("info/attributes"), attributes).map_err(|error| {
                filesystem_fault("copy snapshot attributes", &sandbox.git, error)
            })?;
        }
        for entry in configuration.split('\0').filter(|entry| !entry.is_empty()) {
            let (key, value) = entry
                .split_once('\n')
                .map_or((entry, None), |(key, value)| (key, Some(value)));
            // Includes have already been expanded at their source locations.
            // Repository layout belongs to the isolated checkout.
            if matches!(
                key,
                "core.repositoryformatversion" | "core.bare" | "core.worktree" | "include.path"
            ) || key.starts_with("extensions.")
                || key.starts_with("includeif.")
            {
                continue;
            }
            sandbox.options.push("-c".into());
            sandbox
                .options
                .push(value.map_or_else(|| key.to_owned(), |value| format!("{key}={value}")));
        }
        Ok(sandbox)
    }

    fn run(&self, root: Worktree<'_>, arguments: &[&str]) -> Result<GitOutput, Error> {
        let git = transaction::git_index_spelling(&self.git, "git::snapshot-recovery-path")?;
        let worktree = transaction::git_index_spelling(&self.path, "git::snapshot-recovery-path")?;
        let empty = self.path.join("empty-config");
        let empty = transaction::git_index_spelling(&empty, "git::snapshot-recovery-path")?;
        let mut args = self.options.iter().map(String::as_str).collect::<Vec<_>>();
        args.extend_from_slice(arguments);
        run_with_environment(
            Worktree::new(&root, &self.ambient),
            &args,
            &[
                ("GIT_DIR", git.as_ref()),
                ("GIT_WORK_TREE", worktree.as_ref()),
                ("GIT_CONFIG_GLOBAL", empty.as_ref()),
                ("GIT_CONFIG_NOSYSTEM", "1"),
            ],
        )
    }

    fn checked(&self, root: Worktree<'_>, args: &[&str], action: &str) -> Result<String, Error> {
        checked(self.run(root, args)?, action)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

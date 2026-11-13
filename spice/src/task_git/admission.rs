use super::*;

/// How long the repository's declared admission command may run.
///
/// This is the cheap load rather than the suite: a command that reads the
/// repository and answers whether it parses at all finishes in seconds. The
/// bound is slack for a cold read on a machine running every lane at once, not
/// time a healthy command spends. A command that needs longer was never the
/// cheap load, and failing here says so instead of stalling completion.
const ADMISSION_RUNTIME_LIMIT: Duration = Duration::from_secs(120);

/// How much of each admission stream a refusal preserves.
///
/// The operator needs the sentence naming what would not load. Whole build
/// transcripts are the suite's output, not this one's.
const ADMISSION_OUTPUT_LIMIT: usize = 64 * 1024;

const ADMISSION_OWNER: &str = "task-git-admission";

/// The cheap load one integrated tree must survive before it is published.
///
/// Completion merges the lane onto a baseline it never saw, so the tree that
/// reaches the remote is an artifact neither side authored. A three-way merge
/// resolving without conflict is not evidence that the result loads: two
/// manifests that each independently moved the same table merge cleanly into
/// one manifest carrying that table twice, and the first thing to read it is
/// whatever the next lane runs. This holds the declared command between the
/// synthesized tree and the push.
pub(super) struct Admission {
    lane_argv: Vec<String>,
    authored_tree: String,
}

impl Admission {
    /// Bind the lane's declared command and the tree it authored.
    ///
    /// A non-empty command comes from the lane's own checkout, so a baseline
    /// carrying a weaker command cannot relax it. An empty command is retained
    /// as the explicit signal to resolve policy from each integrated tree.
    pub(super) fn resolve(
        repository: &Repository,
        ambient: &Ambient,
        agent_head: &str,
    ) -> Result<Self, Error> {
        let resolved = crate::config::load_repository(repository, &[])
            .map_err(|error| Error::fault("git::admission-unreadable", error.to_string()))?;
        Ok(Self {
            lane_argv: resolved.root().policy().admission().to_vec(),
            authored_tree: tree_of(
                Worktree::new(repository.worktree_root(), ambient),
                agent_head,
            )?,
        })
    }

    /// Refuse one integrated commit whose tree the declared command rejects.
    pub(super) fn admit(
        &self,
        repository: &Repository,
        ambient: &Ambient,
        integrated: &str,
        label: &str,
    ) -> Result<(), Error> {
        let root = Worktree::new(repository.worktree_root(), ambient);
        // Bytes identical to the lane's own tree are the ones it already
        // worked, so a completion that merged nothing new is not charged for
        // the command. Every synthesized and every raced tree differs here,
        // which is exactly the set nobody has read.
        if tree_of(root, integrated)? == self.authored_tree {
            return Ok(());
        }
        let argv = self.selected_command(repository)?;
        if argv.is_empty() {
            return Ok(());
        }
        let command = render_command(&argv);
        let output = run_admission(root, &argv)?;
        if output.success() {
            return Ok(());
        }
        Err(Error::refusal(
            "git::admission-failed",
            output.detail(&command),
            format!(
                concat!(
                    "the integrated tree is checked out at HEAD and is not ",
                    "published; repair what `{command}` refused, commit, and ",
                    "retry task completion for {label}",
                ),
                command = command,
                label = label,
            ),
        ))
    }

    /// Select one command without letting an arriving baseline weaken the lane.
    ///
    /// A non-empty lane declaration remains fixed for the entire publication,
    /// including every race retry. An empty lane declaration defers to the
    /// currently materialized integrated tree, so a baseline that introduces
    /// admission cannot arrive together with an unreadable tree unchecked.
    fn selected_command(&self, repository: &Repository) -> Result<Vec<String>, Error> {
        if !self.lane_argv.is_empty() {
            return Ok(self.lane_argv.clone());
        }
        let resolved = crate::config::load_repository(repository, &[])
            .map_err(|error| Error::fault("git::admission-unreadable", error.to_string()))?;
        Ok(resolved.root().policy().admission().to_vec())
    }
}

fn render_command(argv: &[String]) -> String {
    argv.join(" ")
}

/// One completed admission command.
struct AdmissionOutput {
    status: i32,
    stdout: String,
    stderr: String,
}

impl AdmissionOutput {
    fn success(&self) -> bool {
        self.status == 0
    }

    fn detail(&self, command: &str) -> String {
        let output = [&self.stdout, &self.stderr]
            .into_iter()
            .map(|part| part.trim())
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        if output.is_empty() {
            format!("`{command}` exited {}", self.status)
        } else {
            format!("`{command}` exited {}\n{output}", self.status)
        }
    }
}

fn tree_of(root: Worktree<'_>, commit: &str) -> Result<String, Error> {
    required_read(
        root,
        &["rev-parse", &format!("{commit}^{{tree}}")],
        "read integrated tree identity",
    )
}

/// Run the declared command over the checkout at `root`.
fn run_admission(root: Worktree<'_>, argv: &[String]) -> Result<AdmissionOutput, Error> {
    let outcome = sill::spawn(admission_spec(root, argv)?)
        .map_err(|error| Error::fault("git::admission-launch-failed", error.to_string()))?
        .wait()
        .map_err(|error| Error::fault("git::admission-lost", error.to_string()))?;
    let status = match outcome.completion() {
        sill::Completion::Exited(status) => status.code().unwrap_or(-1),
        sill::Completion::TimedOut(_) => {
            return Err(Error::fault(
                "git::admission-timeout",
                format!(
                    "`{}` exceeded {} seconds",
                    argv.join(" "),
                    ADMISSION_RUNTIME_LIMIT.as_secs(),
                ),
            ));
        }
        sill::Completion::Cancelled(_) => {
            return Err(Error::fault(
                "git::admission-cancelled",
                format!("`{}` was cancelled", argv.join(" ")),
            ));
        }
    };
    Ok(AdmissionOutput {
        status,
        stdout: captured(outcome.stdout()),
        stderr: captured(outcome.stderr()),
    })
}

fn captured(output: Option<&sill::CapturedOutput>) -> String {
    output.map_or_else(String::new, |captured| {
        String::from_utf8_lossy(captured.bytes()).into_owned()
    })
}

fn admission_spec(root: Worktree<'_>, argv: &[String]) -> Result<sill::Spec, Error> {
    let (program, arguments) = argv
        .split_first()
        .ok_or_else(|| invalid_admission("command must name a program"))?;
    let capture = sill::StreamPolicy::Capture {
        limit: ADMISSION_OUTPUT_LIMIT,
    };
    Ok(sill::Spec {
        argv: sill::Argv::new(program.as_str(), arguments)
            .map_err(|error| invalid_admission(&error.to_string()))?,
        cwd: root.to_path_buf(),
        environment: root
            .ambient
            .environment()
            .map_err(|error| invalid_admission(&error.to_string()))?,
        group: sill::ProcessGroup::New,
        deadlines: sill::Deadlines::new(
            ADMISSION_RUNTIME_LIMIT,
            OUTPUT_DRAIN_LIMIT,
            PROCESS_GROUP_REAP_LIMIT,
        )
        .map_err(|error| invalid_admission(&error.to_string()))?,
        wait: sill::WaitPolicy::new(
            ADMISSION_OWNER,
            sill::Cancellation::Never,
            sill::RestartDisposition::Never,
        )
        .map_err(|error| invalid_admission(&error.to_string()))?,
        output: sill::Output::new(sill::StdinPolicy::Null, capture, capture),
        redaction: sill::Redaction::All,
        grace: PROCESS_GROUP_GRACE,
    })
}

fn invalid_admission(detail: &str) -> Error {
    Error::refusal(
        "git::admission-invalid",
        detail.to_owned(),
        "declare policy.admission as one runnable command and its arguments",
    )
}

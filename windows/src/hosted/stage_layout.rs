use std::path::{Path, PathBuf};

pub(super) const RESULT_PATH: &str = "result.json";
pub(super) const STAGE_DIRECTORY: &str = "stage";
pub(super) const STAGE_LOG_PATH: &str = "stage.log";
pub(super) const SMOKE_LOG_PATH: &str = "smoke.log";
pub(super) const SMOKE_DIRECTORY: &str = "smoke";
pub(super) const RUNNER_PATH: &str = "runner/windows-hosted.exe";

/// The one relative layout published by every native hosted stage.
pub(super) struct StageLayout {
    root: PathBuf,
}

impl StageLayout {
    pub(super) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    pub(super) fn result(&self) -> PathBuf {
        self.root.join(RESULT_PATH)
    }

    pub(super) fn stage(&self) -> PathBuf {
        self.root.join(STAGE_DIRECTORY)
    }

    pub(super) fn stage_log(&self) -> PathBuf {
        self.root.join(STAGE_LOG_PATH)
    }

    pub(super) fn smoke_log(&self) -> PathBuf {
        self.root.join(SMOKE_LOG_PATH)
    }

    pub(super) fn smoke(&self) -> PathBuf {
        self.root.join(SMOKE_DIRECTORY)
    }

    pub(super) fn runner(&self) -> PathBuf {
        self.root.join(RUNNER_PATH)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{RESULT_PATH, RUNNER_PATH, SMOKE_LOG_PATH, STAGE_LOG_PATH};

    const SUBJECT_ROOT: &str = "${{ runner.temp }}/result/";
    const STAGE_PAYLOAD: &str = "stage/targets/native/payload.json";
    const STAGE_RECEIPT: &str = "stage/proofs/source/native/receipt.json";
    const SMOKE_PAYLOAD: &str = "smoke/targets/native/payload.json";
    const LINUX_WORKFLOW: &str = include_str!("../../../.github/workflows/linux.yml");
    const MACOS_WORKFLOW: &str = include_str!("../../../.github/workflows/macos.yml");
    const WINDOWS_WORKFLOW: &str = include_str!("../../../.github/workflows/windows.yml");

    #[test]
    fn native_workflows_attest_the_admitted_stage_layout() {
        let contracts = [
            (
                "Linux",
                LINUX_WORKFLOW,
                &[
                    RESULT_PATH,
                    STAGE_PAYLOAD,
                    STAGE_RECEIPT,
                    STAGE_LOG_PATH,
                    SMOKE_LOG_PATH,
                    SMOKE_PAYLOAD,
                ][..],
            ),
            (
                "macOS",
                MACOS_WORKFLOW,
                &[RESULT_PATH, STAGE_PAYLOAD, STAGE_RECEIPT][..],
            ),
            (
                "Windows",
                WINDOWS_WORKFLOW,
                &[RESULT_PATH, STAGE_PAYLOAD, STAGE_RECEIPT, RUNNER_PATH][..],
            ),
        ];

        for (platform, workflow, admitted_paths) in contracts {
            let observed = relative_subjects(workflow);
            assert_eq!(
                observed,
                BTreeSet::from(["**".to_owned()]),
                "{platform} must attest its complete artifact root exactly once"
            );
            for admitted in admitted_paths {
                assert!(
                    observed.iter().any(|subject| matches(subject, admitted)),
                    "{platform} does not attest admitted path {admitted}"
                );
            }
        }
    }

    fn relative_subjects(workflow: &str) -> BTreeSet<String> {
        subject_paths(workflow)
            .into_iter()
            .map(|path| path.replace('\\', "/"))
            .map(|path| {
                path.strip_prefix(SUBJECT_ROOT)
                    .unwrap_or_else(|| {
                        panic!("attestation subject is outside {SUBJECT_ROOT}: {path}")
                    })
                    .to_owned()
            })
            .collect()
    }

    fn subject_paths(workflow: &str) -> Vec<String> {
        let mut lines = workflow.lines().peekable();
        let mut subjects = Vec::new();
        while let Some(line) = lines.next() {
            let trimmed = line.trim_start();
            let Some(value) = trimmed.strip_prefix("subject-path:") else {
                continue;
            };
            let indent = line.len() - trimmed.len();
            let value = value.trim();
            if value != "|" {
                subjects.push(value.to_owned());
                continue;
            }
            while let Some(line) = lines.peek().copied() {
                if line.trim().is_empty() {
                    lines.next();
                    continue;
                }
                let nested = line.len() - line.trim_start().len();
                if nested <= indent {
                    break;
                }
                subjects.push(line.trim().to_owned());
                lines.next();
            }
        }
        subjects
    }

    fn matches(subject: &str, path: &str) -> bool {
        subject == "**"
            || subject == path
            || subject.strip_suffix("/**").is_some_and(|prefix| {
                path.strip_prefix(prefix)
                    .is_some_and(|suffix| suffix.starts_with('/'))
            })
    }
}

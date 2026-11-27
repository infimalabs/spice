//! A completed native workflow's attested declaration of the exact root it
//! submitted to provider attestation.
//!
//! The declaration separates a producer that omitted a required record, which
//! only a fresh run of the current workflow can repair, from a record that is
//! declared but not yet verifiable, which stays attached and resumable.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use super::{HostedRun, WindowsHostedError, admit_file_set, verify_attestation};
use crate::hosted::stage_layout::{NativeStageContract, SUBJECTS_PATH, StageLayout};

/// Every file a completed producer declared it attested beside its inventory.
#[derive(Debug)]
pub(super) struct DeclaredSubjects {
    root: PathBuf,
    subjects: BTreeSet<PathBuf>,
}

impl DeclaredSubjects {
    /// Verify the inventory's own attestation, then decode its declaration.
    pub(super) fn load(
        run: &HostedRun<'_>,
        workflow: &str,
        run_id: u64,
        layout: &StageLayout,
    ) -> Result<Self, WindowsHostedError> {
        let path = layout.subjects();
        verify_attestation(run, workflow, run_id, &path)?;
        let bytes = fs::read(&path).map_err(|source| WindowsHostedError::Io {
            path: path.clone(),
            source,
        })?;
        Self::decode(layout.root().to_path_buf(), &bytes)
    }

    fn decode(root: PathBuf, bytes: &[u8]) -> Result<Self, WindowsHostedError> {
        let declared: Vec<String> = serde_json::from_slice(bytes).map_err(|error| {
            WindowsHostedError::OutputContract(format!("{SUBJECTS_PATH} does not decode: {error}"))
        })?;
        let mut subjects = BTreeSet::new();
        for subject in declared {
            let path = PathBuf::from(&subject);
            let canonical = !subject.is_empty()
                && !subject.contains('\\')
                && subject.split('/').all(|part| !part.is_empty())
                && path
                    .components()
                    .all(|component| matches!(component, Component::Normal(_)))
                && path != Path::new(SUBJECTS_PATH);
            if !canonical || !subjects.insert(path) {
                return Err(WindowsHostedError::OutputContract(format!(
                    "{SUBJECTS_PATH} declares invalid or repeated subject {subject:?}"
                )));
            }
        }
        Ok(Self { root, subjects })
    }

    /// Require the producer to have declared every path in `required`.
    pub(super) fn require<P: AsRef<Path>>(&self, required: &[P]) -> Result<(), WindowsHostedError> {
        let missing = required
            .iter()
            .map(AsRef::as_ref)
            .filter(|path| !self.subjects.contains(*path))
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(WindowsHostedError::OutputContract(format!(
                "completed producer omitted required attestation subjects: {}",
                missing.join(", ")
            )))
        }
    }

    /// Require the target's production result, payload, object, proof, smoke,
    /// and runner records before interpreting any producer result.
    pub(super) fn require_contract(
        &self,
        contract: NativeStageContract,
    ) -> Result<(), WindowsHostedError> {
        contract
            .require(&self.subjects)
            .map_err(|error| WindowsHostedError::OutputContract(error.to_string()))
    }

    /// Require the declaration to be exactly `required`, then the downloaded
    /// root to hold exactly the declared files and the inventory itself.
    pub(super) fn admit_exact(&self, required: &[PathBuf]) -> Result<(), WindowsHostedError> {
        self.require(required)?;
        let required = required
            .iter()
            .map(PathBuf::as_path)
            .collect::<BTreeSet<_>>();
        let extra = self
            .subjects
            .iter()
            .filter(|path| !required.contains(path.as_path()))
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>();
        if !extra.is_empty() {
            return Err(WindowsHostedError::OutputContract(format!(
                "completed producer declared unrequired attestation subjects: {}",
                extra.join(", ")
            )));
        }
        let mut files = self.subjects.iter().cloned().collect::<Vec<_>>();
        files.push(PathBuf::from(SUBJECTS_PATH));
        admit_file_set(&self.root, files)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::DeclaredSubjects;
    use crate::hosted::client::WindowsHostedError;
    use crate::hosted::stage_layout::SUBJECTS_PATH;

    struct Root(PathBuf);

    impl Drop for Root {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn root(files: &[&str]) -> Root {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "hosted-subjects-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("fixture root");
        for file in files {
            let path = root.join(file);
            fs::create_dir_all(path.parent().expect("fixture parent")).expect("fixture parent");
            fs::write(path, b"x").expect("fixture file");
        }
        Root(root)
    }

    fn declared(root: &Root, json: &str) -> Result<DeclaredSubjects, WindowsHostedError> {
        DeclaredSubjects::decode(root.0.clone(), json.as_bytes())
    }

    fn contract(result: Result<(), WindowsHostedError>) -> String {
        match result {
            Err(WindowsHostedError::OutputContract(detail)) => detail,
            other => panic!("expected an output-contract failure: {other:?}"),
        }
    }

    /// A producer whose declaration omits a required record fails its output
    /// contract before any per-file verification, even when the file itself
    /// reached the download.
    #[test]
    fn an_omitted_required_subject_is_an_output_contract_failure() {
        let fixture = root(&["result.json", "stage/payload.json", SUBJECTS_PATH]);
        let subjects = declared(&fixture, r#"["result.json"]"#).expect("declaration decodes");
        let required = [
            PathBuf::from("result.json"),
            PathBuf::from("stage/payload.json"),
        ];
        assert!(
            contract(subjects.admit_exact(&required)).contains("stage/payload.json"),
            "the failure names the omitted subject"
        );
        assert!(subjects.require(&["result.json"]).is_ok());
    }

    /// The declaration must be exactly the required set, and the downloaded
    /// root exactly that declaration; a download that differs from an exact
    /// declaration is a refusal, not a retryable producer failure.
    #[test]
    fn the_declaration_admits_only_its_exact_required_root() {
        let required = [
            PathBuf::from("result.json"),
            PathBuf::from("stage/payload.json"),
        ];
        let exact = root(&["result.json", "stage/payload.json", SUBJECTS_PATH]);
        declared(&exact, r#"["stage/payload.json","result.json"]"#)
            .expect("declaration decodes")
            .admit_exact(&required)
            .expect("an exact declaration and root admit");

        let extra = declared(
            &exact,
            r#"["result.json","stage/payload.json","stage/extra"]"#,
        )
        .expect("declaration decodes");
        assert!(contract(extra.admit_exact(&required)).contains("stage/extra"));

        let stray = root(&["result.json", "stage/payload.json", "stray", SUBJECTS_PATH]);
        let refused = declared(&stray, r#"["result.json","stage/payload.json"]"#)
            .expect("declaration decodes")
            .admit_exact(&required);
        assert!(matches!(
            refused,
            Err(WindowsHostedError::ProviderResponse(_))
        ));
    }

    #[test]
    fn a_noncanonical_declaration_is_an_output_contract_failure() {
        let fixture = root(&[]);
        for json in [
            "{}",
            r#"["result.json","result.json"]"#,
            r#"["../result.json"]"#,
            r#"["/result.json"]"#,
            r#"["stage\\payload.json"]"#,
            r#"["stage//payload.json"]"#,
            r#"[""]"#,
            r#"["subjects.json"]"#,
        ] {
            assert!(
                matches!(
                    declared(&fixture, json),
                    Err(WindowsHostedError::OutputContract(_))
                ),
                "{json} must refuse"
            );
        }
    }
}

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs;

use super::*;

#[test]
fn missing_identity_refuses_before_credentials_or_mutation() {
    let fixture = Fixture::new();
    let runner = ScriptedRunner::new([successful("0 valid identities found\n", "")]);

    let error =
        share_with(&fixture.request(), &mut Vec::new(), &runner).expect_err("identity must refuse");

    assert!(error.to_string().contains("security find-identity"));
    assert_eq!(runner.labels(), vec!["security"]);
}

#[test]
fn missing_profile_names_setup_and_refuses_before_signing() {
    let fixture = Fixture::new();
    let identity = fixture.identity_line();
    let runner = ScriptedRunner::new([
        successful(&identity, ""),
        failed("", "No Keychain password item found for profile"),
    ]);

    let error =
        share_with(&fixture.request(), &mut Vec::new(), &runner).expect_err("profile must refuse");

    assert!(
        error
            .to_string()
            .contains("xcrun notarytool store-credentials release")
    );
    assert_eq!(runner.labels(), vec!["security", "xcrun"]);
}

#[test]
fn accepted_submission_signs_notarizes_and_verifies_in_order() {
    let fixture = Fixture::new();
    let identity = fixture.identity_line();
    let runner = ScriptedRunner::new([
        successful(&identity, ""),
        successful("{}", ""),
        successful("", ""),
        successful("", "valid on disk"),
        successful("", ""),
        successful(
            r#"{"id":"submission-1","status":"Accepted","message":"ok"}"#,
            "",
        ),
        successful("", "valid on disk"),
        successful("", "accepted"),
    ]);
    let mut rendered = Vec::new();

    share_with(&fixture.request(), &mut rendered, &runner).expect("sharing succeeds");

    assert_eq!(
        runner.labels(),
        vec![
            "security", "xcrun", "codesign", "codesign", "ditto", "xcrun", "codesign", "spctl"
        ]
    );
    let commands = runner.commands.borrow();
    assert!(
        commands[2]
            .1
            .windows(2)
            .any(|pair| pair == ["--options", "runtime"])
    );
    assert!(
        commands[2]
            .1
            .iter()
            .any(|argument| argument == "--timestamp")
    );
    assert!(commands[5].1.iter().any(|argument| argument == "--wait"));
    let rendered = String::from_utf8(rendered).expect("UTF-8 report");
    assert!(rendered.contains("submission: submission-1"));
    assert!(rendered.contains("online Gatekeeper assessment accepted"));
}

#[test]
fn rejected_submission_fetches_and_prints_apples_log() {
    let fixture = Fixture::new();
    let identity = fixture.identity_line();
    let runner = ScriptedRunner::new([
        successful(&identity, ""),
        successful("{}", ""),
        successful("", ""),
        successful("", "valid on disk"),
        successful("", ""),
        failed(
            r#"{"id":"submission-2","status":"Invalid","message":"rejected"}"#,
            "",
        ),
        successful(r#"{"issues":[{"message":"unsigned nested code"}]}"#, ""),
    ]);

    let error =
        share_with(&fixture.request(), &mut Vec::new(), &runner).expect_err("rejection refuses");
    let message = error.to_string();

    assert!(message.contains("submission-2"));
    assert!(message.contains("unsigned nested code"));
    assert_eq!(runner.labels().last().map(String::as_str), Some("xcrun"));
    assert!(
        runner.commands.borrow()[6]
            .1
            .windows(2)
            .any(|pair| pair == ["log", "submission-2"])
    );
}

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "spice-share-darwin-test-{}-{}",
            std::process::id(),
            TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("fixture directory");
        fs::write(root.join("spice"), [0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 0])
            .expect("Mach-O fixture");
        Self { root }
    }

    fn request(&self) -> ShareDarwinRequest {
        ShareDarwinRequest::new(
            self.root.join("spice"),
            DarwinSigning::new(
                dist::DeveloperIdentity::new("Developer ID Application: Example (TEAMID)")
                    .expect("fixture identity"),
                dist::NotaryProfile::new("release").expect("fixture profile"),
            ),
        )
    }

    fn identity_line(&self) -> String {
        format!(
            "  1) ABCDEF \"{}\"\n     1 valid identities found\n",
            self.request().signing.identity().as_str()
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct ScriptedRunner {
    answers: RefCell<VecDeque<ProcessOutput>>,
    commands: RefCell<Vec<(String, Vec<String>)>>,
}

impl ScriptedRunner {
    fn new<const N: usize>(answers: [ProcessOutput; N]) -> Self {
        Self {
            answers: RefCell::new(answers.into()),
            commands: RefCell::new(Vec::new()),
        }
    }

    fn labels(&self) -> Vec<String> {
        self.commands
            .borrow()
            .iter()
            .map(|(program, _)| program.clone())
            .collect()
    }
}

impl Runner for ScriptedRunner {
    fn run(
        &self,
        _label: &'static str,
        program: &Path,
        arguments: &[OsString],
    ) -> Result<ProcessOutput, ShareDarwinError> {
        self.commands.borrow_mut().push((
            program
                .file_name()
                .expect("program file name")
                .to_string_lossy()
                .into_owned(),
            arguments
                .iter()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect(),
        ));
        Ok(self
            .answers
            .borrow_mut()
            .pop_front()
            .expect("scripted command answer"))
    }
}

fn successful(stdout: &str, stderr: &str) -> ProcessOutput {
    ProcessOutput {
        success: true,
        code: Some(0),
        stdout: stdout.as_bytes().to_vec(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

fn failed(stdout: &str, stderr: &str) -> ProcessOutput {
    ProcessOutput {
        success: false,
        code: Some(1),
        stdout: stdout.as_bytes().to_vec(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

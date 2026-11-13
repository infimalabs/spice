use super::{Fixture, admitted, column};
use crate::GateId;
use crate::toolchain::{
    ArmFinding, ArmOutcome, ENV_NAME_LITERAL_MARKER, EnvironmentLedgerKind, FindingDetail,
    PACKAGED_ENV_PROFILE, SourceLocus, TOOLCHAIN_ARMS,
};

/// The Python file carrying one waived access and three unwaived lines.
const SETTINGS: &str = "app/settings.py";

/// The second Python file reading the same environment name.
const WORKER: &str = "app/worker.py";

/// The environment name only the waived access reads.
///
/// Both fixture names match the packaged `SPICE_*` watchlist, which is how
/// the ledger arm finds a name at all, yet name nothing Spice reads, so each
/// carries the ledger's literal escape rather than a manifest entry.
const WAIVED_NAME: &str = concat!("SPICE", "_WAIVED_FIXTURE"); // env-policy: not-an-env SPICE_WAIVED_FIXTURE

/// The environment name both fixture files read without a waiver.
const SHARED_NAME: &str = concat!("SPICE", "_SHARED_FIXTURE"); // env-policy: not-an-env SPICE_SHARED_FIXTURE

/// The exact unwaived read both fixture files spell.
const READ: &str = concat!("cache = os.getenv(\"SPICE", "_SHARED_FIXTURE\")");

/// One line reading a watchlisted name beside a name nobody wrote down.
///
/// Neither extractor sees this line whole: the watchlist names the first read
/// and the access gate is the only thing that catches the second.
const MIXED: &str = concat!(
    "flag = os.getenv(\"SPICE",
    "_SHARED_FIXTURE\") or os.environ[key]"
);

/// One line committing two reads under names nobody wrote down.
///
/// Both accesses report under the same family idiom on the same line of the
/// same file, so the column is the only thing that keeps the second from
/// arriving as a duplicate of the first and being refused.
const DYNAMIC: &str = "both = os.environ[first] or os.getenv(second)";

/// Python reading the environment, one access waived and one name read twice.
fn environment_fixture(label: &str) -> Fixture {
    let mut fixture = Fixture::new(label);
    let marker = PACKAGED_ENV_PROFILE.allow_marker;
    let waived = format!("home = os.getenv(\"{WAIVED_NAME}\")  # {marker}");
    fixture.write(
        SETTINGS,
        &format!("import os\n\n{waived}\n{READ}\n{MIXED}\n{DYNAMIC}\n"),
    );
    fixture.write(WORKER, &format!("import os\n\n{READ}\n"));
    fixture
}

fn names(declared: &[&str]) -> Vec<String> {
    declared.iter().map(|name| (*name).to_owned()).collect()
}

/// The stable finding name the profile gives a dynamic Python access.
fn python_idiom() -> String {
    PACKAGED_ENV_PROFILE
        .finding_names
        .iter()
        .find(|(family, _)| *family == "python")
        .map(|(_, name)| (*name).to_owned())
        .expect("the packaged profile names the python access family")
}

/// Every unwaived access is one finding, and the waived one is none.
///
/// The waiver is the whole point of the arm: an access is admitted by being
/// written down, so an arm that reported the annotated read too would make the
/// inventory unreadable and an arm that reported neither would make it empty.
/// Past the waiver this holds the arm to one finding per access rather than per
/// line, over a line whose two reads only one extractor each can name and a
/// line whose two reads share a name and differ by column alone. Admission
/// refuses findings that agree in every field, so the second of those two is
/// retained only because the column tells it apart.
#[test]
fn the_environment_policy_arm_reports_only_the_unwaived_access() {
    let fixture = environment_fixture("gate-env-policy");
    let declared = names(&[WAIVED_NAME, SHARED_NAME]);
    let measurement = admitted(&fixture.source(&[SETTINGS]), GateId::EnvPolicy, &declared);

    assert_eq!(
        measurement.outcome,
        ArmOutcome::Findings,
        "{}",
        measurement.reason
    );
    let idiom = python_idiom();
    let access = |line: u32, source: &str, needle: &str, name: &str| ArmFinding {
        path: Some(SETTINGS.to_owned()),
        line: Some(line),
        column: Some(column(source, needle)),
        detail: FindingDetail::EnvironmentAccess {
            name: name.to_owned(),
            source: source.to_owned(),
        },
    };
    assert_eq!(
        measurement.findings,
        [
            access(4, READ, SHARED_NAME, SHARED_NAME),
            access(5, MIXED, SHARED_NAME, SHARED_NAME),
            access(5, MIXED, "os.environ", &idiom),
            access(6, DYNAMIC, "os.environ", &idiom),
            access(6, DYNAMIC, "os.getenv", &idiom),
        ]
    );
}

/// A name the profile never declared is unaccounted at every locus reading it.
///
/// The ledger is repository-wide, so it carries loci instead of a locus, and
/// dropping either one would understate where the environment shapes behavior.
/// Nothing was selected as changed here, which is also what proves the policy
/// arm reaches `NotApplicable` only through its own declared predicate.
#[test]
fn the_environment_ledger_arm_accounts_an_unaccounted_name_at_every_locus() {
    let fixture = environment_fixture("gate-env-ledger");
    let declared = names(&[WAIVED_NAME]);
    let source = fixture.source(&[]);

    let ledger = admitted(&source, GateId::EnvNameLedger, &declared);
    assert_eq!(ledger.outcome, ArmOutcome::Findings, "{}", ledger.reason);
    let accounted = ledger
        .findings
        .iter()
        .find(|finding| match &finding.detail {
            FindingDetail::EnvironmentLedger { name, .. } => name == SHARED_NAME,
            _ => false,
        })
        .unwrap_or_else(|| panic!("the ledger never accounted {SHARED_NAME}"));
    assert_eq!(
        (accounted.path.as_deref(), accounted.line, accounted.column),
        (None, None, None),
        "a ledger finding is repository-wide and carries no locus of its own"
    );
    let locus = |path: &str, line| SourceLocus {
        path: path.to_owned(),
        line,
    };
    assert_eq!(
        accounted.detail,
        FindingDetail::EnvironmentLedger {
            mismatch: EnvironmentLedgerKind::Unaccounted,
            name: SHARED_NAME.to_owned(),
            references: vec![locus(SETTINGS, 4), locus(SETTINGS, 5), locus(WORKER, 3)],
        }
    );

    let policy = admitted(&source, GateId::EnvPolicy, &declared);
    assert_eq!(policy.outcome, ArmOutcome::NotApplicable);
    let applicability = TOOLCHAIN_ARMS
        .into_iter()
        .find(|declaration| declaration.id == GateId::EnvPolicy)
        .expect("env-policy is declared")
        .jurisdiction
        .applicability;
    assert!(
        policy.reason.contains(applicability),
        "the skip named something other than the declared predicate: {}",
        policy.reason
    );
}

/// One exact marker admits a non-environment fixture but not a real access.
///
/// Both names match the broad `SPICE_*` watchlist. The Rust test literal names
/// itself after the marker, while the Python line is an actual access and stays
/// unaccounted. The refusal names both the arm's literal escape and the marker
/// the installed pre-commit hook reads, so an author is never sent to a
/// spelling the hook then refuses on the same lines.
#[test]
fn the_ledger_refusal_names_the_literal_escape_without_hiding_an_access() {
    // These become exact literals only inside the isolated fixture written
    // below; this source must not itself declare environment names.
    const FIXTURE_NAME: &str = concat!("SPICE", "_123"); // env-policy: not-an-env SPICE_123
    const ACCESS_NAME: &str = {
        // env-policy: not-an-env SPICE_UNDECLARED
        concat!("SPICE", "_UNDECLARED")
    };
    let mut fixture = environment_fixture("gate-env-literal-marker");
    fixture.write(
        "tests/task_handle.rs",
        &format!(
            concat!(
                "const INVALID: &str = \"{fixture}\"; // ",
                "{marker} {fixture}\n"
            ),
            fixture = FIXTURE_NAME,
            marker = ENV_NAME_LITERAL_MARKER
        ),
    );
    fixture.write(
        "app/undeclared.py",
        &format!("import os\n\nvalue = os.getenv(\"{ACCESS_NAME}\")\n"),
    );

    let measurement = admitted(
        &fixture.source(&[]),
        GateId::EnvNameLedger,
        &names(&[WAIVED_NAME, SHARED_NAME]),
    );
    assert_eq!(measurement.outcome, ArmOutcome::Findings);
    assert!(
        measurement
            .reason
            .contains(&format!("`{ENV_NAME_LITERAL_MARKER} NAME`")),
        "the refusal did not name the literal escape: {}",
        measurement.reason
    );
    assert!(
        measurement
            .reason
            .contains(PACKAGED_ENV_PROFILE.allow_marker),
        "the refusal did not name the marker the commit hook reads: {}",
        measurement.reason
    );
    let named: Vec<&str> = measurement
        .findings
        .iter()
        .filter_map(|finding| match &finding.detail {
            FindingDetail::EnvironmentLedger { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        named.contains(&ACCESS_NAME),
        "the real access passed unseen"
    );
    assert!(
        !named.contains(&FIXTURE_NAME),
        "the marked task-handle fixture remained an environment reference"
    );
}

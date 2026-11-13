use consent::Answer;
use gate::Shape;
use lane::life::Life;
use spice::config::{Document, Inputs, Root};

const COMPLETE: &str = r#"
[taste.zeta]
words = ["delay", "poll", "slept"]
message = "Wait on the real event."

[taste.alpha]
words = ["finish"]
message = "Observe completion."

[consent]
posture = "queue"

[[consent.rule]]
name = "reads"
tool = "read*"
answer = "allow"
budget = 25

[[consent.rule]]
name = "fallback"
tool = "*"
answer = "deny"

[gate.walk]
skip = ["src/**", "fixtures/"]

[board.next]
horizon = "90m"

[lane]
life = "Steer"
lease = "90s"
runtime = "6h"
vendor = "omp"
deaths = 4
young = "45s"
hold = "1h"

[steer]
expiry = "48h"
resend = ["10m", "30m", "2h"]

[vendor.omp]
binary = "/opt/omp"
model = "omp-current"
effort = "max"
fast_mode = true
lsp = true
auto_compact_window_tokens = 250000

"#;

fn decode(document: &str) -> Result<Root, String> {
    let deserializer = toml::Deserializer::parse(document).map_err(|error| error.to_string())?;
    serde_path_to_error::deserialize(deserializer).map_err(|error| error.to_string())
}

#[test]
fn every_policy_shape_round_trips_without_losing_order_or_templates() {
    let decoded = decode(COMPLETE).expect("the complete policy fixture decodes");

    assert_eq!(
        decoded
            .taste()
            .bag()
            .tastes()
            .iter()
            .map(|taste| taste.name().as_str())
            .collect::<Vec<_>>(),
        ["zeta", "alpha"]
    );
    assert_eq!(
        decoded.taste().bag().tastes()[0]
            .triggers()
            .iter()
            .map(taste::Trigger::as_str)
            .collect::<Vec<_>>(),
        ["delay", "poll", "slept"]
    );
    assert_eq!(
        decoded.consent().policy().decide("read file").answer(),
        Answer::Allow
    );
    assert_eq!(
        decoded.consent().policy().decide("write file").answer(),
        Answer::Deny
    );
    assert_eq!(decoded.consent().policy().rules()[0].name(), "reads");
    assert_eq!(decoded.consent().policy().rules()[1].name(), "fallback");

    let skips = decoded.gate().walk().skip().rules();
    assert!(skips[0].matches("src/lib.rs", Shape::File));
    assert!(skips[0].matches("src/deep/module.rs", Shape::File));
    assert!(!skips[0].matches("src", Shape::Directory));
    assert!(!skips[0].matches("docs/src/lib.rs", Shape::File));

    assert_eq!(
        decoded.board().next().horizon(),
        std::time::Duration::from_secs(90 * 60)
    );
    assert_eq!(decoded.lane().life(), Life::Steer);
    assert_eq!(decoded.lane().lease().to_string(), "90s");
    assert_eq!(decoded.lane().runtime().to_string(), "6h");
    assert_eq!(decoded.lane().vendor().as_str(), "omp");
    assert_eq!(decoded.lane().deaths(), 4);
    assert_eq!(decoded.lane().young().to_string(), "45s");
    assert_eq!(decoded.lane().hold().to_string(), "1h");
    assert_eq!(decoded.steer().expiry().to_string(), "48h");
    assert_eq!(
        decoded
            .steer()
            .resend()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["10m", "30m", "2h"]
    );
    assert_eq!(decoded.vendor().omp().binary().as_str(), "/opt/omp");
    assert_eq!(decoded.vendor().omp().model(), "omp-current");
    assert_eq!(decoded.vendor().omp().effort(), vendor::Effort::Max);
    assert!(decoded.vendor().omp().fast_mode());
    assert!(decoded.vendor().omp().lsp());
    assert_eq!(decoded.vendor().omp().auto_compact_window_tokens(), 250_000);
    assert_eq!(
        decoded.vendor().omp().auto_compact_threshold_tokens(),
        212_500
    );

    let rendered = toml::to_string_pretty(&decoded).expect("the typed policy renders as TOML");
    let round_trip = decode(&rendered).expect("the rendered policy decodes");
    assert_eq!(round_trip, decoded);
}

#[test]
fn omp_window_defaults_to_the_boundary_of_the_provider_its_model_reaches() {
    let openai = decode("[vendor.omp]\nmodel = \"gpt-5.6-sol\"\n")
        .expect("an OpenAI model selection is valid");
    assert_eq!(
        openai.vendor().omp().auto_compact_window_tokens(),
        vendor::DEFAULT_OMP_LONG_CONTEXT_AUTO_COMPACT_WINDOW_TOKENS
    );
    assert_eq!(
        openai.vendor().omp().auto_compact_threshold_tokens(),
        231_200
    );
    let pinned =
        decode("[vendor.omp]\nmodel = \"gpt-5.6-sol\"\nauto_compact_window_tokens = 200000\n")
            .expect("a named window is valid");
    assert_eq!(
        pinned.vendor().omp().auto_compact_threshold_tokens(),
        170_000
    );
    let anthropic = decode("[vendor.omp]\nmodel = \"anthropic/claude-sonnet-5\"\n")
        .expect("an Anthropic model selection is valid");
    assert_eq!(
        anthropic.vendor().omp().auto_compact_threshold_tokens(),
        231_200
    );
    let haiku =
        decode("[vendor.omp]\nmodel = \"claude-haiku-4-5\"\n").expect("a Haiku selection is valid");
    assert_eq!(
        haiku.vendor().omp().auto_compact_threshold_tokens(),
        187_000
    );
}

#[test]
fn malformed_policy_values_are_refused_under_their_domain_paths() {
    for (document, path) in [
        (
            "[taste.Bad]\nwords = [\"poll\"]\nmessage = \"wait\"\n",
            "taste.Bad",
        ),
        (
            "[taste.polling]\nwords = [\"poll.*\"]\nmessage = \"wait\"\n",
            "taste.polling.words",
        ),
        (
            "[consent]\n[[consent.rule]]\nname = \"Read Rule\"\ntool = \"read\"\nanswer = \"allow\"\n",
            "consent.rule[0]",
        ),
        ("[lane]\nlease = \"15minutes\"\n", "lane.lease"),
        ("[lane]\nruntime = \"eventually\"\n", "lane.runtime"),
        ("[lane]\nvendor = \"Other\"\n", "lane.vendor"),
        ("[steer]\nresend = [\"15s\", \"60s\"]\n", "steer"),
        ("[steer]\nresend = [\"15s\", \"15s\", \"300s\"]\n", "steer"),
        (
            "[steer]\nexpiry = \"300s\"\nresend = [\"15s\", \"60s\", \"300s\"]\n",
            "steer",
        ),
        ("[gate.walk]\nskip = [\"\"]\n", "gate.walk.skip[0]"),
        ("[proof]\nimage = \"proof:latest\"\n", "proof.image"),
        ("[proof]\ncontainer = \"docker\"\n", "proof.container"),
        ("[proof]\nrunner = \"\"\n", "proof.runner"),
        ("[vendor.Other]\nbinary = \"other\"\n", "vendor.Other"),
        ("[vendor.other]\nmodel = \"x\"\n", "vendor.other"),
        (
            "[vendor.omp]\nauto_compact_window_tokens = 16384\n",
            "vendor.omp.auto_compact_window_tokens",
        ),
        (
            "[vendor.omp]\nmodel_context_window = 1000000\n",
            "vendor.omp.model_context_window",
        ),
        (
            "[policy]\ntest_engines = [\"unknown-engine\"]\n",
            "policy.test_engines[0]",
        ),
        ("[vendor.gemini]\nbinary = \"gemini\"\n", "vendor.gemini"),
    ] {
        let error = decode(document).expect_err("malformed policy must refuse");
        assert!(
            error.contains(path),
            "refusal did not name {path:?}: {error}"
        );
    }

    let duplicate = decode(
        "[consent]\nposture = \"queue\"\n\
         [[consent.rule]]\nname = \"reads\"\ntool = \"read*\"\nanswer = \"allow\"\n\
         [[consent.rule]]\nname = \"reads\"\ntool = \"cat*\"\nanswer = \"deny\"\n",
    )
    .expect_err("duplicate consent identities must refuse");
    assert!(duplicate.contains("consent.rule"), "{duplicate}");

    let duplicate = decode("[policy]\ntest_engines = [\"cargo-workspace\", \"cargo-workspace\"]\n")
        .expect_err("duplicate test engines must refuse");
    assert!(duplicate.contains("policy.test_engines"), "{duplicate}");
}

/// Every tracked structural-policy key Gate admits is one this schema accepts.
///
/// Gate's registry decides that a tracked key is legitimate; this schema
/// decides whether the document carrying it loads at all. When the two
/// disagree the registry advertises a setting that makes the whole file
/// refuse. Each declaration below is therefore checked against Gate's sole
/// registry before the complete document is decoded.
#[test]
fn every_tracked_structural_policy_key_is_one_this_schema_admits() {
    for (key, _) in TRACKED_STRUCTURAL_DECLARATIONS {
        assert!(
            gate::CONFIG_KEY_DECLARATIONS
                .iter()
                .any(|entry| entry.template() == key),
            "Gate does not admit {key} through its current registry"
        );
    }

    let policy = decode(&structural_policy_document())
        .expect("every tracked structural-policy key declared together loads")
        .policy()
        .clone();
    assert_structural_policy_values(&policy);
}

/// Every tracked structural-policy key reaches its accessor through the resolver.
///
/// The schema test above proves serde accepts these spellings, but a tracked
/// document reaches serde only after `resolve` sanitizes it against the
/// descriptor registry. A key missing from that registry is refused as
/// `IssueKind::Unknown`, leaving the accessor unreachable while Gate still
/// advertises the setting.
#[test]
fn every_tracked_structural_policy_key_reaches_its_accessor() {
    let document = structural_policy_document();
    let resolved = spice::config::resolve(Inputs::new(
        Some(Document::new("spice.toml", &document)),
        None,
        &[],
    ))
    .unwrap_or_else(|error| panic!("current resolution refused a tracked key: {error}"));
    assert_structural_policy_values(resolved.root().policy());
}

/// Each value is distinct so no reading can be satisfied by another's bytes.
const TRACKED_STRUCTURAL_DECLARATIONS: [(&str, &str); 10] = [
    ("policy.repo_truth.docs", "[\"DOCTRINE.md\"]"),
    ("policy.file_shape.source_suffixes", "[\".rs\"]"),
    ("policy.languages.complexity", "[\".rs\", \".py\"]"),
    ("policy.languages.magic", "[\".rs\"]"),
    ("policy.package_roots", "[\"src/sample\"]"),
    ("policy.generated_paths", "[\"build/derived\"]"),
    ("policy.name_cluster_threshold", "3"),
    ("policy.test_engines", "[\"cargo-workspace\"]"),
    ("policy.test_paths", "[\"suite\"]"),
    ("policy.package.boundary_underscore_pattern", "\"^[a-z]+$\""),
];

fn structural_policy_document() -> String {
    TRACKED_STRUCTURAL_DECLARATIONS
        .iter()
        .map(|(key, value)| format!("{key} = {value}\n"))
        .collect()
}

fn assert_structural_policy_values(policy: &spice::config::WorkspacePolicy) {
    assert_eq!(
        policy.repo_truth().docs(),
        Some(["DOCTRINE.md".to_owned()].as_slice())
    );
    assert_eq!(
        policy.file_shape().source_suffixes(),
        Some([".rs".to_owned()].as_slice())
    );
    assert_eq!(
        policy.languages().complexity(),
        Some([".rs".to_owned(), ".py".to_owned()].as_slice())
    );
    assert_eq!(
        policy.languages().magic(),
        Some([".rs".to_owned()].as_slice())
    );
    assert_eq!(policy.package_roots(), ["src/sample".to_owned()]);
    assert_eq!(policy.generated_paths(), ["build/derived".to_owned()]);
    assert_eq!(policy.name_cluster_threshold(), Some(3));
    assert_eq!(
        policy.test_engines(),
        [spice::config::TestEngine::CargoWorkspace]
    );
    assert_eq!(policy.test_paths(), ["suite".to_owned()]);
    assert_eq!(
        policy.package().boundary_underscore_pattern(),
        Some("^[a-z]+$")
    );
}

#[test]
fn unknown_fields_name_their_domain_and_key() {
    for (document, domain) in [
        (
            "[taste.polling]\nwords = [\"poll\"]\nmessage = \"wait\"\nextra = 1\n",
            "taste.polling",
        ),
        ("[consent]\nextra = 1\n", "consent"),
        ("[gate.walk]\nextra = 1\n", "gate.walk"),
        ("[board.next]\nextra = 1\n", "board.next"),
        ("[lane]\nextra = 1\n", "lane"),
        ("[steer]\nextra = 1\n", "steer"),
        ("[vendor.omp]\nbinary = \"omp\"\nextra = 1\n", "vendor.omp"),
        ("[proof]\nextra = 1\n", "proof"),
    ] {
        let error = decode(document).expect_err("unknown policy key must refuse");
        assert!(
            error.contains(domain),
            "refusal omitted {domain:?}: {error}"
        );
        assert!(error.contains("extra"), "refusal omitted the key: {error}");
    }
}

#[test]
fn a_malformed_horizon_refuses_by_its_key() {
    let horizon = decode("[board.next]\nhorizon = \"2 hours\"\n")
        .expect_err("an overdue horizon is a duration word");
    assert!(horizon.contains("board.next.horizon"), "{horizon}");
}

#[test]
fn regex_punctuation_is_only_ever_an_exact_consent_word_pattern() {
    let root = decode(
        "[consent]\nposture = \"queue\"\n\
         [[consent.rule]]\nname = \"literal\"\ntool = \"^read$\"\nanswer = \"deny\"\n",
    )
    .expect("regex-looking punctuation is admitted as literal text");

    assert_eq!(
        root.consent().policy().decide("read").answer(),
        Answer::Queue
    );
    assert_eq!(
        root.consent().policy().decide("^read$").answer(),
        Answer::Deny
    );
}

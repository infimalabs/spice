//! In-process enforcement contracts over walks, declarations, identities, and ratchets.

use gate::jurisdiction::{Admission, Shape};
use gate::measure::Measurement;
use gate::ratchet::{Malady, Movement};
use gate::{Ignore, Jurisdiction, Metric, Parser, Policy, Ratchet, measurement_digest};
use wire::{Digest, Domain};

use super::support::Tree;

fn ratchet(text: &str) -> Ratchet {
    Ratchet::parse(text).expect("parse the fixture ratchet")
}

#[test]
fn a_walk_records_a_reason_for_every_path_it_reaches() {
    let tree = Tree::new("reasons");
    tree.write("src/lib.rs", "one\ntwo\n");
    tree.write("target/junk.rs", "noise\n");
    tree.write("notes.txt", "prose\n");

    let ignore = Ignore::parse("/target/\n*.txt\n").expect("parse the policy");
    let walked = Jurisdiction::walk(&tree.root, &ignore).expect("walk the fixture");

    let recorded: Vec<(&str, Shape, Admission)> = walked
        .entries()
        .iter()
        .map(|entry| (entry.path(), entry.shape(), entry.admission()))
        .collect();

    // `target` is recorded as excluded and never descended, so the absence of
    // `target/junk.rs` is explained by the record rather than merely true.
    assert_eq!(
        recorded,
        [
            ("notes.txt", Shape::File, Admission::Ignored(1)),
            ("src", Shape::Directory, Admission::Unnamed),
            ("src/lib.rs", Shape::File, Admission::Unnamed),
            ("target", Shape::Directory, Admission::Ignored(0)),
        ]
    );
    assert_eq!(
        walked.inventory(),
        "ignored:1 file notes.txt\n\
         unnamed directory src\n\
         unnamed file src/lib.rs\n\
         ignored:0 directory target\n"
    );
}

#[test]
fn a_readmitting_rule_beats_the_broad_rule_above_it() {
    let tree = Tree::new("readmit");
    tree.write("vendor/keep.rs", "kept\n");
    tree.write("vendor/drop.rs", "dropped\n");

    let ignore = Ignore::parse("vendor/*.rs\n!vendor/keep.rs\n").expect("parse the policy");
    let walked = Jurisdiction::walk(&tree.root, &ignore).expect("walk the fixture");

    let admitted: Vec<&str> = walked.measurable().map(gate::Entry::path).collect();
    assert_eq!(admitted, ["vendor/keep.rs"]);
    assert_eq!(
        walked
            .entries()
            .iter()
            .find(|entry| entry.path() == "vendor/keep.rs")
            .map(gate::Entry::admission),
        Some(Admission::Readmitted(1)),
        "the later rule is the one that decided, and the record says so"
    );
}

#[test]
fn a_walk_is_a_total_function_of_the_tree() {
    let tree = Tree::new("total");
    for index in 0..24 {
        tree.write(&format!("crate{index}/src/lib.rs"), "line\n");
    }

    let ignore = Ignore::parse("").expect("parse an empty policy");
    let first = Jurisdiction::walk(&tree.root, &ignore).expect("walk once");
    let again = Jurisdiction::walk(&tree.root, &ignore).expect("walk again");

    assert_eq!(first.inventory(), again.inventory());
    assert_eq!(first.inventory_digest(), again.inventory_digest());
    assert_eq!(
        first.tree_digest().expect("digest the tree"),
        again.tree_digest().expect("digest the tree again")
    );

    let paths: Vec<&str> = first.entries().iter().map(gate::Entry::path).collect();
    let mut sorted = paths.clone();
    sorted.sort_unstable();
    assert_eq!(paths, sorted, "entries are sorted, not merely stable");
}

/// Gate publishes five digest classes, including the file digest nested inside
/// the tree listing. Freeze every spelling so moving one mint site to another
/// domain cannot silently change what an existing receipt identity means.
#[test]
fn every_gate_digest_is_frozen_at_its_published_spelling() {
    let tree = Tree::new("digest-domains");
    let file_bytes = b"one\ntwo\n";
    tree.write(
        "fixture.txt",
        std::str::from_utf8(file_bytes).expect("fixture is text"),
    );
    let jurisdiction = Jurisdiction::walk(
        &tree.root,
        &Ignore::parse("").expect("parse an empty policy"),
    )
    .expect("walk the fixture");

    // `tree_digest` mints the file identity internally. Reconstructing its
    // one-entry listing with the named file domain ties that nested mint site
    // to this fixture without exposing a file-digest API solely for a test.
    let file_digest = Digest::compute(Domain::GateFile, file_bytes);
    let tree_listing = format!("{file_digest} fixture.txt\n");
    let tree_digest = jurisdiction.tree_digest().expect("digest the tree");
    assert_eq!(
        tree_digest,
        Digest::compute(Domain::GateTree, tree_listing.as_bytes()),
        "the tree must nest GateFile identities under GateTree",
    );

    let policy = Policy::new(
        Ignore::parse("*.tmp\n").expect("parse the fixture policy"),
        Vec::<Metric>::new(),
    )
    .expect("declare the fixture policy");
    let measurements = [Measurement::new("fixture.lines", 2, ["fixture.txt"])];

    for (class, computed, frozen) in [
        (
            Domain::GateFile,
            file_digest,
            "2edc805176833b058cd6136c912ff6c9e3b7c430d158e7172b0e56a6bec84f2c",
        ),
        (
            Domain::GateTree,
            tree_digest,
            "73c42b39bc0fcbdeb3f3e38250edb0b703aa168b353726e224bbc91254c73f2e",
        ),
        (
            Domain::GateInventory,
            jurisdiction.inventory_digest(),
            "f01b9548a1181e65ac91efa6bf4489560340c29e075b3db3ca79e741a5be0171",
        ),
        (
            Domain::GatePolicy,
            policy.digest(),
            "18a0e223485c527ffdbe4332a5a49790c4eff66ed1deb536acaa665d82265cdc",
        ),
        (
            Domain::GateMeasurement,
            measurement_digest(&measurements),
            "61d093ca25f57ff332ea5154d018806347b75dfec952b70a3153f9cd1d3496d9",
        ),
    ] {
        assert_eq!(
            computed.as_str(),
            frozen,
            "{} no longer digests to the spelling it published",
            class.tag(),
        );
    }
}

#[test]
fn ignore_lines_with_more_than_one_spelling_are_refused() {
    for line in [" *.rs", "*.rs ", "!"] {
        let refused = Ignore::parse(line).expect_err("a second spelling is refused");
        assert_eq!(refused.line(), line, "the refusal quotes the exact line");
    }

    let accepted = Ignore::parse("# a comment\n\n**/*.rs\na\\ b\n")
        .expect("comments, blanks, crossing stars, and escapes are Gitignore syntax");
    assert_eq!(accepted.rules().len(), 2);
    assert_eq!(accepted.render(), "**/*.rs\na\\ b\n");

    // A rendered policy has to parse back to itself, or the policy digest would
    // be over a spelling that no operator could commit and re-derive.
    let written = "*.rs\n/target/\nvendor/*.rs\n!vendor/keep.rs\na b\n";
    let parsed = Ignore::parse(written).expect("parse a policy with every rule shape");
    assert_eq!(parsed.render(), written);
    assert_eq!(
        Ignore::parse(&parsed.render()).expect("reparse the rendering"),
        parsed
    );
}

#[test]
fn a_malformed_baseline_is_refused_by_line_and_never_half_read() {
    for (text, number, malady) in [
        ("rust.lines\n", 1, Malady::NotTwoFields),
        ("rust.lines  4\n", 1, Malady::NotACount),
        ("rust.lines -1\n", 1, Malady::NotACount),
        ("a 1\na 2\n", 2, Malady::Duplicate),
        ("b 1\na 2\n", 2, Malady::Unsorted),
    ] {
        let refused = Ratchet::parse(text).expect_err("a malformed ratchet is refused");
        assert_eq!((refused.number(), refused.malady()), (number, malady));
    }

    let read = ratchet("a 1\nb 2\n");
    assert_eq!(read.render(), "a 1\nb 2\n", "one baseline, one spelling");
}

#[test]
fn comparison_and_tightening_cover_every_ratchet_movement_without_raising_a_floor() {
    let committed = ratchet("rust.held 3\nrust.regressed 2\nrust.retired 4\nrust.shrank 9\n");
    let measured = [
        Measurement::new("rust.held", 3, ["held.rs"]),
        Measurement::new("rust.new", 7, ["new.rs"]),
        Measurement::new("rust.regressed", 5, ["regressed.rs"]),
        Measurement::new("rust.shrank", 1, ["shrunk.rs"]),
    ];
    assert_eq!(
        committed
            .compare(&measured)
            .into_iter()
            .map(|finding| (finding.metric, finding.movement))
            .collect::<Vec<_>>(),
        [
            ("rust.held".to_owned(), Movement::Held { baseline: 3 }),
            ("rust.new".to_owned(), Movement::Unbaselined { measured: 7 }),
            (
                "rust.regressed".to_owned(),
                Movement::Regressed {
                    baseline: 2,
                    measured: 5,
                },
            ),
            ("rust.retired".to_owned(), Movement::Retired { baseline: 4 }),
            (
                "rust.shrank".to_owned(),
                Movement::Shrank {
                    baseline: 9,
                    measured: 1,
                },
            ),
        ]
    );

    for baseline in 0..12_u64 {
        let committed = ratchet(&format!("rust.lines {baseline}\n"));
        for measured in 0..12_u64 {
            let taken = [Measurement::new("rust.lines", measured, ["src/lib.rs"])];
            assert_eq!(
                committed.tighten(&taken).baseline("rust.lines"),
                Some(baseline.min(measured))
            );
        }
    }
}

#[test]
fn a_metric_that_omits_what_doctrine_requires_is_refused() {
    let parser = || Parser::new("fixture-parser", ["count"]).expect("declare a parser");
    let declare = |name: &str, language: &str, target: &str, files: &str, exclusions: &str| {
        Metric::new(name, language, target, parser(), files, exclusions)
    };

    for (name, language, target, files) in [
        ("", "rust", "lines", "*.rs\n"),
        ("rust.lines", "", "lines", "*.rs\n"),
        ("rust.lines", "rust", "", "*.rs\n"),
        ("rust.lines", "rust", "lines", ""),
    ] {
        assert!(
            declare(name, language, target, files, "").is_err(),
            "a metric missing a declaration was accepted"
        );
    }

    assert!(
        declare("rust lines", "rust", "lines", "*.rs\n", "").is_err(),
        "a spaced name would make a rendered policy line unsplittable"
    );
    assert!(
        declare("rust.lines", "rust", "lines", "!*.rs\n", "").is_err(),
        "scope is stated by the two lists, not by negation inside one of them"
    );
    assert!(Parser::new("", ["-c"]).is_err(), "a parser must be named");
    let first =
        declare("rust.lines", "rust", "lines", "*.rs\n", "").expect("declare the first metric");
    let duplicate = declare("rust.lines", "rust", "other lines", "*.rs\n", "")
        .expect("declare the duplicate metric");
    assert!(
        Policy::new(Ignore::parse("").expect("parse ignore"), [first, duplicate]).is_err(),
        "two metrics under one name would make ratchet comparison ambiguous"
    );
}

#[test]
fn no_declaration_can_forge_a_policy_line() {
    // Arguments render one per line rather than space-joined, so these two
    // parsers are two policies. Joined, both would have spelled `-c a b`.
    let split = |arguments: [&str; 3]| {
        Policy::new(
            Ignore::parse("").expect("parse an empty policy"),
            [Metric::new(
                "rust.lines",
                "rust",
                "lines",
                Parser::new("/bin/sh", arguments).expect("declare a parser"),
                "*.rs\n",
                "",
            )
            .expect("declare a metric")],
        )
        .expect("declare a policy")
        .digest()
    };
    assert_ne!(split(["-c", "a b", "x"]), split(["-c", "a", "b x"]));

    // And a value carrying a line break is refused outright, which is what
    // keeps the one-per-line rendering injective in the first place.
    assert!(
        Parser::new("/bin/sh", ["-c\nmetric rust.lines files *.md"]).is_err(),
        "an argument opened a policy line of its own"
    );
    for forged in ["ru\nst", "li\rnes"] {
        assert!(
            Metric::new(
                "rust.lines",
                forged,
                "lines",
                Parser::new("/bin/sh", ["-c"]).expect("declare a parser"),
                "*.rs\n",
                "",
            )
            .is_err(),
            "a declared value carried a line break"
        );
    }
}

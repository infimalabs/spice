use super::*;

#[test]
fn diagnostics_accumulate_unknown_policy_and_malformed_values() {
    let error = resolve(documents(
        Some(
            r#"
[tasks]
owner = "bootstrap"

[policy]
mode = "bootstrap"

[lane]
lease = "soon"
deaths = 0
mystery = 1

[board.next]
band = 2

[proof]
image = "latest"

[serve.pack]
rows = "many"
"#,
        ),
        Some(
            r#"
[lane]
life = "Drain"

[listen]
window = "never"

[lock.alpha]
directory = "locks/alpha"
shards = 0

[lock.beta]
directory = "locks/beta"
shards = 2
mystery = 3

[serve.pack]
rows = 0

[bogus]
value = 1
"#,
        ),
    ))
    .expect_err("every independent issue refuses the result");

    let rows = error
        .issues()
        .iter()
        .map(|issue| {
            (
                issue.path().map(ToString::to_string),
                issue.source(),
                issue.kind(),
                issue.message(),
            )
        })
        .collect::<Vec<_>>();
    for (path, source, kind) in [
        ("board.next.band", ConfigSource::Tracked, IssueKind::Unknown),
        ("bogus", ConfigSource::Local, IssueKind::Unknown),
        ("serve.pack.rows", ConfigSource::Tracked, IssueKind::Policy),
        ("serve.pack.rows", ConfigSource::Local, IssueKind::Malformed),
        ("lane.deaths", ConfigSource::Tracked, IssueKind::Malformed),
        ("lane.lease", ConfigSource::Tracked, IssueKind::Malformed),
        ("lane.life", ConfigSource::Local, IssueKind::Policy),
        ("lane.mystery", ConfigSource::Tracked, IssueKind::Unknown),
        ("listen.window", ConfigSource::Local, IssueKind::Malformed),
        ("lock.alpha", ConfigSource::Local, IssueKind::Malformed),
        ("lock.beta.mystery", ConfigSource::Local, IssueKind::Unknown),
        ("policy.mode", ConfigSource::Tracked, IssueKind::Unknown),
        ("proof.image", ConfigSource::Tracked, IssueKind::Unknown),
        ("tasks", ConfigSource::Tracked, IssueKind::Unknown),
    ] {
        assert!(
            rows.iter()
                .any(|row| { row.0.as_deref() == Some(path) && row.1 == source && row.2 == kind }),
            "missing {source:?} {kind:?} issue at {path}: {rows:#?}"
        );
    }
    for (path, source, home) in [
        ("lane.life", ConfigSource::Local, "tracked spice.toml"),
        (
            "serve.pack.rows",
            ConfigSource::Tracked,
            ".spice/config.toml",
        ),
    ] {
        let refusal = error
            .issues()
            .iter()
            .find(|issue| {
                issue.kind() == IssueKind::Policy
                    && issue.source() == source
                    && issue.path().is_some_and(|found| found.to_string() == path)
            })
            .unwrap_or_else(|| panic!("{path} refuses in the wrong layer"));
        assert!(refusal.message().contains(home), "{path} names its home");
    }
    assert!(error.issues().windows(2).all(|pair| {
        pair[0].path().map(ToString::to_string) <= pair[1].path().map(ToString::to_string)
    }));
}

#[test]
fn syntax_positions_and_other_layer_diagnostics_are_both_returned() {
    let error = resolve(Inputs::new(
        Some(Document::new(
            "config/spice.toml",
            "# caf\u{e9}\n[lane]\nlease = [\n",
        )),
        Some(Document::new(".spice/local.toml", "[unknown]\nvalue = 1\n")),
        &[],
    ))
    .expect_err("syntax and unknown table refuse");

    let syntax = error
        .issues()
        .iter()
        .find(|issue| issue.kind() == IssueKind::Syntax)
        .expect("syntax issue");
    assert_eq!(syntax.document(), "config/spice.toml");
    assert_eq!(syntax.source(), ConfigSource::Tracked);
    assert!(syntax.line().is_some_and(|line| line >= 3));
    assert!(syntax.column().is_some());
    assert!(error.issues().iter().any(|issue| {
        issue.kind() == IssueKind::Unknown
            && issue.source() == ConfigSource::Local
            && issue
                .path()
                .is_some_and(|path| path.to_string() == "unknown")
    }));
}

#[test]
fn flag_values_cannot_cross_descriptor_types_or_duplicate_mappings() {
    let life_mapping = descriptor(&[PathSegment::Key("lane"), PathSegment::Key("life")])
        .and_then(|entry| entry.flag())
        .expect("life flag mapping");
    assert_eq!(
        FlagOverride::new(
            life_mapping,
            FlagValue::Name("other".parse().expect("valid name"))
        ),
        Err(FlagError::WrongValue {
            expected: spice::config::ValueKind::Life,
            actual: spice::config::ValueKind::String,
        })
    );

    let life =
        FlagOverride::new(life_mapping, FlagValue::Life(Life::Steer)).expect("typed life flag");
    let flags = [life.clone(), life];
    let error = resolve(Inputs::new(None, None, &flags)).expect_err("duplicate flag refuses");
    assert!(error.issues().iter().any(|issue| {
        issue.kind() == IssueKind::DuplicateFlag
            && issue
                .path()
                .is_some_and(|path| path.to_string() == "lane.life")
    }));
}

#[test]
fn equivalent_repeated_resolutions_are_identical() {
    let tracked = "[lane]\nvendor = \"omp\"\n";
    let local = "[listen]\nwindow = \"3m\"\n[lock.alpha]\ndirectory = 'locks/alpha'\nshards = 2\n";
    let first = resolve(documents(Some(tracked), Some(local))).expect("first resolution");
    let second = resolve(documents(Some(tracked), Some(local))).expect("second resolution");
    assert_eq!(first, second);
}

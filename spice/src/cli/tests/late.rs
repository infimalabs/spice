use super::*;

/// Every declared configuration flag must resolve through the generated parser tree.
#[test]
fn configuration_flag_mappings_resolve_in_the_generated_tree() {
    let parser = command();
    for mapping in crate::config::descriptors()
        .iter()
        .filter_map(|descriptor| descriptor.flag())
    {
        let contract = contract_at(CONTRACTS, mapping.command())
            .unwrap_or_else(|| panic!("missing command contract for {mapping:?}"));
        assert!(
            contract.syntax.contains(mapping.flag()),
            "contract syntax lost {mapping:?}: {:?}",
            contract.syntax
        );

        let leaf = mapping.command().iter().fold(&parser, |command, name| {
            command
                .find_subcommand(name)
                .unwrap_or_else(|| panic!("generated tree lost {mapping:?}"))
        });
        assert!(
            leaf.get_arguments()
                .any(|argument| argument.get_long() == mapping.flag().strip_prefix("--")),
            "generated parser lost {mapping:?}"
        );
    }
}

#[test]
fn mutating_contract_syntax_mutates_registration() {
    let mut contracts = CONTRACTS.to_vec();
    let doctor = contracts
        .iter_mut()
        .find(|contract| contract.path == ["doctor"])
        .expect("doctor contract exists");
    doctor.syntax = "doctor REQUIRED";

    let parser = command_from(&contracts, &[]);
    assert!(
        parser
            .clone()
            .try_get_matches_from(["spice", "doctor"])
            .is_err(),
        "a required operand added to the table must change registration",
    );
    parser
        .clone()
        .try_get_matches_from(["spice", "doctor", "value"])
        .expect("the syntax mutation admits its required operand");
}

#[test]
#[should_panic]
fn a_registered_command_without_a_summary_refuses_the_parser() {
    let mut contracts = CONTRACTS.to_vec();
    let mut novel = *contracts
        .iter()
        .find(|contract| contract.path == ["doctor"])
        .expect("doctor contract exists");
    novel.path = &["task", "unsummarized"];
    novel.syntax = "task unsummarized";
    contracts.push(novel);

    let _ = command_from(&contracts, &[]);
}

/// First-run paths must not expose an undocumented repository option.
#[test]
fn every_first_run_option_has_help() {
    let parser = command();
    let json = parser
        .get_arguments()
        .find(|argument| argument.get_long() == Some(JSON))
        .expect("root owns the global renderer option");
    assert!(json.get_help().is_some(), "global --json has no help");

    for path in [
        &["up"][..],
        &["down"][..],
        &["install"][..],
        &["lane", "add"][..],
        &["lane", "wake"][..],
        &["doctor"][..],
    ] {
        let leaf = path.iter().fold(&parser, |command, name| {
            command
                .find_subcommand(name)
                .unwrap_or_else(|| panic!("missing first-run command {path:?}"))
        });
        for argument in leaf
            .get_arguments()
            .filter(|argument| argument.get_long().is_some())
        {
            assert!(
                argument.get_help().is_some(),
                "{path:?} --{} has no help",
                argument.get_long().expect("filtered long option")
            );
        }
    }
}

/// The CLI uses the same charge polarity sentence as agent-facing Design
/// requests, so neither surface can silently invert the signed vote.
#[test]
fn room_charge_help_uses_the_shared_vote_guidance() {
    let parser = command();
    let contribute = ["room", "contribute"]
        .iter()
        .fold(&parser, |command, name| {
            command
                .find_subcommand(name)
                .unwrap_or_else(|| panic!("missing command {name}"))
        });
    let charge = contribute
        .get_arguments()
        .find(|argument| argument.get_long() == Some("charge"))
        .expect("room contribute owns --charge");

    assert_eq!(
        charge.get_help().map(ToString::to_string).as_deref(),
        Some(wire::rooms::RoomCharge::VOTE_GUIDANCE)
    );
}

#[test]
fn lifecycle_owner_controls_are_symmetric() {
    for argv in [["spice", "up", "--keeper"], ["spice", "down", "--keeper"]] {
        let parsed = command()
            .try_get_matches_from(argv)
            .unwrap_or_else(|error| panic!("{argv:?} must parse: {error}"));
        assert!(
            selected_matches(&parsed).get_flag("keeper"),
            "{argv:?} retains its lifecycle-owner request"
        );
    }
}

#[test]
fn task_resource_maps_use_one_repeatable_spelling_at_creation_and_replacement() {
    for argv in [
        &[
            "spice",
            "task",
            "add",
            "Use shared hardware",
            "--resource",
            "windows-guest",
            "--resource",
            "gpu=2",
        ][..],
        &[
            "spice",
            "task",
            "require",
            "SPICE-example",
            "--resource",
            "windows-guest",
            "--resource",
            "gpu=2",
        ][..],
    ] {
        let parsed = command()
            .try_get_matches_from(argv)
            .unwrap_or_else(|error| panic!("{argv:?} must parse: {error}"));
        let resources = selected_matches(&parsed)
            .get_many::<String>("resource")
            .expect("resources remain repeated")
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert_eq!(resources, ["windows-guest", "gpu=2"]);
    }
}

/// Help rows are part of the syntax contract, not best-effort decorations.
#[test]
#[should_panic(expected = "names unregistered --foreground")]
fn option_help_for_an_unaccepted_option_refuses_the_parser() {
    let mut contracts = CONTRACTS.to_vec();
    contracts
        .iter_mut()
        .find(|contract| contract.path == ["up"])
        .expect("up contract exists")
        .syntax = "up";

    let _ = command_from(&contracts, &[]);
}

#[test]
fn selected_leaf_recovers_the_same_contract_row() {
    let matches = command()
        .try_get_matches_from(["spice", "doctor"])
        .expect("doctor parses");
    assert_eq!(
        selected_contract(&matches),
        contract_at(CONTRACTS, &["doctor"])
    );
}

/// The renderer flag is recognised wherever spice's own words are.
///
/// A position spice fails to recognise silently renders a sentence to a
/// caller who asked for a document, and that caller learns of it by failing to
/// parse what arrived.
#[test]
fn the_renderer_flag_is_read_wherever_spice_owns_the_word() {
    for argv in [
        &["spice", "--json", "task", "next"][..],
        &["spice", "task", "--json", "next"][..],
        &["spice", "task", "next", "--json"][..],
        &["spice", "--json", "doctor"][..],
        &["spice", "doctor", "--json"][..],
    ] {
        let matches = command()
            .try_get_matches_from(argv)
            .unwrap_or_else(|error| panic!("{argv:?} must parse: {error}"));
        assert!(json_selected(&matches), "{argv:?} asked for JSON");
    }

    let matches = command()
        .try_get_matches_from(["spice", "task", "next"])
        .expect("an unflagged leaf parses");
    assert!(!json_selected(&matches), "nobody asked for JSON");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn bootstrap_renderer_selection_stops_at_the_child_command_delimiter() {
    for argv in [
        &["spice", "--json", "doctor"][..],
        &["spice", "doctor", "--json"][..],
    ] {
        assert!(
            bootstrap_json_selected(argv.iter().map(|argument| OsString::from(*argument))),
            "{argv:?} asks the bootstrap refusal renderer for JSON",
        );
    }
    let child = ["spice", "run", "--", "echo", "--json"];
    assert!(
        !bootstrap_json_selected(child.into_iter().map(OsString::from)),
        "the child command owns every argument after its delimiter",
    );
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn bootstrap_lock_selection_names_only_the_top_level_leaf() {
    for argv in [
        &["spice", "lock"][..],
        &["spice", "--json", "lock"][..],
        &["spice", "lock", "run", "worker"][..],
    ] {
        assert!(
            bootstrap_lock_selected(argv.iter().map(|argument| OsString::from(*argument))),
            "{argv:?} selects the top-level Lock leaf",
        );
    }
    for argv in [
        &["spice", "config", "lock.worker.shards"][..],
        &["spice", "run", "--", "lock"][..],
        &["spice", "--json", "task", "lock"][..],
    ] {
        assert!(
            !bootstrap_lock_selected(argv.iter().map(|argument| OsString::from(*argument))),
            "{argv:?} must not borrow Lock's configuration refusal path",
        );
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn bootstrap_configuration_owners_are_exact_top_level_leaves() {
    for argv in [
        &["spice", "config"][..],
        &["spice", "--json", "config"][..],
        &["spice", "config", "lock.worker.shards", "--json"][..],
        &["spice", "doctor"][..],
    ] {
        assert!(
            bootstrap_configuration_owner_selected(
                argv.iter().map(|argument| OsString::from(*argument))
            ),
            "{argv:?} selects a top-level configuration owner",
        );
    }
    for argv in [
        &["spice", "task", "config"][..],
        &["spice", "run", "--", "config"][..],
        &["spice", "--json", "task", "doctor"][..],
    ] {
        assert!(
            !bootstrap_configuration_owner_selected(
                argv.iter().map(|argument| OsString::from(*argument))
            ),
            "{argv:?} must not borrow a configuration owner's refusal path",
        );
    }
}
/// A placeholder for each operand a contract requires before its command
/// delimiter, or nothing when it admits no child command.
///
/// The delimiter is found through the same tokeniser registration reads the
/// row with, so a test cannot come to a different conclusion about where one
/// leaf's words end from the one the parser was built on.
fn operands_before_delimiter(contract: &Contract) -> Option<Vec<&'static str>> {
    let tail = contract
        .syntax
        .strip_prefix(contract.path.join(" ").as_str())
        .expect("syntax begins with its registered path");
    let tokens = syntax_tokens(tail.trim()).expect("registered syntax tokenises");
    let delimiter = tokens.iter().position(|token| *token == "--")?;
    Some(
        tokens[..delimiter]
            .iter()
            .filter(|token| !is_optional(token))
            .map(|_| "OPERAND")
            .collect(),
    )
}

/// Every command delimiter in the registry ends spice's words and begins the
/// caller's.
///
/// This walks the rows rather than naming `run`, because the property belongs
/// to the delimiter and a second leaf that admits a child command would
/// otherwise inherit the claim without ever being asked. It fails in the
/// opposite direction from the placement law above: a delimiter spice reads
/// past both silently changes its own rendering and hands the child a shorter
/// command than the operator typed, and the child is an arbitrary tool that is
/// as likely as spice to have a flag by that name.
#[test]
fn a_child_commands_words_begin_at_the_delimiter() {
    let delimited = CONTRACTS
        .iter()
        .filter_map(|contract| Some((contract, operands_before_delimiter(contract)?)))
        .collect::<Vec<_>>();
    assert!(
        !delimited.is_empty(),
        "the registry admits at least one child command",
    );

    for (contract, operands) in delimited {
        let mut argv = vec!["spice"];
        argv.extend(contract.path.iter().copied());
        argv.extend(operands);

        let mut renderer = argv.clone();
        renderer.extend(["--json", "--", "echo"]);
        let matches = command()
            .try_get_matches_from(&renderer)
            .unwrap_or_else(|error| panic!("{renderer:?} must parse: {error}"));
        assert!(
            json_selected(&matches),
            "{renderer:?} asked for JSON before the delimiter",
        );

        let mut child = argv;
        child.extend(["--", "echo", "--json"]);
        let matches = command()
            .try_get_matches_from(&child)
            .unwrap_or_else(|error| panic!("{child:?} must parse: {error}"));
        assert!(
            !json_selected(&matches),
            "{child:?} spelled the child's flag, not the renderer's",
        );
    }
}

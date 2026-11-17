use super::*;

fn node(id: i64, path: &str, text: &str) -> Node {
    Node {
        id,
        path: path.parse().unwrap(),
        terms: TermCounts::from_texts([text]),
    }
}

fn statement(text: &str) -> Query {
    Query {
        statement: TermCounts::from_texts([text]),
        origin: None,
        tail: None,
    }
}

fn tailed(text: &str, tail: &str) -> Query {
    let wire::TopicInput::Tail(tail) = tail.parse().unwrap() else {
        panic!("{tail} is not relative");
    };
    Query {
        tail: Some(tail),
        ..statement(text)
    }
}

fn all() -> Scope {
    Scope {
        roots: RoutingRoots::All,
        unmatched: None,
    }
}

fn unmatched(topic: &str) -> Scope {
    Scope {
        roots: RoutingRoots::All,
        unmatched: Some(topic.parse().unwrap()),
    }
}

fn topic(path: &str) -> Topic {
    path.parse().unwrap()
}

fn picked(decision: &Decision) -> i64 {
    match &decision.target {
        Target::Node(pick) => pick.node,
        Target::Unmatched(topic) => panic!("expected a node, got unmatched {topic}"),
    }
}

fn structure(decision: &Decision) -> &Structure {
    decision.structure.as_ref().expect("a tail was authored")
}

#[test]
fn session_control_markers_cannot_override_the_statement_subject() {
    for marker in ["SYN", "ACK", "NAK", "FIN"] {
        let nodes = || {
            vec![
                node(1, "/briefing/encode", "encode bitmap image pixels"),
                node(
                    2,
                    "/briefing/transcribe",
                    &format!("transcribe blind reader accuracy {marker}"),
                ),
            ]
        };
        let plain = Scored::new(nodes(), &statement("encode bitmap image pixels"));
        let noisy = Scored::new(
            nodes(),
            &statement(&format!(
                "encode bitmap image pixels {}",
                format!("{marker} ").repeat(64)
            )),
        );
        assert_eq!(picked(&plain.pick(&all()).unwrap()), 1);
        assert_eq!(picked(&noisy.pick(&all()).unwrap()), 1, "{marker}");
    }
}

#[test]
fn descent_follows_unique_improvement_and_stops_at_the_deepest_explained_parent() {
    let scored = Scored::new(
        vec![
            node(1, "/engine", "compiler parser memory"),
            node(2, "/engine/parser", "compiler parser"),
            node(3, "/engine/parser/lexer", "compiler parser"),
            node(4, "/engine/memory", "memory"),
            node(5, "/engine/memory/deep", "compiler parser"),
        ],
        &statement("compiler parser"),
    );
    let decision = scored.pick(&all()).unwrap();
    assert_eq!(decision.target.topic().as_str(), "/engine/parser");
    assert_eq!(decision.destination.as_str(), "/engine/parser");
    assert_eq!(decision.destination_node, Some(2));
    assert_eq!(decision.structure, None);
    assert_eq!(decision.tier, RouteTier::Path);
    assert_eq!(decision.stop, RouteStop::ChildrenBelow);
    assert_eq!(decision.confidence(), RouteConfidence::Secondary);
    assert!(decision.overlap > 0.0);
    assert!(decision.residual.is_empty(), "{:?}", decision.residual);
}

#[test]
fn structural_roots_and_gaps_preserve_targets_and_restrictions_reuse_scores() {
    let scored = Scored::new(
        vec![
            node(1, "/engine/frontend/parser", "parser syntax"),
            node(
                2,
                "/engine/frontend/parser/errors",
                "parser syntax diagnostics",
            ),
            node(3, "/engine2/parser", "parser syntax diagnostics"),
        ],
        &statement("parser syntax diagnostics"),
    );
    let roots = Scope {
        roots: RoutingRoots::Selected(vec!["/engine".parse().unwrap()]),
        unmatched: None,
    };
    let restricted = scored.pick(&roots).unwrap();
    assert_eq!(picked(&restricted), 2);
    assert_eq!(restricted.stop, RouteStop::Leaf);
    assert_eq!(picked(&scored.pick(&all()).unwrap()), 3);
}

#[test]
fn one_root_with_zero_signal_lands_low_confidence_and_reports_every_residual_term() {
    let query = statement("unrelated lexer");
    let unique = Scored::new(vec![node(1, "/single", "parser")], &query);
    let decision = unique.pick(&all()).unwrap();
    assert_eq!(picked(&decision), 1);
    assert_eq!(decision.overlap, 0.0);
    assert_eq!(decision.confidence(), RouteConfidence::Low);
    assert_eq!(decision.residual, vec!["lexer", "unrelated"]);
    let nothing_admitted = unique.pick(&Scope {
        roots: RoutingRoots::Selected(vec![]),
        unmatched: None,
    });
    assert_eq!(
        nothing_admitted, None,
        "no admitted root and no unmatched topic"
    );
}

#[test]
fn unconsulted_origin_and_path_scores_cannot_invent_statement_confidence() {
    for query in [
        Query {
            statement: TermCounts::from_texts(["unrelated"]),
            origin: Some(TermCounts::from_texts(["compiler"])),
            tail: None,
        },
        statement("engine"),
    ] {
        let decision = Scored::new(vec![node(1, "/engine", "compiler")], &query)
            .pick(&all())
            .unwrap();
        assert_eq!(picked(&decision), 1);
        assert_eq!(decision.tier, RouteTier::Statement);
        assert_eq!(decision.confidence(), RouteConfidence::Low);
        assert_eq!(decision.overlap, 0.0);
    }
}

#[test]
fn an_empty_semantic_corpus_lands_on_the_declared_unmatched_topic_or_else_stays_undecided() {
    let scored = Scored::new(Vec::new(), &statement("compiler"));
    assert_eq!(scored.pick(&all()), None);
    let declared = scored.pick(&unmatched("/triage")).unwrap();
    assert_eq!(declared.target, Target::Unmatched(topic("/triage")));
    assert_eq!(declared.confidence(), RouteConfidence::Low);
    assert_eq!(declared.overlap, 0.0);
    assert_eq!(declared.residual, ["compiler"]);
    let composed = Scored::new(Vec::new(), &tailed("compiler", "render/css"))
        .pick(&unmatched("/triage"))
        .unwrap();
    assert_eq!(composed.target, Target::Unmatched(topic("/triage")));
    assert_eq!(composed.destination, topic("/triage/render/css"));
    assert_eq!(composed.destination_node, None);
    assert_eq!(composed.stop, RouteStop::Unmatched);
    assert_eq!(structure(&composed).connection, RouteConnection::Unresolved);
    assert_eq!(composed.residual, ["compiler"]);
}

#[test]
fn unresolved_disjoint_roots_land_on_the_unmatched_topic_outside_scoring() {
    let tied = Scored::new(
        vec![node(2, "/second", "memory"), node(1, "/first", "parser")],
        &statement("unrelated"),
    );
    assert_eq!(tied.pick(&all()), None, "without an unmatched topic");
    let decision = tied.pick(&unmatched("/triage")).unwrap();
    assert_eq!(decision.target, Target::Unmatched(topic("/triage")));
    assert_eq!(decision.destination, topic("/triage"));
    assert_eq!(decision.stop, RouteStop::Unmatched);
    assert_eq!(decision.tier, RouteTier::Path, "every tier was consulted");
    assert_eq!(decision.confidence(), RouteConfidence::Low);
    assert_eq!(decision.residual, vec!["unrelated"]);
    let positive_tie = Scored::new(
        vec![node(2, "/second", "parser"), node(1, "/first", "parser")],
        &statement("parser"),
    );
    assert_eq!(
        positive_tie.pick(&all()),
        None,
        "an equal positive overlap is still no evidence for either root"
    );
}

#[test]
fn tied_or_equal_children_stop_at_the_parent_with_a_low_confidence_reason() {
    let tied = Scored::new(
        vec![
            node(1, "/root", "parser memory"),
            node(2, "/root/alpha", "parser"),
            node(3, "/root/beta", "parser"),
        ],
        &statement("parser"),
    );
    let decision = tied.pick(&unmatched("/triage")).unwrap();
    assert_eq!(picked(&decision), 1);
    assert_eq!(decision.stop, RouteStop::ChildrenTied);
    assert_eq!(decision.confidence(), RouteConfidence::Low);
    let equal = Scored::new(
        vec![node(1, "/root", "parser"), node(2, "/root/child", "parser")],
        &statement("parser"),
    );
    let decision = equal.pick(&all()).unwrap();
    assert_eq!(picked(&decision), 1);
    assert_eq!(decision.stop, RouteStop::ChildrenEqual);
    assert_eq!(decision.confidence(), RouteConfidence::Low);
}

#[test]
fn origin_only_breaks_an_unresolved_statement_pass() {
    let nodes = || {
        vec![
            node(1, "/frontend", "layout css"),
            node(2, "/backend", "storage records"),
        ]
    };
    let secondary = Scored::new(
        nodes(),
        &Query {
            statement: TermCounts::from_texts(["unrelated"]),
            origin: Some(TermCounts::from_texts(["storage records"])),
            tail: None,
        },
    );
    let decision = secondary.pick(&all()).unwrap();
    assert_eq!(picked(&decision), 2);
    assert_eq!(decision.tier, RouteTier::Origin);
    assert_eq!(decision.confidence(), RouteConfidence::Secondary);
    let primary = Scored::new(
        nodes(),
        &Query {
            statement: TermCounts::from_texts(["layout"]),
            origin: Some(TermCounts::from_texts(["storage records"])),
            tail: None,
        },
    );
    let decision = primary.pick(&all()).unwrap();
    assert_eq!(
        picked(&decision),
        1,
        "the statement decided; origin is never consulted"
    );
    assert_eq!(decision.tier, RouteTier::Statement);
    assert_eq!(decision.confidence(), RouteConfidence::Evidenced);
}

#[test]
fn path_components_discriminate_only_after_statement_and_origin_cannot() {
    let scored = Scored::new(
        vec![
            node(1, "/engine/parser", "compiler"),
            node(2, "/engine/memory", "compiler"),
        ],
        &statement("parser"),
    );
    let decision = scored.pick(&all()).unwrap();
    assert_eq!(picked(&decision), 1);
    assert_eq!(decision.tier, RouteTier::Path);
    assert_eq!(decision.confidence(), RouteConfidence::Secondary);
    let described = Scored::new(
        vec![
            node(1, "/engine/parser", "compiler"),
            node(2, "/engine/memory", "parser tables"),
        ],
        &statement("parser"),
    );
    assert_eq!(
        picked(&described.pick(&all()).unwrap()),
        2,
        "a described overlap outranks a matching path component"
    );
}

#[test]
fn a_tail_matched_leaf_up_inside_the_decided_subtree_is_reused() {
    let scored = Scored::new(
        vec![
            node(1, "/app", "frontend"),
            node(2, "/app/render-pipeline", "render pipeline"),
            node(3, "/app/render-pipeline/css-tools", "stylesheet"),
            node(4, "/app/editor", "editor"),
        ],
        &tailed("render pipeline", "render-pipeline/css-tools"),
    );
    let decision = scored.pick(&all()).unwrap();
    assert_eq!(
        picked(&decision),
        2,
        "the statement explains /app/render-pipeline"
    );
    assert_eq!(decision.stop, RouteStop::ChildrenBelow);
    assert_eq!(
        decision.destination,
        topic("/app/render-pipeline/css-tools")
    );
    assert_eq!(decision.destination_node, Some(3));
    assert_eq!(
        *structure(&decision),
        Structure {
            prefix: Some(topic("/app/render-pipeline")),
            matched: Some(Pick {
                node: 3,
                topic: topic("/app/render-pipeline/css-tools"),
            }),
            matched_components: 2,
            connection: RouteConnection::Overlap { overlap: 1 },
            composed_overlap: 0,
            alternative: None,
        }
    );
}

#[test]
fn a_partial_tail_match_appends_its_remainder_and_reports_meet_or_gap() {
    let meet = Scored::new(
        vec![
            node(1, "/app", "frontend"),
            node(2, "/app/render", "render"),
            node(3, "/app/editor", "editor"),
        ],
        &tailed("frontend", "render/css"),
    )
    .pick(&all())
    .unwrap();
    assert_eq!(picked(&meet), 1);
    assert_eq!(meet.stop, RouteStop::ChildrenBelow);
    assert_eq!(meet.destination, topic("/app/render/css"));
    assert_eq!(meet.destination_node, None, "css is new");
    let evidence = structure(&meet);
    assert_eq!(evidence.prefix, Some(topic("/app")));
    assert_eq!(evidence.matched.as_ref().map(|pick| pick.node), Some(2));
    assert_eq!(evidence.matched_components, 1);
    assert_eq!(evidence.connection, RouteConnection::Meet);

    let gap = Scored::new(
        vec![
            node(1, "/app", "frontend"),
            node(2, "/app/render", "web"),
            node(3, "/app/web", "web"),
            node(4, "/app/web/render/css", "web"),
        ],
        &tailed("frontend", "render/css"),
    )
    .pick(&all())
    .unwrap();
    assert_eq!(picked(&gap), 1);
    assert_eq!(
        gap.destination,
        topic("/app/web/render/css"),
        "the full suffix outranks a longer-looking initial segment"
    );
    assert_eq!(gap.destination_node, Some(4));
    let evidence = structure(&gap);
    assert_eq!(evidence.matched_components, 2);
    assert_eq!(
        evidence.connection,
        RouteConnection::Gap {
            components: vec!["web".to_owned()],
        }
    );
}

#[test]
fn equally_supported_matches_compose_at_the_explained_prefix_and_stay_ambiguous() {
    let scored = Scored::new(
        vec![
            node(1, "/app", "frontend"),
            node(2, "/app/render", "render"),
            node(3, "/app/render/css", "render"),
            node(4, "/app/editor", "editor"),
            node(5, "/app/editor/css", "editor"),
        ],
        &tailed("frontend", "css"),
    );
    let decision = scored.pick(&all()).unwrap();
    assert_eq!(picked(&decision), 1);
    assert_eq!(decision.stop, RouteStop::ChildrenBelow);
    assert_eq!(decision.destination, topic("/app/css"));
    assert_eq!(decision.destination_node, None);
    let evidence = structure(&decision);
    assert_eq!(evidence.matched, None);
    assert_eq!(
        evidence.connection,
        RouteConnection::Ambiguous {
            candidates: vec![topic("/app/editor/css"), topic("/app/render/css")],
        }
    );
    assert_eq!(evidence.composed_overlap, 0);
}

#[test]
fn a_unique_structural_match_narrows_tied_roots_without_overriding_semantic_prefixes() {
    let nodes = || {
        vec![
            node(1, "/app", "x"),
            node(2, "/app/render", "x"),
            node(3, "/app/render/css", "x"),
            node(4, "/engine", "x"),
            node(5, "/engine/parser", "x"),
        ]
    };
    let narrowed = Scored::new(nodes(), &tailed("unrelated", "render/css"))
        .pick(&all())
        .unwrap();
    assert_eq!(picked(&narrowed), 3, "structure walked the tied roots down");
    assert_eq!(narrowed.destination, topic("/app/render/css"));
    assert_eq!(narrowed.tier, RouteTier::Path);
    assert_eq!(narrowed.overlap, 0.0, "structure is not statement cosine");
    assert_eq!(narrowed.confidence(), RouteConfidence::Low);
    let evidence = structure(&narrowed);
    assert_eq!(evidence.prefix, None, "nothing semantic chose");
    assert_eq!(
        evidence.connection,
        RouteConnection::Gap {
            components: vec!["app".to_owned()],
        }
    );

    let mut both = nodes();
    both.push(node(6, "/engine/render/css", "x"));
    let ambiguous = Scored::new(both, &tailed("unrelated", "render/css"))
        .pick(&unmatched("/triage"))
        .unwrap();
    assert_eq!(ambiguous.target, Target::Unmatched(topic("/triage")));
    assert_eq!(ambiguous.destination, topic("/triage/render/css"));
    assert_eq!(
        structure(&ambiguous).connection,
        RouteConnection::Ambiguous {
            candidates: vec![topic("/app/render/css"), topic("/engine/render/css")],
        }
    );

    let mut described = nodes();
    described[3] = node(4, "/engine", "parser");
    let semantic = Scored::new(described, &tailed("parser", "render/css"))
        .pick(&all())
        .unwrap();
    assert_eq!(
        picked(&semantic),
        4,
        "/engine's statement overlap wins the root"
    );
    assert_eq!(semantic.destination, topic("/engine/render/css"));
    let evidence = structure(&semantic);
    assert_eq!(evidence.prefix, Some(topic("/engine")));
    assert_eq!(evidence.connection, RouteConnection::Unresolved);
    assert_eq!(evidence.matched, None);
}

#[test]
fn a_relative_tail_spelling_an_existing_root_path_stays_relative_and_names_the_alternative() {
    let scored = Scored::new(
        vec![
            node(1, "/app", "frontend"),
            node(2, "/app/render", "frontend"),
            node(3, "/engine", "compiler"),
        ],
        &tailed("compiler", "app/render"),
    );
    let decision = scored.pick(&all()).unwrap();
    assert_eq!(picked(&decision), 3);
    assert_eq!(decision.destination, topic("/engine/app/render"));
    let evidence = structure(&decision);
    assert_eq!(evidence.connection, RouteConnection::Unresolved);
    assert_eq!(evidence.alternative, Some(topic("/app/render")));
    let chosen = Scored::new(
        vec![
            node(1, "/app", "frontend"),
            node(2, "/app/render", "frontend"),
        ],
        &tailed("frontend", "app/render"),
    )
    .pick(&all())
    .unwrap();
    assert_eq!(
        chosen.destination,
        topic("/app/render"),
        "/app's tail folds its own root"
    );
    assert_eq!(structure(&chosen).alternative, None);
}

#[test]
fn composition_folds_maximal_exact_boundary_overlap_and_never_a_private_owner() {
    let own = Scored::new(
        vec![node(1, "/app/css", "styles")],
        &tailed("styles", "css/css"),
    )
    .pick(&all())
    .unwrap();
    assert_eq!(own.destination, topic("/app/css/css"));
    assert_eq!(own.destination_node, None);
    assert_eq!(
        structure(&own).connection,
        RouteConnection::Overlap { overlap: 1 }
    );

    let private = Scored::new(Vec::new(), &tailed("styles", "task/css"))
        .unmatched_at(&topic("018f82d4-6f59-7d06-92e3-bce913116411/task"));
    assert_eq!(
        private.destination,
        topic("018f82d4-6f59-7d06-92e3-bce913116411/task/css")
    );
    assert_eq!(structure(&private).composed_overlap, 1);
    assert_eq!(private.stop, RouteStop::Unmatched);

    let whole = Scored::new(Vec::new(), &tailed("styles", "task")).unmatched_at(&topic("/task"));
    assert_eq!(
        whole.destination,
        topic("/task"),
        "zero remainder is a real result"
    );
    assert_eq!(structure(&whole).composed_overlap, 1);
    assert_eq!(structure(&whole).connection, RouteConnection::Unresolved);

    let refused = Scored::new(
        vec![
            node(1, "/engine", "compiler"),
            node(2, "/engine/render", "compiler"),
        ],
        &tailed("compiler", "render/css"),
    );
    let picked_first = refused.pick(&all()).unwrap();
    assert_eq!(picked_first.destination, topic("/engine/render/css"));
    let landed = refused.unmatched_at(&topic("/triage"));
    assert_eq!(landed.target, Target::Unmatched(topic("/triage")));
    assert_eq!(landed.destination, topic("/triage/render/css"));
    assert_eq!(structure(&landed).matched, None);
}

#[test]
fn private_jargon_cannot_win_or_disambiguate_public_placement() {
    let nodes = || {
        vec![
            node(1, "/frontend", "CSS LSP"),
            node(2, "/backend", "x86"),
            node(3, "018f82d4-6f59-7d06-92e3-bce913116411/task/a", "CSS p95"),
        ]
    };
    let public = Scored::new(nodes(), &statement("css"));
    assert_eq!(picked(&public.pick(&all()).unwrap()), 1);
    for query in ["p95", "--CSS 03Z 2026-11-06"] {
        let no_public_signal = Scored::new(nodes(), &statement(query));
        assert_eq!(no_public_signal.pick(&all()), None, "{query}");
    }
}

#[test]
fn drive_roots_follow_literal_prefixes_without_using_phase_or_runtime_admission() {
    let rules: crate::team_policy::Rules = wire::decode(
        br#"[
        {"topic":["/engine/*/parser","/storage/**"],"phase":["review"]},
        {"topic":["/engine/front-end"]}
    ]"#,
    )
    .unwrap();
    assert_eq!(
        rules.routing_roots().unwrap(),
        RoutingRoots::Selected(vec![
            "/engine".parse().unwrap(),
            "/engine/front-end".parse().unwrap(),
            "/storage".parse().unwrap()
        ])
    );
    let unrestricted: crate::team_policy::Rules = wire::decode(br#"[{"phase":[]}]"#).unwrap();
    assert_eq!(unrestricted.routing_roots().unwrap(), RoutingRoots::All);
    let empty: crate::team_policy::Rules = wire::decode(br#"[{"topic":[]}]"#).unwrap();
    assert_eq!(
        empty.routing_roots().unwrap(),
        RoutingRoots::Selected(vec![])
    );
}

#[test]
fn drive_globs_without_a_literal_prefix_keep_public_roots_reachable() {
    let scored = Scored::new(
        vec![
            node(1, "/engine/parser", "compiler parser"),
            node(2, "/storage/records", "durable storage"),
        ],
        &statement("compiler parser"),
    );
    for pattern in ["**", "**/parser", "{/engine,/storage}/**"] {
        let rules: crate::team_policy::Rules =
            wire::decode(format!(r#"[{{"topic":["{pattern}"]}}]"#).as_bytes()).unwrap();
        let scope = Scope {
            roots: rules.routing_roots().unwrap(),
            unmatched: None,
        };
        assert_eq!(picked(&scored.pick(&scope).unwrap()), 1, "{pattern}");
    }
}

#[test]
fn policy_unmatched_must_be_public_and_outside_oops() {
    let policy = |text: &str| -> Result<Scope, RouteError> {
        let policy: crate::team_policy::Policy = wire::decode(text.as_bytes()).unwrap();
        policy.routing_scope()
    };
    assert_eq!(
        policy(r#"{"drive":[{"topic":["/engine/**"]}],"unmatched":"/engine/triage-intake"}"#)
            .unwrap(),
        Scope {
            roots: RoutingRoots::Selected(vec!["/engine".parse().unwrap()]),
            unmatched: Some("/engine/triage-intake".parse().unwrap()),
        }
    );
    for invalid in [
        r#"{"unmatched":"/oops/triage"}"#,
        r#"{"unmatched":"018f82d4-6f59-7d06-92e3-bce913116411"}"#,
    ] {
        assert!(
            matches!(policy(invalid), Err(RouteError::InvalidUnmatched(_))),
            "{invalid}"
        );
    }
    assert!(
        wire::decode::<crate::team_policy::Config>(br#"{"default":{"unmatched":"/oops"}}"#)
            .is_err(),
        "an invalid unmatched topic refuses the whole configuration"
    );
}

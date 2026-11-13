use wire::TaskStamp;
use wire::intent::TASK_RELATION_ROOT_LIMIT;

use super::{
    AppRoute, ChatAnchor, CreatorContextTarget, EntryState, GraphView, HistoryEffect, ImageTarget,
    JobsView, NavigationCause, Presentation, RouteEvidence, TaskView, WorkLens, WorkOptions,
    WorkPlace, WorkRoute, WorkRouteTopic, WorkRow, WorkSelection, history_effect, image_path,
    normalize_app_location, opening_auth_token, parse_route, team_stream_card_target,
    team_stream_card_team,
};
use crate::work::WorkRelationshipDirection;

const CARD: &str = "message-agent-01234567-89ab-cdef-0123-456789abcdef-4-8-2-1";

fn stamp(index: i64) -> TaskStamp {
    TaskStamp::for_micros(1_800_000_000_000_000 + index).expect("fixture stamp")
}

fn route(path: &str) -> Option<AppRoute> {
    parse_route(path).map(|parsed| parsed.route)
}

fn task(task: TaskStamp, view: Option<TaskView>) -> AppRoute {
    AppRoute::work_place(WorkPlace::Task { task, view })
}

fn selection(tasks: impl IntoIterator<Item = TaskStamp>, related: Option<TaskStamp>) -> AppRoute {
    AppRoute::work_place(WorkPlace::Selection(
        WorkSelection::new(tasks, related).expect("fixture selection"),
    ))
}

/// A canonical location parses to `route`, spells back exactly, and opens
/// without a rewrite or notice.
fn assert_canonical(path: &str, route: &AppRoute) {
    assert_eq!(self::route(path).as_ref(), Some(route), "{path}");
    assert_eq!(route.path(), path);
    let location = normalize_app_location("", &format!("#{path}"));
    assert_eq!(location.route(), route, "{path}");
    assert!(!location.needs_replace(), "{path}");
    assert_eq!(location.notice(), None, "{path}");
}

#[test]
fn canonical_durable_routes_round_trip_exactly() {
    for (path, route) in [
        ("/chat", AppRoute::chat()),
        (
            "/chat/card/message-agent-01234567-89ab-cdef-0123-456789abcdef-4-8-2-1",
            AppRoute::card(CARD.to_owned()).expect("fixture card"),
        ),
        ("/spice", AppRoute::Spice),
    ] {
        assert_canonical(path, &route);
    }
}

#[test]
fn image_locators_are_opaque_canonical_route_state() {
    let route = AppRoute::card(CARD.to_owned()).expect("fixture card");
    let image = ImageTarget::new(format!("c:{CARD}:n:2")).expect("fixture image target");
    let path = image_path(&route, Some(&image));
    assert_eq!(path, format!("/chat/card/{CARD}?image=c%3A{CARD}%3An%3A2"));
    let location = normalize_app_location("", &format!("#{path}"));
    assert_eq!(location.route(), &route);
    assert_eq!(location.image(), Some(&image));
    assert!(!location.needs_replace());

    let reordered = normalize_app_location("", "#/work?image=t%3AdYKYdz5Dc%3An%3A0&depth=4");
    assert_eq!(
        image_path(reordered.route(), reordered.image()),
        "/work?depth=4&image=t%3AdYKYdz5Dc%3An%3A0"
    );
    assert!(reordered.needs_replace());
}

#[test]
fn canonical_work_routes_round_trip_exactly() {
    let topic = |name: &str| WorkRouteTopic::new(name.to_owned()).expect("fixture topic");
    let team = "01234567-89ab-cdef-0123-456789abcdef"
        .parse()
        .expect("fixture team");
    for (path, route) in [
        ("/work", WorkRoute::AllOpen),
        ("/work/all", WorkRoute::All),
        ("/work/topic/oops", WorkRoute::Topic(topic("/oops"))),
        (
            "/work/topic/oops/design",
            WorkRoute::Topic(topic("/oops/design")),
        ),
        ("/work/unplaced", WorkRoute::Unplaced),
        ("/work/planning", WorkRoute::Planning { topic: None }),
        ("/work/drafts", WorkRoute::Drafts),
        (
            "/work/planning/alpha_2",
            WorkRoute::Planning {
                topic: Some(topic("/alpha_2")),
            },
        ),
        (
            "/work/planning/app/work/map",
            WorkRoute::Planning {
                topic: Some(topic("/app/work/map")),
            },
        ),
        (
            "/work/team/01234567-89ab-cdef-0123-456789abcdef",
            WorkRoute::Team(team),
        ),
    ] {
        assert_canonical(path, &AppRoute::work(route));
    }
}

#[test]
fn task_and_selection_destinations_round_trip_exactly() {
    let (one, two, three) = (stamp(1), stamp(2), stamp(3));
    assert_canonical(&format!("/work/task/{one}"), &task(one.clone(), None));
    for view in [
        TaskView::Questions,
        TaskView::Description,
        TaskView::Acceptance,
        TaskView::Facts,
        TaskView::History,
    ] {
        assert_canonical(
            &format!("/work/task/{one}/{}", view.label()),
            &task(one.clone(), Some(view)),
        );
    }
    assert_canonical(
        &format!("/work/selection?task={three}&task={one}&task={two}"),
        &selection([three.clone(), one.clone(), two.clone()], None),
    );
    assert_canonical(
        &format!("/work/selection?task={one}&task={two}&related={two}"),
        &selection([one, two.clone()], Some(two)),
    );
}

#[test]
fn a_selection_keeps_every_root_beyond_enrichment_limits() {
    let roots = (0..3 * TASK_RELATION_ROOT_LIMIT)
        .rev()
        .map(|index| stamp(i64::try_from(index).expect("fixture index")))
        .collect::<Vec<_>>();
    let route = selection(roots.clone(), None);
    let path = route.path();
    assert_canonical(&path, &route);
    let reopened = self::route(&path).expect("selection reopens");
    let AppRoute::Work(location) = reopened else {
        panic!("selection is a Work route");
    };
    let WorkPlace::Selection(reopened) = location.place else {
        panic!("selection reopens as a selection");
    };
    assert_eq!(reopened.tasks(), roots.as_slice());
}

#[test]
fn work_adjustments_round_trip_and_omit_defaults() {
    let one = stamp(1);
    let adjusted = WorkOptions {
        filter: "ready & blocked/é".to_owned(),
        lens: WorkLens::Origin,
        direction: WorkRelationshipDirection::Prerequisites,
        depth: 4,
        controllers: false,
        drafts: false,
        settled: true,
    };
    assert_canonical(
        "/work?q=ready%20%26%20blocked%2F%C3%A9&lens=origin&direction=prerequisites&depth=4&controllers=0&drafts=0&settled=1",
        &AppRoute::work(WorkRoute::AllOpen).with_options(adjusted),
    );
    let shallow = WorkOptions {
        depth: 0,
        ..WorkOptions::default()
    };
    assert_canonical(
        "/work/drafts?depth=0",
        &AppRoute::work(WorkRoute::Drafts).with_options(shallow),
    );
    let review = WorkOptions {
        lens: WorkLens::Review,
        ..WorkOptions::default()
    };
    assert_canonical(
        &format!("/work/task/{one}/facts?lens=review"),
        &task(one.clone(), Some(TaskView::Facts)).with_options(review),
    );
    let dependents = WorkOptions {
        direction: WorkRelationshipDirection::Dependents,
        ..WorkOptions::default()
    };
    assert_canonical(
        &format!("/work/selection?task={one}&related={one}&direction=dependents"),
        &selection([one.clone()], Some(one)).with_options(dependents),
    );
    assert_canonical(
        "/work/all?direction=none&drafts=0",
        &AppRoute::work(WorkRoute::All).with_options(WorkOptions {
            direction: WorkRelationshipDirection::None,
            drafts: false,
            ..WorkOptions::default()
        }),
    );
    assert_eq!(
        AppRoute::work(WorkRoute::All)
            .with_options(WorkOptions::default())
            .path(),
        "/work/all"
    );
    assert_eq!(
        AppRoute::Spice.with_options(WorkOptions {
            settled: true,
            ..WorkOptions::default()
        }),
        AppRoute::Spice
    );
}

#[test]
fn noncanonical_spellings_replace_to_their_one_canonical_address() {
    let (one, two) = (stamp(1), stamp(2));
    for (hash, canonical) in [
        (
            format!("#/work/task/{one}/room"),
            format!("/work/task/{one}"),
        ),
        (
            format!("#/work/selection?task={two}&task={one}&task={two}"),
            format!("/work/selection?task={two}&task={one}"),
        ),
        (
            format!("#/work/selection?related={one}&task={two}"),
            format!("/work/selection?task={two}&task={one}&related={one}"),
        ),
        (
            "#/work?settled=1&lens=origin".to_owned(),
            "/work?lens=origin&settled=1".to_owned(),
        ),
        (
            "#/work?q=&lens=dependency&direction=both&depth=2&settled=0".to_owned(),
            "/work".to_owned(),
        ),
        ("#/work?".to_owned(), "/work".to_owned()),
        ("#/work?q=a+b".to_owned(), "/work?q=a%2Bb".to_owned()),
        ("#/work?q=x%2a".to_owned(), "/work?q=x%2A".to_owned()),
        (
            "#/work?depth=1&depth=3".to_owned(),
            "/work?depth=3".to_owned(),
        ),
    ] {
        let location = normalize_app_location("", &hash);
        assert_eq!(location.route().path(), canonical, "{hash}");
        assert!(location.needs_replace(), "{hash}");
        assert_eq!(location.notice(), None, "{hash}");
    }
}

#[test]
fn ignored_parts_keep_the_containing_route_and_are_named() {
    let (one, two) = (stamp(1), stamp(2));
    for (hash, kept, ignored) in [
        (
            "#/work?scope=all".to_owned(),
            "/work".to_owned(),
            "scope=all",
        ),
        (
            format!("#/work/task/{one}/bogus"),
            format!("/work/task/{one}"),
            "/bogus",
        ),
        (
            format!("#/work/task/{one}?task={two}"),
            format!("/work/task/{one}"),
            "task=",
        ),
        (
            format!("#/work/selection?task={one}&task=nope"),
            format!("/work/selection?task={one}"),
            "task=nope",
        ),
        (
            format!("#/work/selection?task={one}&related={one}&related={two}"),
            format!("/work/selection?task={one}&related={one}"),
            "related=",
        ),
        ("#/work?depth=5".to_owned(), "/work".to_owned(), "depth=5"),
        ("#/work?depth=02".to_owned(), "/work".to_owned(), "depth=02"),
        (
            "#/work?lens=claim".to_owned(),
            "/work".to_owned(),
            "lens=claim",
        ),
        (
            "#/work?direction=up".to_owned(),
            "/work".to_owned(),
            "direction=up",
        ),
        (
            "#/work?settled=yes".to_owned(),
            "/work".to_owned(),
            "settled=yes",
        ),
        ("#/work?q=%zz".to_owned(), "/work".to_owned(), "q=%zz"),
        ("#/work/all?flag".to_owned(), "/work/all".to_owned(), "flag"),
    ] {
        let location = normalize_app_location("", &hash);
        assert_eq!(location.route().path(), kept, "{hash}");
        assert!(location.needs_replace(), "{hash}");
        let notice = location.notice().expect("ignored input is named");
        assert!(notice.contains(ignored), "{hash}: {notice}");
    }
}

#[test]
fn team_stream_card_targets_name_one_canonical_team() {
    let team = "01234567-89ab-cdef-0123-456789abcdef"
        .parse()
        .expect("fixture team");
    let target = "team-stream-01234567-89ab-cdef-0123-456789abcdef";
    assert_eq!(team_stream_card_target(team), target);
    assert_eq!(team_stream_card_team(target), Some(team));
    for malformed in [
        "team-stream-",
        "team-stream-not-a-team",
        "team-stream-01234567-89ab-cdef-0123-456789abcdef-extra",
        "other-01234567-89ab-cdef-0123-456789abcdef",
    ] {
        assert_eq!(team_stream_card_team(malformed), None, "{malformed}");
    }
}

#[test]
fn only_resident_token_fragments_become_opening_credentials() {
    assert_eq!(
        opening_auth_token("#remote-token-0123456789"),
        Some("remote-token-0123456789")
    );
    for hash in [
        "",
        "#dash",
        "#/chat",
        "#/work",
        "#/spice",
        "#/chat/card/message-agent-01234567",
        "#short",
        "#remote/token/0123456789",
        &format!("#{}", "a".repeat(257)),
    ] {
        assert_eq!(opening_auth_token(hash), None, "{hash}");
    }
}

#[test]
fn malformed_locations_are_not_routes() {
    for path in [
        "/card/message-agent",
        "/chat/card/",
        "/chat/card/message/agent",
        "/chat/card/message_agent",
        "/chat/message-agent",
        "/chat?card=message-agent",
        "/dash/card/message-agent",
        "/spice/",
        "/spice/panel",
        "/spice?open=1",
        "/work/",
        "/work/topic",
        "/work/topic/",
        "/work/topic/Alpha",
        "/work/topic/a//b",
        "/work/topic/a/",
        "/work/topic/a.b",
        "/work/topic/a%2fb",
        "/work/project/a",
        "/work/stem/public/alpha",
        "/work/planning/alpha/../extra",
        "/work/team",
        "/work/team/01234567-89AB-CDEF-0123-456789ABCDEF",
        "/work/group",
        "/work/group/123456789",
        "/work/group/dVxHkZhQY",
        "/work/groups/dVxHkZhQY",
        "/work/task",
        "/work/task/",
        "/work/task/not-a-stamp",
        "/work/tasks/dVxHkZhQY",
        "/work/selection",
        "/work/selection/",
        "/work/selection?task=nope",
        "/work/selection?related=nope&q=x",
        "/work/unknown",
    ] {
        assert_eq!(route(path), None, "{path}");
    }
}

#[test]
fn app_locations_canonicalize_chat_spellings_without_a_notice() {
    for (hash, replace) in [
        ("#/chat", false),
        ("", true),
        ("#", true),
        ("#/", true),
        ("#chat", true),
        ("#//chat/", true),
    ] {
        let location = normalize_app_location("", hash);
        assert_eq!(location.route(), &AppRoute::chat(), "{hash}");
        assert_eq!(location.needs_replace(), replace, "{hash}");
        assert_eq!(location.notice(), None, "{hash}");
    }
}

#[test]
fn unknown_app_locations_open_chat_and_name_the_refused_input() {
    for (search, hash) in [
        ("?page=work", ""),
        ("?filter=ready", "#/work"),
        ("", "#/unknown"),
        ("", "#/work/"),
        ("", "#/work/topic"),
        ("", "#/work/topic/Alpha"),
        ("", "#/work/topic/a%2fb"),
        ("", "#/work/stem/public/alpha"),
        ("", "#/work/planning/alpha/../extra"),
        ("", "#/work/selection"),
        ("", "#/work/task/not-a-stamp"),
        ("", "#/card/message-agent"),
        ("", "#/work/public/a%2f"),
        ("", "#/work/team/not-a-team"),
        ("", "#/work/team/01234567-89AB-CDEF-0123-456789ABCDEF"),
        ("", "#/work/group/123456789"),
        ("", "#/work/teams/01234567-89ab-cdef-0123-456789abcdef"),
        ("", "#/spice?open=1"),
        ("", "#/dash"),
        ("", "#dash"),
        ("", "#//dash/"),
    ] {
        let location = normalize_app_location(search, hash);
        assert_eq!(location.route(), &AppRoute::chat());
        assert!(location.needs_replace());
        let notice = location.notice().expect("invalid input is named");
        assert!(notice.contains(&format!("{search}{hash}")), "{notice}");
    }
}

#[test]
fn a_recorded_design_link_replaces_to_its_exact_task_address() {
    let location = normalize_app_location("", "#/chat/design/dVxHkZhQY");
    let design = "dVxHkZhQY".parse::<TaskStamp>().expect("fixture stamp");
    assert_eq!(location.route(), &task(design, None));
    assert_eq!(location.route().path(), "/work/task/dVxHkZhQY");
    assert!(location.needs_replace());
    assert_eq!(location.notice(), None);

    for hash in [
        "#/chat/design/",
        "#/chat/design/not-a-stamp",
        "#/chat/design/dVxHkZhQY/extra",
    ] {
        let location = normalize_app_location("", hash);
        assert_eq!(location.route(), &AppRoute::chat(), "{hash}");
        assert!(location.notice().is_some(), "{hash}");
    }
}

#[test]
fn settled_missing_chat_card_rewrites_to_chat() {
    let card = AppRoute::card(CARD.to_owned()).expect("fixture card");
    assert_eq!(card.unavailable(RouteEvidence::default()), None);
    let (next, notice) = card
        .unavailable(RouteEvidence { card: Some(false) })
        .expect("missing card refuses");
    assert_eq!(next, AppRoute::chat());
    assert!(notice.contains(CARD), "{notice}");
}

#[test]
fn settled_missing_tasks_refuse_without_widening_a_selection() {
    let (one, two, three) = (stamp(1), stamp(2), stamp(3));
    let options = WorkOptions {
        depth: 3,
        ..WorkOptions::default()
    };
    let gone = |missing: &[&TaskStamp]| {
        let missing = missing
            .iter()
            .map(|task| (*task).clone())
            .collect::<Vec<_>>();
        move |task: &TaskStamp| missing.contains(task)
    };

    let single = task(one.clone(), Some(TaskView::Facts)).with_options(options.clone());
    assert_eq!(single.without_missing_tasks(gone(&[&two])), None);
    let (next, notice) = single
        .without_missing_tasks(gone(&[&one]))
        .expect("a missing task refuses");
    assert_eq!(
        next,
        AppRoute::work(WorkRoute::AllOpen).with_options(options.clone())
    );
    assert!(notice.contains(one.as_str()), "{notice}");

    let selected = selection(
        [one.clone(), two.clone(), three.clone()],
        Some(three.clone()),
    )
    .with_options(options.clone());
    assert_eq!(selected.without_missing_tasks(gone(&[])), None);
    let (next, notice) = selected
        .without_missing_tasks(gone(&[&two]))
        .expect("a missing root refuses");
    assert_eq!(
        next,
        selection([one.clone(), three.clone()], Some(three.clone())).with_options(options.clone())
    );
    assert!(notice.contains(two.as_str()), "{notice}");
    let (next, _) = selected
        .without_missing_tasks(gone(&[&three]))
        .expect("a missing lead refuses");
    assert_eq!(
        next,
        selection([one.clone(), two.clone()], None).with_options(options)
    );
    let (next, notice) = selected
        .without_missing_tasks(gone(&[&one, &two, &three]))
        .expect("an emptied selection refuses");
    assert_eq!(next, AppRoute::chat());
    assert!(notice.contains(one.as_str()), "{notice}");

    for route in [
        AppRoute::chat(),
        AppRoute::Spice,
        AppRoute::work(WorkRoute::AllOpen),
    ] {
        assert_eq!(route.without_missing_tasks(|_| true), None);
    }
}

#[test]
fn only_a_new_reader_destination_adds_a_history_entry() {
    let (one, two) = (stamp(1), stamp(2));
    let chat = AppRoute::chat();
    let first = task(one.clone(), None);
    let filtered = AppRoute::work(WorkRoute::AllOpen).with_options(WorkOptions {
        filter: "ready".to_owned(),
        ..WorkOptions::default()
    });
    for (current, next) in [
        (&chat, &first),
        (&first, &task(one.clone(), Some(TaskView::Facts))),
        (&first, &selection([one.clone(), two.clone()], Some(two))),
        (&first, &AppRoute::Spice),
        (&AppRoute::Spice, &chat),
        (&filtered, &AppRoute::work(WorkRoute::All)),
    ] {
        assert_eq!(
            history_effect(current, next, NavigationCause::Destination),
            HistoryEffect::Push,
            "{} -> {}",
            current.path(),
            next.path()
        );
        for cause in [NavigationCause::Adjustment, NavigationCause::Normalization] {
            assert_eq!(
                history_effect(current, next, cause),
                HistoryEffect::Replace,
                "{cause:?}"
            );
        }
    }
    for cause in [
        NavigationCause::Destination,
        NavigationCause::Adjustment,
        NavigationCause::Normalization,
    ] {
        assert_eq!(
            history_effect(&first, &first.clone(), cause),
            HistoryEffect::Unchanged
        );
        assert_eq!(
            history_effect(&first, &task(one.clone(), Some(TaskView::Room)), cause),
            HistoryEffect::Unchanged,
            "the room is the task's base address"
        );
    }
}

#[test]
fn creator_context_targets_round_trip_the_exact_source_boundary() {
    let lane = "01234567-89ab-cdef-0123-456789abcdef"
        .parse()
        .expect("fixture lane");
    let source = "fedcba98-7654-3210-fedc-ba9876543210"
        .parse()
        .expect("fixture source");
    let context = wire::body::TaskCreationContext::Transcript {
        session: "session".to_owned(),
        boundary: wire::TranscriptSourceHead {
            lane,
            vendor: "omp".parse().expect("fixture vendor"),
            stream: wire::body::TASK_CREATION_CONTEXT_STREAM.to_owned(),
            source,
            next: wire::body::TranscriptPosition {
                member: 17,
                offset: 4096,
            },
        },
        creator_team: None,
    };
    let expected = concat!(
        "creator-context-01234567-89ab-cdef-0123-456789abcdef-",
        "fedcba98-7654-3210-fedc-ba9876543210-17-4096",
    );
    let target = CreatorContextTarget::from_creation(&context).expect("transcript context links");

    assert_eq!(target.target(), expected);
    assert_eq!(CreatorContextTarget::parse(expected), Some(target.clone()));
    assert!(AppRoute::card(expected.to_owned()).is_some());
    let locus = |member, offset| wire::MessageLocus {
        member,
        offset,
        length: 1,
        event: 0,
    };
    assert!(target.owns_source_parts(lane, Some(source)));
    assert!(!target.owns_source_parts(lane, None));
    assert!(
        !target.owns_source_parts(
            lane,
            Some(
                "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
                    .parse()
                    .expect("other fixture source"),
            ),
        )
    );
    assert!(target.contains_locus(lane, locus(17, 4095)));
    assert!(target.contains_locus(lane, locus(16, u64::MAX)));
    assert!(!target.contains_locus(lane, locus(17, 4096)));
    assert!(!target.contains_locus(lane, locus(18, 0)));
    assert!(
        CreatorContextTarget::from_creation(&wire::body::TaskCreationContext::unavailable(
            wire::body::TaskCreationUnavailable::PrincipalActor,
        ),)
        .is_none()
    );
}

#[test]
fn entry_state_round_trips_presentation_and_keeps_authority_out() {
    let mut entry = EntryState::new("owner", 3, Some(1), "/work/task/dY1".to_owned());
    entry.presentation = Presentation {
        row: Some(WorkRow::Open(stamp(7))),
        panel: Some(240.5),
        chat: vec![ChatAnchor {
            plane: "agent-1".to_owned(),
            card: CARD.to_owned(),
            offset: -12.0,
            focus: true,
        }],
        graph: Some(GraphView {
            zoom: 1.5,
            scroll: (40.0, 8.0),
        }),
        nodes: Some(240),
        jobs: Some(JobsView {
            session: "epoch".to_owned(),
            target: Some("job-1-2-3".to_owned()),
            filter: "service".to_owned(),
            query: "cargo".to_owned(),
        }),
        focus: Some("work-row:dY1".to_owned()),
    };
    let encoded = entry.encode();
    assert_eq!(EntryState::decode(&encoded, "owner"), Some(entry.clone()));
    let value: serde_json::Value = serde_json::from_str(&encoded).expect("entry JSON");
    let keys = |value: &serde_json::Value| {
        value
            .as_object()
            .expect("JSON object")
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
    };
    assert_eq!(
        keys(&value),
        [
            "version",
            "owner",
            "position",
            "opener",
            "path",
            "presentation"
        ]
        .map(str::to_owned)
        .into()
    );
    assert_eq!(
        keys(&value["presentation"]),
        ["row", "panel", "chat", "graph", "nodes", "jobs", "focus"]
            .map(str::to_owned)
            .into()
    );
    assert_eq!(
        value["presentation"]["row"],
        serde_json::json!({ "open": stamp(7) })
    );
}

#[test]
fn entry_state_discards_other_owners_and_versions() {
    let entry = EntryState::new("owner", 0, None, "/".to_owned());
    let encoded = entry.encode();
    assert_eq!(EntryState::decode(&encoded, "owner"), Some(entry));
    assert_eq!(EntryState::decode(&encoded, "other"), None);
    let mut value: serde_json::Value = serde_json::from_str(&encoded).expect("entry JSON");
    value["version"] = serde_json::json!(1);
    assert_eq!(EntryState::decode(&value.to_string(), "owner"), None);
    assert_eq!(EntryState::decode("{\"owner\":\"owner\"}", "owner"), None);
    assert_eq!(EntryState::decode("not json", "owner"), None);
}

#[test]
fn empty_presentation_is_omitted_and_absent_fields_default() {
    let entry = EntryState::new("owner", 2, None, "/spice".to_owned());
    let value: serde_json::Value = serde_json::from_str(&entry.encode()).expect("entry JSON");
    assert!(value.get("presentation").is_none());
    assert!(value.get("opener").is_none());
    let sparse = r#"{"version":2,"owner":"owner","position":2,"path":"/spice","presentation":{"row":"closed","nodes":80}}"#;
    let decoded = EntryState::decode(sparse, "owner").expect("sparse entry");
    assert_eq!(
        decoded.presentation,
        Presentation {
            row: Some(WorkRow::Closed),
            nodes: Some(80),
            ..Presentation::default()
        }
    );
}

#[test]
fn plane_anchor_replaces_its_predecessor() {
    let anchor = |card: &str, offset| ChatAnchor {
        plane: "agent-1".to_owned(),
        card: card.to_owned(),
        offset,
        focus: false,
    };
    let mut presentation = Presentation::default();
    presentation.anchor(anchor("first", 0.0));
    presentation.anchor(ChatAnchor {
        plane: "agent-2".to_owned(),
        ..anchor("other", 4.0)
    });
    presentation.anchor(anchor("second", 9.0));
    assert_eq!(
        presentation
            .chat
            .iter()
            .map(|anchor| (anchor.plane.as_str(), anchor.card.as_str()))
            .collect::<Vec<_>>(),
        [("agent-2", "other"), ("agent-1", "second")]
    );
}

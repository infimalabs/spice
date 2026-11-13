use super::*;

fn uuid(value: u8) -> CanonicalUuid {
    format!("00000000-0000-4000-8000-{value:012x}")
        .parse()
        .expect("fixture uuid is canonical")
}

/// A running OMP member of `team`, its own flag set by `fast`.
fn member(lane: u8, team: u8, fast: bool) -> wire::LaneResidency {
    wire::LaneResidency {
        lane: uuid(lane),
        team: uuid(team),
        name: None,
        worktree: format!("/fleet/main-{lane}"),
        life: "drive".to_owned(),
        vendor: "omp".parse().expect("fixture vendor"),
        model: None,
        effort: None,
        fast_mode: fast,
        occupant: Some(uuid(100 + lane)),
        lineage: Vec::new(),
        capabilities: Vec::new(),
        session: None,
        disposition: "running".to_owned(),
    }
}

/// Team 1 (lanes 1, 2) is TURBO; team 3 (lanes 3, 4) is not, though lane 4
/// sets its own flag.
fn fleet() -> wire::ResidencyHead {
    wire::ResidencyHead {
        epoch: uuid(99),
        state: wire::ResidentState::Seated,
        topology_revision: 0,
        lanes: vec![
            member(1, 1, false),
            member(2, 1, false),
            member(3, 3, false),
            member(4, 3, true),
        ],
        subscriptions: [(1, true), (3, false)]
            .into_iter()
            .map(|(team, fast_mode)| wire::TeamSubscription {
                team: uuid(team),
                formed_at: Some(
                    "2030-04-12T18:44:09.271Z"
                        .parse()
                        .expect("fixture formation instant"),
                ),
                drive: true,
                drain: false,
                name: None,
                fast_mode,
            })
            .collect(),
        renewals: Vec::new(),
    }
}

fn owed(flips: Vec<FastFlip>) -> Vec<(CanonicalUuid, CanonicalUuid, LiveRetune)> {
    flips
        .into_iter()
        .map(|flip| (flip.lane, flip.occupant, flip.live))
        .collect()
}

fn fast(lane: u8, enabled: bool) -> (CanonicalUuid, CanonicalUuid, LiveRetune) {
    (uuid(lane), uuid(100 + lane), LiveRetune::Fast { enabled })
}

#[test]
fn a_global_fast_mode_change_owes_only_the_members_it_flips() {
    let fleet = fleet();

    assert_eq!(
        owed(fast_flips(&fleet, &fleet, true, false)),
        vec![fast(3, false)],
        "global off slows only the member no lane or team flag keeps fast"
    );
    assert_eq!(
        owed(fast_flips(&fleet, &fleet, false, true)),
        vec![fast(3, true)],
        "global on speeds only the member not already fast"
    );
    assert!(owed(fast_flips(&fleet, &fleet, true, true)).is_empty());
}

fn session(model: &str, fast: bool, refusal: Option<&str>) -> wire::LaneSession {
    let mut launch = wire::body::LaneLaunchIdentity::new(model, "high").with_fast_mode(fast);
    launch.fast_refusal = refusal.map(str::to_owned);
    wire::LaneSession {
        lane: uuid(1),
        occupant: uuid(101),
        worktree: "/fleet/main-1".to_owned(),
        vendor: "omp".parse().expect("fixture vendor"),
        launch,
        epoch: uuid(99),
        stream: "stdout".to_owned(),
        identity: "private-session".to_owned(),
        locus: wire::TapeLocus {
            member: 1,
            offset: 2,
            length: 3,
        },
    }
}

#[test]
fn a_seat_keeps_its_fast_mode_refusal_across_launches_until_fast_or_the_model_changes() {
    const REFUSED: &str = "400 'priority' is not supported for 'service_tier' on this model";
    let refused = || {
        let mut row = member(1, 1, true);
        row.session = Some(session("mockgw/gpt-5.4", false, Some(REFUSED)));
        row
    };
    let [off, on] = [false, true].map(|fast_mode| LaneFastRetune {
        topology_revision: 0,
        lane: uuid(1),
        fast_mode,
    });

    let mut row = refused();
    seat_session(&mut row, session("mockgw/gpt-5.4", false, None));
    assert!(
        row.refuses_fast_mode(),
        "the next launch of the same model inherits the refusal"
    );
    seat_session(&mut row, session("openai/gpt-5.5", false, None));
    assert!(
        !row.refuses_fast_mode(),
        "a launch of another model starts without it"
    );

    let mut row = refused();
    Selection::Fast(&off).replace_seat(&mut row);
    assert!(row.refuses_fast_mode(), "turning fast mode off keeps it");
    Selection::Fast(&on).replace_seat(&mut row);
    assert!(!row.refuses_fast_mode(), "turning fast mode on forgets it");

    let mut row = refused();
    Selection::Model(&LaneRetune {
        topology_revision: 0,
        lane: uuid(1),
        model: Some("openai/gpt-5.5".to_owned()),
    })
    .replace_seat(&mut row);
    assert!(!row.refuses_fast_mode(), "selecting a model forgets it");
}

#[test]
fn a_live_model_change_lands_the_lanes_fast_mode_on_its_new_model() {
    const NO_TIER: &str = "Fast mode is unavailable for the current model.";
    let fleet = fleet();
    for (lane, global, effective) in [
        (1, false, true),
        (3, false, false),
        (3, true, true),
        (4, false, true),
    ] {
        let request = LaneRetune {
            topology_revision: 0,
            lane: uuid(lane),
            model: Some("google/gemini-3-pro".to_owned()),
        };
        let (flips, restate) = owed_fast(Selection::Model(&request), &fleet, &fleet, Some(global));
        assert!(flips.is_empty(), "a model change flips no fast mode");
        assert_eq!(
            restate,
            Some(effective),
            "lane {lane} restates its fast mode across lane, team, and global flags"
        );
    }

    let google = LiveRetune::Model {
        provider: "google".to_owned(),
        model: "gemini-3-pro".to_owned(),
    };
    let land = |running_fast: bool, answer: std::io::Result<LiveRetuneResult>| {
        let mut row = member(1, 1, true);
        row.session = Some(session("anthropic/claude-opus-5", running_fast, None));
        let (fast, refusal) = restated(true, answer);
        let changes = [(&google, None), (&fast, refusal.as_deref())];
        assert!(apply_runtime_changes(&mut row, &changes));
        let fact = runtime_fact(uuid(1), uuid(101), &changes);
        let launch = row.session.expect("running session").launch;
        assert_eq!(launch.model, "google/gemini-3-pro");
        assert_eq!(fact.model.as_deref(), Some("google/gemini-3-pro"));
        assert_eq!(
            fact.fast_mode,
            Some(launch.fast_mode),
            "one fact carries both"
        );
        assert_eq!(fact.fast_refusal, launch.fast_refusal);
        (launch.fast_mode, launch.fast_refusal)
    };

    assert_eq!(
        land(false, Ok(LiveRetuneResult::Applied)),
        (true, None),
        "the new family runs the lane's fast mode, not the tier it launched with"
    );
    assert_eq!(
        land(
            false,
            Ok(LiveRetuneResult::Refused {
                detail: NO_TIER.to_owned()
            })
        ),
        (false, Some(NO_TIER.to_owned())),
        "a model without a priority tier runs without it, in OMP's words, though it was already off"
    );
    for answer in [
        Ok(LiveRetuneResult::Refused {
            detail: "RPC session unavailable".to_owned(),
        }),
        Err(std::io::Error::other("no answer")),
    ] {
        let (fast_mode, refusal) = land(true, answer);
        assert!(
            !fast_mode && refusal.is_some_and(|refusal| refusal != NO_TIER),
            "another failure leaves fast mode unconfirmed rather than applied"
        );
    }
}

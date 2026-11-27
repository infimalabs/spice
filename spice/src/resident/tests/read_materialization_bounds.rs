//! Incremental catch-up bounds for resident read generations.

use super::*;

fn seeded_reads(
    name: &str,
    budget: journal::FactReadBudget,
) -> (
    PathBuf,
    RuntimePaths,
    Bus,
    read_materializations::CurrentReads,
) {
    let (root, paths) = resident_fixture(name);
    let mut journal = journal::Journal::open(&paths.journal).expect("materializer journal opens");
    let mut seed = write(Actor::TEST_PRINCIPAL, test_uuid(61), Producer::CommandDoor)
        .expect("seed write starts");
    seed.fact(
        wire::body::TaskWoken {
            task: "1kKRRsDPx".parse().expect("fixture task"),
        },
        FactCause::Root,
    )
    .expect("seed fact builds");
    journal.apply(seed).expect("seed fact commits");
    let bus = Bus::start(journal).expect("materializer bus starts");
    let policy =
        consent::Policy::new(consent::Answer::Queue, Vec::new()).expect("fixture consent policy");
    let reads =
        read_materializations::CurrentReads::start_with_budget(&bus, &paths, &policy, budget)
            .expect("bounded materializer starts");
    reads.current().expect("initial generation settles");
    (root, paths, bus, reads)
}

fn append_facts(bus: &Bus, count: usize) -> wire::Seq {
    let mut delta = write(Actor::TEST_PRINCIPAL, test_uuid(62), Producer::CommandDoor)
        .expect("delta write starts");
    for _ in 0..count {
        delta
            .fact(
                wire::body::TaskWoken {
                    task: "1kKRRsDPx".parse().expect("fixture task"),
                },
                FactCause::Root,
            )
            .expect("delta fact builds");
    }
    match bus.commit(delta).expect("delta commit answers") {
        bus::CommitOutcome::Committed(committed) => {
            wire::Seq::new(committed.last()).expect("committed range ends positively")
        }
        other => panic!("delta did not commit: {other:?}"),
    }
}

#[test]
fn current_generation_crosses_a_large_gap_one_configured_page_at_a_time() {
    let budget = journal::FactReadBudget::new(2, journal::MAX_RETAINED_FACT_PAGE_BYTES)
        .expect("two-row repository budget");
    let (root, _paths, bus, reads) = seeded_reads("bounded-current-catch-up", budget);
    let authority = append_facts(&bus, 7);

    reads.refresh().expect("catch-up refresh queues");
    let generation = reads.current().expect("bounded pages reach authority");

    assert_eq!(generation.cursor(), authority);
    assert_eq!(generation.materialization_steps(), 4);
    drop(reads);
    bus.shutdown().expect("bounded bus stops");
    fs::remove_dir_all(root).expect("bounded fixture cleanup");
}

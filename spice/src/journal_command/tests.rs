use super::{Failure, Span, newest_rows};

const FACTS: u64 = 7;

fn position(ordinal: u64) -> u64 {
    wire::Seq::FIRST.get() + ordinal - 1
}

#[test]
fn a_start_at_the_horizon_reads_from_the_newest_fact() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.page(position(FACTS), 3),
        [position(7), position(6), position(5)]
    );
    assert_eq!(
        fixture.page(position(FACTS), 20),
        (1..=FACTS).rev().map(position).collect::<Vec<_>>()
    );
}

#[test]
fn an_older_start_is_its_page_s_first_row() {
    let fixture = Fixture::new();
    assert_eq!(fixture.page(position(4), 2), [position(4), position(3)]);
    assert_eq!(fixture.page(position(1), 3), [position(1)]);
}

#[test]
fn continuations_visit_every_fact_once_newest_first() {
    let fixture = Fixture::new();
    // The CLI retains one row past its limit and resumes inclusively there.
    let limit = 2;
    let mut start = Some(position(FACTS));
    let mut visited: Vec<u64> = Vec::new();
    while let Some(from) = start {
        let page = fixture.page(from, limit + 1);
        visited.extend(page.iter().take(limit));
        start = page.get(limit).copied();
    }
    assert_eq!(visited, (1..=FACTS).rev().map(position).collect::<Vec<_>>());
}

#[test]
fn a_journal_read_spans_its_facts_from_the_first_position() {
    let fixture = Fixture::new();
    assert_eq!(fixture.span(None), Some((FACTS, FACTS)));
    let older = wire::Seq::new(position(3)).expect("seq").to_string();
    assert_eq!(fixture.span(Some(&older)), Some((3, FACTS)));
    let below = wire::Seq::new(position(1) - 1).expect("seq").to_string();
    assert_eq!(
        fixture.span(Some(&below)),
        None,
        "a start before the journal refuses"
    );
}

struct Fixture {
    journal: journal::Journal,
}

impl Fixture {
    fn new() -> Self {
        let mut journal = journal::Journal::in_memory().expect("fixture journal opens");
        for offset in 0..FACTS {
            let mut write = journal::JournalWrite::new(
                wire::Actor::TEST_PRINCIPAL,
                format!("taste:page-{offset}")
                    .parse()
                    .expect("fixture origin"),
                "2030-04-12T18:44:09.271Z".parse().expect("fixture instant"),
                wire::Producer::ResidentRecognition,
            );
            write
                .fact(
                    wire::body::NarrationCleared {
                        lane: "018f82d4-6f59-7d06-92e3-bce9131163c2"
                            .parse()
                            .expect("fixture lane"),
                        occupant: "018f82d4-6f59-7d06-92e3-bce9131163c4"
                            .parse()
                            .expect("fixture occupant"),
                        moments: 1,
                        prods: 1,
                        relief: wire::body::NarrationRelief::Prose,
                    },
                    journal::FactCause::Root,
                )
                .expect("fixture fact builds");
            journal.apply(write).expect("fixture fact commits");
        }
        Self { journal }
    }

    fn page(&self, start: u64, retain: usize) -> Vec<u64> {
        self.journal
            .read(|read| {
                let horizon = read.cursor()?.expect("fixture journal has a cursor");
                assert_eq!(horizon.get(), position(FACTS));
                let start = wire::Seq::new(start).expect("fixture start");
                Ok(newest_rows(read, start, horizon, retain, 64))
            })
            .expect("fixture snapshot reads")
            .unwrap_or_else(|_| panic!("newest-first page from {start} reads"))
            .into_iter()
            .map(|(row, key)| {
                assert_eq!(key, row.seq.to_string());
                row.seq.get()
            })
            .collect()
    }

    /// Facts at or before the read's start and in the whole journal, or none
    /// when `after` names no position inside the journal.
    fn span(&self, after: Option<&str>) -> Option<(u64, u64)> {
        self.journal
            .read(|read| {
                Ok(Span::read(read, after, |_| {
                    Failure::protocol("journal::fixture-refused", String::new())
                }))
            })
            .expect("fixture snapshot reads")
            .ok()
            .map(|span| (span.population(span.start), span.population(span.horizon)))
    }
}

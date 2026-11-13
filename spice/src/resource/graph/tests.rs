use super::*;
use crate::resource::{Request as ResourceRequest, Resource};

fn query(url: &str) -> Query {
    let ResourceRequest {
        resource: Resource::Read(query),
        ..
    } = ResourceRequest::parse(url).expect("graph URL")
    else {
        panic!("graph read required");
    };
    query
}

fn next(document: &str) -> Option<Query> {
    document
        .lines()
        .last()?
        .rsplit_once("; continue: ")
        .map(|(_, url)| query(url))
}

fn entries() -> Vec<board::Entry> {
    (1..=8)
        .map(|index| {
            let born = wire::TaskStamp::for_micros(index).expect("stamp");
            let phase = "todo".parse::<wire::Phase>().expect("phase");
            let statement = wire::TaskHead {
                preparation: None,
                intent: None,
                born: born.clone(),
                topic_origin: Seq::new(1).expect("sequence"),
                title: format!("Graph node {index}"),
                pitch: "A graph needs nodes to draw.".to_owned(),
                description: None,
                term: Vec::new(),
                requirements: Default::default(),
                resources: Default::default(),
                topic: "/board/graph".parse().expect("fixture topic"),
                priority: None,
                taste: None,
                dependencies: if index > 1 {
                    vec![wire::TaskStamp::for_micros(index - 1).expect("dependency")]
                } else {
                    Vec::new()
                },
                provenance: format!("task:{born}").parse().expect("origin"),
                creation: wire::body::TaskCreationContext::unavailable(
                    wire::body::TaskCreationUnavailable::SpiceActor,
                ),
                revision: Seq::new(1).expect("sequence"),
            };
            let workflow = wire::WorkflowHead {
                flow: vec![wire::Phase::Draft, phase],
                phase,
                status: wire::WorkflowStatus::pending(),
                deferral: None,
                submitted: false,
                generation: 0,
            };
            board::Entry::new(
                statement,
                workflow,
                wire::CompletionHead::opened(wire::Seq::new(1).expect("fixture round")),
                Vec::new(),
            )
            .expect("entry")
        })
        .collect()
}

#[test]
fn native_graph_pages_cover_every_byte_of_each_view_under_each_bound() {
    let entries = entries();
    for view in board::View::ALL {
        let original = board::graph::render(view, &entries, &[]).text().to_owned();
        let expected = original.lines().collect::<Vec<_>>();
        for bounds in [
            "limit=0",
            "limit=1",
            "max-bytes=80",
            "max-text=7",
            "max-bytes=1&max-text=1",
        ] {
            let pages = Pages::default();
            let mut requested = query(&format!("spice://board/graph/{}?{bounds}", view.as_str()));
            let mut document = pages
                .read(&requested, || {
                    Ok((Seq::new(1).expect("sequence"), original.clone()))
                })
                .expect("first page");
            let mut reconstructed = vec![String::new(); expected.len()];
            let mut completed_lines = 0;
            for _ in 0..original.len() + expected.len() + 1 {
                for row in document
                    .lines()
                    .skip(1)
                    .filter(|row| !row.starts_with("withheld "))
                {
                    let (identity, clipped) = row.split_once(' ').expect("fragment identity");
                    let (line, offset) = identity.split_once(':').expect("line and offset");
                    let line = line.parse::<usize>().expect("line") - 1;
                    let offset = offset.parse::<usize>().expect("offset");
                    let text = clipped.split_once("… [+").map_or(clipped, |(text, _)| text);
                    assert_eq!(line, completed_lines, "rows remain in diagram order");
                    assert_eq!(
                        offset,
                        reconstructed[line].len(),
                        "no gap or duplicate bytes"
                    );
                    reconstructed[line].push_str(text);
                    if reconstructed[line] == expected[line] {
                        completed_lines += 1;
                    }
                }
                let Some(continuation) = next(&document) else {
                    break;
                };
                requested = continuation;
                document = pages
                    .read(&requested, || {
                        Ok((
                            Seq::new(2).expect("changed sequence"),
                            "changed board".to_owned(),
                        ))
                    })
                    .expect("snapshot continuation");
            }
            assert!(next(&document).is_none(), "paging finishes");
            assert_eq!(
                reconstructed, expected,
                "one validated diagram survives board changes"
            );
        }
    }
}

#[test]
fn graph_cursors_bind_the_read_and_survive_other_first_pages() {
    let pages = Pages::default();
    let initial = query("spice://board/graph/workflow?limit=1");
    let first = pages
        .read(&initial, || {
            Ok((
                Seq::new(1).expect("sequence"),
                "workflow\nold first\nold second".to_owned(),
            ))
        })
        .expect("page");
    let resumed = next(&first).expect("continuation");
    let mut archive = initial.clone();
    archive.options.push(("all".to_owned(), None));
    let archived = pages
        .read(&archive, || {
            Ok((
                Seq::new(1).expect("sequence"),
                "workflow\nold first\nold second".to_owned(),
            ))
        })
        .expect("archive page");
    let archived = next(&archived).expect("archive continuation");
    pages
        .read(&archived, || unreachable!())
        .expect("identical live and archive text keeps distinct cursors");
    pages
        .read(&initial, || {
            Ok((
                Seq::new(2).expect("sequence"),
                "workflow\nnew first\nnew second".to_owned(),
            ))
        })
        .expect("new snapshot");
    let second = pages
        .read(&resumed, || Err("no fresh board needed".to_owned()))
        .expect("old snapshot survives");
    assert!(second.lines().any(|line| line.ends_with("old first")));
    let mut other_view = resumed.clone();
    other_view.shape = Shape::Graph {
        view: "origin".to_owned(),
    };
    assert!(pages.read(&other_view, || unreachable!()).is_err());
    let mut other_barrier = resumed.clone();
    set(&mut other_barrier, "at", "another".to_owned());
    assert!(pages.read(&other_barrier, || unreachable!()).is_err());
    let mut other_scope = resumed;
    other_scope.options.push(("all".to_owned(), None));
    assert!(pages.read(&other_scope, || unreachable!()).is_err());
}

#[test]
fn malformed_or_foreign_snapshot_cursors_refuse_instead_of_reading_new_state() {
    let pages = Pages::default();
    let first = pages
        .read(&query("spice://board/graph/workflow?max-text=1"), || {
            Ok((Seq::new(1).expect("sequence"), "équipe".to_owned()))
        })
        .expect("page");
    let resumed = next(&first).expect("Unicode requires a wider text bound");
    assert_eq!(option(&resumed, "max-text"), Some("2"));
    assert!(Pages::default().read(&resumed, || unreachable!()).is_err());
    for after in ["broken", "unknown:0:0", "1:unknown:0:0"] {
        let mut malformed = resumed.clone();
        set(&mut malformed, "after", after.to_owned());
        assert!(pages.read(&malformed, || unreachable!()).is_err());
    }
    let written = option(&resumed, "after").expect("cursor");
    let mut malformed = resumed.clone();
    let cursor = Cursor::parse(written).expect("cursor");
    set(
        &mut malformed,
        "after",
        Cursor { byte: 1, ..cursor }.written(),
    );
    assert!(
        pages.read(&malformed, || unreachable!()).is_err(),
        "mid-scalar cursor refuses"
    );
    let mut document = pages
        .read(&resumed, || unreachable!())
        .expect("Unicode continuation");
    let mut restored = String::new();
    for _ in 0.."équipe".len() {
        let row = document.lines().nth(1).expect("one text fragment");
        let (identity, clipped) = row.split_once(' ').expect("fragment");
        let (_, offset) = identity.split_once(':').expect("offset");
        assert_eq!(offset.parse::<usize>().expect("offset"), restored.len());
        restored.push_str(clipped.split_once("… [+").map_or(clipped, |(text, _)| text));
        let Some(continuation) = next(&document) else {
            break;
        };
        document = pages
            .read(&continuation, || unreachable!())
            .expect("next fragment");
    }
    assert_eq!(restored, "équipe");
    assert!(next(&document).is_none());
}

#[test]
fn retained_graph_snapshots_evict_oldest_at_count_and_byte_bounds() {
    let text = "graph workflow\nfirst row\nsecond row".to_owned();
    let retained = Snapshot::new(
        "workflow",
        None,
        false,
        Seq::new(1).expect("sequence"),
        text.clone(),
    )
    .retained_bytes();
    for (snapshots, bytes) in [(2, usize::MAX), (8, retained * 2)] {
        let pages = Pages::with_retention(snapshots, bytes);
        let initial = query("spice://board/graph/workflow?limit=1");
        let mut continuations = Vec::new();
        for sequence in 1..=3 {
            let document = pages
                .read(&initial, || {
                    Ok((Seq::new(sequence).expect("sequence"), text.clone()))
                })
                .expect("first page");
            continuations.push(next(&document).expect("continuation"));
        }
        let expired = pages
            .read(&continuations[0], || unreachable!())
            .expect_err("oldest snapshot is evicted");
        assert!(expired.starts_with("board::cursor-expired:"), "{expired}");
        pages
            .read(&continuations[1], || unreachable!())
            .expect("newer snapshot remains readable");
    }

    let pages = Pages::with_retention(1, retained - 1);
    let oversized = pages
        .read(&query("spice://board/graph/workflow?limit=1"), || {
            Ok((Seq::new(1).expect("sequence"), text))
        })
        .expect_err("one snapshot cannot exceed the byte bound");
    assert!(
        oversized.starts_with("board::graph-snapshot-too-large:"),
        "{oversized}"
    );
}

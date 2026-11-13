//! The bounded read contract: admission boundaries, omission facts, and cursors.

use serde::Serialize;
use wire::SpiceCommand;
use wire::output::{
    Admission, Bounded, Exhausted, LimitError, Limits, Request, Resume, Surface, clip,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct Row {
    id: String,
    text: String,
}

fn row(id: usize, text: &str) -> Row {
    Row {
        id: format!("r{id:03}"),
        text: text.to_owned(),
    }
}

fn bytes(row: &Row) -> usize {
    wire::encoded_len(row).expect("fixture row encodes")
}

const SURFACE: Surface = Surface::new(
    Limits {
        rows: 3,
        bytes: 200,
        text: 16,
    },
    Limits {
        rows: 10,
        bytes: 400,
        text: 64,
    },
);

fn read(rows: &[Row], request: Request) -> Bounded<Row> {
    let budget = SURFACE
        .budget(request)
        .expect("fixture request is admitted");
    let mut admission = Admission::new(budget);
    for row in rows {
        admission.offer(row.clone()).expect("fixture row encodes");
    }
    admission.finish(|resume| {
        let (record, widened) = match resume {
            Resume::At(record) => (record, None),
            Resume::Widened { record, bytes } => (record, Some(bytes)),
            Resume::Export(record) => {
                return SpiceCommand::new("fixture", ["export", record.id.as_str()])
                    .expect("fixture command is valid");
            }
        };
        let mut arguments = vec!["list".to_owned(), "--after".to_owned(), record.id.clone()];
        if let Some(bytes) = widened {
            arguments.extend(["--max-bytes".to_owned(), bytes.to_string()]);
        }
        SpiceCommand::new("fixture", arguments).expect("fixture command is valid")
    })
}

#[test]
fn the_row_limit_admits_exactly_its_count_and_names_the_rest() {
    let rows: Vec<Row> = (0..5).map(|id| row(id, "x")).collect();

    let whole = read(&rows[..3], Request::default());
    assert_eq!(whole.rows, rows[..3]);
    assert!(
        whole.omission.is_none(),
        "a population at the limit is whole"
    );

    let bounded = read(&rows, Request::default());
    let omission = bounded.omission.expect("a fourth row is withheld");
    assert_eq!(bounded.rows, rows[..3]);
    assert_eq!(
        (omission.population, omission.shown, omission.withheld),
        (5, 3, 2)
    );
    assert_eq!(omission.exhausted, Exhausted::Rows);
    assert_eq!(
        omission.shown_bytes,
        rows[..3].iter().map(bytes).sum::<usize>()
    );
    assert_eq!(omission.next_bytes, bytes(&rows[3]));
    assert_eq!(
        omission.continuation.arguments(),
        ["fixture", "list", "--after", "r003"]
    );
}

#[test]
fn the_byte_limit_admits_an_exact_fit_and_refuses_one_byte_more() {
    let first = row(0, &"a".repeat(80));
    let base = bytes(&row(1, ""));
    let exact = row(1, &"b".repeat(200 - bytes(&first) - base));
    assert_eq!(bytes(&first) + bytes(&exact), 200);

    let fitted = read(&[first.clone(), exact.clone()], Request::default());
    assert_eq!(fitted.rows.len(), 2);
    assert!(fitted.omission.is_none());

    let over = row(1, &format!("{}b", exact.text));
    let refused = read(&[first.clone(), over], Request::default());
    let omission = refused.omission.expect("one byte over is withheld");
    assert_eq!(refused.rows, [first]);
    assert_eq!(omission.exhausted, Exhausted::Bytes);
    assert_eq!(omission.shown_bytes + omission.next_bytes, 201);
}

#[test]
fn an_oversized_record_is_charged_and_closes_admission_in_relevance_order() {
    let small = row(1, "small");
    let widened = row(0, &"w".repeat(300));
    let bounded = read(&[widened.clone(), small.clone()], Request::default());
    let omission = bounded
        .omission
        .expect("the oversized first record is withheld");
    assert!(
        bounded.rows.is_empty(),
        "a less relevant record never leapfrogs"
    );
    assert_eq!((omission.shown, omission.withheld), (0, 2));
    assert_eq!(omission.exhausted, Exhausted::Bytes);
    let needed = bytes(&widened).to_string();
    assert_eq!(
        omission.continuation.arguments(),
        [
            "fixture",
            "list",
            "--after",
            "r000",
            "--max-bytes",
            needed.as_str()
        ],
        "a record the surface maximum admits resumes with the limit it needs"
    );

    let resumed = read(
        &[widened.clone(), small],
        Request {
            bytes: Some(bytes(&widened)),
            ..Request::default()
        },
    );
    assert_eq!(resumed.rows.first(), Some(&widened));

    let beyond = row(0, &"e".repeat(500));
    let exported = read(&[beyond], Request::default());
    assert_eq!(
        exported
            .omission
            .expect("a record beyond the maximum is withheld")
            .continuation
            .arguments(),
        ["fixture", "export", "r000"],
        "only an exact file export can carry a record beyond the maximum"
    );
}

#[test]
fn paging_by_cursor_returns_every_record_once_in_order() {
    let rows: Vec<Row> = (0..11)
        .map(|id| row(id, &"p".repeat(id * 7 % 40)))
        .collect();
    let mut seen = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..rows.len() {
        let remaining: Vec<Row> = rows
            .iter()
            .skip_while(|row| after.as_ref().is_some_and(|after| row.id < *after))
            .cloned()
            .collect();
        let page = read(&remaining, Request::default());
        let again = read(&remaining, Request::default());
        assert_eq!(page, again, "one population pages deterministically");
        seen.extend(page.rows.iter().cloned());
        let Some(omission) = page.omission else {
            break;
        };
        assert_eq!(omission.shown + omission.withheld, remaining.len());
        after = Some(omission.continuation.arguments()[3].clone());
    }
    assert_eq!(seen, rows, "no gap, no overlap, no reordering");
}

#[test]
fn text_and_json_carry_the_same_omission_facts() {
    let rows: Vec<Row> = (0..5).map(|id| row(id, "a b")).collect();
    let mut bounded = read(&rows, Request::default());
    let mut omission = bounded.omission.take().expect("rows are withheld");
    omission.continuation = SpiceCommand::new("task", ["list", "--after", "r 003"])
        .expect("command with a spaced cursor is valid");
    let line = omission.to_string();
    let (facts, command) = line
        .split_once("; continue: ")
        .expect("the line ends with its continuation");
    for fact in [
        format!(
            "withheld {} of {} rows",
            omission.withheld, omission.population
        ),
        format!("{} shown in {} bytes", omission.shown, omission.shown_bytes),
        format!("row limit {}", omission.limits.rows),
        format!("next row {} bytes", omission.next_bytes),
    ] {
        assert!(facts.contains(&fact), "{line:?} lacks {fact:?}");
    }
    let words = shell_words::split(command).expect("the continuation is shell words");
    assert_eq!(words[0], "spice");
    assert_eq!(words[1..], *omission.continuation.arguments());

    let json = serde_json::to_value(&omission).expect("omission encodes");
    assert_eq!(json["population"], omission.population);
    assert_eq!(json["withheld"], omission.withheld);
    assert_eq!(json["exhausted"], "rows");
    assert_eq!(
        json["continuation"],
        serde_json::json!(["task", "list", "--after", "r 003"])
    );
    let decoded: wire::output::Omission = serde_json::from_value(json).expect("omission decodes");
    assert_eq!(decoded, *omission);
}

#[test]
fn counted_records_after_closing_join_the_withheld_population() {
    let budget = SURFACE
        .budget(Request {
            rows: Some(1),
            ..Request::default()
        })
        .expect("one row is admitted");
    let mut admission = Admission::new(budget);
    assert!(admission.offer(row(0, "a")).expect("encodes"));
    assert!(!admission.offer(row(1, "b")).expect("encodes"));
    assert!(admission.is_closed());
    admission.withhold(98);
    let omission = admission
        .finish(|_| SpiceCommand::new("fixture", ["next"]).expect("valid"))
        .omission
        .expect("rows are withheld");
    assert_eq!(
        (omission.population, omission.shown, omission.withheld),
        (100, 1, 99)
    );
}

#[test]
#[should_panic(expected = "only after admission closes")]
fn uncounted_withholding_while_open_is_refused() {
    let budget = SURFACE.budget(Request::default()).expect("defaults admit");
    Admission::<Row>::new(budget).withhold(1);
}

#[test]
fn clipping_keeps_a_utf8_boundary_and_counts_the_cut() {
    let text = "naïve 🦀 crab";
    for max in 0..=text.len() {
        let clipped = clip(text, max);
        assert!(clipped.text.len() <= max);
        assert!(text.starts_with(&clipped.text));
        let withheld = clipped.withheld_bytes.map_or(0, usize::from);
        assert_eq!(clipped.text.len() + withheld, text.len());
    }
    let inside_crab = text.find('🦀').expect("fixture has a crab") + 2;
    let clipped = clip(text, inside_crab);
    assert_eq!(clipped.text, "naïve ");
    assert_eq!(clipped.to_string(), "naïve … [+9 bytes]");
    assert_eq!(clip(text, text.len()).to_string(), text);
}

#[test]
fn a_surface_admits_requests_only_within_its_maxima() {
    assert_eq!(
        SURFACE
            .budget(Request::default())
            .map(|budget| budget.limits()),
        Ok(SURFACE.default_limits())
    );
    assert_eq!(
        SURFACE
            .budget(Request {
                rows: Some(0),
                ..Request::default()
            })
            .map(|budget| budget.limits().rows),
        Ok(0),
        "zero rows is a count-only read"
    );
    assert_eq!(
        SURFACE.budget(Request {
            bytes: Some(401),
            ..Request::default()
        }),
        Err(LimitError::Exceeded {
            flag: "--max-bytes",
            requested: 401,
            maximum: 400
        })
    );
    assert_eq!(
        SURFACE.budget(Request {
            text: Some(0),
            ..Request::default()
        }),
        Err(LimitError::Zero { flag: "--max-text" })
    );
}

#[test]
fn a_continuation_resumes_a_count_only_read_with_the_default_rows() {
    let resume = |rows| {
        SURFACE
            .budget(Request {
                rows: Some(rows),
                bytes: Some(300),
                ..Request::default()
            })
            .expect("fixture request is admitted")
            .resume()
    };
    assert_eq!(
        resume(0),
        Limits {
            rows: SURFACE.default_limits().rows,
            bytes: 300,
            text: SURFACE.default_limits().text,
        }
    );
    assert_eq!(resume(7).rows, 7, "an explicit row limit is carried");
}

#[test]
#[should_panic(expected = "repository ceiling")]
fn no_surface_may_declare_maxima_above_the_repository_ceiling() {
    let above = wire::output::CEILING.rows + 1;
    let _ = Surface::new(
        Limits {
            rows: 1,
            bytes: 1,
            text: 1,
        },
        Limits {
            rows: above,
            ..wire::output::CEILING
        },
    );
}

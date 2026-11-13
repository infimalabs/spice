use std::fs;
use std::sync::LazyLock;

use rack::Rack;
use spice::repository::Repository;

use super::task_write::Field;
use super::{Access, Query, Request, Resource, Text, rewrite_continuations, served_reads};
use crate::board_command::{TaskText, ledger_output};

const HANDLE: &str = "TASK-dVlFq9qKp";

fn arguments(url: &str) -> Vec<String> {
    match Request::parse(url) {
        Ok(Request {
            resource: Resource::Read(query),
            ..
        }) => query.arguments(),
        other => panic!("{url} names no CLI read: {other:?}"),
    }
}

fn refusal(url: &str) -> String {
    Request::parse(url).expect_err(url)
}

#[test]
fn fixed_reads_take_precedence_without_loading_the_rack() {
    for url in [
        "spice://",
        "spice://outbox",
        "spice://quote/v1/example",
        "spice://board",
        "spice://tasks",
        "spice://task/TASK-dVlFq9qKp",
        "spice://task/TASK-dVlFq9qKp/description",
        "spice://inbox",
        "spice://room/inbox",
        "spice://lanes",
        "spice://session/briefing",
    ] {
        let request = Request::parse_with_rack(url, |_| {
            panic!("a fixed read must not depend on rack admission");
        })
        .expect("fixed read resolves");
        assert!(!matches!(request.resource, Resource::Rack { .. }), "{url}");
    }
}

#[test]
fn a_served_seat_reads_each_quoted_served_read_as_its_url_and_nothing_else() {
    let text = concat!(
        "- … 3 more: `spice session sweep /t/x.jsonl --limit 4`\n",
        "- board: `spice task status`; steering: `spice inbox`; peers: `spice lane list`\n",
        "- typed: `spice session briefing --json`; act: `spice task next`\n",
        "- held: `spice task show HANDLE` and `spice task show TASK-dVlFq9qKp`\n",
        "- prose `not a command` then an unbalanced ` spice inbox",
    );
    let served = served_reads(text);
    for url in [
        "`spice://board`",
        "`spice://inbox`",
        "`spice://lanes`",
        "`spice://task/TASK-dVlFq9qKp`",
    ] {
        assert!(served.contains(url), "{url} missing: {served}");
    }
    let sweep = served
        .lines()
        .next()
        .and_then(|line| line.split('`').nth(1))
        .expect("first row quotes its read");
    assert_eq!(
        Query::from_arguments(&arguments(sweep)).map(|query| query.url()),
        Some(sweep.to_owned()),
        "the cut row's URL resumes the same sweep"
    );
    assert_eq!(
        arguments(sweep).join(" "),
        "session sweep /t/x.jsonl --limit 4"
    );
    for kept in [
        "`spice session briefing --json`",
        "`spice task next`",
        "`spice task show HANDLE`",
        "`not a command`",
        "unbalanced ` spice inbox",
    ] {
        assert!(served.contains(kept), "{kept} rewritten: {served}");
    }
}

#[test]
fn the_root_and_quote_targets_are_not_cli_reads() {
    assert_eq!(
        Request::parse("spice://")
            .expect("the root is the index")
            .resource,
        Resource::Index
    );
    assert_eq!(
        Request::parse("spice://quote/v1/steer/1kQnrzNZx/00:2-3")
            .expect("a quote target with a selector")
            .resource,
        Resource::Quote("spice://quote/v1/steer/1kQnrzNZx/00".to_owned())
    );
}

const RACK_SKILL: &str = "# skill\nline two\nline three\n";

/// A rack holding an app wasm that is not text, the skill, and some docs.
static RACK: LazyLock<Vec<u8>> = LazyLock::new(|| {
    serve::canvas::gzip::write([
        ("app/app_bg.wasm", b"\0asm\xff".as_slice()),
        ("SKILL.md", RACK_SKILL.as_bytes()),
        ("build.json", RACK_BUILD.as_bytes()),
        ("reference.toml", b"# reference\nvalue = 7\n"),
        ("docs/guide.md", b"# guide\n"),
        ("docs/figure.png", RACK_FIGURE),
        ("docs/notes.xyz", b"untyped"),
        ("docs/planes/distribution.md", b"# planes\n"),
    ])
    .expect("fixture rack writes")
});

const RACK_FIGURE: &[u8] = b"\x89PNG\r\n\x1a\nfixture figure";
const RACK_BUILD: &str = "{\"product\":\"0.9.1\"}";

/// A fresh directory a rack read extracts beneath.
fn extractions() -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "spice-rack-extractions-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ))
}

/// The short code the fixture journal admits the rack under.
const RACK_CODE: wire::SequenceCode = wire::SequenceCode::new(42);

/// Read one rack URL the way the seat does, against the fixture rack,
/// extracting beneath `root` under the fact `code` names.
fn rack_read_coded(
    url: &str,
    root: &std::path::Path,
    code: Option<wire::SequenceCode>,
) -> Result<String, String> {
    let rack = Rack::admit(&RACK).expect("fixture rack admits");
    let request = Request::parse_with_rack(url, |path| Ok(super::rack::holds(&rack, path)))?;
    let Resource::Rack { path, generation } = &request.resource else {
        panic!("{url} names no rack member: {request:?}");
    };
    let document = super::rack::document(&rack, path, *generation, url, || {
        Ok(super::rack::Runtime {
            extractions: root.to_path_buf(),
            code,
        })
    })?;
    match &request.selector {
        Some(selector) => selector.select(&document),
        None => Ok(document),
    }
}

fn rack_read_into(url: &str, root: &std::path::Path) -> Result<String, String> {
    rack_read_coded(url, root, Some(RACK_CODE))
}

fn rack_read(url: &str) -> Result<String, String> {
    rack_read_into(url, &extractions())
}

fn app_rack_read(
    rack: &Rack<'_>,
    path: &str,
    generation: Option<wire::SequenceCode>,
) -> Result<wire::intent::RackAnswer, String> {
    super::rack::read_app_document(
        rack,
        wire::Digest::compute(wire::Domain::DistArtifact, &RACK),
        Some(RACK_CODE),
        &wire::intent::RackRead {
            path: path.to_owned(),
            generation,
        },
    )
}

#[test]
fn app_rack_reads_typed_immediate_docs_content() {
    use wire::intent::{RackContent, RackEntry, RackEntryKind, RackIdentity};

    let rack = Rack::admit(&RACK).expect("fixture rack admits");
    let answer = app_rack_read(&rack, "", Some(RACK_CODE)).expect("docs root reads");
    assert_eq!(
        answer.identity,
        RackIdentity {
            archive: wire::Digest::compute(wire::Domain::DistArtifact, &RACK),
            generation: Some(RACK_CODE),
        }
    );
    assert_eq!(answer.path, "");
    assert_eq!(
        answer.content,
        RackContent::Directory {
            entries: vec![
                RackEntry {
                    path: "figure.png".to_owned(),
                    kind: RackEntryKind::Image,
                    content_type: Some("image/png".to_owned()),
                    bytes: Some(RACK_FIGURE.len() as u64),
                },
                RackEntry {
                    path: "guide.md".to_owned(),
                    kind: RackEntryKind::Document,
                    content_type: Some("text/markdown; charset=utf-8".to_owned()),
                    bytes: Some(8),
                },
                RackEntry {
                    path: "notes.xyz".to_owned(),
                    kind: RackEntryKind::Unsupported,
                    content_type: None,
                    bytes: Some(7),
                },
                RackEntry {
                    path: "planes".to_owned(),
                    kind: RackEntryKind::Directory,
                    content_type: None,
                    bytes: None,
                },
            ],
        }
    );

    let guide = app_rack_read(&rack, "guide.md", None).expect("Markdown reads");
    assert!(matches!(
        guide.content,
        RackContent::Text { ref content_type, ref text }
            if content_type == "text/markdown; charset=utf-8" && text == "# guide\n"
    ));
    let figure = app_rack_read(&rack, "figure.png", None).expect("image reads");
    assert!(matches!(
        figure.content,
        RackContent::Image { ref media }
            if media.content_type == "image/png"
                && media.name == "figure.png"
                && media.decoded_bytes().as_deref() == Ok(RACK_FIGURE)
    ));
}

#[test]
fn app_rack_refuses_paths_and_generations_outside_its_authority() {
    let rack = Rack::admit(&RACK).expect("fixture rack admits");
    for path in [
        "/guide.md",
        "../guide.md",
        "planes/../guide.md",
        "planes//guide.md",
        "planes\\guide.md",
        "guide.md/",
        "guide\n.md",
    ] {
        app_rack_read(&rack, path, None).expect_err(path);
    }
    let long = "a".repeat(wire::intent::RACK_PATH_BYTES_MAX + 1);
    app_rack_read(&rack, &long, None).expect_err("long path");
    app_rack_read(&rack, "guide.md", Some(wire::SequenceCode::new(41)))
        .expect_err("wrong generation");
    app_rack_read(&rack, "missing.md", None).expect_err("missing");
    app_rack_read(&rack, "notes.xyz", None).expect_err("unknown type");
}

#[test]
fn app_rack_enforces_content_and_collection_bounds_before_answering() {
    let exact = vec![b'a'; wire::intent::RACK_TEXT_BYTES_MAX];
    let over = vec![b'a'; wire::intent::RACK_TEXT_BYTES_MAX + 1];
    let invalid = [0xff];
    let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>";
    let mut image = vec![0; wire::media::MAX_MEDIA_DATA_URL_BYTES];
    image[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    let archive = serve::canvas::gzip::write([
        ("docs/exact.md", exact.as_slice()),
        ("docs/over.md", over.as_slice()),
        ("docs/invalid.md", invalid.as_slice()),
        ("docs/vector.svg", svg.as_slice()),
        ("docs/large.png", image.as_slice()),
    ])
    .expect("boundary rack writes");
    let rack = Rack::admit(&archive).expect("boundary rack admits");
    assert!(matches!(
        app_rack_read(&rack, "exact.md", None)
            .expect("exact text bound reads")
            .content,
        wire::intent::RackContent::Text { .. }
    ));
    for path in ["over.md", "invalid.md", "vector.svg", "large.png"] {
        app_rack_read(&rack, path, None).expect_err(path);
    }

    let members = (0..=wire::intent::RACK_DIRECTORY_ENTRIES_MAX)
        .map(|index| (format!("docs/{index:04}.md"), b"x".to_vec()))
        .collect::<Vec<_>>();
    let archive = serve::canvas::gzip::write(
        members
            .iter()
            .map(|(path, bytes)| (path.as_str(), bytes.as_slice())),
    )
    .expect("large directory rack writes");
    let rack = Rack::admit(&archive).expect("large directory rack admits");
    app_rack_read(&rack, "", None).expect_err("directory over bound");
}

#[test]
fn rack_urls_answer_member_text_line_selections_and_listings() {
    assert_eq!(
        rack_read(spice::install::SKILL_RESOURCE).as_deref(),
        Ok(RACK_SKILL)
    );
    assert_eq!(
        rack_read("spice://SKILL.md:1-2").as_deref(),
        Ok("# skill\nline two")
    );
    assert_eq!(rack_read("spice://build.json").as_deref(), Ok(RACK_BUILD));
    refusal("spice://build.json/extra");
    assert_eq!(
        rack_read("spice://reference.toml:2").as_deref(),
        Ok("value = 7")
    );
    assert_eq!(
        rack_read("spice://docs/guide.md").as_deref(),
        Ok("# guide\n")
    );

    let listing = rack_read("spice://docs").expect("a directory lists its members");
    assert!(
        listing.ends_with(
            "\nresources:\n  spice://docs/figure.png\n  spice://docs/guide.md\n  \
             spice://docs/notes.xyz\n  spice://docs/planes/distribution.md"
        ),
        "{listing}"
    );
    let directory = rack_read("spice://docs/planes/").expect("a directory lists its members");
    assert!(
        directory.ends_with("\nresources:\n  spice://docs/planes/distribution.md"),
        "{directory}"
    );

    rack_read("spice://docs/absent.md").expect_err("an absent member is refused");
    rack_read("spice://docs/notes.xyz")
        .expect_err("a member whose extension the table does not name is refused");
    rack_read("spice://docs/plan").expect_err("a path prefix is no directory");
}

#[test]
fn non_text_rack_members_hand_off_digest_named_bytes_once() {
    let root = extractions();
    let digest = wire::Digest::compute(wire::Domain::DistArtifact, RACK_FIGURE);
    let file = root.join(format!("{digest}.png"));
    let expected = format!(
        "image/png {} bytes digest {digest} generation {RACK_CODE} at {}",
        RACK_FIGURE.len(),
        file.display()
    );

    let first = rack_read_into("spice://docs/figure.png", &root).expect("an image hands off");
    assert_eq!(first, expected);
    let bytes = std::fs::read(&file).expect("the handed-off file exists");
    assert_eq!(
        wire::Digest::compute(wire::Domain::DistArtifact, &bytes),
        digest
    );
    let written = std::fs::metadata(&file).expect("handed-off metadata");

    let second = rack_read_into("spice://docs/figure.png", &root).expect("a repeat hands off");
    assert_eq!(second, expected);
    assert_eq!(
        std::fs::metadata(&file)
            .and_then(|metadata| metadata.modified())
            .ok(),
        written.modified().ok(),
        "a repeated read rewrote the file"
    );
    assert_eq!(
        std::fs::read_dir(&root)
            .expect("the extraction directory lists")
            .count(),
        1,
        "only the digest-named file remains"
    );

    let wasm = rack_read_into("spice://app/app_bg.wasm", &root).expect("wasm hands off");
    assert!(
        wasm.starts_with("application/wasm 5 bytes digest "),
        "{wasm}"
    );

    assert_eq!(
        rack_read_into("spice://docs/guide.md:1", &root).as_deref(),
        Ok("# guide")
    );

    std::fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn rack_urls_serve_only_the_linked_generation_by_its_admitting_short_code() {
    let root = extractions();
    let plain = rack_read_into("spice://docs/guide.md", &root).expect("the member reads");
    assert_eq!(
        rack_read_into(&format!("spice://docs/guide.md@{RACK_CODE}"), &root).as_deref(),
        Ok(plain.as_str())
    );
    assert_eq!(
        rack_read_into(&format!("spice://docs@{RACK_CODE}"), &root),
        rack_read_into("spice://docs", &root)
    );
    assert_eq!(
        rack_read_into(&format!("spice://docs/guide.md@{RACK_CODE}:1"), &root).as_deref(),
        Ok("# guide")
    );

    for path in ["SKILL.md", "reference.toml", "docs/guide.md"] {
        assert_eq!(
            rack_read_into(&format!("spice://{path}@{RACK_CODE}"), &root),
            rack_read_into(&format!("spice://{path}"), &root)
        );
    }
    assert_eq!(
        rack_read_into(&format!("spice://docs@{RACK_CODE}/guide.md"), &root).as_deref(),
        Ok("# guide\n")
    );
    let root_listing = rack_read_into(&format!("spice://@{RACK_CODE}"), &root)
        .expect("the qualified root namespace lists");
    let (_, resources) = root_listing
        .split_once("\nresources:\n")
        .expect("root resources");
    assert_eq!(
        resources.lines().map(str::trim).collect::<Vec<_>>(),
        [
            "spice://SKILL.md",
            "spice://build.json",
            "spice://reference.toml"
        ]
    );
    let other = wire::SequenceCode::new(43);
    let refused = rack_read_into(&format!("spice://docs/guide.md@{other}"), &root)
        .expect_err("another code is refused");
    assert!(
        refused.contains(&format!("links generation {RACK_CODE}")),
        "{refused}"
    );

    let unadmitted = rack_read_coded(&format!("spice://docs/guide.md@{RACK_CODE}"), &root, None)
        .expect_err("no fact admits the rack");
    assert!(
        unadmitted.contains("no journal fact admits"),
        "{unadmitted}"
    );
    let pointer = rack_read_coded("spice://docs/figure.png", &root, None).expect("hands off");
    assert!(
        !pointer.contains("generation") && pointer.contains(" at "),
        "{pointer}"
    );

    std::fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn rack_urls_refuse_queries_and_digest_or_uuid_generations() {
    for url in [
        "spice://SKILL.md?release=current",
        "spice://SKILL.md@7?release=current",
        "spice://docs?limit=3",
    ] {
        assert!(
            rack_read(url).expect_err(url).contains("takes no query"),
            "{url}"
        );
    }
    let digest = wire::Digest::compute(wire::Domain::DistArtifact, b"a generation");
    for url in [
        format!("spice://docs/guide.md@{digest}"),
        "spice://docs@11111111-2222-3333-4444-555555555555".to_owned(),
    ] {
        assert!(refusal(&url).contains("journal short code"), "{url}");
    }
    assert!(
        refusal("spice://docs/guide.md@not-a-code").contains("no journal short code"),
        "other text is no code"
    );
}

#[test]
fn a_read_resource_takes_no_write() {
    let ambient = spice::ambient::capture();
    let seat = super::Seat {
        worktree: "/".into(),
        lane: wire::CanonicalUuid::new(uuid::Uuid::from_u128(7)),
        environment: crate::session_environment(&ambient).rooted("/".into()),
        ambient,
        graphs: Default::default(),
    };
    let refused = seat
        .serve("spice://board", Access::Write("# board\n"), |_| {
            panic!("no quote is read")
        })
        .expect_err("a read takes no write");
    assert!(refused.contains("takes no write"), "{refused}");
}

#[test]
fn the_skill_bootstrap_stays_compact_and_names_the_resource() {
    let bootstrap = spice::install::skill_bootstrap();
    assert!(
        bootstrap.len() < spice::install::SKILL_BOOTSTRAP_BYTES_CEILING,
        "the bootstrap stays below its separate 1 KiB budget"
    );
    assert!(bootstrap.contains(spice::install::SKILL_RESOURCE));
}

#[test]
fn unknown_paths_and_malformed_handles_are_refused() {
    for url in [
        "spice://?limit=3",
        "spice://board/graph",
        "spice://session/turns/x",
        "spice://task/TASK-dVlFq9qKp/notes",
        "spice://task/TASK-dVlFq9qKp/artifact/a1/raw",
        "spice://task",
        "spice://task?limit=3",
        "spice://task/TASK-dVlFq9qKp/artifact",
        "spice://task/TASK-dVlFq9qKp/artifact?limit=3",
        "spice://tasks/TASK-dVlFq9qKp",
        "http://task",
    ] {
        let refused = refusal(url);
        assert!(!refused.contains("does not serve"), "{url}: {refused}");
    }
    assert!(refusal("spice://task/not-a-handle").contains("not-a-handle"));
    refusal("spice://task/TASK-dVlFq9qKp%ZZ");
}

#[test]
fn urls_naming_unserved_commands_are_refused_naming_them() {
    for (url, command) in [
        ("spice://task/next", "spice task next"),
        ("spice://task/add", "spice task add"),
        ("spice://task/TASK-dVlFq9qKp/done", "spice task done"),
        ("spice://task/TASK-dVlFq9qKp/review", "spice task review"),
        ("spice://task/TASK-dVlFq9qKp/modify", "spice task modify"),
        (
            "spice://task/TASK-dVlFq9qKp/artifact/add",
            "spice task artifact add",
        ),
        (
            "spice://task/TASK-dVlFq9qKp/artifact/prune",
            "spice task artifact prune",
        ),
        ("spice://lane/add/x", "spice lane add"),
        ("spice://steer", "spice steer"),
        ("spice://board/handout", "spice board handout"),
    ] {
        let refused = refusal(url);
        assert!(
            refused.contains(&format!("`{command}`")) && refused.contains("does not serve"),
            "{url}: {refused}"
        );
    }
}

#[test]
fn query_keys_are_those_the_read_admits_each_once() {
    for url in [
        "spice://board?limit=3",
        "spice://task/TASK-dVlFq9qKp?offset=3",
        "spice://tasks?output=x",
        "spice://tasks?limit",
        "spice://tasks?limit=",
        "spice://tasks?all=yes",
        "spice://tasks?limit=3&limit=4",
        "spice://task/TASK-dVlFq9qKp/artifact/a1?output=/tmp/x",
        "spice://session/turns?export=x",
        "spice://session/turns?view=full&view=dialogue",
        "spice://session/briefing?file=",
        "spice://board/graph/review?output=x",
        "spice://inbox?all",
    ] {
        refusal(url);
    }
}

#[test]
fn a_task_summary_takes_no_query_and_names_the_detail_reads_that_do() {
    for query in [
        "limit=3",
        "max-bytes=4000",
        "max-text=4000",
        "after=x",
        "history",
    ] {
        let url = format!("spice://task/{HANDLE}?{query}");
        let refused = refusal(&url);
        let told = refused
            .strip_prefix(url.as_str())
            .expect("the refusal names its URL");
        assert!(
            told.contains(&format!("spice://task/{HANDLE}/context"))
                && told.contains("/history")
                && told.contains("/detail"),
            "{url}: {refused}"
        );
        for key in ["limit", "max-bytes", "max-text", "after"] {
            assert!(told.contains(key), "{url}: {refused}");
        }
    }
    for (detail, flags) in [
        ("context", &["--context"][..]),
        ("history", &["--history"][..]),
        ("detail", &["--context", "--history"][..]),
    ] {
        let url =
            format!("spice://task/{HANDLE}/{detail}?limit=3&max-bytes=4000&max-text=4000&after=x");
        let mut expected = vec!["task", "show", HANDLE];
        expected.extend(flags);
        expected.extend([
            "--limit",
            "3",
            "--max-bytes",
            "4000",
            "--max-text",
            "4000",
            "--after",
            "x",
        ]);
        assert_eq!(arguments(&url), expected, "{url}");
    }
    let summary = Query::from_arguments(&["task", "show", HANDLE].map(str::to_owned))
        .expect("a task summary is a served read");
    assert_eq!(summary.url(), format!("spice://task/{HANDLE}"));
}

#[test]
fn a_context_url_without_every_coordinate_names_each_absent_one() {
    let url = "spice://session/context/S1?vendor=omp&stream=session&member=1&offset=0&limit=3";
    let refused = refusal(url);
    let told = refused
        .strip_prefix(url)
        .expect("the refusal names its URL");
    for absent in ["lane", "source"] {
        assert!(told.contains(absent), "{absent} unnamed: {refused}");
    }
    for present in ["vendor", "stream", "member", "offset", "limit"] {
        assert!(!told.contains(present), "{present} named absent: {refused}");
    }
    let command = ["session", "context", "S1", "--lane", "L", "--limit", "3"];
    assert_eq!(
        Query::from_arguments(&command.map(str::to_owned)),
        None,
        "a command missing coordinates is no served read"
    );
}

#[test]
fn a_task_view_names_its_context_read_as_the_url_that_runs_it() {
    let command = "spice session context 'S 1' --lane L --vendor omp --stream session --source U --member 1 --offset 40";
    let view = format!(
        "creation_context session=S\n{command}\n  spice session context quoted in a description\nspice session context S2 --lane L"
    );
    let rewritten = super::context_urls(&view);
    let lines = rewritten.lines().collect::<Vec<_>>();
    assert_eq!(lines[0], "creation_context session=S");
    assert_eq!(
        arguments(lines[1]),
        shell_words::split(command.strip_prefix("spice ").expect("command"))
            .expect("rendered command is shell words"),
        "the URL runs exactly the rendered read"
    );
    assert_eq!(&lines[2..], &view.lines().collect::<Vec<_>>()[2..]);
}

#[test]
fn a_task_text_url_names_its_field_and_the_digest_a_write_carries() {
    let digest = wire::Digest::compute(wire::Domain::TaskText, b"read text");
    let handle: wire::TaskHandle = HANDLE.parse().expect("fixture handle");
    for (url, field, named) in [
        (
            format!("spice://task/{HANDLE}/description"),
            Field::Whole(TaskText::Description),
            None,
        ),
        (
            format!("spice://task/{HANDLE}/terms?digest={digest}"),
            Field::Whole(TaskText::Term),
            Some(digest.clone()),
        ),
    ] {
        assert_eq!(
            Request::parse(&url).expect(&url).resource,
            Resource::Text(Text {
                handle: handle.clone(),
                field,
                digest: named,
            }),
            "{url}"
        );
    }
    for url in [
        format!("spice://task/{HANDLE}/description?digest=abc"),
        format!("spice://task/{HANDLE}/description?limit=3"),
        format!("spice://task/{HANDLE}/terms?digest={digest}&digest={digest}"),
        format!("spice://task/{HANDLE}/title"),
    ] {
        refusal(&url);
    }
}

#[test]
fn a_term_url_names_one_item_of_one_task_and_takes_no_query() {
    let url = format!("spice://task/{HANDLE}/term/0a1b2c");
    assert_eq!(
        Request::parse(&url).expect(&url).resource,
        Resource::Text(Text {
            handle: HANDLE.parse().expect("fixture handle"),
            field: Field::Term("0a1b2c".to_owned()),
            digest: None,
        })
    );
    for url in [
        format!("spice://task/{HANDLE}/term"),
        format!("spice://task/{HANDLE}/term/0a1b2c?digest=abc"),
        format!("spice://task/{HANDLE}/term/0a1b2c/more"),
        format!("spice://task/{HANDLE}/terms/0a1b2c"),
    ] {
        refusal(&url);
    }
}

#[test]
fn only_a_whole_task_text_takes_a_write_and_only_a_write_takes_a_digest() {
    let ambient = spice::ambient::capture();
    let seat = super::Seat {
        worktree: "/".into(),
        lane: wire::CanonicalUuid::new(uuid::Uuid::from_u128(7)),
        environment: crate::session_environment(&ambient).rooted("/".into()),
        ambient,
        graphs: Default::default(),
    };
    let digest = wire::Digest::compute(wire::Domain::TaskText, b"read text");
    let serve = |url: &str, access| seat.serve(url, access, |_| panic!("no quote is read"));
    for url in [
        "spice://".to_owned(),
        spice::install::SKILL_RESOURCE.to_owned(),
        "spice://board".to_owned(),
        format!("spice://task/{HANDLE}"),
        format!("spice://task/{HANDLE}/context"),
        format!("spice://task/{HANDLE}/description?digest={digest}:2-3"),
    ] {
        serve(&url, Access::Write("text")).expect_err(&url);
    }
    serve(
        &format!("spice://task/{HANDLE}/description?digest={digest}"),
        Access::Read,
    )
    .expect_err("a read names no digest");
}

#[test]
fn a_task_invite_or_join_write_spells_the_request_its_command_line_spells() {
    let act = |url: &str| match Request::parse(url).expect(url).resource {
        Resource::Act(act) => act,
        other => panic!("{url} names {other:?}"),
    };
    let command_line = |words: &[&str]| {
        let matches = crate::cli::command()
            .try_get_matches_from(["spice", "task", "join"].iter().chain(words))
            .expect("task join parses");
        crate::board_command::join_request(crate::cli::selected_matches(&matches))
            .unwrap_or_else(|_| panic!("{words:?} spells a join request"))
    };
    let invite = format!("spice://task/{HANDLE}/invite");
    let addressed = format!("{invite}/8QkV");
    let join = format!("spice://task/{HANDLE}/join");
    for (url, body, words) in [
        (&invite, "", vec![HANDLE, "--invite"]),
        (&addressed, " \n", vec![HANDLE, "--invite=8QkV"]),
        (&join, "\n", vec![HANDLE]),
    ] {
        assert_eq!(
            act(url).request(body, None),
            Ok(command_line(&words)),
            "{url} <- {body:?}"
        );
    }
    // An invite's whole body is its message: a lone address never selects a
    // recipient, and only the URL names one. The words are the captured host
    // write's, so a body no capture carries proves none and refuses.
    let recipient = "8QkV".parse::<wire::OccupantAddress>().expect("address");
    let carrier = wire::outbox::Carrier {
        occupant: wire::CanonicalUuid::new(uuid::Uuid::from_u128(9)),
        locus: wire::TapeLocus {
            member: 1,
            offset: 64,
            length: 128,
        },
    };
    for (url, body, recipient) in [
        (&invite, " 8QkV\n", None),
        (&addressed, "Review the cut\n\nthen FIN.", Some(recipient)),
    ] {
        assert_eq!(
            act(url).request(body, Some(carrier)),
            Ok(crate::board_command::JoinRequest::Invite {
                task: Some(HANDLE.parse().expect("handle")),
                recipient,
                message: Some(carrier),
            }),
            "{url} <- {body:?}"
        );
        act(url).request(body, None).expect_err(body);
    }
    for body in ["8QkV", HANDLE] {
        act(&join).request(body, None).expect_err(body);
    }
    for url in [
        format!("{invite}?digest=abc"),
        format!("{addressed}?digest=abc"),
        format!("{join}?limit=3"),
        "spice://task/not-a-handle/join".to_owned(),
        "spice://task/not-a-handle/invite/8QkV".to_owned(),
    ] {
        refusal(&url);
    }
}

#[test]
fn a_task_act_write_answers_what_its_command_line_would_tell() {
    // An address the command line refuses earns that refusal, rendered alike.
    let written = "no-such-lane";
    let flag = format!("--invite={written}");
    let matches = crate::cli::command()
        .try_get_matches_from(["spice", "task", "join", HANDLE, &flag])
        .expect("task join parses");
    let refused = crate::board_command::join_request(crate::cli::selected_matches(&matches))
        .expect_err("an unparseable address refuses");
    assert_eq!(
        refusal(&format!("spice://task/{HANDLE}/invite/{written}")),
        super::command_rendering(&refused)
    );

    // Real board mapping and mutation feed the URI submission seam: the
    // successful join and post-resolution invitation refusal each carry the
    // current handle resolved with their own command.
    let outcomes = crate::board_command::moved_uri_join_outcomes();
    let invite_url = format!("spice://task/{}/invite", outcomes.named);
    let moved_invite = match Request::parse(&invite_url).expect(&invite_url).resource {
        Resource::Act(act) => act,
        other => panic!("{invite_url} names {other:?}"),
    };
    let join_url = format!("spice://task/{}/join", outcomes.named);
    let moved_join = match Request::parse(&join_url).expect(&join_url).resource {
        Resource::Act(act) => act,
        other => panic!("{join_url} names {other:?}"),
    };
    let notice = crate::board_command::rename_notice(&outcomes.named, &outcomes.current)
        .expect("fixture task moved");
    let reply_text = super::command_rendering(&outcomes.reply.outcome);
    assert_eq!(
        moved_join.submit_with("", None, |_| outcomes.reply),
        Ok(format!("{notice}\n{reply_text}")),
    );
    let refusal_text = super::command_rendering(&outcomes.refusal.outcome);
    assert_eq!(
        moved_invite.submit_with("", None, |_| outcomes.refusal),
        Err(format!("{notice}\n{refusal_text}")),
    );
}

#[test]
fn out_of_range_limits_are_refused_by_the_cli_read() {
    let root = std::env::temp_dir().join(format!("spice-resource-limits-{}", std::process::id()));
    fs::create_dir_all(root.join(".git")).expect("fixture Git directory exists");
    let repository = Repository::discover(&root).expect("fixture repository resolves");
    let ambient = spice::ambient::capture();
    let seat = super::Seat {
        worktree: root.clone(),
        lane: "ca38ffa1-2c8c-4ac2-87ae-64a413e15261"
            .parse()
            .expect("lane UUID"),
        environment: crate::session_environment(&ambient).rooted(root.clone()),
        ambient,
        graphs: Default::default(),
    };
    for url in [
        "spice://tasks?limit=100000",
        "spice://tasks?max-bytes=0",
        "spice://task/TASK-dVlFq9qKp/detail?max-text=1000000",
        "spice://task/TASK-dVlFq9qKp/history?limit=-1",
    ] {
        let Ok(Request {
            resource: Resource::Read(query),
            ..
        }) = Request::parse(url)
        else {
            panic!("{url} parses");
        };
        let refused = seat.run(&repository, &query).expect_err(url);
        assert!(!refused.is_empty(), "{url}");
    }
    fs::remove_dir_all(&root).expect("fixture cleanup");
}

#[test]
fn a_continuation_url_resumes_exactly_the_command_it_replaces() {
    let commands: [&[&str]; 10] = [
        &[
            "task",
            "show",
            HANDLE,
            "--context",
            "--history",
            "--limit",
            "20",
            "--max-bytes",
            "16000",
            "--max-text",
            "4096",
            "--after",
            "context:20",
        ],
        &[
            "task",
            "list",
            "--topic",
            "/spice/task",
            "--status",
            "pending",
            "--limit",
            "3",
            "--max-bytes",
            "8192",
            "--max-text",
            "240",
            "--after",
            HANDLE,
        ],
        &[
            "task",
            "artifact",
            "list",
            HANDLE,
            "--after",
            "a 1/x",
            "--limit",
            "20",
            "--max-bytes",
            "8192",
        ],
        &[
            "task", "artifact", "show", HANDLE, "a1", "--offset", "4096", "--bytes", "10",
        ],
        &[
            "room",
            "history",
            HANDLE,
            "--limit",
            "2",
            "--after",
            "dVsJd2J43",
            "--max-bytes",
            "8192",
        ],
        &["room", "inbox", "--limit", "1", "--after", "dVv51t4wR"],
        &["lane", "list", "--limit", "2", "--after", "dVv51t4wR"],
        &[
            "session",
            "turns",
            "/tmp/a b.jsonl",
            "/tmp/c.jsonl",
            "--tool",
            "shell",
            "--tool",
            "read",
            "--limit",
            "3",
            "--after",
            "turn:7",
        ],
        &[
            "session",
            "context",
            "01a1276a-903c-7071-89d5-345c833e93c2",
            "--lane",
            "ca38ffa1-2c8c-4ac2-87ae-64a413e15261",
            "--vendor",
            "omp",
            "--stream",
            "session",
            "--source",
            "4df45d2d-b61a-4fe0-a041-6e65e1403cfd",
            "--member",
            "1",
            "--offset",
            "40",
            "--after",
            "row:40",
        ],
        &[
            "doc",
            "ledger",
            HANDLE,
            "--max-bytes",
            "4000",
            "--offset",
            "64",
            "--digest",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ],
    ];
    for command in commands {
        let command = command
            .iter()
            .map(|word| (*word).to_owned())
            .collect::<Vec<_>>();
        let url = Query::from_arguments(&command)
            .expect("a served read has a URL")
            .url();
        assert_eq!(arguments(&url), command, "{url}");
    }
    let mutation = ["task", "done", HANDLE].map(str::to_owned);
    assert_eq!(Query::from_arguments(&mutation), None);
}

#[test]
fn a_page_names_its_continuation_as_a_url() {
    let command = wire::SpiceCommand::new(
        "task",
        [
            "show",
            HANDLE,
            "--history",
            "--limit",
            "2",
            "--after",
            "step:9",
        ],
    )
    .expect("continuation command");
    let rows = [1_u8, 2, 3];
    let mut admission = wire::output::Admission::new(
        wire::output::INTERACTIVE
            .budget(wire::output::Request {
                rows: Some(2),
                bytes: None,
                text: None,
            })
            .expect("budget"),
    );
    for row in rows {
        admission.offer(row).expect("row encodes");
    }
    let omission = admission
        .finish(|_| command.clone())
        .omission
        .expect("a row is withheld");
    let page = format!("handle {HANDLE}\n{omission}\nnext: spice task done {HANDLE}");
    let rewritten = rewrite_continuations(&page);
    let url = format!("spice://task/{HANDLE}/history?limit=2&after=step%3A9");
    assert_eq!(
        rewritten,
        page.replace(&command.to_string(), &url),
        "only the continuation changes"
    );
    assert_eq!(
        arguments(&url),
        command.arguments().to_vec(),
        "the URL resumes the same read"
    );
}

#[test]
fn a_bounded_ledger_page_names_its_native_continuation() {
    let task = HANDLE.parse().expect("fixture handle");
    let document = format!("# Ledger\n\n{}\n", "é".repeat(40));
    let budget = wire::output::INTERACTIVE
        .budget(wire::output::Request {
            rows: None,
            bytes: Some(17),
            text: None,
        })
        .expect("small ledger page is admitted");
    let page = ledger_output(&task, &document, None, budget, 0, None).expect("ledger page renders");
    let page = rewrite_continuations(&page);
    let url = page
        .lines()
        .find_map(|line| line.split_once("; continue: ").map(|(_, url)| url))
        .expect("ledger page carries a continuation URL");
    let offset = wire::output::clip(&document, budget.limits().bytes)
        .text
        .len();
    let digest = wire::Digest::compute(wire::Domain::CliExport, document.as_bytes());
    assert_eq!(
        url,
        format!(
            "spice://task/{HANDLE}/ledger?max-bytes={}&offset={offset}&digest={digest}",
            budget.limits().bytes
        )
    );
    assert_eq!(
        arguments(url),
        vec![
            "doc".to_owned(),
            "ledger".to_owned(),
            HANDLE.to_owned(),
            "--max-bytes".to_owned(),
            budget.limits().bytes.to_string(),
            "--offset".to_owned(),
            offset.to_string(),
            "--digest".to_owned(),
            digest.to_string(),
        ],
    );
}

#[test]
fn a_selector_selects_lines_or_is_refused() {
    let text = "one\ntwo\nthree\nfour\nfive";
    let select = |url: &str| {
        let request = Request::parse(url)?;
        request.selector.expect("a selector is named").select(text)
    };
    for (selector, expected) in [
        ("2-3", "two\nthree"),
        ("4", "four\nfive"),
        ("4-", "four\nfive"),
        ("2+2", "two\nthree"),
        ("-2", "four\nfive"),
        ("1,5", "one\ntwo\nthree\nfour\nfive\nfive"),
        ("3-9", "three\nfour\nfive"),
        ("raw", text),
        ("2-3:raw", "two\nthree"),
    ] {
        assert_eq!(
            select(&format!("spice://board:{selector}")).as_deref(),
            Ok(expected),
            ":{selector}"
        );
    }
    assert!(select("spice://board:6").is_err(), "a start past the end");
    for selector in [
        "img",
        "0",
        "3-2",
        "x",
        "2-3:img",
        "",
        "2+18446744073709551615",
    ] {
        assert!(
            Request::parse(&format!("spice://board:{selector}")).is_err(),
            ":{selector}"
        );
    }
}

#[test]
fn a_task_list_ends_with_each_shown_task_url() {
    let mut page = format!(
        "scope global --all\n{HANDLE} [todo] spice.task Resolve reads\nAPP-dVlMW4bKm [todo] spice.app Show jobs\nwithheld 1 of 3 rows"
    );
    super::board_trailer(&mut page, &super::Shape::Tasks);
    assert!(
        page.ends_with(&format!(
            "\nresources:\n  spice://task/{HANDLE}\n  spice://task/APP-dVlMW4bKm"
        )),
        "{page}"
    );
}

#[test]
fn a_task_summary_names_only_child_urls_that_resolve() {
    let handle: wire::TaskHandle = HANDLE.parse().expect("fixture handle");
    let mut page = String::new();
    super::board_trailer(
        &mut page,
        &super::Shape::Task {
            handle,
            detail: super::Detail::Summary,
        },
    );
    let (_, children) = page
        .split_once("resources:\n")
        .expect("a task summary names its children");
    for url in children.lines().map(str::trim) {
        let refusal = Request::parse(url).err();
        assert!(
            refusal.is_none(),
            "{url} is named but does not resolve: {refusal:?}"
        );
    }
}

use super::*;

/// Raw event identity retained only when no typed call preview is available.
const VENDOR_ID: &str = "toolu_01ABCdefGHIjklMNOpqrs";
const COMPACT_VENDOR_ID: &str = "toolu_01A…lMNOpqrs";

const BASH_INPUT: &str = r#"{"command":"git status --short","description":"read the tree"}"#;

fn lane(value: u8) -> CanonicalUuid {
    format!("00000000-0000-4000-8000-{value:012x}")
        .parse()
        .expect("fixture uuid is canonical")
}

mod presentation;

fn tool(call_id: &str, moment: listen::ToolMoment) -> listen::Tool {
    listen::Tool {
        call_id: call_id.to_owned(),
        moment,
        locus: listen::Locus::new(4, 120, 48),
    }
}

fn call(name: &str, arguments: &str) -> listen::ToolMoment {
    listen::ToolMoment::Call {
        name: name.to_owned(),
        arguments: arguments.to_owned(),
        intent: None,
    }
}

fn update(name: &str, arguments: &str) -> listen::ToolMoment {
    listen::ToolMoment::Update {
        name: name.to_owned(),
        arguments: arguments.to_owned(),
    }
}

fn succeeded() -> listen::ToolMoment {
    result(listen::ToolResult::Succeeded)
}

fn result(outcome: listen::ToolResult) -> listen::ToolMoment {
    listen::ToolMoment::Result(listen::ToolOutcome {
        result: outcome,
        output: None,
        exit_code: None,
        media: Vec::new(),
        external_media: Vec::new(),
    })
}

#[test]
fn media_text_distinguishes_mixed_and_image_only_results() {
    let media = wire::Media::from_base64("capture.png", "image/png", "iVBORw0KGgo=")
        .expect("fixture image admits");
    let tool = |output: Option<&str>| {
        tool(
            VENDOR_ID,
            listen::ToolMoment::Result(listen::ToolOutcome {
                result: listen::ToolResult::Succeeded,
                output: output.map(str::to_owned),
                exit_code: None,
                media: vec![media.clone()],
                external_media: Vec::new(),
            }),
        )
    };

    assert_eq!(
        message_text(
            &tool(Some("Script completed")),
            "view image succeeded".to_owned(),
            std::slice::from_ref(&media),
        ),
        "view image succeeded",
    );
    assert!(
        message_text(
            &tool(Some("  \n")),
            "view image succeeded".to_owned(),
            std::slice::from_ref(&media),
        )
        .is_empty(),
        "an image-only result does not invent visible tool prose",
    );
    assert_eq!(
        message_text(&tool(None), "view image succeeded".to_owned(), &[]),
        "view image succeeded",
        "non-media tool presentation is unchanged",
    );
}

/// Reading back an operator's attachment shows no second copy of that image,
/// while images a lane reads from anywhere else keep their card.
#[test]
fn a_read_of_a_steering_attachment_does_not_repeat_the_operator_image() {
    let media = wire::Media::from_base64("capture.png", "image/png", "iVBORw0KGgo=")
        .expect("fixture image admits");
    let image_result = || {
        listen::ToolMoment::Result(listen::ToolOutcome {
            result: listen::ToolResult::Succeeded,
            output: None,
            exit_code: None,
            media: vec![media.clone()],
            external_media: Vec::new(),
        })
    };
    let digest = "106b88688d8fe98f675979cff3d3e7f093a123e9df68e923b581bf6fd0db162a";
    let directive = "a817f232-b702-4925-9eef-f4b958b9186c";
    let occupant = lane(3);
    let attachment = |owner: CanonicalUuid| {
        format!(
            r#"{{"path":"/repo/.spice/lanes/{owner}/media/{occupant}/{directive}/0-{digest}.png"}}"#
        )
    };
    let shown = |arguments: &str| {
        let mut previews = CallPreviews::default();
        activity(
            lane(1),
            &tool(VENDOR_ID, call("read", arguments)),
            &mut previews,
        );
        let result = tool(VENDOR_ID, image_result());
        activity(lane(1), &result, &mut previews);
        result_media(lane(1), &result, &mut previews)
    };

    assert!(
        shown(&attachment(lane(1))).is_empty(),
        "the lane's own steering attachment is already shown with its directive",
    );
    assert_eq!(
        shown(&attachment(lane(2))),
        vec![media.clone()],
        "another lane's projection layout is not this lane's attachment",
    );
    assert_eq!(
        shown(r#"{"path":"/repo/docs/capture.png"}"#),
        vec![media.clone()],
        "an image the lane reads elsewhere keeps its card",
    );
}

/// A call reads as the work being done, not as the tool's bare name.
///
/// The strip exists so an operator can tell one shell call from the next, and
/// `Bash` alone cannot do that. The description field is in the payload
/// deliberately: a rule that showed any string it found would show it too.
#[test]
fn a_call_carries_the_command_line_and_not_merely_its_tool_name() {
    let mut previews = CallPreviews::default();

    assert_eq!(
        activity(
            lane(1),
            &tool(VENDOR_ID, call("Bash", BASH_INPUT)),
            &mut previews
        ),
        "Bash: git status --short"
    );
}

/// A result names the call it answers, correlating across records to do it.
///
/// The result event carries the vendor id alone, so the only way to render a
/// name is to remember the call. Asserting the id is absent as well as
/// asserting the text keeps a rule that appended the id from passing.
#[test]
fn a_result_names_its_call_and_never_paints_the_vendor_identifier() {
    let mut previews = CallPreviews::default();
    activity(
        lane(1),
        &tool(VENDOR_ID, call("Bash", BASH_INPUT)),
        &mut previews,
    );

    let result = activity(lane(1), &tool(VENDOR_ID, succeeded()), &mut previews);

    assert_eq!(result, "Bash: git status --short succeeded");
    assert!(
        !result.contains(VENDOR_ID),
        "an operator cannot act on a vendor correlation id: {result}"
    );
}

/// Every outcome word survives the correlation, and a failure still says so.
#[test]
fn each_reported_outcome_reaches_the_strip_beside_the_call_it_answers() {
    for (outcome, expected) in [
        (listen::ToolResult::Succeeded, "Read: /etc/hosts succeeded"),
        (listen::ToolResult::Failed, "Read: /etc/hosts failed"),
        (listen::ToolResult::Unreported, "Read: /etc/hosts completed"),
    ] {
        let mut previews = CallPreviews::default();
        activity(
            lane(1),
            &tool(VENDOR_ID, call("Read", r#"{"path":"/etc/hosts"}"#)),
            &mut previews,
        );

        assert_eq!(
            activity(lane(1), &tool(VENDOR_ID, result(outcome)), &mut previews),
            expected
        );
    }
}

/// A result whose call was never seen retains its bounded source identity.
///
/// A call outstanding across a resident restart, or one whose record this lane
/// never selected, leaves nothing to correlate against. The raw identity is
/// more useful than claiming every such operation was the same generic tool.
#[test]
fn an_uncorrelated_result_reads_as_its_bounded_event_identity() {
    let mut previews = CallPreviews::default();

    let result = activity(
        lane(1),
        &tool(VENDOR_ID, result(listen::ToolResult::Failed)),
        &mut previews,
    );

    assert_eq!(result, format!("{COMPACT_VENDOR_ID} failed"));
    assert!(
        !result.contains(VENDOR_ID),
        "the full opaque id stays bounded: {result}"
    );
}

/// One resident watches every lane, so a shared call id must not cross lanes.
///
/// A vendor identifier is unique within one transcript, not across the fleet. A
/// correlation keyed on the id alone would paint one lane's strip with another
/// lane's command, which is worse than painting nothing.
#[test]
fn one_call_identifier_reused_by_two_lanes_keeps_its_lanes_apart() {
    let mut previews = CallPreviews::default();
    activity(
        lane(1),
        &tool(VENDOR_ID, call("Bash", BASH_INPUT)),
        &mut previews,
    );

    assert_eq!(
        activity(lane(2), &tool(VENDOR_ID, succeeded()), &mut previews),
        format!("{COMPACT_VENDOR_ID} succeeded"),
        "another lane's call must not name this lane's result"
    );
    assert_eq!(
        activity(lane(1), &tool(VENDOR_ID, succeeded()), &mut previews),
        "Bash: git status --short succeeded",
        "the owning lane still resolves its own call"
    );
}

/// The correlation table holds in-flight calls only.
///
/// A resident runs for days. If a result did not take its entry, this map would
/// grow one entry per call ever observed and never shrink.
#[test]
fn a_call_preview_is_forgotten_once_its_result_has_landed() {
    let mut previews = CallPreviews::default();
    activity(
        lane(1),
        &tool(VENDOR_ID, call("Bash", BASH_INPUT)),
        &mut previews,
    );
    activity(lane(1), &tool(VENDOR_ID, succeeded()), &mut previews);

    assert!(
        previews.by_call.is_empty(),
        "an answered call is released: {previews:?}"
    );
    assert_eq!(
        activity(lane(1), &tool(VENDOR_ID, succeeded()), &mut previews),
        format!("{COMPACT_VENDOR_ID} succeeded"),
        "a redelivered result must not resurrect a call already answered"
    );
}

/// An update says the call is still running and carries its arguments.
///
/// It also republishes the correlation, because a dialect that only ever emits
/// updates for a long-running call would otherwise leave its result anonymous.
#[test]
fn an_update_reports_the_running_call_and_still_answers_its_result() {
    let mut previews = CallPreviews::default();

    assert_eq!(
        activity(
            lane(1),
            &tool(VENDOR_ID, update("bash", r#"{"command":"cargo test"}"#)),
            &mut previews
        ),
        "bash: cargo test update"
    );
    assert_eq!(
        activity(lane(1), &tool(VENDOR_ID, succeeded()), &mut previews),
        "bash: cargo test succeeded"
    );
}

/// The argument shown is chosen, not stumbled onto.
///
/// Each case is a payload shape a real dialect emits: an argv array, a named
/// field, a field no rule names, a bare string, an empty payload, a payload
/// with no text in it at all, and one this resident cannot parse.
#[test]
fn the_argument_preview_follows_the_shape_the_vendor_sent() {
    for (arguments, expected) in [
        (
            r#"{"command":["cargo","test","-p","spice"]}"#,
            "cargo test -p spice",
        ),
        (r#"{"query":"lane status strip"}"#, "lane status strip"),
        (r#"{"pattern":"ToolMoment"}"#, "ToolMoment"),
        (r#""a bare string payload""#, "a bare string payload"),
        ("{}", ""),
        ("", ""),
        (r#"{"depth":3}"#, "depth=3"),
        ("null", "null"),
        ("17", "17"),
        ("not json at all\nsecond line", "not json at all"),
    ] {
        assert_eq!(
            argument_preview(arguments),
            expected,
            "payload {arguments} previews wrongly"
        );
    }
}

/// A one-line strip stays one line, however long the command was.
///
/// Both halves matter: a newline would break the strip's layout, and an
/// unbounded command line would push every neighbouring control off it.
#[test]
fn a_preview_is_flattened_onto_one_bounded_line() {
    let mut previews = CallPreviews::default();
    let long = "x".repeat(MAX_CHARS * 2);
    let arguments = format!(r#"{{"command":"echo   one\ntwo {long}"}}"#);

    let preview = activity(
        lane(1),
        &tool(VENDOR_ID, call("Bash", &arguments)),
        &mut previews,
    );

    assert_eq!(preview.chars().count(), MAX_CHARS);
    assert!(
        preview.ends_with('\u{2026}'),
        "a truncated line says so: {preview}"
    );
    assert!(
        preview.starts_with("Bash: echo one two "),
        "runs of whitespace collapse to one space: {preview}"
    );
    assert!(!preview.contains('\n'), "the strip is one line: {preview}");
}

/// A preview that fits is left exactly as it reads.
#[test]
fn a_preview_within_the_bound_is_not_elided() {
    let mut previews = CallPreviews::default();
    let arguments = format!(
        r#"{{"command":"{}"}}"#,
        "y".repeat(MAX_CHARS - "Bash: ".len())
    );

    let preview = activity(
        lane(1),
        &tool(VENDOR_ID, call("Bash", &arguments)),
        &mut previews,
    );

    assert_eq!(preview.chars().count(), MAX_CHARS);
    assert!(
        !preview.ends_with('\u{2026}'),
        "a preview at the bound is whole: {preview}"
    );

    let lone = call_preview("", &format!(r#""{}""#, "z".repeat(MAX_CHARS * 2)), None);
    assert_eq!(lone.chars().count(), MAX_CHARS);
    assert!(lone.starts_with("tool: z"));
    assert_eq!(informative_tokens(&lone), MIN_INFORMATIVE_TOKENS);
}

#[test]
fn every_generic_activity_keeps_two_informative_tokens() {
    for (name, arguments, expected) in [
        ("Read", "", "Read tool"),
        ("tool", "", "event-raw activity"),
        ("", r#""status""#, "event-raw: status"),
        ("", "", "event-raw activity"),
        ("tool", r#"{"depth":3}"#, "event-raw: depth=3"),
    ] {
        let preview = activity(
            lane(1),
            &tool("event-raw", call(name, arguments)),
            &mut CallPreviews::default(),
        );
        assert_eq!(preview, expected, "{name:?} {arguments:?}");
        assert!(
            informative_tokens(&preview) >= MIN_INFORMATIVE_TOKENS,
            "{preview:?} retains the minimum visible vocabulary",
        );
    }
}

#[test]
fn shell_families_remain_visible_without_discarding_their_commands() {
    for (name, arguments, expected) in [
        ("Bash", r#"{"command":"pwd"}"#, "Bash: pwd"),
        ("zsh", r#"{"command":"pwd"}"#, "zsh: pwd"),
        (
            "shell",
            r#"{"command":"git status --short"}"#,
            "shell: git status --short",
        ),
    ] {
        assert_eq!(call_preview(name, arguments, None), expected, "{name}");
    }
    assert_eq!(call_preview("Bash", "", None), "Bash shell");
    assert_eq!(call_preview("zsh", "", None), "zsh shell");
}

#[test]
fn harness_tools_preview_the_target_they_act_on() {
    for (name, arguments, expected) in [
        (
            "lsp",
            r#"{"action":"references","file":"app/src/view.rs","line":120,"symbol":"label","i":"Finding label callers"}"#,
            "lsp: references label (app/src/view.rs:120)",
        ),
        (
            "lsp",
            r#"{"action":"symbols","file":"app/src/browser.rs","i":"Listing symbols"}"#,
            "lsp: symbols app/src/browser.rs",
        ),
        (
            "lsp",
            r#"{"action":"symbols","file":"*","query":"Toaster"}"#,
            "lsp: symbols Toaster (*)",
        ),
        (
            "lsp",
            r#"{"action":"rename","file":"src/a.rs","line":4,"symbol":"Foo","new_name":"Bar"}"#,
            "lsp: rename Foo -> Bar (src/a.rs:4)",
        ),
        (
            "lsp",
            r#"{"action":"rename_file","file":"src/a.rs","new_name":"src/b.rs"}"#,
            "lsp: rename_file src/a.rs -> src/b.rs",
        ),
        (
            "lsp",
            r#"{"action":"diagnostics","file":"app/src/**/*.rs"}"#,
            "lsp: diagnostics app/src/**/*.rs",
        ),
        (
            "lsp",
            r#"{"action":"status","i":"Checking"}"#,
            "lsp: status",
        ),
        (
            "grep",
            r#"{"pattern":"fn render","path":"app/src","i":"Finding renderers"}"#,
            "grep: fn render in app/src",
        ),
        ("Grep", r#"{"pattern":"TODO"}"#, "Grep: TODO"),
        (
            "job",
            r#"{"op":"wait","id":"dTtmkKfYY","i":"Waiting for gate"}"#,
            "job: wait dTtmkKfYY",
        ),
        ("job", r#"{"op":"list","i":"Listing jobs"}"#, "job: list"),
    ] {
        assert_eq!(call_preview(name, arguments, None), expected, "{arguments}");
    }
    // A payload missing the tool's own target keeps the generic scan.
    assert_eq!(
        call_preview("lsp", r#"{"file":"src/a.rs"}"#, None),
        "lsp: src/a.rs"
    );
    let long = format!(
        r#"{{"action":"hover","symbol":"value","file":"{}.rs","line":9}}"#,
        "deep/".repeat(MAX_CHARS)
    );
    let bounded = call_preview("lsp", &long, None);
    assert_eq!(bounded.chars().count(), MAX_CHARS);
    assert!(bounded.starts_with("lsp: hover value (deep/"), "{bounded}");
}

/// A job's typed intent is what a reader recognizes; its operation and job
/// id stay beside it so the line still correlates with the job it names.
///
/// Updates repeat no intent, so the call's carries through each update and
/// into the result, which append their state once. A progress update puts its
/// latest output row right after the tool's name, so narrow status bars
/// preserve the result and it still reads as that tool's output; the intent
/// and operation remain as trailing context. An update with no recognizable
/// tool name keeps its output first. A call without an intent keeps the
/// operation and id, and only `job` changes: other tools' typed intents leave
/// their previews as they were.
#[test]
fn a_job_call_keeps_its_intent_and_names_its_tool_before_progress_output() {
    let mut previews = CallPreviews::default();
    let job = |intent: Option<&str>| listen::ToolMoment::Call {
        name: "job".to_owned(),
        arguments: r#"{"op":"wait","id":"dVW6Zkf0h","i":"argument prose"}"#.to_owned(),
        intent: intent.map(str::to_owned),
    };
    let wait = r#"{"op":"wait","id":"dVW6Zkf0h"}"#;

    assert_eq!(
        activity(
            lane(1),
            &tool("call-1", job(Some("  Waiting\nfor the gate "))),
            &mut previews
        ),
        "job: Waiting for the gate (wait dVW6Zkf0h)"
    );
    for _ in 0..2 {
        assert_eq!(
            activity(lane(1), &tool("call-1", update("job", wait)), &mut previews),
            "job: Waiting for the gate (wait dVW6Zkf0h) update"
        );
    }
    let progress = serde_json::json!({
        "content": [{
            "type": "text",
            "text": "[job dVW6Zkf0h] spice gate prove\nchecking\ncargo tests passed"
        }]
    })
    .to_string();
    assert_eq!(
        activity(
            lane(1),
            &tool("call-1", update("job", &progress)),
            &mut previews
        ),
        "job: cargo tests passed — Waiting for the gate (wait dVW6Zkf0h)"
    );
    assert_eq!(
        activity(lane(1), &tool("call-1", succeeded()), &mut previews),
        "job: Waiting for the gate (wait dVW6Zkf0h) succeeded"
    );
    let unnamed = activity(
        lane(1),
        &tool("call-3", update("", &progress)),
        &mut previews,
    );
    assert_eq!(
        unnamed, "cargo tests passed — call-3: content[1]",
        "an unnamed update keeps leading with its output"
    );

    for intent in [None, Some(" \n ")] {
        assert_eq!(
            activity(lane(1), &tool("call-2", job(intent)), &mut previews),
            "job: wait dVW6Zkf0h"
        );
    }
    assert_eq!(
        call_preview("job", r#"{"op":"list"}"#, Some("Listing jobs")),
        "job: Listing jobs (list)"
    );
    assert_eq!(
        call_preview("job", "{}", Some("Checking jobs")),
        "job: Checking jobs"
    );
    assert_eq!(
        call_preview("grep", r#"{"pattern":"TODO"}"#, Some("Finding work")),
        "grep: TODO"
    );

    let long = call_preview("job", wait, Some(&"waiting ".repeat(MAX_CHARS)));
    assert_eq!(long.chars().count(), MAX_CHARS);
    assert!(long.ends_with("\u{2026} (wait dVW6Zkf0h)"), "{long}");
}

#[test]
fn an_empty_call_remains_visible_through_its_correlated_result() {
    let mut previews = CallPreviews::default();
    assert_eq!(
        activity(
            lane(1),
            &tool("current-bash", call("bash", "")),
            &mut previews
        ),
        "bash shell",
    );
    assert_eq!(
        activity(
            lane(1),
            &tool("current-bash", result(listen::ToolResult::Unreported)),
            &mut previews,
        ),
        "bash shell completed",
    );
}

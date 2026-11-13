use super::{LineControls, plain_text, render_line};

const KNOWN: &str = "MOSAIC-1kKQXgDdx";
const UNKNOWN: &str = "IDENTIT-dRyDYZqJJ";

fn live(markdown: &str) -> String {
    let known = KNOWN
        .parse::<wire::TaskHandle>()
        .expect("fixture handle is canonical");
    render_line(
        markdown,
        &LineControls::Live {
            resolves: &|task| *task == known,
        },
    )
}

fn chips(html: &str) -> Vec<&str> {
    html.split("data-task-handle=\"")
        .skip(1)
        .filter_map(|tail| tail.split('"').next())
        .collect()
}

#[test]
fn a_line_renders_inline_forms_and_projects_their_text() {
    let title = format!("Fix `Foo::bar` *now* ~~later~~ in $O(n)$ for {KNOWN} and {UNKNOWN}");

    let html = live(&title);

    assert!(html.contains("<code>Foo::bar</code>"), "{html}");
    assert!(html.contains("<em>now</em>"), "{html}");
    assert!(html.contains("<del>later</del>"), "{html}");
    assert!(html.contains("class=\"math math-inline\""), "{html}");
    assert_eq!(chips(&html), [KNOWN], "{html}");
    assert!(!html.contains("<p>"), "{html}");
    assert_eq!(
        plain_text(&title),
        format!("Fix Foo::bar now later in O(n) for {KNOWN} and {UNKNOWN}")
    );
    assert_eq!(plain_text("pre*mid*post"), "premidpost");
}

#[test]
fn block_constructs_never_apply_to_a_line() {
    for (line, projected) in [
        ("# Heading title", "# Heading title"),
        ("1. Fix it", "1. Fix it"),
        ("> quoted", "> quoted"),
        ("- [ ] open item", "- [ ] open item"),
        ("first\n\n- second\n- third", "first - second - third"),
    ] {
        let html = live(line);

        assert!(
            !["<h1", "<ol", "<ul", "<li", "<blockquote", "<input", "<p>"]
                .iter()
                .any(|tag| html.contains(tag)),
            "{line}: {html}"
        );
        assert_eq!(plain_text(line), projected);
    }
}

#[test]
fn a_nested_line_holds_no_control_and_raw_html_stays_text() {
    let line = format!("[docs](https://example.test) for {KNOWN} <img src=x onerror=alert(1)>");

    let html = render_line(&line, &LineControls::Nested);

    assert!(!html.contains("<a"), "{html}");
    assert!(chips(&html).is_empty(), "{html}");
    assert!(!html.contains("<img"), "{html}");
    assert!(
        html.contains(&format!("docs for {KNOWN} &lt;img")),
        "{html}"
    );
    assert_eq!(
        plain_text(&line),
        format!("docs for {KNOWN} <img src=x onerror=alert(1)>")
    );
}

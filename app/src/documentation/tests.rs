use super::*;
use wire::intent::RackContent;

fn target(path: &str) -> Target {
    Target {
        path: path.into(),
        fragment: String::new(),
    }
}
fn answer(path: &str, generation: u8) -> RackAnswer {
    RackAnswer {
        identity: RackIdentity {
            archive: format!("{generation:064x}").parse().unwrap(),
            generation: None,
        },
        path: path.into(),
        content: RackContent::Text {
            content_type: "text/markdown; charset=utf-8".into(),
            text: path.into(),
        },
    }
}
fn visit(state: &mut Documents, path: &str, visit: Visit) {
    let read = state.begin(target(path), visit);
    assert!(state.accept(&read, Ok(answer(path, 1))));
}

#[test]
fn documentation_links_stay_inside_the_admitted_root() {
    assert_eq!(
        resolve("guide/start.md", false, "../reference/run%20it.md#Usage"),
        Link::Internal(Target {
            path: "reference/run it.md".into(),
            fragment: "Usage".into()
        })
    );
    assert_eq!(
        resolve("guide", true, "start.md"),
        Link::Internal(target("guide/start.md"))
    );
    assert_eq!(
        resolve("guide/start.md", false, "spice://docs/reference.md"),
        Link::Internal(target("reference.md"))
    );
    assert_eq!(
        resolve("guide/start.md", false, "#section"),
        Link::Internal(Target {
            path: "guide/start.md".into(),
            fragment: "section".into()
        })
    );
    for unsafe_link in [
        "../../escape",
        "%2e%2e/%2e%2e/escape",
        "//host/file",
        "/etc/passwd",
        "javascript:alert(1)",
        "file:///secret",
        "spice://other/read",
        "spice://docs-extra/read",
        "bad%FF",
        "bad%5cpath",
        "a.md?read=secret",
    ] {
        assert_eq!(
            resolve("guide/start.md", false, unsafe_link),
            Link::Inert,
            "{unsafe_link}"
        );
    }
    let destination = Target {
        path: "guide/café notes.md".into(),
        fragment: "part two".into(),
    };
    assert_eq!(
        resolve("", true, &destination.href()),
        Link::Internal(destination)
    );
    assert_eq!(
        resolve("", true, "https://example.org/docs"),
        Link::External("https://example.org/docs".into())
    );
}

#[test]
fn documentation_history_moves_only_after_acceptance_and_branches_locally() {
    let mut state = Documents::default();
    visit(&mut state, "", Visit::Push);
    visit(&mut state, "a.md", Visit::Push);
    visit(&mut state, "b.md", Visit::Push);
    visit(&mut state, "a.md", Visit::History(1));
    let refused = state.begin(target("missing.md"), Visit::Push);
    state.accept(&refused, Err("rack::missing".into()));
    assert_eq!(state.target(), target("a.md"));
    assert_eq!(state.page.as_ref().unwrap().path, "a.md");
    assert_eq!(state.issue.as_deref(), Some("rack::missing"));
    assert_eq!(state.history, [target(""), target("a.md"), target("b.md")]);
    visit(&mut state, "c.md", Visit::Push);
    assert_eq!(state.history, [target(""), target("a.md"), target("c.md")]);
    visit(&mut state, "c.md", Visit::Reload);
    assert_eq!(state.position, 2);
    visit(&mut state, "c.md", Visit::Push);
    assert_eq!(state.history, [target(""), target("a.md"), target("c.md")]);
}

#[test]
fn documentation_late_answers_cannot_replace_a_new_selection_or_error() {
    let mut state = Documents::default();
    visit(&mut state, "", Visit::Push);
    let old = state.begin(target("old.md"), Visit::Push);
    let current = state.begin(target("current.md"), Visit::Push);
    state.accept(&current, Err("rack::refused".into()));
    assert!(!state.accept(&old, Ok(answer("old.md", 1))));
    assert_eq!(state.page.as_ref().unwrap().path, "");
    assert_eq!(state.issue.as_deref(), Some("rack::refused"));
    let wrong = state.begin(target("wanted.md"), Visit::Push);
    assert!(!state.accept(&wrong, Ok(answer("other.md", 1))));
    assert_eq!(state.target(), target(""));
}

#[test]
fn documentation_identity_changes_and_reconnects_invalidate_cached_bytes() {
    let mut state = Documents::default();
    visit(&mut state, "a.md", Visit::Push);
    let first = state.identity.clone().unwrap();
    let read = state.begin(target("b.md"), Visit::Push);
    assert!(state.accept(&read, Ok(answer("b.md", 2))));
    assert!(state.cached(&first, "a.md").is_none());
    assert!(!state.cache_media(&read, &first, "old.png", answer("old.png", 1)));
    let current = state.identity.clone().unwrap();
    assert!(state.cached(&current, "b.md").is_some());
    state.reset();
    assert!(state.page.is_none());
    assert!(state.cached(&current, "b.md").is_none());
    assert!(!state.accept(&read, Ok(answer("b.md", 2))));
    assert_eq!(state.target(), target("b.md"));
}

#[test]
fn documentation_markdown_does_not_fetch_unadmitted_images_or_preserve_html() {
    assert!(is_markdown_content_type("text/markdown; charset=utf-8"));
    assert!(!is_markdown_content_type("text/plain; charset=utf-8"));
    let rendered = crate::composer_markdown::document::render(
        "[relative](guide.md) [unsafe](javascript:alert) ![remote](https://example.org/leak.png) <script>bad()</script>",
        &|href, image| {
            if image {
                String::new()
            } else {
                match resolve("", true, href) {
                    Link::Internal(target) => target.href(),
                    Link::External(url) => url,
                    Link::Inert => String::new(),
                }
            }
        },
    );
    assert!(rendered.html.contains("href=\"#docs/guide.md#\""));
    assert!(!rendered.html.contains("<img"));
    assert!(!rendered.html.contains("<script>"));
    assert!(!rendered.html.contains("javascript:"));
}

//! Where the console's links go.
//!
//! The window is the console's only webview, so a link must never replace
//! the console or open a second webview. A navigation to an http(s) URL of
//! another origin leaves for the system browser, whether a plain click,
//! keyboard activation, modified click, or context-menu "Open Link" started
//! it; a new-window request leaves the same way whatever its origin. The
//! console's own routes, hash moves, and the frames it creates (its
//! same-origin renderer documents, `about:`, `data:`, and `blob:` frames)
//! stay inside. The system browser receives only http(s) URLs: the platform
//! launcher opens whatever a URL names, and a `file:` link must not run a
//! local application.

/// The console's link policy, shared by the window's navigation and
/// new-window handlers.
pub(crate) struct Links {
    /// The console's scheme and authority; `None` only if the window's URL
    /// was not an absolute http(s) URL, which keeps every http(s) navigation
    /// outside.
    console: Option<(String, String)>,
    external: Box<dyn Fn(&str) + Send + Sync>,
}

impl Links {
    pub(crate) fn new(console: &str, external: impl Fn(&str) + Send + Sync + 'static) -> Self {
        Self {
            console: web_origin(console)
                .map(|(scheme, authority)| (scheme.to_owned(), authority.to_owned())),
            external: Box::new(external),
        }
    }

    /// Whether the webview may follow a navigation to `url`; a navigation it
    /// may not follow has already left for the system browser.
    pub(crate) fn navigate(&self, url: &str) -> bool {
        let Some((scheme, authority)) = web_origin(url) else {
            return true;
        };
        let console = self.console.as_ref().is_some_and(|(own, host)| {
            own.eq_ignore_ascii_case(scheme) && host.eq_ignore_ascii_case(authority)
        });
        if !console {
            (self.external)(url);
        }
        console
    }

    /// Answer the page's request for a new window: the window stays the only
    /// webview, and an http(s) URL leaves for the system browser.
    pub(crate) fn new_window(&self, url: &str) {
        if web_origin(url).is_some() {
            (self.external)(url);
        }
    }
}

/// The scheme and authority of an absolute http(s) URL, which is its web
/// origin as the webview writes it: scheme and host lowercase, a default
/// port omitted. Userinfo stays in the authority, so it never matches the
/// console's.
fn web_origin(url: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = url.split_once("://")?;
    if !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")) {
        return None;
    }
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    (!authority.is_empty()).then_some((scheme, authority))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::Links;

    const CONSOLE: &str = "http://127.0.0.1:4317/app?handoff=1";

    fn links() -> (Links, Arc<Mutex<Vec<String>>>) {
        let opened = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&opened);
        let links = Links::new(CONSOLE, move |url| {
            sink.lock().unwrap().push(url.to_owned());
        });
        (links, opened)
    }

    #[test]
    fn only_other_web_origins_leave_the_console() {
        let (links, opened) = links();
        for url in [
            "http://127.0.0.1:4317/",
            "HTTP://127.0.0.1:4317/rooms#card-7",
            "http://127.0.0.1:4317?view=board",
            "about:blank",
            "about:srcdoc",
            "data:text/html,<p>",
            "blob:http://127.0.0.1:4317/5f1c",
            "mailto:a@example.test",
            "file:///Applications/Calculator.app",
        ] {
            assert!(links.navigate(url), "{url} stays");
        }
        assert!(opened.lock().unwrap().is_empty());
        let leaving = [
            "https://example.test/a?b#c",
            "http://127.0.0.1:4318/",
            "https://127.0.0.1:4317/",
            "http://localhost:4317/",
            "http://127.0.0.1:4317@example.test/",
        ];
        for url in leaving {
            assert!(!links.navigate(url), "{url} leaves");
        }
        assert_eq!(*opened.lock().unwrap(), leaving);
    }

    #[test]
    fn new_windows_hand_only_web_urls_to_the_system_browser() {
        let (links, opened) = links();
        for url in [
            "http://127.0.0.1:4317/rooms",
            "https://example.test/",
            "file:///Applications/Calculator.app",
            "mailto:a@example.test",
            "about:blank",
        ] {
            links.new_window(url);
        }
        assert_eq!(
            *opened.lock().unwrap(),
            ["http://127.0.0.1:4317/rooms", "https://example.test/"]
        );
    }
}

//! Rack-local navigation. Nothing here touches the app route or browser history.
use std::collections::BTreeMap;
use std::rc::Rc;

use wire::intent::{RackAnswer, RackIdentity};

mod links;
pub(crate) use links::{Link, Target, resolve};
#[cfg(test)]
mod tests;
pub(crate) fn is_markdown_content_type(content_type: &str) -> bool {
    content_type
        .split(';')
        .next()
        .is_some_and(|essence| essence.trim().eq_ignore_ascii_case("text/markdown"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Visit {
    Push,
    History(usize),
    Reload,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Read {
    pub(crate) sequence: u64,
    pub(crate) target: Target,
    pub(crate) visit: Visit,
}

#[derive(Default)]
pub(crate) struct Documents {
    sequence: u64,
    pub(crate) identity: Option<RackIdentity>,
    cache: BTreeMap<String, Rc<RackAnswer>>,
    pub(crate) page: Option<Rc<RackAnswer>>,
    pub(crate) history: Vec<Target>,
    pub(crate) position: usize,
    pub(crate) pending: Option<Read>,
    pub(crate) issue: Option<String>,
    pub(crate) failed: Option<Read>,
    pub(crate) media_pending: usize,
}

impl Documents {
    pub(crate) fn reset(&mut self) {
        self.sequence += 1;
        self.identity = None;
        self.cache.clear();
        self.page = None;
        self.pending = None;
        self.failed = None;
        self.issue = None;
        self.media_pending = 0;
    }

    pub(crate) fn target(&self) -> Target {
        self.history.get(self.position).cloned().unwrap_or_default()
    }

    pub(crate) fn begin(&mut self, target: Target, visit: Visit) -> Read {
        self.sequence += 1;
        let read = Read {
            sequence: self.sequence,
            target,
            visit,
        };
        self.pending = Some(read.clone());
        self.failed = None;
        self.issue = None;
        self.media_pending = 0;
        read
    }

    pub(crate) fn current(&self, read: &Read) -> bool {
        self.sequence == read.sequence
    }

    /// Page reads always consult the resident, discovering a changed generation
    /// before any cached media is reused. The identity scopes the whole cache.
    pub(crate) fn accept(&mut self, read: &Read, answer: Result<RackAnswer, String>) -> bool {
        if !self.current(read) {
            return false;
        }
        self.pending = None;
        let answer = match answer {
            Ok(answer) if answer.path == read.target.path => answer,
            Ok(_) => {
                self.fail(
                    read,
                    "Rack response did not name the requested path.".into(),
                );
                return false;
            }
            Err(issue) => {
                self.fail(read, issue);
                return false;
            }
        };
        if self.identity.as_ref() != Some(&answer.identity) {
            self.cache.clear();
            self.page = None;
            self.identity = Some(answer.identity.clone());
        }
        let answer = Rc::new(answer);
        self.cache.insert(answer.path.clone(), answer.clone());
        self.page = Some(answer);
        match read.visit {
            Visit::Push => {
                if self.history.get(self.position) != Some(&read.target) {
                    if !self.history.is_empty() {
                        self.history.truncate(self.position + 1);
                    }
                    self.history.push(read.target.clone());
                    self.position = self.history.len() - 1;
                }
            }
            Visit::History(position) => self.position = position,
            Visit::Reload => {}
        }
        true
    }

    pub(crate) fn fail(&mut self, read: &Read, issue: String) {
        if self.current(read) {
            self.pending = None;
            self.issue = Some(issue);
            self.failed = Some(read.clone());
        }
    }

    pub(crate) fn cached(&self, identity: &RackIdentity, path: &str) -> Option<Rc<RackAnswer>> {
        (self.identity.as_ref() == Some(identity))
            .then(|| self.cache.get(path).cloned())
            .flatten()
    }

    pub(crate) fn cache_media(
        &mut self,
        read: &Read,
        identity: &RackIdentity,
        path: &str,
        answer: RackAnswer,
    ) -> bool {
        if !self.current(read) || self.identity.as_ref() != Some(identity) {
            return false;
        }
        if &answer.identity != identity || answer.path != path {
            self.reset();
            self.issue = Some("Documentation generation changed; reload the page.".into());
            return false;
        }
        self.cache.insert(path.to_owned(), Rc::new(answer));
        true
    }
}

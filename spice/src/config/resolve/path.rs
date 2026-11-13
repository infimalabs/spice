//! Owned structural paths used by configuration diagnostics and provenance.

use std::fmt;

use super::PathSegment;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum OwnedSegment {
    Key(String),
    Index(usize),
}

/// An owned, structurally segmented path produced by the resolver.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ConcretePath(pub(super) Vec<OwnedSegment>);

impl ConcretePath {
    pub(super) fn root() -> Self {
        Self(Vec::new())
    }

    /// Borrow every key and index without reparsing display text.
    pub fn segments(&self) -> impl ExactSizeIterator<Item = PathSegment<'_>> {
        self.0.iter().map(|segment| match segment {
            OwnedSegment::Key(key) => PathSegment::Key(key),
            OwnedSegment::Index(index) => PathSegment::Index(*index),
        })
    }

    pub(super) fn borrowed(&self) -> Vec<PathSegment<'_>> {
        self.segments().collect()
    }

    pub(super) fn starts_with(&self, prefix: &Self) -> bool {
        self.0.starts_with(&prefix.0)
    }

    pub(super) fn related(&self, other: &Self) -> bool {
        self.starts_with(other) || other.starts_with(self)
    }

    pub(super) fn push_key(&mut self, key: &str) {
        self.0.push(OwnedSegment::Key(key.to_owned()));
    }

    pub(super) fn push_index(&mut self, index: usize) {
        self.0.push(OwnedSegment::Index(index));
    }

    pub(super) fn pop(&mut self) {
        self.0.pop();
    }
}

impl fmt::Display for ConcretePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return formatter.write_str("<root>");
        }
        let mut first = true;
        for segment in &self.0 {
            match segment {
                OwnedSegment::Key(key) => {
                    if !first {
                        formatter.write_str(".")?;
                    }
                    write_key(formatter, key)?;
                }
                OwnedSegment::Index(index) => write!(formatter, "[{index}]")?,
            }
            first = false;
        }
        Ok(())
    }
}

fn write_key(formatter: &mut fmt::Formatter<'_>, key: &str) -> fmt::Result {
    if !key.is_empty()
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        formatter.write_str(key)
    } else {
        formatter.write_str("\"")?;
        for character in key.chars() {
            for escaped in character.escape_default() {
                write!(formatter, "{escaped}")?;
            }
        }
        formatter.write_str("\"")
    }
}

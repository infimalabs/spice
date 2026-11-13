//! The bounded read contract every output-bearing CLI surface admits through.
//!
//! A read shows what is relevant now, not everything that ever happened. Each
//! surface declares a [`Surface`]: small interactive defaults and its own
//! maxima, both inside the repository [`CEILING`]. A caller's explicit
//! `--limit`, `--max-bytes`, or `--max-text` raises detail within those
//! maxima; nothing makes the terminal answer unbounded.
//!
//! Records enter an [`Admission`] in relevance order, current/actionable
//! first. The first record that does not fit closes admission, so a smaller
//! later record never leapfrogs a more relevant one and a page boundary is a
//! single cursor. Every record is charged, including the first, so one
//! oversized record cannot bypass the aggregate byte limit.
//!
//! Nothing is withheld silently. A [`Bounded`] answer whose rows are not the
//! whole population carries an [`Omission`]: population, shown and withheld
//! counts, admitted bytes, which limit closed admission, the size of the next
//! record, and the exact command that continues at it. Its JSON form and its
//! [`Display`](fmt::Display) line state the same facts. Long text fields are
//! cut with [`clip`], which keeps a UTF-8 boundary and reports withheld bytes.
//!
//! Complete canonical documents and evidence never stream to the terminal
//! unbounded. They leave through an explicit file export, whose receipt is an
//! [`Export`]: the exact path, length, and [`Domain::CliExport`] digest.
//! Mutation replies are outside this contract: they stay complete and concise.
//!
//! [`Domain::CliExport`]: crate::Domain::CliExport

use std::fmt;
use std::num::NonZeroUsize;

use serde::{Deserialize, Serialize};

use crate::{Digest, SpiceCommand};

/// Flag that admits more rows.
pub const ROWS_FLAG: &str = "--limit";
/// Flag that admits more aggregate bytes.
pub const BYTES_FLAG: &str = "--max-bytes";
/// Flag that admits longer text fields.
pub const TEXT_FLAG: &str = "--max-text";
/// Flag that resumes after a continuation cursor.
pub const CURSOR_FLAG: &str = "--after";

/// The repository maximum no surface may exceed, whatever a caller asks for.
pub const CEILING: Limits = Limits {
    rows: 1_000,
    bytes: 262_144,
    text: 16_384,
};

/// Interactive defaults and maxima for an ordinary read surface.
pub const INTERACTIVE: Surface = Surface::new(
    Limits {
        rows: 20,
        bytes: 8_192,
        text: 240,
    },
    Limits {
        rows: 500,
        bytes: 131_072,
        text: 4_096,
    },
);

/// Row, aggregate-byte, and per-field text ceilings for one read.
///
/// `bytes` counts the encoded JSON length of each admitted record, so the
/// charge is the same whichever form renders the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// Records admitted at most.
    pub rows: usize,
    /// Encoded bytes admitted at most across every record.
    pub bytes: usize,
    /// UTF-8 bytes one clipped text field keeps at most.
    pub text: usize,
}

impl Limits {
    const fn within(self, maximum: Self) -> bool {
        self.rows <= maximum.rows && self.bytes <= maximum.bytes && self.text <= maximum.text
    }
}

/// One surface's documented defaults and enforced maxima.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Surface {
    default: Limits,
    maximum: Limits,
}

impl Surface {
    /// Declare a surface.
    ///
    /// # Panics
    ///
    /// Refuses, at compile time when used in a `const`, defaults above the
    /// surface maxima, maxima above [`CEILING`], or a zero byte or text limit.
    #[must_use]
    pub const fn new(default: Limits, maximum: Limits) -> Self {
        assert!(
            default.bytes > 0 && default.text > 0,
            "a surface admits at least one byte and one text byte"
        );
        assert!(
            default.within(maximum),
            "surface defaults exceed its maxima"
        );
        assert!(
            maximum.within(CEILING),
            "surface maxima exceed the repository ceiling"
        );
        Self { default, maximum }
    }

    /// Limits a bare invocation uses.
    #[must_use]
    pub const fn default_limits(&self) -> Limits {
        self.default
    }

    /// Limits no invocation of this surface may exceed.
    #[must_use]
    pub const fn maximum(&self) -> Limits {
        self.maximum
    }

    /// Admit a caller's explicit requests against this surface.
    ///
    /// An absent request takes the default. Zero rows is a count-only read;
    /// zero bytes or zero text is refused.
    ///
    /// # Errors
    ///
    /// Returns [`LimitError`] naming the flag whose value is zero where that
    /// is meaningless or exceeds the surface maximum.
    pub fn budget(&self, request: Request) -> Result<Budget, LimitError> {
        let (default, maximum) = (self.default, self.maximum);
        Ok(Budget {
            limits: Limits {
                rows: admit(ROWS_FLAG, request.rows, default.rows, maximum.rows, true)?,
                bytes: admit(
                    BYTES_FLAG,
                    request.bytes,
                    default.bytes,
                    maximum.bytes,
                    false,
                )?,
                text: admit(TEXT_FLAG, request.text, default.text, maximum.text, false)?,
            },
            maximum,
            default_rows: default.rows,
        })
    }
}

fn admit(
    flag: &'static str,
    requested: Option<usize>,
    default: usize,
    maximum: usize,
    zero_allowed: bool,
) -> Result<usize, LimitError> {
    let value = requested.unwrap_or(default);
    if value == 0 && !zero_allowed {
        Err(LimitError::Zero { flag })
    } else if value > maximum {
        Err(LimitError::Exceeded {
            flag,
            requested: value,
            maximum,
        })
    } else {
        Ok(value)
    }
}

/// A caller's explicit limit flags; absent fields take the surface default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Request {
    /// `--limit`.
    pub rows: Option<usize>,
    /// `--max-bytes`.
    pub bytes: Option<usize>,
    /// `--max-text`.
    pub text: Option<usize>,
}

/// A limit request the surface does not admit.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LimitError {
    /// The flag's value exceeds the surface maximum.
    #[error("{flag}={requested} exceeds the maximum {maximum}")]
    Exceeded {
        /// Offending flag.
        flag: &'static str,
        /// Requested value.
        requested: usize,
        /// Surface maximum.
        maximum: usize,
    },
    /// The flag cannot be zero.
    #[error("{flag} must be at least 1")]
    Zero {
        /// Offending flag.
        flag: &'static str,
    },
}

/// The admitted limits of one read, with the surface maxima they sit under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    limits: Limits,
    maximum: Limits,
    default_rows: usize,
}

impl Budget {
    /// Limits this read admits.
    #[must_use]
    pub const fn limits(&self) -> Limits {
        self.limits
    }

    /// Limits this read's continuation carries: a count-only read resumes
    /// with the surface's default row limit, so following it shows rows.
    #[must_use]
    pub const fn resume(&self) -> Limits {
        Limits {
            rows: if self.limits.rows == 0 {
                self.default_rows
            } else {
                self.limits.rows
            },
            ..self.limits
        }
    }

    /// Cut one text field to this read's text limit.
    #[must_use]
    pub fn clip(&self, text: &str) -> ClippedText {
        clip(text, self.limits.text)
    }
}

/// Keep at most `max_bytes` of `text`, ending on a UTF-8 boundary.
#[must_use]
pub fn clip(text: &str, max_bytes: usize) -> ClippedText {
    let end = text.floor_char_boundary(max_bytes);
    ClippedText {
        text: text[..end].to_owned(),
        withheld_bytes: NonZeroUsize::new(text.len() - end),
    }
}

/// A text field and the bytes cut from its end, if any.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClippedText {
    /// Kept prefix.
    pub text: String,
    /// Bytes cut from the end; absent when the field is whole.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub withheld_bytes: Option<NonZeroUsize>,
}

impl fmt::Display for ClippedText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text)?;
        match self.withheld_bytes {
            Some(withheld) => write!(formatter, "… [+{withheld} bytes]"),
            None => Ok(()),
        }
    }
}

/// Which limit closed admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Exhausted {
    /// The row limit was reached.
    Rows,
    /// The next record did not fit the remaining aggregate bytes.
    Bytes,
}

/// Where the continuation of a closed admission resumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resume<'a, T> {
    /// Resume at this record under the same limits.
    At(&'a T),
    /// Resume at this record with `--max-bytes` raised to `bytes`, which the
    /// record alone needs and the surface maximum admits.
    Widened {
        /// First withheld record.
        record: &'a T,
        /// Byte limit the record alone needs.
        bytes: usize,
    },
    /// The record alone exceeds the surface byte maximum; only an exact file
    /// export can carry it.
    Export(&'a T),
}

/// Relevance-ordered admission of records against one [`Budget`].
#[derive(Debug)]
pub struct Admission<T> {
    budget: Budget,
    rows: Vec<T>,
    shown_bytes: usize,
    population: usize,
    stop: Option<Stop<T>>,
}

#[derive(Debug)]
struct Stop<T> {
    exhausted: Exhausted,
    bytes: usize,
    record: T,
}

impl<T: Serialize> Admission<T> {
    /// Open an admission under one budget.
    #[must_use]
    pub const fn new(budget: Budget) -> Self {
        Self {
            budget,
            rows: Vec::new(),
            shown_bytes: 0,
            population: 0,
            stop: None,
        }
    }

    /// Offer the next record in relevance order; true when it was admitted.
    ///
    /// After the first refusal admission is closed: later offers are counted
    /// as withheld without being measured.
    ///
    /// # Errors
    ///
    /// Returns the encoder error when the record cannot be measured.
    pub fn offer(&mut self, record: T) -> Result<bool, serde_json::Error> {
        self.population += 1;
        if self.stop.is_some() {
            return Ok(false);
        }
        let bytes = crate::encoded_len(&record)?;
        let exhausted = if self.rows.len() >= self.budget.limits.rows {
            Exhausted::Rows
        } else if bytes > self.budget.limits.bytes - self.shown_bytes {
            Exhausted::Bytes
        } else {
            self.shown_bytes += bytes;
            self.rows.push(record);
            return Ok(true);
        };
        self.stop = Some(Stop {
            exhausted,
            bytes,
            record,
        });
        Ok(false)
    }

    /// Whether a record has been refused, so later records are only counted.
    #[must_use]
    pub const fn is_closed(&self) -> bool {
        self.stop.is_some()
    }

    /// Count `count` further records without materializing them.
    ///
    /// # Panics
    ///
    /// Panics when admission is still open and `count` is non-zero: an
    /// unmeasured record before the first refusal would be a hidden omission
    /// with no cursor.
    pub fn withhold(&mut self, count: usize) {
        assert!(
            count == 0 || self.stop.is_some(),
            "records can be withheld uncounted only after admission closes"
        );
        self.population += count;
    }

    /// Close the admission; `continuation` builds the command that resumes at
    /// the first withheld record.
    pub fn finish<C, F>(self, continuation: F) -> Bounded<T, C>
    where
        F: FnOnce(Resume<'_, T>) -> C,
    {
        let shown = self.rows.len();
        let omission = self.stop.map(|stop| {
            let resume =
                if stop.exhausted == Exhausted::Rows || stop.bytes <= self.budget.limits.bytes {
                    Resume::At(&stop.record)
                } else if stop.bytes <= self.budget.maximum.bytes {
                    Resume::Widened {
                        record: &stop.record,
                        bytes: stop.bytes,
                    }
                } else {
                    Resume::Export(&stop.record)
                };
            Box::new(Omission {
                population: self.population,
                shown,
                withheld: self.population - shown,
                shown_bytes: self.shown_bytes,
                exhausted: stop.exhausted,
                next_bytes: stop.bytes,
                limits: self.budget.limits,
                continuation: continuation(resume),
            })
        });
        Bounded {
            rows: self.rows,
            omission,
        }
    }
}

/// Admitted rows and, when they are not the whole population, the omission.
///
/// `C` is how a caller continues: a [`SpiceCommand`] for every `spice` CLI
/// read, or a surface's own cursor type when its caller is not the CLI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    deny_unknown_fields,
    bound(deserialize = "T: Deserialize<'de>, C: Deserialize<'de>")
)]
pub struct Bounded<T, C = SpiceCommand> {
    /// Admitted records, in relevance order.
    pub rows: Vec<T>,
    /// What was withheld and how to continue; absent when nothing was.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::optional::present"
    )]
    pub omission: Option<Box<Omission<C>>>,
}

/// Every fact about the records a bounded answer withheld.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Omission<C = SpiceCommand> {
    /// Records offered in total.
    pub population: usize,
    /// Records admitted.
    pub shown: usize,
    /// Records withheld; `population - shown`.
    pub withheld: usize,
    /// Encoded bytes the admitted records spent.
    pub shown_bytes: usize,
    /// Limit that closed admission.
    pub exhausted: Exhausted,
    /// Encoded bytes of the first withheld record.
    pub next_bytes: usize,
    /// Limits the read ran under.
    pub limits: Limits,
    /// Exact command or cursor that resumes at the first withheld record.
    pub continuation: C,
}

impl<C: fmt::Display> fmt::Display for Omission<C> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (limit, value) = match self.exhausted {
            Exhausted::Rows => ("row", self.limits.rows),
            Exhausted::Bytes => ("byte", self.limits.bytes),
        };
        write!(
            formatter,
            "withheld {} of {} rows ({} shown in {} bytes; {limit} limit {value}; next row {} \
             bytes); continue: {}",
            self.withheld,
            self.population,
            self.shown,
            self.shown_bytes,
            self.next_bytes,
            self.continuation,
        )
    }
}

/// Receipt for one complete document written to an explicit file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Export {
    /// Resolved destination path.
    pub path: String,
    /// Exact byte length written.
    pub bytes: u64,
    /// [`Domain::CliExport`](crate::Domain::CliExport) digest of those bytes.
    pub digest: Digest,
}

impl fmt::Display for Export {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "exported {} bytes to {} digest {}",
            self.bytes, self.path, self.digest
        )
    }
}

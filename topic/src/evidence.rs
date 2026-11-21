//! Lexical evidence of one task statement.
//!
//! [`prepare_fields`] is fit-independent: per-field term counts and the
//! admitted terms of one statement. A set of prepared tasks is fitted by
//! [`crate::words`], which yields their graph rows.

use std::collections::BTreeMap;

use crate::lexical::{self, Counts};

pub use crate::lexical::{names_term, spellings};

/// Authored fields of a statement: title, pitch, description, acceptance.
pub const FIELDS: usize = 4;
/// Keep title, then purpose and acceptance, then description at the cap.
const ADMISSION_TIERS: [u8; FIELDS] = [3, 2, 1, 2];
/// Most distinct terms one task contributes.
pub const TERM_BUDGET: usize = 256;
/// Greatest count of one term that routing evidence distinguishes.
pub const COUNT_CEILING: u32 = 255;

/// One statement's fit-independent evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepared {
    pub id: String,
    /// Counts of admitted terms in each field, as authored.
    pub fields: [Counts; crate::evidence::FIELDS],
    /// Number of nonblank authored acceptance items, independent of their words.
    pub(crate) acceptance_items: usize,
    /// Counts of admitted terms over all fields, held to [`COUNT_CEILING`].
    pub admitted: Counts,
}

/// Count the provided fields, admitting terms by strongest field, then by
/// frequency and spelling, up to [`TERM_BUDGET`]. An absent field is an empty slice.
#[must_use]
pub fn prepare_fields(id: &str, texts: [&[String]; FIELDS]) -> Prepared {
    prepare_counts(id, texts.map(lexical::counts), texts[3])
}

/// Prepare exactly the authored text available to a task filing command.
#[must_use]
pub fn prepare_statement(
    id: &str,
    title: &str,
    pitch: &str,
    description: Option<&str>,
    terms: &[String],
) -> Prepared {
    prepare_counts(
        id,
        [
            lexical::counts([title]),
            lexical::counts([pitch]),
            lexical::counts(description),
            lexical::counts(terms),
        ],
        terms,
    )
}

fn prepare_counts(id: &str, mut fields: [Counts; FIELDS], terms: &[String]) -> Prepared {
    let own = id.to_lowercase();
    let mut total = BTreeMap::<&str, (u32, u8)>::new();
    for (field, tier) in fields.iter().zip(ADMISSION_TIERS) {
        for (term, count) in field {
            if *term != own {
                let (frequency, strongest) = total.entry(term.as_str()).or_default();
                *frequency += count;
                *strongest = (*strongest).max(tier);
            }
        }
    }
    let mut order: Vec<_> = total.into_iter().collect();
    if order.len() > TERM_BUDGET {
        order.sort_unstable_by(|(word, (count, tier)), (other, (frequency, strongest))| {
            strongest
                .cmp(tier)
                .then(frequency.cmp(count))
                .then(word.cmp(other))
        });
        order.truncate(TERM_BUDGET);
    }
    let admitted: Counts = order
        .into_iter()
        .map(|(term, (count, _))| (term.to_owned(), count.min(COUNT_CEILING)))
        .collect();
    for field in &mut fields {
        field.retain(|term, _| admitted.contains_key(term));
    }
    Prepared {
        id: id.to_owned(),
        fields,
        acceptance_items: terms.iter().filter(|term| !term.trim().is_empty()).count(),
        admitted,
    }
}

/// A term of a fitted set: the number its word holds while a row counts it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TermId(pub u32);

/// One task's weights over its terms, in the spelling order of their words.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SparseRow(Vec<(TermId, f64)>);

impl SparseRow {
    #[must_use]
    pub fn entries(&self) -> &[(TermId, f64)] {
        &self.0
    }
}

impl FromIterator<(TermId, f64)> for SparseRow {
    fn from_iter<I: IntoIterator<Item = (TermId, f64)>>(weights: I) -> Self {
        Self(weights.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::{COUNT_CEILING, TERM_BUDGET, prepare_statement};

    #[test]
    fn admission_prefers_stronger_fields_then_frequency_and_drops_own_identity() {
        let filler: String = (0..TERM_BUDGET + 40)
            .map(|index| {
                let letters: String = index
                    .to_string()
                    .bytes()
                    .map(|digit| char::from(b"bcdfghjklm"[usize::from(digit - b'0')]))
                    .collect();
                format!("word{letters} word{letters} ")
            })
            .collect();
        let prepared = prepare_statement(
            "zebra",
            "Zebra crossing aardvark",
            "Urgency",
            Some(&format!("{filler} zebra gadget {}", "lattice ".repeat(300))),
            &["quokka".into()],
        );
        assert_eq!(prepared.admitted.len(), TERM_BUDGET);
        assert_eq!(prepared.admitted.get("crossing"), Some(&1));
        assert_eq!(prepared.admitted.get("aardvark"), Some(&1));
        assert!(prepared.fields[1].contains_key("urgency"));
        assert!(prepared.fields[3].contains_key("quokka"));
        assert_eq!(prepared.admitted.get("lattice"), Some(&COUNT_CEILING));
        assert_eq!(prepared.fields[2].get("lattice"), Some(&300));
        assert!(!prepared.admitted.contains_key("zebra"));
        assert!(!prepared.admitted.contains_key("gadget"));
        assert!(
            prepared
                .fields
                .iter()
                .flatten()
                .all(|(term, _)| prepared.admitted.contains_key(term))
        );
    }
}

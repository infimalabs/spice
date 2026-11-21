//! The words of the rows a tree holds, each numbered once.
//!
//! A tree fits every set whose tasks changed on every arrival. Comparing
//! spellings there costs far more than the fit, so a row's words become
//! slots when it arrives and stay numbers until it leaves. All of it
//! follows from the rows.
//!
//! [`Fits::graph`] is one cold fit over a set and carries nothing from any
//! other fit. It weights words by logarithmic term frequency times smoothed
//! inverse document frequency and normalises each field on its own. The
//! weighted fields are then combined and normalised again.

use std::collections::{BTreeMap, VecDeque};

use crate::evidence::{FIELDS, Prepared, SparseRow, TermId};

#[derive(Clone, Default)]
pub(crate) struct Words {
    /// Slot of every word a held row counts, by spelling.
    slots: BTreeMap<String, usize>,
    /// How many times held rows count each slot's word; at zero it is free.
    uses: Vec<u32>,
    free: Vec<usize>,
    rows: VecDeque<Numbered>,
}

/// One row's evidence over slots.
#[derive(Clone)]
pub(crate) struct Numbered {
    /// Admitted words, in spelling order.
    admitted: Vec<usize>,
    /// Each field's counted words, in spelling order.
    fields: [Vec<Counted>; FIELDS],
    /// `ln(1 + items)`: acceptance weighs more the more items state it.
    acceptance_weight: f64,
}

#[derive(Clone, Copy)]
struct Counted {
    slot: usize,
    /// Where the word sits among the row's admitted ones.
    seat: Option<usize>,
    /// `ln(count) + 1`.
    gain: f64,
}

/// A row that has not arrived, numbered beside the held ones.
pub(crate) struct Foreseen {
    pub(crate) row: Numbered,
    /// How many slots past the held ones the row's own words take.
    pub(crate) beyond: usize,
}

fn counted(row: &Prepared) -> impl Iterator<Item = (&String, &u32)> {
    row.fields.iter().flatten().filter(|&(_, &count)| count > 0)
}

fn number<'a>(row: &'a Prepared, mut slot: impl FnMut(&'a str) -> usize) -> Numbered {
    let admitted: Vec<usize> = row.admitted.iter().map(|word| slot(word)).collect();
    let fields = std::array::from_fn(|field| {
        (row.fields[field].iter())
            .filter(|&(_, &count)| count > 0)
            .map(|(word, &count)| {
                let slot = slot(word);
                Counted {
                    slot,
                    seat: admitted.iter().position(|&held| held == slot),
                    gain: f64::from(count).ln() + 1.0,
                }
            })
            .collect()
    });
    Numbered {
        admitted,
        fields,
        acceptance_weight: (row.acceptance_items as f64).ln_1p(),
    }
}

impl Words {
    /// Number the row that arrives after every held one.
    pub(crate) fn push(&mut self, row: &Prepared) {
        let numbered = number(row, |word| {
            let slot = match self.slots.get(word) {
                Some(&slot) => slot,
                None => {
                    let slot = self.free.pop().unwrap_or_else(|| {
                        self.uses.push(0);
                        self.uses.len() - 1
                    });
                    self.slots.insert(word.to_owned(), slot);
                    slot
                }
            };
            self.uses[slot] += 1;
            slot
        });
        self.rows.push_back(numbered);
    }

    /// Forget the evicted row, releasing slots no other row needs.
    pub(crate) fn remove(&mut self, index: usize, row: &Prepared) {
        self.rows.remove(index);
        for word in (row.admitted.iter()).chain(counted(row).map(|(word, _)| word)) {
            let slot = self.slots[word.as_str()];
            self.uses[slot] -= 1;
            if self.uses[slot] == 0 {
                self.slots.remove(word.as_str());
                self.free.push(slot);
            }
        }
    }

    /// The held row at `index`, oldest first.
    pub(crate) fn row(&self, index: usize) -> &Numbered {
        &self.rows[index]
    }

    /// Number a row beside the held ones without holding it.
    pub(crate) fn foresee(&self, row: &Prepared) -> Foreseen {
        let mut unheld: BTreeMap<&str, usize> = BTreeMap::new();
        let row = number(row, |word| match self.slots.get(word) {
            Some(&slot) => slot,
            None => {
                let next = self.uses.len() + unheld.len();
                *unheld.entry(word).or_insert(next)
            }
        });
        Foreseen {
            row,
            beyond: unheld.len(),
        }
    }

    /// Working space for fits over the held rows and a foreseen one that
    /// brings `beyond` slots.
    pub(crate) fn fits(&self, beyond: usize) -> Fits {
        Fits {
            frequency: vec![0; self.uses.len() + beyond],
            column: vec![0; self.uses.len() + beyond],
            admitted: Vec::new(),
            inverse: Vec::new(),
            weights: Vec::new(),
        }
    }
}

/// Working space of the fits one arrival makes. A fit touches only its own
/// set's words, so nothing here is cleared whole between fits.
pub(crate) struct Fits {
    /// How many tasks of the set being fitted admit each slot's word; zero
    /// between fits.
    frequency: Vec<u32>,
    /// The column each slot in `admitted` takes in the set being fitted.
    column: Vec<u32>,
    /// Slots the set being fitted admits, in the order its tasks list them.
    admitted: Vec<usize>,
    inverse: Vec<f64>,
    weights: Vec<f64>,
}

impl Fits {
    /// Graph rows of `tasks` fitted as a set of their own, in task order.
    /// A row lists a task's admitted words in spelling order, each under
    /// the column the set gives it.
    pub(crate) fn graph(&mut self, tasks: &[&Numbered]) -> Vec<SparseRow> {
        let Self {
            frequency,
            column,
            admitted,
            inverse,
            weights,
        } = self;
        for task in tasks {
            for &slot in &task.admitted {
                if frequency[slot] == 0 {
                    column[slot] = admitted.len() as u32;
                    admitted.push(slot);
                }
                frequency[slot] += 1;
            }
        }
        // Smoothed inverse document frequency of a word `frequency` tasks admit.
        inverse.clear();
        inverse.extend(
            (0..=tasks.len())
                .map(|frequency| ((tasks.len() + 1) as f64 / (frequency + 1) as f64).ln() + 1.0),
        );
        let rows = (tasks.iter())
            .map(|task| {
                let mut row: Vec<(TermId, f64)> = (task
                    .admitted
                    .iter()
                    .map(|&slot| (TermId(column[slot]), 0.0)))
                .collect();
                // Title, pitch, description, acceptance.
                let shares = [2.0, 1.5, 0.5, task.acceptance_weight];
                for (field, share) in task.fields.iter().zip(shares) {
                    weights.clear();
                    weights.extend(
                        (field.iter())
                            .map(|word| word.gain * inverse[frequency[word.slot] as usize]),
                    );
                    let length = (weights.iter().map(|weight| weight * weight))
                        .sum::<f64>()
                        .sqrt();
                    for (word, weight) in field.iter().zip(weights.iter()) {
                        if let Some(seat) = word.seat {
                            row[seat].1 += share * (weight / length);
                        }
                    }
                }
                let length = (row.iter().map(|(_, weight)| weight * weight))
                    .sum::<f64>()
                    .sqrt();
                for (_, weight) in &mut row {
                    *weight /= length;
                }
                row.into_iter().collect()
            })
            .collect();
        for slot in admitted.drain(..) {
            frequency[slot] = 0;
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::{Fits, Numbered, Words};
    use crate::evidence::{Prepared, prepare_fields};

    fn task(id: &str, title: &str, description: &str) -> Prepared {
        prepare_fields(id, [&[title.into()], &[], &[description.into()], &[]])
    }

    fn held(words: &Words) -> Vec<&Numbered> {
        words.rows.iter().collect()
    }

    /// Each fitted row's weights in the spelling order of its words.
    fn weights(fits: &mut Fits, tasks: &[&Numbered]) -> Vec<Vec<f64>> {
        (fits.graph(tasks).iter())
            .map(|row| row.entries().iter().map(|entry| entry.1).collect())
            .collect()
    }

    fn fitted(rows: &[Prepared]) -> Vec<Vec<f64>> {
        let mut words = Words::default();
        for row in rows {
            words.push(row);
        }
        weights(&mut words.fits(0), &held(&words))
    }

    #[test]
    fn rarer_shared_words_weigh_more_in_rows_of_unit_length() {
        let rows = fitted(&[
            task("a", "Parser recovers after malformed input", ""),
            task("b", "Parser accepts unicode input", ""),
            task("c", "Scheduler recovers stalled lanes", ""),
        ]);
        // Spelling order of the first row: input, malformed, parser, recover.
        assert_eq!(rows[0].len(), 4);
        assert!(rows[0][1] > rows[0][2]);
        let length: f64 = rows[0].iter().map(|weight| weight * weight).sum();
        assert!((length - 1.0).abs() < 1e-12);
    }

    #[test]
    fn pitch_and_acceptance_match_more_strongly_than_description_alone() {
        use crate::evidence::prepare_statement;

        let task = |title, pitch, description, term: &str| {
            prepare_statement("task", title, pitch, Some(description), &[term.into()])
        };
        let rows = [
            task("beacon", "workbench", "turnstile", "notebook"),
            task("workbench", "beacon", "turnstile", "notebook"),
            task("workbench", "turnstile", "beacon", "notebook"),
            task("workbench", "turnstile", "notebook", "beacon"),
            prepare_statement("topic", "", "", Some("beacon"), &[]),
        ];
        let fit = fitted(&rows);
        // The described topic has only one term and a unit vector. Each
        // statement's cosine with it is therefore its fitted beacon weight.
        let [title, pitch, description, acceptance] =
            std::array::from_fn(|i| fit[i][0] * fit[4][0]);
        assert!(title > pitch && title > acceptance);
        assert!(pitch > description && acceptance > description);
        assert!(description > 0.0);
    }

    #[test]
    fn acceptance_can_outweigh_title_with_ten_authored_items() {
        use crate::evidence::prepare_statement;

        let acceptance = [
            "beacon", "cache", "decoder", "encoder", "fixture", "guard", "header", "index",
            "journal", "ledger",
        ];
        // Only the split into items changes, not the words or their counts.
        let [one, several, ten] = [10, 3, 1].map(|words_per_item| {
            let items: Vec<_> = acceptance
                .chunks(words_per_item)
                .map(|words| words.join(" "))
                .collect();
            prepare_statement("task", "abacus", "", None, &items)
        });
        let fixed = prepare_statement("topic", "", "", Some(&acceptance.join(" ")), &[]);
        let fit = fitted(&[one, several, ten, fixed]);
        let acceptance_cosine = |i: usize| {
            fit[i][1..]
                .iter()
                .zip(&fit[3])
                .map(|(a, b)| a * b)
                .sum::<f64>()
        };
        assert!(acceptance_cosine(0) < acceptance_cosine(1));
        assert!(acceptance_cosine(1) < fit[1][0]);
        assert!(acceptance_cosine(2) > fit[2][0]);
    }

    #[test]
    fn numbering_kept_across_arrivals_and_departures_weighs_like_a_fresh_one() {
        let rows = [
            task(
                "a",
                "Parser recovers after malformed input",
                "zebra crossing",
            ),
            task("b", "Parser accepts unicode input", "aardvark burrow"),
            task("c", "Scheduler recovers stalled lanes", "parser backlog"),
            task("d", "Scheduler drains queued lanes", "unicode backlog"),
            task("e", "Mailbox retries aardvark delivery", "quokka parser"),
        ];
        let mut kept = Words::default();
        for row in &rows[..4] {
            kept.push(row);
        }
        kept.remove(0, &rows[0]);
        kept.remove(0, &rows[1]);
        // The last row is weighed the same foreseen as arrived, when it
        // takes slots the departed rows freed. One working space serves a
        // fit of some of the rows and then a fit of all of them.
        let foreseen = kept.foresee(&rows[4]);
        let mut beside = held(&kept);
        beside.push(&foreseen.row);
        let mut fits = kept.fits(foreseen.beyond);
        assert_eq!(weights(&mut fits, &beside[1..]), fitted(&rows[3..]));
        let expected = fitted(&rows[2..]);
        assert_eq!(weights(&mut fits, &beside), expected);
        kept.push(&rows[4]);
        assert_eq!(weights(&mut kept.fits(0), &held(&kept)), expected);
    }
}

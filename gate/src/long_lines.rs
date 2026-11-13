//! Per-file attribution for Rust source lines longer than the current limit.
//!
//! A repository-wide count lets one file pay for another. This scanner instead
//! compares each current Rust file only with the same path at captured `HEAD`.
//! Existing long-line text is consumed as a per-file multiset; every remaining
//! current occurrence is an introduced line and retains its current locus.

#[cfg(not(target_arch = "wasm32"))]
use std::collections::BTreeMap;
#[cfg(not(target_arch = "wasm32"))]
use std::fs;
#[cfg(not(target_arch = "wasm32"))]
use std::io;

use serde::{Deserialize, Serialize};

use crate::input::LONG_LINE_LIMIT;
#[cfg(not(target_arch = "wasm32"))]
use crate::jurisdiction::Jurisdiction;
#[cfg(not(target_arch = "wasm32"))]
use crate::metrics::CommittedTexts;

/// One newly introduced over-limit Rust source line.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LongLineFinding {
    path: String,
    line: u64,
    bytes: u64,
    baseline_debt: u64,
    current_debt: u64,
}

impl LongLineFinding {
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn new(
        path: impl Into<String>,
        line: u64,
        bytes: u64,
        baseline_debt: u64,
        current_debt: u64,
    ) -> Self {
        Self {
            path: path.into(),
            line,
            bytes,
            baseline_debt,
            current_debt,
        }
    }

    /// Repository-relative Rust source path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// One-based current source line.
    #[must_use]
    pub const fn line(&self) -> u64 {
        self.line
    }

    /// Byte length of the current source line.
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Number of over-limit lines in this file at captured `HEAD`.
    #[must_use]
    pub const fn baseline_debt(&self) -> u64 {
        self.baseline_debt
    }

    /// Number of over-limit lines in this file now.
    #[must_use]
    pub const fn current_debt(&self) -> u64 {
        self.current_debt
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn scan_jurisdiction(
    jurisdiction: &Jurisdiction,
    baselines: &CommittedTexts,
) -> Result<Vec<LongLineFinding>, LongLineReadError> {
    let mut inputs = Vec::new();
    for entry in jurisdiction.measurable() {
        if !entry.path().ends_with(".rs") {
            continue;
        }
        let path = entry.path().to_owned();
        let bytes =
            fs::read(jurisdiction.root().join(&path)).map_err(|source| LongLineReadError {
                path: path.clone(),
                source,
            })?;
        let source = String::from_utf8_lossy(&bytes).into_owned();
        inputs.push(LongLineInput {
            baseline: baselines.baseline(&path, &bytes).map(str::to_owned),
            path,
            source,
        });
    }
    Ok(scan(inputs))
}

#[cfg(not(target_arch = "wasm32"))]
struct LongLineInput {
    path: String,
    source: String,
    baseline: Option<String>,
}

#[cfg(not(target_arch = "wasm32"))]
fn scan(mut inputs: Vec<LongLineInput>) -> Vec<LongLineFinding> {
    inputs.sort_by(|left, right| left.path.cmp(&right.path));
    let mut findings = Vec::new();

    for input in inputs {
        let Some(baseline) = input.baseline.as_deref() else {
            scan_file(&input.path, &input.source, "", &mut findings);
            continue;
        };
        if baseline != input.source {
            scan_file(&input.path, &input.source, baseline, &mut findings);
        }
    }
    findings
}

pub(crate) fn validate(findings: &[LongLineFinding]) -> Result<(), &'static str> {
    let mut previous: Option<(&str, u64)> = None;
    for finding in findings {
        if finding.path().is_empty()
            || finding.path().contains(['\n', '\r'])
            || !finding.path().ends_with(".rs")
            || finding.line() == 0
            || finding.bytes() <= LONG_LINE_LIMIT as u64
            || finding.current_debt() == 0
        {
            return Err("a rust.long-lines finding has an invalid path, locus, or debt");
        }
        let current = (finding.path(), finding.line());
        if previous.is_some_and(|earlier| earlier >= current) {
            return Err("rust.long-lines findings are not sorted and unique by path and locus");
        }
        previous = Some(current);
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn scan_file(path: &str, current: &str, baseline: &str, findings: &mut Vec<LongLineFinding>) {
    let mut inherited = BTreeMap::<&str, u64>::new();
    let mut baseline_debt = 0_u64;
    for line in baseline
        .split('\n')
        .filter(|line| line.len() > LONG_LINE_LIMIT)
    {
        baseline_debt += 1;
        *inherited.entry(line).or_default() += 1;
    }

    let current_lines = current.split('\n').collect::<Vec<_>>();
    let current_debt = u64::try_from(
        current_lines
            .iter()
            .filter(|line| line.len() > LONG_LINE_LIMIT)
            .count(),
    )
    .expect("a source file cannot contain more lines than u64 can name");

    for (offset, line) in current_lines.into_iter().enumerate() {
        if line.len() <= LONG_LINE_LIMIT {
            continue;
        }
        if let Some(remaining) = inherited.get_mut(line)
            && *remaining > 0
        {
            *remaining -= 1;
            continue;
        }
        findings.push(LongLineFinding::new(
            path,
            u64::try_from(offset).expect("a source locus fits u64") + 1,
            u64::try_from(line.len()).expect("a source-line length fits u64"),
            baseline_debt,
            current_debt,
        ));
    }
}

/// Why one selected current Rust source could not be read.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub(crate) struct LongLineReadError {
    pub(crate) path: String,
    pub(crate) source: io::Error,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn long(label: &str) -> String {
        format!("{label}{}", "x".repeat(LONG_LINE_LIMIT + 1))
    }

    #[test]
    fn input_order_cannot_move_path_and_line_order() {
        let a = long("a");
        let b = long("b");
        let findings = scan(vec![
            LongLineInput {
                path: "z.rs".to_owned(),
                source: format!("short\n{b}\n"),
                baseline: Some("short\n".to_owned()),
            },
            LongLineInput {
                path: "a.rs".to_owned(),
                source: format!("{a}\nshort\n"),
                baseline: Some("short\n".to_owned()),
            },
        ]);

        assert_eq!(
            findings
                .iter()
                .map(|finding| (finding.path(), finding.line()))
                .collect::<Vec<_>>(),
            [("a.rs", 1), ("z.rs", 2)]
        );
    }

    #[test]
    fn equal_repository_totals_cannot_cancel_debt_between_files() {
        let removed = long("removed");
        let introduced = long("introduced");
        let findings = scan(vec![
            LongLineInput {
                path: "src/remove.rs".to_owned(),
                source: "short\n".to_owned(),
                baseline: Some(format!("{removed}\n")),
            },
            LongLineInput {
                path: "src/add.rs".to_owned(),
                source: format!("short\n{introduced}\n"),
                baseline: Some("short\n".to_owned()),
            },
        ]);

        let [finding] = findings.as_slice() else {
            panic!("one introduced line must remain attributable")
        };
        assert_eq!((finding.path(), finding.line()), ("src/add.rs", 2));
        assert_eq!((finding.baseline_debt(), finding.current_debt()), (0, 1));
    }

    #[test]
    fn new_files_start_without_inherited_debt() {
        let findings = scan(vec![LongLineInput {
            path: "src/new.rs".to_owned(),
            source: format!("{}\n", long("new")),
            baseline: None,
        }]);

        let [finding] = findings.as_slice() else {
            panic!("the new over-limit line must be attributed")
        };
        assert_eq!((finding.baseline_debt(), finding.current_debt()), (0, 1));
    }

    #[test]
    fn inherited_debt_is_a_per_file_multiset_of_exact_lines() {
        let inherited = long("inherited");
        let introduced = long("introduced");
        let findings = scan(vec![LongLineInput {
            path: "src/lib.rs".to_owned(),
            source: format!("{inherited}\n{introduced}\n"),
            baseline: Some(format!("{inherited}\n")),
        }]);
        let [finding] = findings.as_slice() else {
            panic!("one additional long line must remain")
        };
        assert_eq!((finding.path(), finding.line()), ("src/lib.rs", 2));
        assert_eq!((finding.baseline_debt(), finding.current_debt()), (1, 2));

        let replacement = scan(vec![LongLineInput {
            path: "src/lib.rs".to_owned(),
            source: format!("{introduced}\n"),
            baseline: Some(format!("{inherited}\n")),
        }]);
        let [finding] = replacement.as_slice() else {
            panic!("different long text is new debt even when totals hold")
        };
        assert_eq!((finding.baseline_debt(), finding.current_debt()), (1, 1));
    }
}

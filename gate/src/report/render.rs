use super::*;

/// Name every cause refusing this tree, and the evidence behind each one.
///
/// Public so a gate caller outside this crate can explain a refused live
/// workspace with the same words the in-crate report fixtures use, instead of a
/// bare standing that names no metric and no path.
///
/// A refusal here says nothing about an earlier compiler or test step having
/// passed: those steps can be green on a tree these assertions reject.
/// Selected execution and explicit study arms reach this battery directly;
/// only the battery reads the ratchet against the working tree.
///
/// Long-line loci are retained report evidence. The per-file metric counts
/// come from re-running each metric's declared parser, so this attribution
/// cannot drift from what the gate measured. Every carrying file is listed and
/// none is dropped, heaviest first only so the order is stable to read: the
/// file that moved a baseline is often not the file that holds the most, so
/// the top of this list is where to start looking rather than the answer.
#[must_use]
pub fn refusal_detail(root: &Path, contract: &Contract, report: &GateReport) -> String {
    let mut detail = String::from(
        "the live workspace is refused, so this run carries neither a clean \
         standing nor a tightening proposal. a passing compiler or test step \
         does not contradict this: this battery is what reads the ratchet.",
    );
    if !report.long_lines().is_empty() {
        let _ = write!(
            detail,
            "\n\nrust.long-lines exceeds the {}-byte limit:",
            crate::LONG_LINE_LIMIT
        );
        for finding in report.long_lines() {
            let _ = write!(
                detail,
                "\n  {}:{} is {} bytes (limit {})",
                finding.path(),
                finding.line(),
                finding.bytes(),
                crate::LONG_LINE_LIMIT
            );
        }
    }
    for finding in report
        .findings()
        .iter()
        .filter(|finding| !finding.movement.passes())
    {
        let _ = write!(detail, "\n\n{} {}", finding.metric, finding.movement);
        let Some(metric) = contract
            .policy()
            .metrics()
            .iter()
            .find(|metric| metric.name() == finding.metric)
        else {
            let _ = write!(
                detail,
                "\n  nothing declares this metric, so the baseline is what moves"
            );
            continue;
        };
        let _ = write!(detail, "\n  counts {}", metric.target());
        let Some(measurement) = report
            .measurements()
            .iter()
            .find(|measurement| measurement.metric() == finding.metric)
        else {
            continue;
        };
        let mut carried: Vec<(u64, &str)> = measurement
            .files()
            .iter()
            .filter_map(|file| {
                let count = metric_count(root, metric, file)?;
                (count > 0).then_some((count, file.as_str()))
            })
            .collect();
        carried.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(right.1)));
        let attributed: u64 = carried.iter().map(|(count, _)| count).sum();
        if attributed == measurement.value() {
            let _ = write!(
                detail,
                "\n  {} of {} measured files hold all {}:",
                carried.len(),
                measurement.files().len(),
                measurement.value()
            );
        } else {
            let _ = write!(
                detail,
                "\n  these total {attributed}, not the measured {}, so read them \
                 as where to look:",
                measurement.value()
            );
        }
        for (count, file) in carried {
            let _ = write!(detail, "\n    {count} {file}");
        }
    }
    detail
}

/// Re-run one metric's declared parser over a single file.
///
/// Returns nothing when the parser cannot be run or does not print a count;
/// such a file drops out of the attribution, which the total then reports as
/// a shortfall rather than passing off as a clean breakdown.
fn metric_count(root: &Path, metric: &Metric, file: &str) -> Option<u64> {
    let output = Command::new(metric.parser().program())
        .args(metric.parser().arguments())
        .arg(file)
        .current_dir(root)
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

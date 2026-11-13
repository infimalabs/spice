//! Stable visible report for both Cargo cache root classes.

use std::io::Write;

use super::{BuildCacheDisposition, BuildCacheError, BuildCacheReport, BuildCacheRootKind};

pub(super) fn render_report(
    report: &BuildCacheReport,
    apply: bool,
    output: &mut impl Write,
) -> Result<(), BuildCacheError> {
    match &report.hierarchy {
        Some(hierarchy) => write!(output, "[build-cache] hierarchy={}", hierarchy.display()),
        None => write!(output, "[build-cache] hierarchy=none"),
    }
    .map_err(BuildCacheError::Output)?;
    writeln!(
        output,
        " scope=managed-build-and-worktree-targets mode={}",
        if apply { "apply" } else { "dry-run" },
    )
    .map_err(BuildCacheError::Output)?;
    writeln!(
        output,
        "[build-cache] total={} selected={} reclaimed={} kept={} skipped={} target={} available={}",
        format_bytes(report.total_bytes),
        format_bytes(report.selected_bytes),
        format_bytes(report.reclaimed_bytes),
        format_bytes(report.kept_bytes),
        format_bytes(report.skipped_bytes),
        format_bytes(report.target_bytes),
        report
            .available_bytes
            .map(format_bytes)
            .unwrap_or_else(|| "unknown".to_owned()),
    )
    .map_err(BuildCacheError::Output)?;
    for root in &report.roots {
        writeln!(
            output,
            "[build-cache] kind={} root={} boundary={} bytes={} last_use={} evidence={} locks={} disposition={}",
            root_kind_label(root.kind),
            root.path.display(),
            root.removal_boundary.display(),
            root.bytes,
            root.last_use_epoch_seconds,
            root.last_use_evidence.display(),
            root.locks.len(),
            disposition_label(&root.disposition),
        )
        .map_err(BuildCacheError::Output)?;
    }
    Ok(())
}

fn root_kind_label(kind: BuildCacheRootKind) -> &'static str {
    match kind {
        BuildCacheRootKind::ManagedBuild => "managed-build",
        BuildCacheRootKind::WorktreeTarget => "worktree-target",
    }
}

fn disposition_label(disposition: &BuildCacheDisposition) -> &str {
    match disposition {
        BuildCacheDisposition::Kept => "kept",
        BuildCacheDisposition::RetainedCurrent => "retained-current",
        BuildCacheDisposition::RetainedLiveWorktree => "retained-live-worktree",
        BuildCacheDisposition::SkippedActive => "skipped-active",
        BuildCacheDisposition::SkippedMalformed(_) => "skipped-malformed",
        BuildCacheDisposition::WouldReclaim => "would-reclaim",
        BuildCacheDisposition::Reclaimed => "reclaimed",
    }
}

fn format_bytes(bytes: u64) -> String {
    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;
    if bytes >= GIB {
        format!("{:.1}GiB({bytes})", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1}MiB({bytes})", bytes as f64 / MIB as f64)
    } else {
        format!("{bytes}B")
    }
}

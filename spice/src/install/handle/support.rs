use super::*;

/// Mint the publication name shared by the handle and file-maintenance layers.
pub(in super::super) fn temporary_name() -> OsString {
    OsString::from(format!(
        ".spice-install-{}-{}.tmp",
        std::process::id(),
        TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Name the one recoverable temporary for a retirement plan slot.
pub(in super::super) fn retirement_temporary_name(index: usize) -> OsString {
    OsString::from(format!(".spice-install-retirement-{index}.tmp"))
}

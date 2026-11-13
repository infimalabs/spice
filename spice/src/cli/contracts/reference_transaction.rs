use super::*;

/// Hook-only leaf intercepted before mount or repository configuration loading.
pub(super) const CONTRACT: Contract = row!(
    ["gate", "reference-transaction"],
    "gate reference-transaction preparing|prepared|committed|aborted",
    Local,
    None,
    [],
    [Daemonless, Toolful],
    REFERENCE_TRANSACTION_TOOLS,
    Reply,
    "gate::reference_transaction"
);

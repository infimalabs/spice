//! Opt-in warm-admission timing written only for benchmark diagnostics.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Serialize;

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct Report {
    version: u64,
    internal_total_micros: u64,
    appliance_capture_micros: u64,
    installed_appliance_exact_byte_audit_micros: u64,
    repository_core_micros: u64,
    tool_authority_micros: u64,
    fingerprint_index_receipt_reads_micros: u64,
    remaining_admission_micros: u64,
    git_subprocesses: u64,
}

pub(super) struct Profile {
    path: Option<PathBuf>,
    started: Instant,
    appliance_capture_micros: u64,
    installed_appliance_exact_byte_audit_micros: u64,
    repository_core_micros: u64,
    tool_authority_micros: u64,
    fingerprint_index_receipt_reads_micros: u64,
    remaining_admission_micros: u64,
    git_subprocesses_started: u64,
}

impl Profile {
    pub(super) fn disabled() -> Self {
        Self::new(None)
    }

    pub(super) fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            started: crate::process_started(),
            appliance_capture_micros: 0,
            installed_appliance_exact_byte_audit_micros: 0,
            repository_core_micros: 0,
            tool_authority_micros: 0,
            fingerprint_index_receipt_reads_micros: 0,
            remaining_admission_micros: 0,
            git_subprocesses_started: ::gate::core::git_subprocess_count(),
        }
    }

    pub(super) fn appliance_capture(&mut self, elapsed: Duration) {
        self.appliance_capture_micros = micros(elapsed);
    }

    pub(super) fn installed_appliance_exact_byte_audit(&mut self, elapsed: Duration) {
        self.installed_appliance_exact_byte_audit_micros = micros(elapsed);
    }

    pub(super) fn repository_core(&mut self, elapsed: Duration) {
        self.repository_core_micros = micros(elapsed);
    }

    pub(super) fn tool_authority(&mut self, elapsed: Duration) {
        self.tool_authority_micros = micros(elapsed);
    }

    pub(super) fn fingerprint_index_receipt_reads(&mut self, elapsed: Duration) {
        self.fingerprint_index_receipt_reads_micros = micros(elapsed);
    }

    pub(super) fn remaining_admission(&mut self, elapsed: Duration) {
        self.remaining_admission_micros = micros(elapsed);
    }

    pub(super) fn publish(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let report = Report {
            version: 1,
            internal_total_micros: micros(self.started.elapsed()),
            appliance_capture_micros: self.appliance_capture_micros,
            installed_appliance_exact_byte_audit_micros: self
                .installed_appliance_exact_byte_audit_micros,
            repository_core_micros: self.repository_core_micros,
            tool_authority_micros: self.tool_authority_micros,
            fingerprint_index_receipt_reads_micros: self.fingerprint_index_receipt_reads_micros,
            remaining_admission_micros: self.remaining_admission_micros,
            git_subprocesses: ::gate::core::git_subprocess_count()
                .saturating_sub(self.git_subprocesses_started),
        };
        if let Ok(bytes) = serde_json::to_vec(&report) {
            let _ = fs::write(path, bytes);
        }
    }
}

fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

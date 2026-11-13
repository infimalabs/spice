use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::{ProcessState, linux, membership};

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "sill-process-state-{label}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("exclusive scratch root is created");
        Self(fs::canonicalize(path).expect("scratch root canonicalizes"))
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// Publish one `/proc/<pid>/stat` record the way the kernel writes it.
    ///
    /// The executable name is deliberately given spaces and a closing paren so
    /// every record also holds the field-splitting rule honest: reading from the
    /// first paren instead of the last would take the name apart as fields.
    fn process(&self, pid: u32, state: char, group: u32) -> &Self {
        let directory = self.0.join(pid.to_string());
        fs::create_dir_all(&directory).expect("process directory is created");
        fs::write(
            directory.join("stat"),
            format!("{pid} (a name (with) parens) {state} 1 {group} 0 0\n"),
        )
        .expect("stat record is written");
        self
    }

    /// Publish a `/proc/<pid>/stat` the field reader cannot take apart.
    fn unreadable_process(&self, pid: u32) -> &Self {
        let directory = self.0.join(pid.to_string());
        fs::create_dir_all(&directory).expect("process directory is created");
        fs::write(directory.join("stat"), "no closing paren here\n").expect("stat is written");
        self
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Nothing in the group is absent, so a group answer can only come from members.
fn group_is_present() -> bool {
    false
}

/// The kernel no longer knows the group identity.
fn group_is_absent() -> bool {
    true
}

#[test]
fn only_a_terminal_answer_collapses_to_the_bit_consumers_take() {
    assert!(ProcessState::Terminal.observed_terminal());
    assert!(!ProcessState::Live.observed_terminal());
    assert!(
        !ProcessState::Indeterminate.observed_terminal(),
        "an unreadable probe was collapsed into an observed end"
    );
}

#[test]
fn an_unreadable_member_lowers_the_group_instead_of_reporting_it_live() {
    let fold = |members: &[ProcessState]| membership(members.iter().copied(), group_is_present);

    assert_eq!(
        fold(&[ProcessState::Terminal, ProcessState::Terminal]),
        ProcessState::Terminal
    );
    assert_eq!(
        fold(&[ProcessState::Terminal, ProcessState::Indeterminate]),
        ProcessState::Indeterminate,
        "a member the kernel would not describe was folded away"
    );
    assert_eq!(
        fold(&[ProcessState::Indeterminate, ProcessState::Live]),
        ProcessState::Live,
        "a positively observed live member must outrank an unreadable one"
    );
}

#[test]
fn a_group_with_no_members_is_settled_by_the_kernel_identity() {
    assert_eq!(
        membership([].into_iter(), group_is_absent),
        ProcessState::Terminal
    );
    assert_eq!(
        membership([].into_iter(), group_is_present),
        ProcessState::Indeterminate,
        "a scan that observed nothing while the group still resolves claimed an end"
    );
}

#[test]
fn the_published_proc_probes_read_the_kernel_directory() {
    let pid = std::process::id();

    // Both sides answer for the same host, so this holds wherever it runs and
    // pins the published probe to the directory the reader was handed.
    assert_eq!(
        linux::process_zombie(pid),
        linux::zombie_under(Path::new(linux::KERNEL_PROCESSES), pid),
        "the published probe read a different directory than the kernel's"
    );
    assert_eq!(
        linux::process_group_members_terminal(u32::MAX),
        ProcessState::Indeterminate,
        "a process identity wider than the kernel type was accepted"
    );
}

#[test]
fn the_proc_reader_classifies_an_unreadable_record_indeterminate() {
    let scratch = Scratch::new("unreadable");
    scratch.unreadable_process(41);

    assert_eq!(
        linux::zombie_under(scratch.path(), 40),
        ProcessState::Indeterminate,
        "a status record that could not be opened was answered for"
    );
    assert_eq!(
        linux::zombie_under(scratch.path(), 41),
        ProcessState::Indeterminate,
        "a status record that could not be taken apart was answered for"
    );
    assert_eq!(
        linux::zombie_under(&scratch.path().join("absent"), 41),
        ProcessState::Indeterminate
    );
}

#[test]
fn the_proc_reader_separates_a_running_process_from_a_dead_one() {
    let scratch = Scratch::new("states");
    scratch.process(10, 'R', 10);
    scratch.process(11, 'Z', 10);
    scratch.process(12, 'X', 10);

    assert_eq!(linux::zombie_under(scratch.path(), 10), ProcessState::Live);
    assert_eq!(
        linux::zombie_under(scratch.path(), 11),
        ProcessState::Terminal
    );
    assert_eq!(
        linux::zombie_under(scratch.path(), 12),
        ProcessState::Terminal
    );
}

#[test]
fn the_proc_scan_classifies_an_unreadable_directory_indeterminate() {
    let scratch = Scratch::new("scan-unreadable");
    scratch.process(20, 'Z', 20);
    scratch.unreadable_process(21);

    assert_eq!(
        linux::group_members_terminal_under(&scratch.path().join("absent"), 20, group_is_present),
        ProcessState::Indeterminate,
        "a directory that could not be listed was answered for"
    );
    assert_eq!(
        linux::group_members_terminal_under(scratch.path(), 20, group_is_present),
        ProcessState::Indeterminate,
        "an entry whose group went unread was assumed to be in some other group"
    );
}

#[test]
fn the_proc_scan_still_reports_a_running_member_live() {
    let scratch = Scratch::new("scan-live");
    scratch.process(30, 'Z', 30);
    scratch.process(31, 'R', 30);
    scratch.process(32, 'R', 99);

    assert_eq!(
        linux::group_members_terminal_under(scratch.path(), 30, group_is_present),
        ProcessState::Live,
        "a running member was not reported live"
    );
    assert_eq!(
        linux::group_members_terminal_under(scratch.path(), 99, group_is_present),
        ProcessState::Live
    );
}

#[test]
fn the_proc_scan_reports_a_group_of_dead_members_terminal() {
    let scratch = Scratch::new("scan-terminal");
    scratch.process(50, 'Z', 50);
    scratch.process(51, 'X', 50);
    scratch.process(52, 'R', 60);

    assert_eq!(
        linux::group_members_terminal_under(scratch.path(), 50, group_is_present),
        ProcessState::Terminal
    );
    assert_eq!(
        linux::group_members_terminal_under(scratch.path(), 70, group_is_present),
        ProcessState::Indeterminate,
        "a group with no members observed while it still resolves claimed an end"
    );
}

#[cfg(target_os = "macos")]
mod libproc {
    use std::mem::size_of;

    use super::super::{ProcessState, macos, process_state};

    #[test]
    fn a_width_conversion_that_does_not_fit_reports_indeterminate() {
        assert!(
            macos::probe_arguments(1, size_of::<libc::proc_bsdshortinfo>()).is_some(),
            "the ordinary arguments must convert or the probe never runs at all"
        );
        assert!(
            macos::probe_arguments(u32::MAX, size_of::<libc::proc_bsdshortinfo>()).is_none(),
            "a process identity wider than the kernel type was accepted"
        );
        assert!(
            macos::probe_arguments(1, usize::MAX).is_none(),
            "a destination size wider than the kernel type was accepted"
        );

        // The refusal above reaches the caller as the unreadable answer, which
        // is what an identity too wide to convert must produce end to end.
        assert_eq!(macos::process_zombie(u32::MAX), ProcessState::Indeterminate);
        assert_eq!(
            macos::process_group_members_terminal(u32::MAX),
            ProcessState::Indeterminate
        );
    }

    #[test]
    fn an_unreadable_process_information_call_reports_indeterminate() {
        let size = libc::c_int::try_from(size_of::<libc::proc_bsdshortinfo>()).expect("size fits");

        assert_eq!(
            macos::short_info_state(0, size, Some(libc::ESRCH)),
            Some(ProcessState::Terminal),
            "a refusal naming no such process is a read, not a failure to read"
        );
        assert_eq!(
            macos::short_info_state(0, size, Some(libc::EPERM)),
            Some(ProcessState::Indeterminate),
            "a refusal for another reason was read as an answer about the process"
        );
        assert_eq!(
            macos::short_info_state(size - 1, size, None),
            Some(ProcessState::Indeterminate),
            "a short read left the status word unset and was still answered for"
        );
        assert_eq!(
            macos::short_info_state(size, size, None),
            None,
            "an exact read must defer to the status word"
        );
    }

    #[test]
    fn the_status_word_separates_a_zombie_from_a_running_process() {
        assert_eq!(macos::bsd_status(libc::SZOMB), ProcessState::Terminal);
        assert_eq!(macos::bsd_status(libc::SRUN), ProcessState::Live);
        assert_eq!(macos::bsd_status(libc::SSLEEP), ProcessState::Live);
    }

    #[test]
    fn a_listing_that_could_not_be_sized_reports_indeterminate() {
        assert_eq!(
            macos::listing_capacity(613),
            Ok(614),
            "a sizing answer must leave room for the length check to notice a cut-off listing"
        );
        assert_eq!(
            macos::listing_capacity(-1),
            Err(ProcessState::Indeterminate),
            "a sizing call that refused was counted as a membership"
        );
    }

    #[test]
    fn a_listing_buffer_wider_than_the_kernel_count_reports_indeterminate() {
        let ordinary = size_of::<libc::pid_t>() * 614;
        assert_eq!(
            macos::listing_bytes(ordinary),
            Ok(libc::c_int::try_from(ordinary).expect("ordinary byte count fits")),
            "an ordinary listing buffer could not be passed to libproc"
        );

        let wider_than_c_int = usize::try_from(libc::c_int::MAX).expect("c_int fits usize") + 1;
        assert_eq!(
            macos::listing_bytes(wider_than_c_int),
            Err(ProcessState::Indeterminate),
            "a listing buffer wider than the kernel count was accepted"
        );
    }

    #[test]
    fn a_listing_that_filled_the_buffer_reports_indeterminate() {
        assert_eq!(macos::listing_length(1, 614), Ok(1));
        assert_eq!(
            macos::listing_length(613, 613),
            Err(ProcessState::Indeterminate),
            "a listing that reached capacity may have been cut off"
        );
        assert_eq!(
            macos::listing_length(-1, 614),
            Err(ProcessState::Indeterminate),
            "a listing call that refused was counted as a membership"
        );
    }

    #[test]
    fn an_unreadable_probe_with_an_unresolvable_identity_stays_indeterminate() {
        // Neither the status record nor the identity table can say anything
        // about an identity too wide to convert, so nothing is observed.
        assert_eq!(process_state(u32::MAX), ProcessState::Indeterminate);
    }
}

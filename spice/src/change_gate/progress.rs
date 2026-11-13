//! Shared serialization for concurrently produced change-gate progress.

use std::fmt;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use gate::TestSelectionReport;

use super::ChangeGateError;

pub(super) struct SharedProgress<'a, W: Write + Send + ?Sized> {
    inner: Arc<Mutex<&'a mut W>>,
}

impl<'a, W: Write + Send + ?Sized> SharedProgress<'a, W> {
    pub(super) fn new(progress: &'a mut W) -> Self {
        Self {
            inner: Arc::new(Mutex::new(progress)),
        }
    }

    fn sink(&self) -> io::Result<std::sync::MutexGuard<'_, &'a mut W>> {
        self.inner
            .lock()
            .map_err(|_| io::Error::other("change-gate progress lock was poisoned"))
    }
}

impl<W: Write + Send + ?Sized> Clone for SharedProgress<'_, W> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<W: Write + Send + ?Sized> Write for SharedProgress<'_, W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.sink()?.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.sink()?.flush()
    }

    fn write_fmt(&mut self, arguments: fmt::Arguments<'_>) -> io::Result<()> {
        self.sink()?.write_fmt(arguments)
    }
}

pub(super) fn publish_test_selection(
    progress: &mut impl Write,
    tests: &TestSelectionReport,
) -> Result<(), ChangeGateError> {
    writeln!(
        progress,
        "[rust-test-selection-report] detail=canonical-report source={} changes={} engines={} tests={} edges={} admits={}",
        tests.source(),
        tests.changed_sources().len(),
        tests.engines().len(),
        tests.tests().len(),
        tests.edges().len(),
        tests.admits(),
    )
    .map_err(|error| ChangeGateError::environment("change-gate-progress-unavailable", error))?;
    progress
        .flush()
        .map_err(|error| ChangeGateError::environment("change-gate-progress-unavailable", error))
}

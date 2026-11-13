//! Native console interruption translated into one process-local callback.

use std::io;
use std::thread::JoinHandle;

/// One installed native console-control relay.
pub(crate) struct ControlRelay {
    #[cfg(unix)]
    handle: signal_hook::iterator::Handle,
    #[cfg(windows)]
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl ControlRelay {
    /// Run `interrupted` for each native interactive interruption until drop:
    /// `SIGHUP`, `SIGINT`, or `SIGTERM` on Unix, and Ctrl-C or Ctrl-Break on
    /// Windows.
    pub(crate) fn install(mut interrupted: impl FnMut() + Send + 'static) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};

            let mut signals = signal_hook::iterator::Signals::new([SIGHUP, SIGINT, SIGTERM])?;
            let handle = signals.handle();
            let worker = std::thread::Builder::new()
                .name("spice-control-signals".to_owned())
                .spawn(move || {
                    for _signal in signals.forever() {
                        interrupted();
                    }
                })?;
            Ok(Self {
                handle,
                worker: Some(worker),
            })
        }
        #[cfg(windows)]
        {
            use std::sync::Arc;
            use std::sync::atomic::AtomicBool;

            let mut observed = console::register()?;
            let stop = Arc::new(AtomicBool::new(false));
            let worker_stop = Arc::clone(&stop);
            let worker = std::thread::Builder::new()
                .name("spice-control-signals".to_owned())
                .spawn(move || {
                    while let Some(current) = console::next(observed, &worker_stop) {
                        observed = current;
                        interrupted();
                    }
                })
                .inspect_err(|_| console::unregister())?;
            Ok(Self {
                stop,
                worker: Some(worker),
            })
        }
    }
}

impl Drop for ControlRelay {
    fn drop(&mut self) {
        #[cfg(unix)]
        self.handle.close();
        #[cfg(windows)]
        console::stop(&self.stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        #[cfg(windows)]
        console::unregister();
    }
}

/// The process-wide console control handler shared by every relay.
#[cfg(windows)]
mod console {
    use std::io;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};

    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler,
    };

    struct Control {
        /// Interruptions delivered since the process started.
        events: u64,
        /// Installed relays; the handler is registered while any exist.
        relays: usize,
    }

    static CONTROL: Mutex<Control> = Mutex::new(Control {
        events: 0,
        relays: 0,
    });
    static CHANGED: Condvar = Condvar::new();

    fn control() -> MutexGuard<'static, Control> {
        CONTROL.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Register one relay and return the interruption count it starts from.
    pub(super) fn register() -> io::Result<u64> {
        let mut control = control();
        // SAFETY: `handler` is a `'static` function with the routine's ABI.
        if control.relays == 0 && unsafe { SetConsoleCtrlHandler(Some(handler), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        control.relays += 1;
        Ok(control.events)
    }

    pub(super) fn unregister() {
        let mut control = control();
        control.relays = control.relays.saturating_sub(1);
        if control.relays == 0 {
            // SAFETY: removes the routine `register` installed.
            unsafe { SetConsoleCtrlHandler(Some(handler), 0) };
        }
    }

    /// Block until the interruption count passes `observed`, or `None` once
    /// `stop` is set.
    pub(super) fn next(observed: u64, stop: &AtomicBool) -> Option<u64> {
        let control = CHANGED
            .wait_while(control(), |control| {
                control.events == observed && !stop.load(Ordering::Acquire)
            })
            .unwrap_or_else(PoisonError::into_inner);
        (!stop.load(Ordering::Acquire)).then_some(control.events)
    }

    pub(super) fn stop(stop: &AtomicBool) {
        // Setting the flag under the lock orders it before any waiter's
        // recheck, so the wakeup cannot be lost.
        let _control = control();
        stop.store(true, Ordering::Release);
        CHANGED.notify_all();
    }

    /// The console routine runs on its own system thread, so it may block on
    /// the lock briefly.
    unsafe extern "system" fn handler(event: u32) -> i32 {
        if matches!(event, CTRL_C_EVENT | CTRL_BREAK_EVENT) {
            control().events += 1;
            CHANGED.notify_all();
            1
        } else {
            0
        }
    }
}

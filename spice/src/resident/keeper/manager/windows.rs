//! Task Scheduler over COM, bound to the per-user facility of the caller.
//!
//! The connection is to the local service as the current user, so every
//! task it registers runs as that user with an interactive-token logon and
//! no stored password; nothing here needs elevation.

use std::io;

use windows::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, RPC_E_CHANGED_MODE, VARIANT_BOOL, VARIANT_FALSE, VARIANT_TRUE,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::TaskScheduler::{
    IRegisteredTask, ITaskFolder, ITaskService, TASK_CREATE_OR_UPDATE,
    TASK_LOGON_INTERACTIVE_TOKEN, TASK_STATE_RUNNING, TaskScheduler,
};
use windows::Win32::System::Variant::VARIANT;
use windows::core::{BSTR, HRESULT};

use super::{Manager, Status};

/// Root of every task path the owner registers.
const ROOT: &str = "\\";

/// One COM apartment this thread entered and leaves on drop.
struct Apartment {
    entered: bool,
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.entered {
            // SAFETY: balances the successful `CoInitializeEx` recorded in `entered`.
            unsafe { CoUninitialize() };
        }
    }
}

/// The local Task Scheduler service as the current user.
pub(crate) struct TaskService {
    root: ITaskFolder,
    _service: ITaskService,
    _apartment: Apartment,
}

impl TaskService {
    /// Connect to the local service.
    ///
    /// # Errors
    ///
    /// Returns the COM refusal entering the apartment, creating the service,
    /// or connecting to it.
    pub(crate) fn connect() -> io::Result<Self> {
        // SAFETY: no reserved pointer; the apartment is left in `Apartment::drop`.
        let entered = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if entered.is_err() && entered != RPC_E_CHANGED_MODE {
            return Err(com_error(entered.into()));
        }
        let apartment = Apartment {
            entered: entered.is_ok(),
        };
        // SAFETY: the class identifier names the in-process Task Scheduler
        // service and no outer unknown aggregates it.
        let service: ITaskService =
            unsafe { CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER) }
                .map_err(com_error)?;
        let empty = VARIANT::default();
        // SAFETY: empty variants select the local machine as the current user.
        unsafe { service.Connect(&empty, &empty, &empty, &empty) }.map_err(com_error)?;
        // SAFETY: the connected service answers folder lookups.
        let root = unsafe { service.GetFolder(&BSTR::from(ROOT)) }.map_err(com_error)?;
        Ok(Self {
            root,
            _service: service,
            _apartment: apartment,
        })
    }

    fn task(&self, path: &str) -> io::Result<Option<IRegisteredTask>> {
        // SAFETY: the root folder resolves full task paths.
        match unsafe { self.root.GetTask(&BSTR::from(path)) } {
            Ok(task) => Ok(Some(task)),
            Err(error) if error.code() == not_found() => Ok(None),
            Err(error) => Err(com_error(error)),
        }
    }

    fn require(&self, path: &str) -> io::Result<IRegisteredTask> {
        self.task(path)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("task {path} is not registered"),
            )
        })
    }

    /// Create the folder every segment of `path` but the last names.
    fn ensure_folders(&self, path: &str) -> io::Result<()> {
        let Some((folders, _)) = path.rsplit_once('\\') else {
            return Ok(());
        };
        if folders.is_empty() {
            return Ok(());
        }
        // SAFETY: the root folder resolves full folder paths.
        match unsafe { self.root.GetFolder(&BSTR::from(folders)) } {
            Ok(_) => Ok(()),
            Err(error) if error.code() == not_found() => {
                // SAFETY: an empty security descriptor inherits the parent's.
                unsafe {
                    self.root
                        .CreateFolder(&BSTR::from(folders), &VARIANT::default())
                }
                .map(|_| ())
                .map_err(com_error)
            }
            Err(error) => Err(com_error(error)),
        }
    }
}

impl Manager for TaskService {
    fn register(&mut self, path: &str, definition: &str) -> io::Result<()> {
        self.ensure_folders(path)?;
        let empty = VARIANT::default();
        // SAFETY: the definition's principal names the user; the empty user
        // and password variants keep the interactive-token logon unpassworded.
        unsafe {
            self.root.RegisterTask(
                &BSTR::from(path),
                &BSTR::from(definition),
                TASK_CREATE_OR_UPDATE.0,
                &empty,
                &empty,
                TASK_LOGON_INTERACTIVE_TOKEN,
                &empty,
            )
        }
        .map(|_| ())
        .map_err(com_error)
    }

    fn status(&mut self, path: &str) -> io::Result<Option<Status>> {
        let Some(task) = self.task(path)? else {
            return Ok(None);
        };
        // SAFETY: a registered task answers its own state.
        let (enabled, state) = unsafe { (task.Enabled(), task.State()) };
        Ok(Some(Status {
            enabled: enabled.map_err(com_error)? != VARIANT_FALSE,
            running: state.map_err(com_error)? == TASK_STATE_RUNNING,
        }))
    }

    fn set_enabled(&mut self, path: &str, enabled: bool) -> io::Result<()> {
        let task = self.require(path)?;
        let flag: VARIANT_BOOL = if enabled { VARIANT_TRUE } else { VARIANT_FALSE };
        // SAFETY: a registered task accepts its enabled flag.
        unsafe { task.SetEnabled(flag) }.map_err(com_error)
    }

    fn run(&mut self, path: &str) -> io::Result<()> {
        let task = self.require(path)?;
        // SAFETY: a registered task answers its state and starts on request.
        if unsafe { task.State() }.map_err(com_error)? == TASK_STATE_RUNNING {
            return Ok(());
        }
        // SAFETY: an empty variant passes no parameters to the action.
        unsafe { task.Run(&VARIANT::default()) }
            .map(|_| ())
            .map_err(com_error)
    }

    fn stop(&mut self, path: &str) -> io::Result<()> {
        let Some(task) = self.task(path)? else {
            return Ok(());
        };
        // SAFETY: flags are reserved and zero.
        unsafe { task.Stop(0) }.map_err(com_error)
    }

    fn delete(&mut self, path: &str) -> io::Result<()> {
        // SAFETY: flags are reserved and zero.
        match unsafe { self.root.DeleteTask(&BSTR::from(path), 0) } {
            Ok(()) => Ok(()),
            Err(error) if error.code() == not_found() => Ok(()),
            Err(error) => Err(com_error(error)),
        }
    }
}

fn not_found() -> HRESULT {
    HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0)
}

fn com_error(error: windows::core::Error) -> io::Error {
    io::Error::other(format!("Task Scheduler: {error}"))
}

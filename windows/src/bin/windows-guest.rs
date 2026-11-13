//! Executor inside the persistent Windows developer guest: one argv request on
//! standard input, one bounded result on standard output.

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("spice Windows guest executor requires Windows");
    std::process::ExitCode::from(70)
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    guest::main()
}

#[cfg(windows)]
mod guest {
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::io::{self, Read, Write};
    use std::os::windows::io::AsRawHandle;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command as ProcessCommand, ExitCode, Stdio};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    use windows_execution::guest::{Request, SCRATCH_DIRECTORY, WORK_DIRECTORY};
    use windows_execution::protocol::{Capture, CommandResult, Completion};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::WaitForSingleObject;

    const REFUSAL_EXIT: u8 = 70;
    const MAX_REQUEST_BYTES: u64 = 1024 * 1024;
    /// Inherited variables a Windows process needs to locate its profile and
    /// system directories; every other ambient variable is withheld.
    const INHERITED: [&str; 18] = [
        "SystemRoot",
        "SystemDrive",
        "windir",
        "ComSpec",
        "PATHEXT",
        "OS",
        "USERNAME",
        "USERDOMAIN",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "ProgramData",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "CommonProgramFiles",
        "COMPUTERNAME",
        "NUMBER_OF_PROCESSORS",
        "PROCESSOR_ARCHITECTURE",
    ];
    const SYSTEM_PATH: &str = r"C:\Windows\System32;C:\Windows;C:\Windows\System32\Wbem;C:\Windows\System32\WindowsPowerShell\v1.0";

    pub fn main() -> ExitCode {
        match run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("spice Windows guest executor refused: {error}");
                ExitCode::from(REFUSAL_EXIT)
            }
        }
    }

    fn run() -> Result<(), String> {
        let mut input = Vec::new();
        io::stdin()
            .lock()
            .take(MAX_REQUEST_BYTES)
            .read_to_end(&mut input)
            .map_err(|error| format!("cannot read request: {error}"))?;
        let request: Request = serde_json::from_slice(&input)
            .map_err(|error| format!("request is malformed: {error}"))?;
        request.admit().map_err(|error| error.to_string())?;

        let work = fs::canonicalize(WORK_DIRECTORY).map_err(|error| {
            format!("synced tree {WORK_DIRECTORY} is unavailable: {error}; run windows-guest sync")
        })?;
        let program = resolve_program(&request.command.program, &work)?;
        let scratch = Scratch::create()?;
        let environment = environment(&scratch.0);
        let (completion, stdout, stderr) = execute(
            &program,
            &request.command.arguments,
            &work,
            &environment,
            request.limits.runtime_seconds,
            request.limits.output_bytes as usize,
        )?;
        drop(scratch);
        let result = CommandResult {
            completion,
            stdout,
            stderr,
        };
        let mut output = io::stdout().lock();
        serde_json::to_writer(&mut output, &result)
            .map_err(|error| format!("cannot encode result: {error}"))?;
        output
            .flush()
            .map_err(|error| format!("cannot write result: {error}"))
    }

    fn resolve_program(program: &str, work: &Path) -> Result<PathBuf, String> {
        let relative = Path::new(program);
        if relative.is_absolute() || relative.has_root() {
            return Err("guest program must be a path relative to the synced tree".to_owned());
        }
        let selected = work.join(relative);
        let canonical = fs::canonicalize(&selected).map_err(|error| {
            format!(
                "cannot resolve guest program {}: {error}",
                selected.display()
            )
        })?;
        if !canonical.starts_with(work) {
            return Err(format!(
                "guest program escapes the synced tree: {}",
                canonical.display()
            ));
        }
        if !canonical.is_file() {
            return Err(format!(
                "guest program is not a regular file: {}",
                canonical.display()
            ));
        }
        Ok(canonical)
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the guest executor rim admits a fixed allowlist of its inherited environment"
    )]
    fn environment(scratch: &Path) -> Vec<(OsString, OsString)> {
        let mut environment = std::env::vars_os()
            .filter(|(name, _)| {
                INHERITED
                    .iter()
                    .any(|allowed| OsStr::new(allowed).eq_ignore_ascii_case(name))
            })
            .collect::<Vec<_>>();
        // The canonical work path carries a `\\?\` prefix that PATH search
        // does not honor, so PATH names the fixed directory instead.
        let mut path = OsString::from(WORK_DIRECTORY);
        path.push(r"\bin;");
        path.push(SYSTEM_PATH);
        environment.push(("PATH".into(), path));
        environment.push(("TEMP".into(), scratch.into()));
        environment.push(("TMP".into(), scratch.into()));
        environment
    }

    struct Scratch(PathBuf);

    impl Scratch {
        fn create() -> Result<Self, String> {
            let path = Path::new(SCRATCH_DIRECTORY).join(std::process::id().to_string());
            match fs::remove_dir_all(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!("cannot clear scratch {}: {error}", path.display()));
                }
            }
            fs::create_dir_all(&path)
                .map_err(|error| format!("cannot create scratch {}: {error}", path.display()))?;
            Ok(Self(path))
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _removed = fs::remove_dir_all(&self.0);
        }
    }

    fn execute(
        program: &Path,
        arguments: &[String],
        work: &Path,
        environment: &[(OsString, OsString)],
        runtime_seconds: u32,
        output_limit: usize,
    ) -> Result<(Completion, Capture, Capture), String> {
        let mut command = ProcessCommand::new(program);
        command
            .args(arguments)
            .current_dir(work)
            .env_clear()
            .envs(environment.iter().map(|(name, value)| (name, value)))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|error| format!("guest command did not start: {error}"))?;
        let job = match Job::assign(&child) {
            Ok(job) => job,
            Err(assign) => {
                let teardown = child
                    .kill()
                    .and_then(|()| child.wait().map(|_| ()))
                    .map_err(|error| error.to_string());
                return match teardown {
                    Ok(()) => Err(assign),
                    Err(teardown) => Err(format!(
                        "{assign}; unassigned process teardown also failed: {teardown}"
                    )),
                };
            }
        };
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "stdout capture is unavailable".to_owned())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "stderr capture is unavailable".to_owned())?;
        let budget = Arc::new(AtomicUsize::new(output_limit));
        let stdout = capture(stdout, Arc::clone(&budget));
        let stderr = capture(stderr, budget);
        let completion = job.wait(&mut child, runtime_seconds)?;
        drop(job);
        let stdout = stdout
            .join()
            .map_err(|_| "stdout capture panicked".to_owned())??;
        let stderr = stderr
            .join()
            .map_err(|_| "stderr capture panicked".to_owned())??;
        Ok((completion, stdout, stderr))
    }

    fn capture(
        mut stream: impl Read + Send + 'static,
        budget: Arc<AtomicUsize>,
    ) -> thread::JoinHandle<Result<Capture, String>> {
        thread::spawn(move || {
            let mut retained = Vec::new();
            let mut buffer = [0_u8; 8192];
            let mut truncated = false;
            loop {
                let read = stream
                    .read(&mut buffer)
                    .map_err(|error| format!("output capture failed: {error}"))?;
                if read == 0 {
                    break;
                }
                let mut available = budget.load(Ordering::Acquire);
                loop {
                    let keep = available.min(read);
                    match budget.compare_exchange_weak(
                        available,
                        available - keep,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => {
                            retained.extend_from_slice(&buffer[..keep]);
                            truncated |= keep < read;
                            break;
                        }
                        Err(current) => available = current,
                    }
                }
            }
            Ok(Capture::from_bytes(&retained, truncated))
        })
    }

    /// Kill-on-close Job Object owning the guest command's whole process tree.
    struct Job(HANDLE);

    impl Job {
        fn assign(child: &Child) -> Result<Self, String> {
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    return Err(format!(
                        "cannot create Windows Job Object: {}",
                        io::Error::last_os_error()
                    ));
                }
                let mut information = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                information.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let configured = SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    std::ptr::from_ref(&information).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if configured == 0 {
                    let error = io::Error::last_os_error();
                    CloseHandle(handle);
                    return Err(format!("cannot bound Windows Job Object: {error}"));
                }
                if AssignProcessToJobObject(handle, child.as_raw_handle() as HANDLE) == 0 {
                    let error = io::Error::last_os_error();
                    CloseHandle(handle);
                    return Err(format!(
                        "cannot assign process tree to Windows Job Object: {error}"
                    ));
                }
                Ok(Self(handle))
            }
        }

        fn wait(&self, child: &mut Child, seconds: u32) -> Result<Completion, String> {
            let milliseconds = seconds.saturating_mul(1_000);
            let waited =
                unsafe { WaitForSingleObject(child.as_raw_handle() as HANDLE, milliseconds) };
            match waited {
                WAIT_OBJECT_0 => {
                    let status = child
                        .wait()
                        .map_err(|error| format!("cannot reap guest command: {error}"))?;
                    Ok(Completion::Exited {
                        code: status.code().unwrap_or(-1),
                    })
                }
                WAIT_TIMEOUT => {
                    if unsafe { TerminateJobObject(self.0, u32::from(REFUSAL_EXIT)) } == 0 {
                        return Err(format!(
                            "cannot terminate timed-out process tree: {}",
                            io::Error::last_os_error()
                        ));
                    }
                    let _status = child
                        .wait()
                        .map_err(|error| format!("cannot reap timed-out command: {error}"))?;
                    Ok(Completion::TimedOut)
                }
                other => Err(format!(
                    "Windows process wait returned {other}: {}",
                    io::Error::last_os_error()
                )),
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

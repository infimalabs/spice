//! Repository-owned Windows guest for one provider-authenticated execution.

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("spice hosted Windows runner requires x86-64 Windows");
    std::process::ExitCode::from(70)
}

#[cfg(windows)]
mod windows {
    use std::env;
    use std::fs::{self, File};
    use std::io::{self, Read, Write};
    use std::os::windows::io::AsRawHandle;
    use std::path::{Path, PathBuf};
    use std::process::{Command as ProcessCommand, ExitCode, Stdio};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    use sha2::{Digest as _, Sha256};
    use sill::authority;
    use windows_execution::hosted::{
        Artifact, GITHUB_PROVIDER, Identity, Isolation, Request, ResultDocument, WORKFLOW,
        X86_64_ARCHITECTURE,
    };
    use windows_execution::protocol::{
        Capture, Command as HostedCommand, CommandResult, Completion,
    };
    use windows_sys::Wdk::System::SystemServices::RtlGetVersion;
    use windows_sys::Win32::Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Registry::{
        HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RegGetValueW,
    };
    use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;
    use windows_sys::Win32::System::Threading::WaitForSingleObject;
    use wire::{Digest, Domain};

    const REFUSAL_EXIT: u8 = 70;
    const NETSH: &str = r"C:\Windows\System32\netsh.exe";
    const NON_LOOPBACK_REMOTE: &str = "remoteip=0.0.0.0-126.255.255.255,128.0.0.0-255.255.255.255,::-::,::2-ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff";

    pub fn main() -> ExitCode {
        match run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("hosted Windows runner refused: {error}");
                ExitCode::from(REFUSAL_EXIT)
            }
        }
    }

    fn run() -> Result<(), String> {
        let arguments = env::args_os().collect::<Vec<_>>();
        if arguments.get(1).and_then(|value| value.to_str()) == Some("--smoke-argv") {
            return smoke_argv(&arguments[2..]);
        }
        let [_, request_path, result_path] = arguments.as_slice() else {
            return Err("usage: windows-hosted REQUEST.json RESULT.json".to_owned());
        };
        let request_bytes = fs::read(request_path).map_err(|error| {
            format!(
                "cannot read request {}: {error}",
                Path::new(request_path).display()
            )
        })?;
        let request: Request = serde_json::from_slice(&request_bytes)
            .map_err(|error| format!("request does not decode: {error}"))?;
        request.admit().map_err(|error| error.to_string())?;
        admit_host(&request)?;

        let workspace = required_path("GITHUB_WORKSPACE")?;
        let temp = required_path("RUNNER_TEMP")?;
        let vendor = required_path("SPICE_HOSTED_VENDOR")?; // env-policy: allow
        let run_id = required_u64("GITHUB_RUN_ID")?;
        let run_attempt = required_u32("GITHUB_RUN_ATTEMPT")?;
        let executable =
            env::current_exe().map_err(|error| format!("runner path is unavailable: {error}"))?;
        let runner = digest_file(Domain::ProofNativeRunner, &executable)?;
        let tree = tree_digest(&request.toolchain.git_path, &workspace)?;
        if tree != request.tree {
            return Err(format!(
                "source tree changed: expected {}, observed {}",
                request.tree, tree
            ));
        }
        admit_toolchain(&request)?;

        let scratch = temp.join(format!("spice-hosted-{}", request.challenge));
        let cargo_home = scratch.join("cargo-home");
        let target = scratch.join("target");
        let stage = temp.join(format!("spice-hosted-stage-{}", request.challenge));
        create_empty(&scratch)?;
        fs::create_dir(&cargo_home)
            .map_err(|error| format!("cannot create fresh Cargo home: {error}"))?;
        fs::create_dir(&target).map_err(|error| format!("cannot create fresh target: {error}"))?;
        create_empty(&stage)?;
        configure_cargo_home(&cargo_home, &vendor)?;
        make_read_only("source", &workspace, &request.challenge)?;
        fs::read(workspace.join("Cargo.toml"))
            .map_err(|error| format!("sealed source manifest is unreadable: {error}"))?;
        make_read_only("vendored dependencies", &vendor, &request.challenge)?;

        let firewall = Firewall::establish(&request.challenge)?;
        let executions = request
            .commands
            .iter()
            .map(|command| {
                let program = resolve_program(&request, command, &workspace, &executable)?;
                execute(
                    &program,
                    &command.arguments,
                    &workspace,
                    &cargo_home,
                    &target,
                    &stage,
                    &request,
                )
                .map(|(completion, stdout, stderr)| CommandResult {
                    completion,
                    stdout,
                    stderr,
                })
            })
            .collect::<Result<Vec<_>, String>>();
        let cleanup = firewall.close();
        let outputs = match (executions, cleanup) {
            (Ok(executions), Ok(())) => executions,
            (Err(error), Ok(())) | (Ok(_), Err(error)) => return Err(error),
            (Err(execution), Err(cleanup)) => {
                return Err(format!(
                    "{execution}; guest firewall teardown also failed: {cleanup}"
                ));
            }
        };
        let artifacts = artifact_inventory(&stage)?;
        let result = ResultDocument {
            version: windows_execution::hosted::PROTOCOL_VERSION,
            challenge: request.challenge.clone(),
            identity: Identity {
                provider: GITHUB_PROVIDER.to_owned(),
                repository: request.repository.clone(),
                workflow: WORKFLOW.to_owned(),
                run_id,
                run_attempt,
                image: request.image.clone(),
                runner,
                source: request.source.clone(),
                toolchain: request.toolchain.clone(),
            },
            outputs,
            artifacts,
            isolation: Isolation {
                source_read_only: true,
                fresh_cargo_home: true,
                fresh_target: true,
                loopback_only: true,
                process_tree_bounded: true,
            },
        };
        let encoded = serde_json::to_vec(&result)
            .map_err(|error| format!("result does not encode: {error}"))?;
        let mut file = File::create(result_path).map_err(|error| {
            format!(
                "cannot create result {}: {error}",
                Path::new(result_path).display()
            )
        })?;
        file.write_all(&encoded)
            .map_err(|error| format!("cannot write result: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("cannot sync result: {error}"))?;
        fs::remove_dir_all(&scratch)
            .map_err(|error| format!("cannot tear down scratch: {error}"))?;
        Ok(())
    }

    fn smoke_argv(arguments: &[std::ffi::OsString]) -> Result<(), String> {
        let mut exit = 0;
        for argument in arguments {
            let argument = argument
                .to_str()
                .ok_or_else(|| "smoke argv is not Unicode".to_owned())?;
            if let Some(code) = argument.strip_prefix("--exit=") {
                exit = code
                    .parse::<i32>()
                    .map_err(|_| "invalid smoke exit".to_owned())?;
            } else {
                println!("{argument}");
            }
        }
        std::process::exit(exit);
    }

    fn admit_host(request: &Request) -> Result<(), String> {
        if env::consts::ARCH != "x86_64" || env::consts::OS != "windows" {
            return Err(format!(
                "unsupported guest {}-{}",
                env::consts::ARCH,
                env::consts::OS
            ));
        }
        exact_env("GITHUB_REPOSITORY", &request.repository)?;
        exact_env("GITHUB_REF", &format!("refs/tags/{}", request.source_ref))?;
        exact_env("ImageOS", &request.image.provider_os)?;
        exact_env("ImageVersion", &request.image.version)?;
        exact_env("RUNNER_ARCH", X86_64_ARCHITECTURE)?;
        let observed_os = windows_version()?;
        if observed_os != request.image.os_version {
            return Err(format!(
                "Windows build drifted: expected {}, observed {observed_os}",
                request.image.os_version
            ));
        }
        Ok(())
    }

    fn windows_version() -> Result<String, String> {
        let mut version = OSVERSIONINFOW {
            dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
            ..OSVERSIONINFOW::default()
        };
        let version_status = unsafe { RtlGetVersion(&raw mut version) };
        if version_status != 0 {
            return Err(format!(
                "cannot inspect Windows version: NTSTATUS {version_status}"
            ));
        }
        let mut revision = 0_u32;
        let mut revision_bytes = std::mem::size_of::<u32>() as u32;
        let revision_status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                windows_sys::w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion"),
                windows_sys::w!("UBR"),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                std::ptr::from_mut(&mut revision).cast(),
                &raw mut revision_bytes,
            )
        };
        if revision_status != 0 || revision_bytes != std::mem::size_of::<u32>() as u32 {
            return Err(format!(
                "cannot inspect Windows update revision: WIN32_ERROR {revision_status}"
            ));
        }
        Ok(format!(
            "{}.{}.{}.{}",
            version.dwMajorVersion, version.dwMinorVersion, version.dwBuildNumber, revision
        ))
    }

    fn admit_toolchain(request: &Request) -> Result<(), String> {
        exact_version(
            &request.toolchain.rustc_path,
            &["--version"],
            &request.toolchain.rust,
        )?;
        exact_version(
            &request.toolchain.cargo_path,
            &["--version"],
            &request.toolchain.cargo,
        )?;
        exact_version(
            &request.toolchain.git_path,
            &["--version"],
            &request.toolchain.git,
        )?;
        exact_version(
            &request.toolchain.trunk_path,
            &["--version"],
            &request.toolchain.trunk,
        )?;
        path_carries(&request.toolchain.cl_path, &request.toolchain.msvc, "MSVC")?;
        path_carries(
            &request.toolchain.link_path,
            &request.toolchain.msvc,
            "MSVC linker",
        )?;
        path_carries(
            &request.toolchain.rc_path,
            &request.toolchain.sdk,
            "Windows SDK",
        )?;
        path_list(&request.toolchain.path, "PATH")?;
        path_list(&request.toolchain.include, "INCLUDE")?;
        path_list(&request.toolchain.lib, "LIB")?;
        Ok(())
    }

    fn exact_version(program: &str, arguments: &[&str], expected: &str) -> Result<(), String> {
        let output = ProcessCommand::new(program)
            .args(arguments)
            .env_clear()
            .output()
            .map_err(|error| format!("cannot inspect pinned tool {program}: {error}"))?;
        if !output.status.success() {
            return Err(format!("pinned tool {program} refused version inspection"));
        }
        let observed = String::from_utf8(output.stdout)
            .map_err(|_| format!("pinned tool {program} returned a non-UTF-8 version"))?;
        if observed.trim() != expected {
            return Err(format!(
                "pinned tool {program} drifted: expected {expected:?}, observed {:?}",
                observed.trim()
            ));
        }
        Ok(())
    }

    fn path_carries(path: &str, pin: &str, name: &str) -> Result<(), String> {
        let path = Path::new(path);
        if !path.is_absolute() || !path.is_file() || !path.to_string_lossy().contains(pin) {
            return Err(format!(
                "pinned {name} path does not carry {pin:?}: {}",
                path.display()
            ));
        }
        Ok(())
    }

    fn path_list(value: &str, name: &str) -> Result<(), String> {
        for entry in value.split(';') {
            let path = Path::new(entry);
            if entry.is_empty() || !path.is_absolute() || !path.is_dir() {
                return Err(format!(
                    "pinned {name} entry is not an absolute directory: {}",
                    path.display()
                ));
            }
        }
        Ok(())
    }

    fn tree_digest(git: &str, workspace: &Path) -> Result<String, String> {
        let mut child = ProcessCommand::new(git)
            .current_dir(workspace)
            .args(["ls-tree", "-r", "-z", "--full-tree", "HEAD"])
            .env_clear()
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("cannot inspect selected source tree: {error}"))?;
        let mut listing = child
            .stdout
            .take()
            .ok_or_else(|| "selected source tree has no stdout".to_owned())?;
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = listing
                .read(&mut buffer)
                .map_err(|error| format!("cannot read selected source tree: {error}"))?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
        }
        let output = child
            .wait_with_output()
            .map_err(|error| format!("cannot finish selected source tree: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "selected source tree inspection failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(format!("{:x}", digest.finalize()))
    }

    fn digest_file(domain: Domain, path: &Path) -> Result<Digest, String> {
        let file =
            File::open(path).map_err(|error| format!("cannot open {}: {error}", path.display()))?;
        Digest::compute_reader(domain, file, 512 * 1024 * 1024)
            .map(|(_, digest)| digest)
            .map_err(|error| format!("cannot digest {}: {error}", path.display()))
    }

    fn artifact_inventory(root: &Path) -> Result<Vec<Artifact>, String> {
        fn walk(root: &Path, directory: &Path, files: &mut Vec<Artifact>) -> Result<(), String> {
            let mut entries = fs::read_dir(directory)
                .map_err(|error| {
                    format!("cannot read staged output {}: {error}", directory.display())
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("cannot enumerate staged output: {error}"))?;
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let path = entry.path();
                if path.is_dir() {
                    walk(root, &path, files)?;
                    continue;
                }
                if !path.is_file() {
                    return Err(format!(
                        "staged output is not a regular file: {}",
                        path.display()
                    ));
                }
                let relative = path
                    .strip_prefix(root)
                    .expect("walked output stays beneath its root")
                    .components()
                    .map(|component| {
                        component
                            .as_os_str()
                            .to_str()
                            .ok_or_else(|| "staged output path is not Unicode".to_owned())
                    })
                    .collect::<Result<Vec<_>, _>>()?
                    .join("/");
                let bytes = fs::metadata(&path)
                    .map_err(|error| {
                        format!("cannot inspect staged output {}: {error}", path.display())
                    })?
                    .len();
                files.push(Artifact {
                    path: relative,
                    bytes,
                    digest: digest_file(Domain::DistArtifact, &path)?,
                });
            }
            Ok(())
        }

        if !root.exists() {
            return Ok(Vec::new());
        }
        let mut files = Vec::new();
        walk(root, root, &mut files)?;
        Ok(files)
    }

    fn create_empty(path: &Path) -> Result<(), String> {
        match fs::remove_dir_all(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("cannot clear scratch {}: {error}", path.display())),
        }
        fs::create_dir(path)
            .map_err(|error| format!("cannot create scratch {}: {error}", path.display()))
    }

    fn configure_cargo_home(cargo_home: &Path, vendor: &Path) -> Result<(), String> {
        if !vendor.is_dir() {
            return Err(format!(
                "vendored dependency closure is not a directory: {}",
                vendor.display()
            ));
        }
        let vendor = vendor.to_string_lossy().replace('\\', "/");
        if vendor.contains(['"', '\r', '\n']) {
            return Err("vendored dependency path cannot be written to Cargo config".to_owned());
        }
        let config = format!(
            "[source.crates-io]\nreplace-with = \"vendored-sources\"\n\
             [source.vendored-sources]\ndirectory = \"{vendor}\"\n"
        );
        fs::write(cargo_home.join("config.toml"), config)
            .map_err(|error| format!("cannot configure fresh Cargo home: {error}"))
    }

    fn make_read_only(kind: &str, path: &Path, challenge: &str) -> Result<(), String> {
        seal_tree(path).map_err(|error| format!("cannot seal {kind}: {error}"))?;
        let probe = path.join(format!(".spice-read-only-{challenge}"));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)
        {
            Ok(file) => {
                drop(file);
                fs::remove_file(&probe)
                    .map_err(|error| format!("cannot remove writable {kind} probe: {error}"))?;
                Err(format!("{kind} remained writable after ACL sealing"))
            }
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => Ok(()),
            Err(error) => Err(format!("cannot verify {kind} write refusal: {error}")),
        }
    }

    fn seal_tree(path: &Path) -> io::Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if authority::is_link_like(&metadata) {
            return Ok(());
        }
        let mode = if metadata.is_dir() {
            for entry in fs::read_dir(path)? {
                seal_tree(&entry?.path())?;
            }
            0o500
        } else if metadata.is_file() {
            0o500
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} is not a regular file or directory", path.display()),
            ));
        };
        authority::set_owner_mode(path, mode)
    }

    fn resolve_program(
        request: &Request,
        command: &HostedCommand,
        workspace: &Path,
        runner: &Path,
    ) -> Result<PathBuf, String> {
        let selected = match command.program.as_str() {
            "@runner" => runner.to_path_buf(),
            "@cargo" => PathBuf::from(&request.toolchain.cargo_path),
            "@rustc" => PathBuf::from(&request.toolchain.rustc_path),
            "@git" => PathBuf::from(&request.toolchain.git_path),
            "@cl" => PathBuf::from(&request.toolchain.cl_path),
            "@rc" => PathBuf::from(&request.toolchain.rc_path),
            relative => {
                let path = Path::new(relative);
                if path.is_absolute() {
                    return Err(
                        "hosted program must be a source-relative path or admitted tool token"
                            .to_owned(),
                    );
                }
                let selected = workspace.join(path);
                let canonical = fs::canonicalize(&selected).map_err(|error| {
                    format!(
                        "cannot resolve hosted program {}: {error}",
                        selected.display()
                    )
                })?;
                if !canonical.starts_with(workspace) {
                    return Err(format!(
                        "hosted program escapes selected source: {}",
                        canonical.display()
                    ));
                }
                canonical
            }
        };
        if !selected.is_file() {
            return Err(format!(
                "hosted program is not a regular file: {}",
                selected.display()
            ));
        }
        Ok(selected)
    }

    fn execute(
        program: &Path,
        arguments: &[String],
        workspace: &Path,
        cargo_home: &Path,
        target: &Path,
        stage: &Path,
        request: &Request,
    ) -> Result<(Completion, Capture, Capture), String> {
        let toolchain = &request.toolchain;
        let runtime_seconds = request.limits.runtime_seconds;
        let output_limit = request.limits.output_bytes as usize;
        let mut command = ProcessCommand::new(program);
        command
            .args(arguments)
            .current_dir(workspace)
            .env_clear()
            .env("SystemRoot", r"C:\Windows")
            .env("CARGO_HOME", cargo_home)
            .env("CARGO_TARGET_DIR", target)
            .env("CARGO_NET_OFFLINE", "true")
            .env("RUST_TEST_THREADS", "1")
            .env("TEMP", target)
            .env("TMP", target)
            .env("RUSTC", &toolchain.rustc_path)
            .env("CC", &toolchain.cl_path)
            .env("CXX", &toolchain.cl_path)
            .env("SPICE_HOSTED_STAGE", stage) // env-policy: allow
            .env("SPICE_SOURCE_ARCHIVE", &request.source) // env-policy: allow
            .env(
                "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER",
                &toolchain.link_path,
            )
            .env("PATH", &toolchain.path)
            .env("INCLUDE", &toolchain.include)
            .env("LIB", &toolchain.lib)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child =
            sill::spawn_owned_command(&mut command, sill::ProcessGroup::New).map_err(|error| {
                format!("hosted command did not start inside its process-tree boundary: {error}")
            })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "stdout capture is unavailable".to_owned())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "stderr capture is unavailable".to_owned())?;
        let budget = Arc::new(AtomicUsize::new(output_limit));
        let stdout = capture(stdout, Arc::clone(&budget), io::stdout());
        let stderr = capture(stderr, budget, io::stderr());
        let completion = wait_for_owned(&mut child, runtime_seconds)?;
        drop(child);
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
        mut live: impl Write + Send + 'static,
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
                            live.write_all(&buffer[..keep])
                                .and_then(|()| live.flush())
                                .map_err(|error| format!("live output failed: {error}"))?;
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

    fn wait_for_owned(child: &mut sill::OwnedChild, seconds: u32) -> Result<Completion, String> {
        let milliseconds = seconds.saturating_mul(1_000);
        let waited = unsafe { WaitForSingleObject(child.as_raw_handle() as HANDLE, milliseconds) };
        match waited {
            WAIT_OBJECT_0 => {
                let status = child
                    .wait()
                    .map_err(|error| format!("cannot reap hosted command: {error}"))?;
                Ok(Completion::Exited {
                    code: status.code().unwrap_or(-1),
                })
            }
            WAIT_TIMEOUT => {
                child
                    .terminate_tree()
                    .map_err(|error| format!("cannot terminate timed-out process tree: {error}"))?;
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

    struct Firewall {
        name: String,
        active: bool,
    }

    impl Firewall {
        fn establish(challenge: &str) -> Result<Self, String> {
            let name = format!("spice-hosted-{}", &challenge[..16]);
            let name_argument = format!("name={name}");
            // A machine-wide rule covers every descendant executable. Its
            // explicit IPv4 and IPv6 ranges exclude only the two loopback
            // spaces instead of relying on process-specific rules children
            // could evade.
            let status = ProcessCommand::new(NETSH)
                .args(["advfirewall", "firewall", "add", "rule"])
                .arg(&name_argument)
                .args([
                    "dir=out",
                    "action=block",
                    "enable=yes",
                    "profile=any",
                    NON_LOOPBACK_REMOTE,
                ])
                .status()
                .map_err(|error| format!("cannot establish guest firewall: {error}"))?;
            if !status.success() {
                return Err(format!("guest firewall refused with {status}"));
            }
            Ok(Self { name, active: true })
        }

        fn close(mut self) -> Result<(), String> {
            let result = delete_firewall(&self.name);
            if result.is_ok() {
                self.active = false;
            }
            result
        }
    }

    impl Drop for Firewall {
        fn drop(&mut self) {
            if self.active {
                let _deleted = delete_firewall(&self.name);
            }
        }
    }

    fn delete_firewall(name: &str) -> Result<(), String> {
        let name = format!("name={name}");
        let status = ProcessCommand::new(NETSH)
            .args(["advfirewall", "firewall", "delete", "rule"])
            .arg(name)
            .status()
            .map_err(|error| format!("cannot remove guest firewall: {error}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("guest firewall teardown refused with {status}"))
        }
    }

    fn required_path(name: &'static str) -> Result<PathBuf, String> {
        required_env(name).map(PathBuf::from)
    }

    fn required_env(name: &'static str) -> Result<String, String> {
        env::var(name).map_err(|_| format!("provider did not supply {name}"))
    }

    fn exact_env(name: &'static str, expected: &str) -> Result<(), String> {
        let observed = required_env(name)?;
        if observed == expected {
            Ok(())
        } else {
            Err(format!(
                "provider {name} drifted: expected {expected:?}, observed {observed:?}"
            ))
        }
    }

    fn required_u64(name: &'static str) -> Result<u64, String> {
        required_env(name)?
            .parse()
            .map_err(|_| format!("provider {name} is not an integer"))
    }

    fn required_u32(name: &'static str) -> Result<u32, String> {
        required_env(name)?
            .parse()
            .map_err(|_| format!("provider {name} is not an integer"))
    }
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    windows::main()
}

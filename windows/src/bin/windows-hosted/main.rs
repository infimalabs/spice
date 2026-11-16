//! Repository-owned Windows guest for one provider-authenticated execution.

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("spice hosted Windows runner requires x86-64 Windows");
    std::process::ExitCode::from(70)
}
#[cfg(windows)]
mod canvas;

#[cfg(windows)]
mod windows {
    use base64::Engine as _;
    use sha2::{Digest as _, Sha256};
    use sill::authority;
    use std::env;
    use std::fs::{self, File};
    use std::io::{self, Read, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::os::windows::io::AsRawHandle;
    use std::path::{Path, PathBuf};
    use std::process::{Command as ProcessCommand, ExitCode, Stdio};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use std::time::Duration;
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
    const POWERSHELL: &str = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
    const GUEST_ARGUMENT: &str = "--guest";
    const NON_LOOPBACK_REMOTE: [&str; 4] = [
        "0.0.0.0-126.255.255.255",
        "128.0.0.0-255.255.255.255",
        "::-::",
        "::2-ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
    ];
    const NETWORK_PROBE: &str = "1.1.1.1:443";

    #[derive(serde::Deserialize, serde::Serialize)]
    #[serde(deny_unknown_fields)]
    struct GuestEnvironment {
        workspace: PathBuf,
        temp: PathBuf,
        vendor: PathBuf,
        system_root: PathBuf,
        provider_identity: String,
        run_id: u64,
        run_attempt: u32,
    }

    impl GuestEnvironment {
        fn capture(request: &Request) -> Result<Self, String> {
            exact_env("GITHUB_REPOSITORY", &request.repository)?;
            exact_env("GITHUB_REF", &request.workflow_ref)?;
            exact_env("ImageOS", &request.image.provider_os)?;
            exact_env("ImageVersion", &request.image.version)?;
            exact_env("RUNNER_ARCH", X86_64_ARCHITECTURE)?;
            let environment = Self {
                workspace: required_path("GITHUB_WORKSPACE")?,
                temp: required_path("RUNNER_TEMP")?,
                vendor: required_path("SPICE_HOSTED_VENDOR")?, // env-policy: allow
                system_root: required_path("SystemRoot")?,
                provider_identity: authority::process_owners()
                    .map_err(|error| format!("cannot inspect provider process identity: {error}"))?
                    .user,
                run_id: required_u64("GITHUB_RUN_ID")?,
                run_attempt: required_u32("GITHUB_RUN_ATTEMPT")?,
            };
            environment.admit()?;
            Ok(environment)
        }

        fn decode(encoded: &std::ffi::OsStr) -> Result<Self, String> {
            let encoded = encoded
                .to_str()
                .ok_or_else(|| "guest environment is not Unicode".to_owned())?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| "guest environment is not base64".to_owned())?;
            let environment = serde_json::from_slice::<Self>(&bytes)
                .map_err(|error| format!("guest environment does not decode: {error}"))?;
            environment.admit()?;
            Ok(environment)
        }

        fn encode(&self) -> Result<String, String> {
            let bytes = serde_json::to_vec(self)
                .map_err(|error| format!("cannot encode guest environment: {error}"))?;
            Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
        }

        fn admit(&self) -> Result<(), String> {
            for (name, path) in [
                ("workspace", &self.workspace),
                ("temporary directory", &self.temp),
                ("vendored dependencies", &self.vendor),
                ("system root", &self.system_root),
            ] {
                if !path.is_absolute() {
                    return Err(format!("guest {name} path is not absolute"));
                }
            }
            let mut components = self.provider_identity.split('-');
            if components.next() != Some("S")
                || components.next() != Some("1")
                || components.next() != Some("5")
                || components.clone().next().is_none()
                || !components.all(|component| component.parse::<u32>().is_ok())
            {
                return Err("guest provider identity is not a canonical user SID".to_owned());
            }
            if !self.system_root.is_dir() {
                return Err("guest system root is not a directory".to_owned());
            }
            if self.run_id == 0 || self.run_attempt == 0 {
                return Err("guest run coordinates are zero".to_owned());
            }
            Ok(())
        }
    }

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
        if arguments.get(1).and_then(|value| value.to_str()) == Some(GUEST_ARGUMENT) {
            let [
                _,
                _,
                request_path,
                result_path,
                guest_identity,
                guest_environment,
            ] = arguments.as_slice()
            else {
                return Err(
                    "usage: windows-hosted --guest REQUEST.json RESULT.json SID ENVIRONMENT"
                        .to_owned(),
                );
            };
            return run_guest(request_path, result_path, guest_identity, guest_environment);
        }
        let [_, request_path, result_path, source_archive] = arguments.as_slice() else {
            return Err("usage: windows-hosted REQUEST.json RESULT.json SOURCE.tar".to_owned());
        };
        run_parent(request_path, result_path, source_archive)
    }

    fn run_parent(
        request_path: &std::ffi::OsStr,
        result_path: &std::ffi::OsStr,
        source_archive: &std::ffi::OsStr,
    ) -> Result<(), String> {
        let request = read_request(request_path)?;
        request.admit().map_err(|error| error.to_string())?;
        admit_machine(&request)?;
        let guest_environment = GuestEnvironment::capture(&request)?;
        let source_archive = Path::new(source_archive);
        admit_source(&request, &guest_environment.workspace, source_archive)?;
        let encoded_guest_environment = guest_environment.encode()?;

        let workspace = guest_environment.workspace.clone();
        let temp = guest_environment.temp.clone();
        let vendor = guest_environment.vendor.clone();
        let executable =
            env::current_exe().map_err(|error| format!("runner path is unavailable: {error}"))?;
        let tree = tree_digest(&request.toolchain.git_path, &workspace)?;
        if tree != request.tree {
            return Err(format!(
                "source tree changed: expected {}, observed {}",
                request.tree, tree
            ));
        }
        admit_toolchain(&request)?;

        let scratch = scratch_root(&temp, &request.challenge);
        let cargo_home = scratch.join("cargo-home");
        let target = scratch.join("target");
        let guest_result = scratch.join("result.json");
        let stage = temp.join(format!("windows-stage-{}", request.challenge));
        create_empty(&scratch)?;
        create_empty(&stage)?;

        eprintln!("[windows-hosted] creating isolated account");
        let account = GuestAccount::create(&request.challenge)?;
        eprintln!("[windows-hosted] sealing source and vendor");
        grant_ancestor_traversal(&workspace, account.sid())?;
        grant_ancestor_traversal(&vendor, account.sid())?;
        grant_ancestor_traversal(&scratch, account.sid())?;
        grant_ancestor_traversal(&stage, account.sid())?;
        authority::grant_identity_mode(&scratch, account.sid(), 0o700)
            .map_err(|error| format!("cannot grant guest scratch access: {error}"))?;
        authority::grant_identity_mode(&stage, account.sid(), 0o700)
            .map_err(|error| format!("cannot grant guest stage access: {error}"))?;
        fs::create_dir(&cargo_home)
            .map_err(|error| format!("cannot create fresh Cargo home: {error}"))?;
        fs::create_dir(&target).map_err(|error| format!("cannot create fresh target: {error}"))?;
        configure_cargo_home(&cargo_home, &vendor)?;

        make_read_only("source", &workspace, &request.challenge, account.sid())?;
        fs::read(workspace.join("Cargo.toml"))
            .map_err(|error| format!("sealed source manifest is unreadable: {error}"))?;
        make_read_only(
            "vendored dependencies",
            &vendor,
            &request.challenge,
            account.sid(),
        )?;
        if let Some(canvas) = super::canvas::admitted_root(&temp, &request)? {
            grant_ancestor_traversal(&canvas, account.sid())?;
            make_read_only(
                "provider canvas",
                &canvas,
                &request.challenge,
                account.sid(),
            )?;
        }
        eprintln!("[windows-hosted] granting isolated tool access");
        grant_file_access(Path::new(request_path), account.sid(), 0o400)?;
        grant_file_access(&executable, account.sid(), 0o500)?;
        let toolchain_root = Path::new(&request.toolchain.cargo_path)
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| "pinned Cargo path has no toolchain root".to_owned())?;
        grant_ancestor_traversal(toolchain_root, account.sid())?;
        grant_tree_access(toolchain_root, account.sid(), 0o500, 0o500)?;
        let trunk_root = Path::new(&request.toolchain.trunk_path)
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| "pinned Trunk path has no installation root".to_owned())?;
        grant_ancestor_traversal(trunk_root, account.sid())?;
        grant_tree_access(trunk_root, account.sid(), 0o500, 0o500)?;

        eprintln!("[windows-hosted] establishing principal firewall");
        verify_non_loopback_available()?;
        let firewall = Firewall::establish(&request.challenge, account.sid())?;
        eprintln!("[windows-hosted] starting isolated guest");
        let execution = account.execute(
            &executable,
            request_path,
            &guest_result,
            &encoded_guest_environment,
        );
        let firewall_cleanup = firewall.close();
        let account_cleanup = account.close();
        combine_cleanup(execution, firewall_cleanup, account_cleanup)?;
        eprintln!("[windows-hosted] isolated guest completed");

        let encoded = fs::read(&guest_result)
            .map_err(|error| format!("cannot read guest result: {error}"))?;
        let result = serde_json::from_slice::<ResultDocument>(&encoded)
            .map_err(|error| format!("guest result does not decode: {error}"))?;
        let provider_artifacts = artifact_inventory(&stage)?;
        if provider_artifacts != result.artifacts {
            return Err("provider-visible staged artifacts differ from guest inventory".to_owned());
        }
        write_result(result_path, &encoded)?;
        fs::remove_dir_all(&scratch)
            .map_err(|error| format!("cannot tear down scratch: {error}"))?;
        Ok(())
    }

    fn run_guest(
        request_path: &std::ffi::OsStr,
        result_path: &std::ffi::OsStr,
        expected_identity: &std::ffi::OsStr,
        encoded_environment: &std::ffi::OsStr,
    ) -> Result<(), String> {
        let expected_identity = expected_identity
            .to_str()
            .ok_or_else(|| "guest identity is not Unicode".to_owned())?;
        let observed_identity = authority::process_owners()
            .map_err(|error| format!("cannot inspect guest process identity: {error}"))?
            .user;
        if observed_identity != expected_identity {
            return Err(format!(
                "guest process identity drifted: expected {expected_identity}, observed {observed_identity}"
            ));
        }
        eprintln!("[windows-hosted] guest identity admitted");
        let request = read_request(request_path)?;
        request.admit().map_err(|error| error.to_string())?;
        admit_machine(&request)?;
        let GuestEnvironment {
            workspace,
            temp,
            vendor,
            system_root,
            provider_identity,
            run_id,
            run_attempt,
        } = GuestEnvironment::decode(encoded_environment)?;
        admit_workspace_source(&request, &workspace)?;
        eprintln!("[windows-hosted] guest environment admitted");
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
        verify_non_loopback_denied()?;
        eprintln!("[windows-hosted] guest network isolation admitted");

        let scratch = scratch_root(&temp, &request.challenge);
        let cargo_home = scratch.join("cargo-home");
        let target = scratch.join("target");
        let stage = temp.join(format!("windows-stage-{}", request.challenge));
        verify_read_only("source", &workspace, &request.challenge)?;
        fs::read(workspace.join("Cargo.toml"))
            .map_err(|error| format!("sealed source manifest is unreadable: {error}"))?;
        verify_read_only("vendored dependencies", &vendor, &request.challenge)?;
        let canvas = super::canvas::root(&temp, &request);
        if let Some(canvas) = &canvas {
            verify_read_only("provider canvas", canvas, &request.challenge)?;
        }
        verify_writable("fresh Cargo home", &cargo_home, &request.challenge)?;
        verify_writable("fresh target", &target, &request.challenge)?;
        verify_writable("staged output", &stage, &request.challenge)?;
        eprintln!(
            "[windows-hosted] guest filesystem admitted; executing {} command(s)",
            request.commands.len()
        );

        let outputs = request
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
                    &system_root,
                    &stage,
                    canvas.as_deref(),
                    &request,
                )
                .map(|(completion, stdout, stderr)| CommandResult {
                    completion,
                    stdout,
                    stderr,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let artifacts = artifact_inventory(&stage)?;
        grant_tree_contents_access(&stage, &provider_identity, 0o400, 0o500)?;
        let result = ResultDocument {
            version: windows_execution::hosted::PROTOCOL_VERSION,
            challenge: request.challenge.clone(),
            request: request.clone(),
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
        write_result(result_path, &encoded)
    }

    fn read_request(path: &std::ffi::OsStr) -> Result<Request, String> {
        let bytes = fs::read(path).map_err(|error| {
            format!("cannot read request {}: {error}", Path::new(path).display())
        })?;
        serde_json::from_slice(&bytes).map_err(|error| format!("request does not decode: {error}"))
    }

    fn write_result(path: &std::ffi::OsStr, encoded: &[u8]) -> Result<(), String> {
        let mut file = File::create(path).map_err(|error| {
            format!(
                "cannot create result {}: {error}",
                Path::new(path).display()
            )
        })?;
        file.write_all(encoded)
            .map_err(|error| format!("cannot write result: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("cannot sync result: {error}"))
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

    fn admit_machine(request: &Request) -> Result<(), String> {
        if env::consts::ARCH != "x86_64" || env::consts::OS != "windows" {
            return Err(format!(
                "unsupported guest {}-{}",
                env::consts::ARCH,
                env::consts::OS
            ));
        }
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

    fn admit_source(request: &Request, workspace: &Path, archive: &Path) -> Result<(), String> {
        let raw = raw_sha256(archive)?;
        if raw != request.archive_sha256 {
            return Err(format!(
                "transported archive changed: expected {}, observed {raw}",
                request.archive_sha256
            ));
        }
        let archive_identity = dist::source_archive_identity(archive)?;
        if archive_identity.to_string() != request.source {
            return Err(format!(
                "source archive identity changed: expected {}, observed {archive_identity}",
                request.source
            ));
        }
        admit_workspace_source(request, workspace)
    }

    fn admit_workspace_source(request: &Request, workspace: &Path) -> Result<(), String> {
        let source = dist::source_identity(workspace)?
            .ok_or_else(|| "selected source tree records no canonical commit".to_owned())?;
        if source.commit() != request.source_commit {
            return Err(format!(
                "canonical source changed: expected {}, observed {}",
                request.source_commit,
                source.commit()
            ));
        }
        Ok(())
    }

    fn raw_sha256(path: &Path) -> Result<String, String> {
        let mut file =
            File::open(path).map_err(|error| format!("cannot open {}: {error}", path.display()))?;
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
        }
        Ok(format!("{:x}", digest.finalize()))
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

    /// The scratch root parent and guest share for one request.
    ///
    /// MSVC `link.exe` is not long-path aware, and nested Cargo build
    /// directories beneath this root already spend most of `MAX_PATH`, so the
    /// name carries only the challenge prefix the guest account and firewall
    /// rule already use; the runner is disposable, so the prefix is unique.
    fn scratch_root(temp: &Path, challenge: &str) -> PathBuf {
        temp.join(format!("hosted-{}", &challenge[..16]))
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

    fn make_read_only(
        kind: &str,
        path: &Path,
        challenge: &str,
        identity: &str,
    ) -> Result<(), String> {
        seal_tree(path, identity).map_err(|error| format!("cannot seal {kind}: {error}"))?;
        verify_read_only(kind, path, challenge)
    }

    fn verify_read_only(kind: &str, path: &Path, challenge: &str) -> Result<(), String> {
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

    fn verify_writable(kind: &str, path: &Path, challenge: &str) -> Result<(), String> {
        let probe = path.join(format!(".spice-writable-{challenge}"));
        fs::write(&probe, b"write-authority")
            .map_err(|error| format!("{kind} is not writable: {error}"))?;
        fs::remove_file(&probe)
            .map_err(|error| format!("cannot remove {kind} write probe: {error}"))
    }

    fn seal_tree(path: &Path, identity: &str) -> io::Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if authority::is_link_like(&metadata) {
            return Ok(());
        }
        let mode = if metadata.is_dir() {
            for entry in fs::read_dir(path)? {
                seal_tree(&entry?.path(), identity)?;
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
        authority::set_owner_and_identity_mode(path, identity, mode, mode)
    }

    fn grant_tree_contents_access(
        path: &Path,
        identity: &str,
        file_mode: u32,
        directory_mode: u32,
    ) -> Result<(), String> {
        for entry in fs::read_dir(path)
            .map_err(|error| format!("cannot enumerate access tree {}: {error}", path.display()))?
        {
            let entry = entry.map_err(|error| {
                format!("cannot enumerate access tree {}: {error}", path.display())
            })?;
            grant_tree_access(&entry.path(), identity, file_mode, directory_mode)?;
        }
        Ok(())
    }

    fn grant_tree_access(
        path: &Path,
        identity: &str,
        file_mode: u32,
        directory_mode: u32,
    ) -> Result<(), String> {
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| format!("cannot inspect access tree {}: {error}", path.display()))?;
        if authority::is_link_like(&metadata) {
            return Err(format!(
                "access tree refuses link-like path {}",
                path.display()
            ));
        }
        let mode = if metadata.is_dir() {
            for entry in fs::read_dir(path).map_err(|error| {
                format!("cannot enumerate access tree {}: {error}", path.display())
            })? {
                let entry = entry.map_err(|error| {
                    format!("cannot enumerate access tree {}: {error}", path.display())
                })?;
                grant_tree_access(&entry.path(), identity, file_mode, directory_mode)?;
            }
            directory_mode
        } else if metadata.is_file() {
            file_mode
        } else {
            return Err(format!(
                "access tree path is not regular: {}",
                path.display()
            ));
        };
        authority::grant_identity_mode(path, identity, mode).map_err(|error| {
            format!(
                "cannot grant identity access to {}: {error}",
                path.display()
            )
        })
    }

    fn grant_file_access(path: &Path, identity: &str, mode: u32) -> Result<(), String> {
        grant_ancestor_traversal(path, identity)?;
        authority::grant_identity_mode(path, identity, mode)
            .map_err(|error| format!("cannot grant guest access to {}: {error}", path.display()))
    }

    fn grant_ancestor_traversal(path: &Path, identity: &str) -> Result<(), String> {
        let mut directory = path.parent();
        while let Some(parent) = directory {
            if parent.parent().is_none() {
                break;
            }
            authority::grant_identity_mode(parent, identity, 0o100).map_err(|error| {
                format!(
                    "cannot grant guest traversal through {}: {error}",
                    parent.display()
                )
            })?;
            directory = parent.parent();
        }
        Ok(())
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
        system_root: &Path,
        stage: &Path,
        linux: Option<&Path>,
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
            .env("SystemRoot", system_root)
            // Admitted tools are executables; `which`-style lookups such as
            // Trunk's resolve a bare tool name to `NAME.exe` only through it.
            .env("PATHEXT", ".EXE")
            .env("CARGO_HOME", cargo_home)
            .env("CARGO_TARGET_DIR", target)
            .env("CARGO_NET_OFFLINE", "true")
            .env("TRUNK_OFFLINE", "true")
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
        if let Some(canvas) = linux {
            if let Some(inputs) = &request.release_inputs {
                command
                    .env("SPICE_RELEASE_CANVAS", canvas.join("canvas.zip")) // env-policy: allow
                    .env("SPICE_RELEASE_APP", inputs.app.as_str()); // env-policy: allow
            } else {
                command.env("SPICE_LINUX_STAGE", canvas); // env-policy: allow
            }
        }
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

    fn verify_non_loopback_available() -> Result<(), String> {
        let address = NETWORK_PROBE
            .parse::<SocketAddr>()
            .map_err(|error| format!("invalid network probe address: {error}"))?;
        TcpStream::connect_timeout(&address, Duration::from_secs(5))
            .map(|stream| drop(stream))
            .map_err(|error| format!("provider network was unavailable before isolation: {error}"))
    }

    fn verify_non_loopback_denied() -> Result<(), String> {
        let address = NETWORK_PROBE
            .parse::<SocketAddr>()
            .map_err(|error| format!("invalid network probe address: {error}"))?;
        match TcpStream::connect_timeout(&address, Duration::from_secs(3)) {
            Ok(stream) => {
                drop(stream);
                Err("isolated guest retained non-loopback network access".to_owned())
            }
            Err(_) => Ok(()),
        }
    }

    struct GuestAccount {
        name: String,
        password: String,
        sid: String,
        active: bool,
    }

    impl GuestAccount {
        fn create(challenge: &str) -> Result<Self, String> {
            let suffix = challenge
                .get(..12)
                .filter(|value| value.bytes().all(|byte| byte.is_ascii_alphanumeric()))
                .ok_or_else(|| "challenge cannot name a Windows guest account".to_owned())?;
            let name = format!("spice{suffix}");
            let mut random = [0_u8; 24];
            getrandom::fill(&mut random)
                .map_err(|error| format!("cannot generate guest account password: {error}"))?;
            let password = format!(
                "Sp1!{}",
                random
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            );
            let script = format!(
                "$ErrorActionPreference = 'Stop'\n\
                 $password = ConvertTo-SecureString {} -AsPlainText -Force\n\
                 New-LocalUser -Name {} -Password $password \
                     -AccountNeverExpires -PasswordNeverExpires \
                     -UserMayNotChangePassword | Out-Null\n\
                 $sid = (Get-LocalUser -Name {}).SID.ToString()\n\
                 if ([string]::IsNullOrWhiteSpace($sid)) {{ \
                     throw 'new local user has no SID' \
                 }}\n\
                 Write-Output $sid\n",
                ps_literal(&password),
                ps_literal(&name),
                ps_literal(&name),
            );
            let sid = match powershell_output(&script, "create guest account") {
                Ok(sid) => sid,
                Err(error) => {
                    let cleanup = delete_guest(&name).err();
                    return Err(format!(
                        "{error}{}",
                        cleanup
                            .map(|cleanup| { format!("; account cleanup also failed: {cleanup}") })
                            .unwrap_or_default()
                    ));
                }
            };
            let sid = sid.trim().to_owned();
            if !sid.starts_with("S-1-5-")
                || !sid
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || byte == b'S' || byte == b'-')
            {
                let cleanup = delete_guest(&name).err();
                return Err(format!(
                    "guest account returned invalid SID {sid:?}{}",
                    cleanup
                        .map(|error| format!("; account cleanup also failed: {error}"))
                        .unwrap_or_default()
                ));
            }
            Ok(Self {
                name,
                password,
                sid,
                active: true,
            })
        }

        fn sid(&self) -> &str {
            &self.sid
        }

        fn execute(
            &self,
            executable: &Path,
            request: &std::ffi::OsStr,
            result: &Path,
            guest_environment: &str,
        ) -> Result<(), String> {
            let guest_arguments = [
                GUEST_ARGUMENT.to_owned(),
                quoted_argument(Path::new(request)),
                quoted_argument(result),
                self.sid.clone(),
                guest_environment.to_owned(),
            ]
            .into_iter()
            .map(|argument| ps_literal(&argument))
            .collect::<Vec<_>>()
            .join(", ");
            let script = format!(
                "$ErrorActionPreference = 'Stop'\n\
                 $password = ConvertTo-SecureString {} -AsPlainText -Force\n\
                 $credential = [Management.Automation.PSCredential]::new(\
                     \"$env:COMPUTERNAME\\{}\", $password)\n\
                 $process = Start-Process -FilePath {} -ArgumentList @({guest_arguments}) \
                     -Credential $credential -NoNewWindow -PassThru\n\
                 $process.WaitForExit()\n\
                 exit $process.ExitCode\n",
                ps_literal(&self.password),
                self.name,
                ps_literal(&executable.display().to_string()),
            );
            powershell_status(&script, "launch isolated guest")
        }

        fn close(mut self) -> Result<(), String> {
            let result = delete_guest(&self.name);
            if result.is_ok() {
                self.active = false;
            }
            result
        }
    }

    impl Drop for GuestAccount {
        fn drop(&mut self) {
            if self.active {
                let _deleted = delete_guest(&self.name);
            }
        }
    }

    fn delete_guest(name: &str) -> Result<(), String> {
        let script = format!(
            "$ErrorActionPreference = 'Stop'\n\
             Remove-LocalUser -Name {}\n",
            ps_literal(name)
        );
        powershell_status(&script, "remove guest account")
    }

    struct Firewall {
        name: String,
        active: bool,
    }

    impl Firewall {
        fn establish(challenge: &str, identity: &str) -> Result<Self, String> {
            let name = format!("hosted-{}", &challenge[..16]);
            let addresses = NON_LOOPBACK_REMOTE
                .iter()
                .map(|address| ps_literal(address))
                .collect::<Vec<_>>()
                .join(", ");
            let principal = format!("D:(A;;CC;;;{identity})");
            let script = format!(
                "$ErrorActionPreference = 'Stop'\n\
                 New-NetFirewallRule -DisplayName {} -Direction Outbound -Action Block \
                     -Enabled True -Profile Any -RemoteAddress @({addresses}) -LocalUser {} \
                     | Out-Null\n",
                ps_literal(&name),
                ps_literal(&principal),
            );
            powershell_status(&script, "establish guest firewall")?;
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
        let script = format!(
            "$ErrorActionPreference = 'Stop'\n\
             Remove-NetFirewallRule -DisplayName {}\n",
            ps_literal(name)
        );
        powershell_status(&script, "remove guest firewall")
    }

    fn combine_cleanup(
        execution: Result<(), String>,
        firewall: Result<(), String>,
        account: Result<(), String>,
    ) -> Result<(), String> {
        let mut errors = [execution.err(), firewall.err(), account.err()]
            .into_iter()
            .flatten();
        let Some(mut combined) = errors.next() else {
            return Ok(());
        };
        for error in errors {
            combined.push_str("; cleanup also failed: ");
            combined.push_str(&error);
        }
        Err(combined)
    }

    fn quoted_argument(path: &Path) -> String {
        format!("\"{}\"", path.display().to_string().replace('"', "\\\""))
    }

    fn ps_literal(value: &str) -> String {
        format!("'{}'", value.replace('\'', "''"))
    }

    fn powershell_output(script: &str, action: &str) -> Result<String, String> {
        let output = powershell_command(script, Stdio::piped(), Stdio::piped(), action)?
            .wait_with_output()
            .map_err(|error| format!("cannot finish PowerShell to {action}: {error}"))?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() {
            return Err(format!("PowerShell refused to {action}: {}", stderr.trim()));
        }
        if !stderr.trim().is_empty() {
            return Err(format!(
                "PowerShell reported an error while trying to {action}: {}",
                stderr.trim()
            ));
        }
        String::from_utf8(output.stdout)
            .map_err(|_| format!("PowerShell returned non-UTF-8 output while trying to {action}"))
    }

    fn powershell_status(script: &str, action: &str) -> Result<(), String> {
        let status = powershell_command(script, Stdio::inherit(), Stdio::inherit(), action)?
            .wait()
            .map_err(|error| format!("cannot finish PowerShell to {action}: {error}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("PowerShell refused to {action} with {status}"))
        }
    }

    fn powershell_command(
        script: &str,
        stdout: Stdio,
        stderr: Stdio,
        action: &str,
    ) -> Result<std::process::Child, String> {
        let mut guarded = String::with_capacity(script.len() + 160);
        guarded.push_str(
            "try {\n\
             $ProgressPreference = 'SilentlyContinue'\n\
             $ErrorActionPreference = 'Stop'\n",
        );
        guarded.push_str(script);
        guarded.push_str(
            "\nexit 0\n\
             } catch {\n\
             [Console]::Error.WriteLine(($_ | Out-String))\n\
             exit 1\n\
             }\n",
        );
        let encoded = guarded
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        let encoded = base64::engine::general_purpose::STANDARD.encode(encoded);
        let child = ProcessCommand::new(POWERSHELL)
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-EncodedCommand",
            ])
            .arg(encoded)
            .env_remove("PSModulePath")
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .spawn()
            .map_err(|error| format!("cannot start PowerShell to {action}: {error}"))?;
        Ok(child)
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

//! Native target build adapter and debug-artifact capture.

use super::*;
pub(super) struct CommandBuilder<'a, E> {
    pub(super) root: &'a Path,
    pub(super) target: ReleaseTarget,
    pub(super) app_root: &'a Path,
    pub(super) target_dir: &'a Path,
    pub(super) programs: &'a Programs,
    pub(super) version: &'a ProductVersion,
    pub(super) executor: &'a E,
    #[cfg(unix)]
    pub(super) commit: &'a str,
    #[cfg(unix)]
    pub(super) signing: Option<&'a DarwinSigning>,
    pub(super) symbols: Option<MacSymbols>,
    pub(super) symbol_artifact: Option<Vec<u8>>,
    pub(super) windows_debug_artifact: Option<Vec<u8>>,
    pub(super) canvas: Option<crate::app::StagedCanvas>,
    pub(super) executable: Option<PathBuf>,
}

impl<E: Executor> Builder for CommandBuilder<'_, E> {
    fn build_app(&mut self) -> Result<Canvas, BuildError> {
        if let Some(staged) = &self.canvas {
            return Ok(staged.canvas().clone());
        }
        let staged = build_release_app(
            self.executor,
            self.programs,
            self.root,
            self.app_root,
            self.version,
        )?;
        let canvas = staged.canvas().clone();
        self.canvas = Some(staged);
        Ok(canvas)
    }

    fn build_native(
        &mut self,
        target: ReleaseTarget,
        canvas: &Canvas,
    ) -> Result<Vec<u8>, BuildError> {
        if target != self.target {
            return Err(BuildError::new(format!(
                "builder target {} does not match requested {target}",
                self.target
            )));
        }
        let staged = self
            .canvas
            .as_ref()
            .filter(|staged| staged.canvas() == canvas)
            .ok_or_else(|| {
                BuildError::new("native build names a canvas the app build did not stage")
            })?;
        let build = native::cargo_build(target, self.target_dir, staged).map_err(build_error)?;
        let native = self.run(CommandSpec::new(
            "native-build",
            &self.programs.cargo,
            build.arguments,
        ))?;
        let cargo = crate::native_link::cargo_output(&native.stdout, &build.link.object)
            .map_err(build_error)?;
        let emitted = cargo.emitted.is_some();
        if let Some(plan) = cargo.emitted {
            plan.publish(&build.link.plan).map_err(build_error)?;
        }
        let executable = cargo.executable;
        let mut native = read_regular(&executable).map_err(build_error)?;
        if canvas.verify_linked(&native).is_err() {
            if emitted {
                return Err(BuildError::new(
                    "rustc completed a final link that did not carry the staged canvas",
                ));
            }
            let plan = crate::native_link::LinkPlan::read(&build.link.plan, &build.link.object)
                .map_err(build_error)?;
            let (set, unset) = plan.environment();
            self.run(
                CommandSpec::new(
                    "native-relink",
                    plan.program(),
                    plan.arguments().iter().cloned(),
                )
                .with_environment(set, unset),
            )?;
            plan.publish_output(&executable).map_err(build_error)?;
            native = read_regular(&executable).map_err(build_error)?;
            canvas.verify_linked(&native).map_err(build_error)?;
        }
        #[cfg(unix)]
        let symbols = if target == ReleaseTarget::Aarch64AppleDarwin {
            Some(self.derive_macos_symbols(&executable)?)
        } else {
            None
        };
        // Sign last: after the dSYM is derived and the image stripped, and
        // before the bytes the pipeline digests are read, so the seal binds
        // the signed executable and nothing changes it afterwards.
        #[cfg(unix)]
        if let Some(pair) = self.signing {
            signing::sign(&executable, pair, self.target_dir, self.executor)?;
        }
        let native = read_regular(&executable).map_err(build_error)?;
        #[cfg(unix)]
        if let Some(symbols) = symbols {
            self.symbol_artifact = Some(symbols.capture.encode(
                &symbols.public.uuid,
                Digest::compute(Domain::DistArtifact, &native),
                self.commit,
            )?);
            self.symbols = Some(symbols.public);
        }
        if target == ReleaseTarget::X86_64PcWindowsMsvc {
            self.windows_debug_artifact =
                Some(read_regular(&executable.with_extension("pdb")).map_err(build_error)?);
        }
        self.executable = Some(executable);
        Ok(native)
    }

    fn report_version(&mut self) -> Result<String, BuildError> {
        let executable = self
            .executable
            .as_ref()
            .ok_or_else(|| BuildError::new("no final executable has been built"))?;
        let report = self.run(CommandSpec::new(
            "canvas-version",
            executable,
            ["--version"],
        ))?;
        String::from_utf8(report.stdout)
            .map_err(|_| BuildError::new("final executable version report is not UTF-8"))
    }

    fn take_symbol_artifact(&mut self) -> Option<Vec<u8>> {
        self.symbol_artifact.take()
    }

    fn take_windows_debug_artifact(&mut self) -> Option<Vec<u8>> {
        self.windows_debug_artifact.take()
    }
}

/// Build the release app into `app_root` through the release executor and
/// admit its staged canvas as the generation `version` derives.
///
/// The full stage and the app-only checkpoint both run exactly this command,
/// so the checkpoint proves the environment the full stage's app build gets.
pub(super) fn build_release_app<E: Executor>(
    executor: &E,
    programs: &Programs,
    root: &Path,
    app_root: &Path,
    version: &ProductVersion,
) -> Result<crate::app::StagedCanvas, BuildError> {
    let record = executor
        .execute(CommandSpec::new(
            "app-build",
            &programs.xtask,
            [
                OsString::from("app"),
                OsString::from("--stage-root"),
                app_root.as_os_str().to_owned(),
            ],
        ))
        .map_err(build_error)?;
    successful(record).map_err(build_error)?;
    let build = dist::build_info(root, dist::Build::Staged).map_err(BuildError::new)?;
    if &build.product != version {
        return Err(BuildError::new(format!(
            "release build expected product {version} but source derives {}",
            build.product
        )));
    }
    crate::app::StagedCanvas::read_from(root, app_root, &build).map_err(build_error)
}

impl<E: Executor> CommandBuilder<'_, E> {
    fn run(&self, command: CommandSpec) -> Result<CommandRecord, BuildError> {
        let record = self.executor.execute(command).map_err(build_error)?;
        successful(record).map_err(build_error)
    }

    #[cfg(unix)]
    fn derive_macos_symbols(&self, executable: &Path) -> Result<DerivedMacSymbols, BuildError> {
        let tools = self
            .programs
            .macos
            .as_ref()
            .expect("macOS target discovery admits symbol tools");
        let bundle = dsym_path(executable);
        self.run(CommandSpec::new(
            "dsym-generate",
            &tools.dsymutil,
            [
                executable.as_os_str().to_owned(),
                OsString::from("-o"),
                bundle.as_os_str().to_owned(),
            ],
        ))?;
        let capture = SymbolBundle::capture(&bundle, executable)?;
        let executable_uuid = self.run(CommandSpec::new(
            "executable-uuid",
            &tools.dwarfdump,
            [OsString::from("--uuid"), executable.as_os_str().to_owned()],
        ))?;
        let executable_uuid = parse_macho_uuid(&executable_uuid.stdout, "executable UUID")?;
        let symbol_uuid = {
            let input = capture.uuid_input()?;
            self.run(CommandSpec::new(
                "dsym-uuid",
                &tools.dwarfdump,
                [
                    OsString::from("--uuid"),
                    input.path().as_os_str().to_owned(),
                ],
            ))?
        };
        let symbol_uuid = parse_macho_uuid(&symbol_uuid.stdout, "dSYM UUID")?;
        if executable_uuid != symbol_uuid {
            return Err(BuildError::new(format!(
                "executable UUID {executable_uuid} differs from dSYM UUID {symbol_uuid}"
            )));
        }
        // A full `strip`, not `-S`: release DWARF already lives in the object
        // files the dSYM was built from, so `-S` removed nothing, and the
        // retained symbol table was 15.8M of the 77.7M aarch64 executable
        // (142,118 names). `LC_UUID` survives, so the dSYM still pairs.
        self.run(CommandSpec::new(
            "native-strip",
            &tools.strip,
            [executable.as_os_str().to_owned()],
        ))?;
        Ok(DerivedMacSymbols {
            public: MacSymbols {
                bundle,
                uuid: executable_uuid,
            },
            capture,
        })
    }
}

#[cfg(unix)]
pub(super) fn dsym_path(executable: &Path) -> PathBuf {
    let mut path = executable.as_os_str().to_owned();
    path.push(".dSYM");
    PathBuf::from(path)
}

#[cfg(unix)]
fn parse_macho_uuid(output: &[u8], label: &'static str) -> Result<String, BuildError> {
    let line = one_line(output, label).map_err(build_error)?;
    let Some(body) = line.strip_prefix("UUID: ") else {
        return Err(BuildError::new(format!("{label} is malformed: {line:?}")));
    };
    let Some((uuid, suffix)) = body.split_once(' ') else {
        return Err(BuildError::new(format!("{label} is malformed: {line:?}")));
    };
    let valid_uuid = uuid.len() == MACHO_UUID_LENGTH
        && uuid.bytes().enumerate().all(|(index, byte)| {
            if MACHO_UUID_HYPHENS.contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        });
    if !valid_uuid || !suffix.starts_with("(arm64) ") || suffix == "(arm64) " {
        return Err(BuildError::new(format!("{label} is malformed: {line:?}")));
    }
    Ok(uuid.to_ascii_uppercase())
}

pub(super) fn build_error(error: impl fmt::Display) -> BuildError {
    BuildError::new(error.to_string())
}

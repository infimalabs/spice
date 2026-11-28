//! Native target build adapter and debug-artifact capture.

use super::*;
const MIN_ELF_BUILD_ID_HEX_LENGTH: usize = 16;

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
    pub(super) capture_symbols: bool,
    pub(super) symbols: Option<MacSymbols>,
    pub(super) symbol_artifact: Option<Vec<u8>>,
    pub(super) linux_debug_artifact: Option<Vec<u8>>,
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
        let staged = self.admit_staged_canvas(target, canvas)?;
        let build = native::cargo_build(target, self.target_dir, staged).map_err(build_error)?;
        let (executable, _) = self.build_linked_native(build, canvas)?;
        #[cfg(unix)]
        let symbols = self.prepare_macos_symbols(target, &executable)?;
        // Sign last: after the dSYM is derived and the image stripped, and
        // before the bytes the pipeline digests are read, so the seal binds
        // the signed executable and nothing changes it afterwards.
        #[cfg(unix)]
        if let Some(pair) = self.signing {
            signing::sign(&executable, pair, self.target_dir, self.executor)?;
        }
        self.prepare_linux_symbols(target, &executable)?;
        let native = read_regular(&executable).map_err(build_error)?;
        #[cfg(unix)]
        self.record_macos_symbols(symbols, &native)?;
        self.prepare_windows_symbols(target, &executable)?;
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
    fn build_id(&mut self, target: ReleaseTarget, native: &[u8]) -> Result<String, BuildError> {
        let executable = self
            .executable
            .as_ref()
            .ok_or_else(|| BuildError::new("no final executable has been built"))?;
        match target {
            #[cfg(unix)]
            ReleaseTarget::Aarch64AppleDarwin => self.macho_uuid(executable),
            ReleaseTarget::X86_64UnknownLinuxMusl => {
                let record = self.run(CommandSpec::new(
                    "linux-build-id",
                    Path::new("readelf"),
                    [OsString::from("--notes"), executable.as_os_str().to_owned()],
                ))?;
                parse_elf_build_id(&record.stdout)
            }
            ReleaseTarget::X86_64PcWindowsMsvc => {
                Ok(Digest::compute(Domain::DistArtifact, native).to_string())
            }
            #[cfg(not(unix))]
            ReleaseTarget::Aarch64AppleDarwin => Err(BuildError::new(
                "Mach-O build identity requires a Unix host",
            )),
        }
    }

    fn take_linux_debug_artifact(&mut self) -> Option<Vec<u8>> {
        self.linux_debug_artifact.take()
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
    fn admit_staged_canvas(
        &self,
        target: ReleaseTarget,
        canvas: &Canvas,
    ) -> Result<&crate::app::StagedCanvas, BuildError> {
        if target != self.target {
            return Err(BuildError::new(format!(
                "builder target {} does not match requested {target}",
                self.target
            )));
        }
        self.canvas
            .as_ref()
            .filter(|staged| staged.canvas() == canvas)
            .ok_or_else(|| {
                BuildError::new("native build names a canvas the app build did not stage")
            })
    }

    fn build_linked_native(
        &self,
        build: native::NativeBuild,
        canvas: &Canvas,
    ) -> Result<(PathBuf, Vec<u8>), BuildError> {
        let record = self.run(CommandSpec::new(
            "native-build",
            &self.programs.cargo,
            build.arguments,
        ))?;
        let cargo = crate::native_link::cargo_output(&record.stdout, &build.link.object)
            .map_err(build_error)?;
        let emitted = cargo.emitted.is_some();
        if let Some(plan) = cargo.emitted {
            plan.publish(&build.link.plan).map_err(build_error)?;
        }
        let executable = cargo.executable;
        let mut native = read_regular(&executable).map_err(build_error)?;
        if canvas.verify_linked(&native).is_ok() {
            return Ok((executable, native));
        }
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
        Ok((executable, native))
    }

    #[cfg(unix)]
    fn prepare_macos_symbols(
        &self,
        target: ReleaseTarget,
        executable: &Path,
    ) -> Result<Option<DerivedMacSymbols>, BuildError> {
        if target != ReleaseTarget::Aarch64AppleDarwin {
            return Ok(None);
        }
        if self.capture_symbols {
            return self.derive_macos_symbols(executable).map(Some);
        }
        self.strip_macos(executable)?;
        Ok(None)
    }

    fn prepare_linux_symbols(
        &mut self,
        target: ReleaseTarget,
        executable: &Path,
    ) -> Result<(), BuildError> {
        if target != ReleaseTarget::X86_64UnknownLinuxMusl {
            return Ok(());
        }
        if self.capture_symbols {
            self.linux_debug_artifact = Some(read_regular(executable).map_err(build_error)?);
        }
        self.run(CommandSpec::new(
            "native-strip",
            Path::new("strip"),
            [
                OsString::from("--strip-all"),
                executable.as_os_str().to_owned(),
            ],
        ))?;
        Ok(())
    }

    #[cfg(unix)]
    fn record_macos_symbols(
        &mut self,
        symbols: Option<DerivedMacSymbols>,
        native: &[u8],
    ) -> Result<(), BuildError> {
        let Some(symbols) = symbols else {
            return Ok(());
        };
        self.symbol_artifact = Some(symbols.capture.encode(
            &symbols.public.uuid,
            Digest::compute(Domain::DistArtifact, native),
            self.commit,
        )?);
        self.symbols = Some(symbols.public);
        Ok(())
    }

    fn prepare_windows_symbols(
        &mut self,
        target: ReleaseTarget,
        executable: &Path,
    ) -> Result<(), BuildError> {
        if self.capture_symbols && target == ReleaseTarget::X86_64PcWindowsMsvc {
            self.windows_debug_artifact =
                Some(read_regular(&executable.with_extension("pdb")).map_err(build_error)?);
        }
        Ok(())
    }

    fn run(&self, command: CommandSpec) -> Result<CommandRecord, BuildError> {
        let record = self.executor.execute(command).map_err(build_error)?;
        successful(record).map_err(build_error)
    }
    #[cfg(unix)]
    fn macho_uuid(&self, executable: &Path) -> Result<String, BuildError> {
        let tools = self
            .programs
            .macos
            .as_ref()
            .expect("macOS target discovery admits symbol tools");
        let record = self.run(CommandSpec::new(
            "executable-uuid",
            &tools.dwarfdump,
            [OsString::from("--uuid"), executable.as_os_str().to_owned()],
        ))?;
        parse_macho_uuid(&record.stdout, "executable UUID")
    }

    #[cfg(unix)]
    fn strip_macos(&self, executable: &Path) -> Result<(), BuildError> {
        let tools = self
            .programs
            .macos
            .as_ref()
            .expect("macOS target discovery admits symbol tools");
        self.run(CommandSpec::new(
            "native-strip",
            &tools.strip,
            [executable.as_os_str().to_owned()],
        ))?;
        Ok(())
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
        let executable_uuid = self.macho_uuid(executable)?;
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
        self.strip_macos(executable)?;
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

fn parse_elf_build_id(output: &[u8]) -> Result<String, BuildError> {
    let output = std::str::from_utf8(output)
        .map_err(|_| BuildError::new("ELF build-id report is not UTF-8"))?;
    let build_id = output
        .lines()
        .find_map(|line| line.trim().strip_prefix("Build ID: "))
        .filter(|value| {
            value.len() >= MIN_ELF_BUILD_ID_HEX_LENGTH
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or_else(|| BuildError::new("ELF build-id report carries no canonical build ID"))?;
    Ok(build_id.to_owned())
}

pub(super) fn build_error(error: impl fmt::Display) -> BuildError {
    BuildError::new(error.to_string())
}

//! Target-specific Cargo invocation for one linked appliance executable.

use std::ffi::OsString;
use std::path::Path;

use wire::manifest::ReleaseTarget;

/// Cargo request and stable link assets for one appliance executable.
pub(super) struct NativeBuild {
    pub(super) arguments: Vec<OsString>,
    pub(super) link: crate::app::NativeLink,
}

/// Ask Cargo for the exact linked binary carrying `canvas`, which also names
/// the version the executable reports, retaining line tables for targets
/// whose declared debug artifact consumes them. The macOS executable must
/// leave the link unstripped:
/// `dsymutil` follows the symbol table's object-file entries to assemble the
/// dSYM, after which the builder strips the executable itself. Linux declares
/// no debug artifact, so its symbol table goes at link. MSVC receives no strip
/// setting because rustc spells one as `/DEBUG:NONE`, which would suppress the
/// declared PDB.
pub(super) fn cargo_build(
    target: ReleaseTarget,
    target_dir: &Path,
    canvas: &crate::app::StagedCanvas,
) -> Result<NativeBuild, crate::app::AppBuildError> {
    let object = match target {
        ReleaseTarget::Aarch64AppleDarwin => rack::link::Target::MacAarch64,
        ReleaseTarget::X86_64UnknownLinuxMusl => rack::link::Target::LinuxX86_64,
        ReleaseTarget::X86_64PcWindowsMsvc => rack::link::Target::WindowsX86_64,
    };
    let binary = "spice";
    let mut arguments = vec![
        OsString::from("rustc"),
        OsString::from("--locked"),
        OsString::from("--release"),
        OsString::from("-p"),
        OsString::from("spice"),
        OsString::from("--bin"),
        OsString::from(binary),
    ];
    arguments.extend([
        OsString::from("--target"),
        OsString::from(target.as_str()),
        OsString::from("--target-dir"),
    ]);
    arguments.push(target_dir.as_os_str().to_owned());
    arguments.push(OsString::from("--message-format=json-render-diagnostics"));
    if target.debug_path().is_some() {
        arguments.extend([
            OsString::from("--config"),
            OsString::from(super::candidate().debug_config()),
        ]);
    }
    if target == ReleaseTarget::X86_64UnknownLinuxMusl {
        arguments.extend([
            OsString::from("--config"),
            OsString::from("profile.release.strip=\"symbols\""),
        ]);
    }
    let link = canvas.native_link(target_dir, object, crate::app::LinkProfile::Release)?;
    arguments.extend(link.arguments.iter().cloned());
    Ok(NativeBuild { arguments, link })
}

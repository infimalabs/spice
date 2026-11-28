//! Target-specific Cargo invocation for one linked appliance executable.

use std::ffi::OsString;
use std::path::Path;

use wire::manifest::ReleaseTarget;

/// Cargo request and stable link assets for one appliance executable.
pub(super) struct NativeBuild {
    pub(super) arguments: Vec<OsString>,
    pub(super) link: crate::app::NativeLink,
}

/// Ask Cargo for the exact linked binary carrying `canvas`. Every candidate
/// retains line tables in its private build outputs so an authenticated symbol
/// sidecar can be reproduced later. The platform adapter strips the public
/// executable before it enters the staged payload.
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
    arguments.extend([
        OsString::from("--config"),
        OsString::from(super::candidate().debug_config()),
    ]);
    let mut link = canvas.native_link(target_dir, object, crate::app::LinkProfile::Release)?;
    if target == ReleaseTarget::X86_64UnknownLinuxMusl {
        link.arguments.extend([
            OsString::from("-C"),
            OsString::from("link-arg=-Wl,--build-id=sha1"),
        ]);
    }
    arguments.extend(link.arguments.iter().cloned());
    Ok(NativeBuild { arguments, link })
}

//! Tracked declaration of the guest's host tools, machine shape, and pinned
//! provisioning inputs.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::Digest as _;

const DECLARATION: &str = "dist/windows/guest.toml";
pub(super) const UNATTEND_TEMPLATE: &str = "dist/windows/guest/autounattend.xml";
pub(super) const SETUP_SCRIPT: &str = "dist/windows/guest/setup.ps1";
const SCHEMA: u64 = 1;
const SHA256_HEX_LENGTH: usize = 64;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Declaration {
    schema: u64,
    pub tools: Tools,
    pub machine: Machine,
    pub image: Image,
    pub drivers: Drivers,
    pub openssh: Download,
    pub runtime: Download,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Tools {
    pub qemu_system: Pin,
    pub qemu_img: Pin,
    pub firmware: Pin,
    pub firmware_variables: Pin,
    pub wimlib: Pin,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Pin {
    pub path: PathBuf,
    sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Machine {
    pub cpus: u16,
    pub memory_mib: u32,
    pub disk_gib: u32,
    pub ssh_port: u16,
    pub qmp_port: u16,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Download {
    pub url: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Image {
    url: String,
    sha256: String,
    bytes: u64,
    pub setup_media_index: u32,
    pub preinstallation_index: u32,
    pub setup_index: u32,
    pub edition_index: u32,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Drivers {
    url: String,
    sha256: String,
    bytes: u64,
    pub network: String,
    pub serial: String,
}

impl Declaration {
    /// Load the tracked declaration and the identity of every input that
    /// shapes the clean base: pinned downloads, edition, and both templates.
    pub(super) fn load(root: &Path) -> Result<(Self, String), String> {
        let path = root.join(DECLARATION);
        let text = fs::read_to_string(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let declaration: Self = toml::from_str(&text)
            .map_err(|error| format!("{} is malformed: {error}", path.display()))?;
        if declaration.schema != SCHEMA {
            return Err(format!(
                "{} declares schema {}, expected {SCHEMA}",
                path.display(),
                declaration.schema
            ));
        }
        for (name, digest) in [
            ("tools.qemu_system", &declaration.tools.qemu_system.sha256),
            ("tools.qemu_img", &declaration.tools.qemu_img.sha256),
            ("tools.firmware", &declaration.tools.firmware.sha256),
            (
                "tools.firmware_variables",
                &declaration.tools.firmware_variables.sha256,
            ),
            ("tools.wimlib", &declaration.tools.wimlib.sha256),
            ("image", &declaration.image.sha256),
            ("drivers", &declaration.drivers.sha256),
            ("openssh", &declaration.openssh.sha256),
            ("runtime", &declaration.runtime.sha256),
        ] {
            if digest.len() != SHA256_HEX_LENGTH
                || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(format!("{name}.sha256 is not a SHA-256 digest"));
            }
        }
        let mut hasher = sha2::Sha256::new();
        hasher.update(b"spice-windows-guest-provision-v1\0");
        hasher.update(
            serde_json::to_vec(&(
                &declaration.image,
                &declaration.drivers,
                &declaration.openssh,
                &declaration.runtime,
            ))
            .map_err(|error| format!("cannot encode provisioning pins: {error}"))?,
        );
        for template in [UNATTEND_TEMPLATE, SETUP_SCRIPT] {
            let path = root.join(template);
            let bytes = fs::read(&path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
            hasher.update((bytes.len() as u64).to_le_bytes());
            hasher.update(bytes);
        }
        Ok((declaration, format!("{:x}", hasher.finalize())))
    }
}

impl Tools {
    /// Refuse unless every host tool still carries its pinned bytes.
    pub(super) fn verify(&self) -> Result<(), String> {
        for (name, pin) in [
            ("qemu-system-aarch64", &self.qemu_system),
            ("qemu-img", &self.qemu_img),
            ("UEFI firmware", &self.firmware),
            ("UEFI variable template", &self.firmware_variables),
            ("wimlib-imagex", &self.wimlib),
        ] {
            let observed = super::sha256_file(&pin.path).map_err(|error| {
                format!("{name} is unavailable ({error}); install the Homebrew bottles named in {DECLARATION}")
            })?;
            if observed != pin.sha256 {
                return Err(format!(
                    "{name} at {} has sha256 {observed}, but {DECLARATION} pins {}",
                    pin.path.display(),
                    pin.sha256
                ));
            }
        }
        Ok(())
    }
}

impl Image {
    pub(super) fn download(&self) -> Download {
        Download {
            url: self.url.clone(),
            sha256: self.sha256.clone(),
            bytes: self.bytes,
        }
    }
}

impl Drivers {
    pub(super) fn download(&self) -> Download {
        Download {
            url: self.url.clone(),
            sha256: self.sha256.clone(),
            bytes: self.bytes,
        }
    }
}

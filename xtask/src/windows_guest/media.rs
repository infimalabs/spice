//! Verified downloads and the unattended Windows Setup media.

use std::fs;
use std::io::{Seek as _, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::declaration::{Declaration, Download, SETUP_SCRIPT, UNATTEND_TEMPLATE};
use super::machine::{Paths, remove_file, remove_tree};
use super::{run_tool, sha256_file};

const CURL: &str = "/usr/bin/curl";
const DITTO: &str = "/usr/bin/ditto";
const DISKUTIL: &str = "/usr/sbin/diskutil";
const HDIUTIL: &str = "/usr/bin/hdiutil";
const SSH_KEYGEN: &str = "/usr/bin/ssh-keygen";
const PASSWORD_PLACEHOLDER: &str = "@PASSWORD@";
const VOLUME_LABEL: &str = "SPICEGUEST";
/// FAT32 cannot hold a file of 4 GiB or more.
const FAT32_FILE_LIMIT: u64 = 4 * 1024 * 1024 * 1024;
const MEDIA_SLACK: u64 = 512 * 1024 * 1024;
/// Windows ignores the partitions of a read-only MBR disk whose signature is
/// zero, so the media carries a fixed nonzero one.
const DISK_SIGNATURE: [u8; 4] = *b"SPIC";
const DISK_SIGNATURE_OFFSET: u64 = 440;

/// Verified downloads that feed one provisioning run.
pub(super) struct Inputs<'a> {
    pub image: &'a Path,
    pub drivers: &'a Path,
    pub openssh: &'a Path,
    pub runtime: &'a Path,
}

/// Return the cached download with the pinned identity, fetching it first
/// when absent.
pub(super) fn fetch(
    cache: &Path,
    name: &str,
    download: &Download,
    output: &mut impl Write,
) -> Result<PathBuf, String> {
    fs::create_dir_all(cache)
        .map_err(|error| format!("cannot create {}: {error}", cache.display()))?;
    let path = cache.join(&download.sha256);
    if path.is_file() {
        if matches(&path, download)? {
            return Ok(path);
        }
        remove_file(&path)?;
    }
    writeln!(
        output,
        "fetching {name} ({} bytes) from {}",
        download.bytes, download.url
    )
    .map_err(|error| error.to_string())?;
    let partial = cache.join(format!("{}.partial", download.sha256));
    run_tool(
        Command::new(CURL)
            .args([
                "--fail",
                "--location",
                "--silent",
                "--show-error",
                "--output",
            ])
            .arg(&partial)
            .arg(&download.url),
        "curl",
    )?;
    if !matches(&partial, download)? {
        let observed = sha256_file(&partial)?;
        remove_file(&partial)?;
        return Err(format!(
            "{name} from {} has sha256 {observed}, but the declaration pins {}",
            download.url, download.sha256
        ));
    }
    fs::rename(&partial, &path)
        .map_err(|error| format!("cannot publish {}: {error}", path.display()))?;
    Ok(path)
}

fn matches(path: &Path, download: &Download) -> Result<bool, String> {
    let bytes = fs::metadata(path)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?
        .len();
    Ok(bytes == download.bytes && sha256_file(path)? == download.sha256)
}

/// Create the host's guest SSH key once; reprovisioning keeps it.
pub(super) fn ensure_key(paths: &Paths) -> Result<(), String> {
    if paths.key().is_file() && paths.public_key().is_file() {
        return Ok(());
    }
    remove_file(&paths.key())?;
    remove_file(&paths.public_key())?;
    run_tool(
        Command::new(SSH_KEYGEN)
            .args([
                "-q",
                "-t",
                "ed25519",
                "-N",
                "",
                "-C",
                "spice-windows-guest",
                "-f",
            ])
            .arg(paths.key()),
        "ssh-keygen",
    )?;
    Ok(())
}

/// Build the FAT32 USB disk image Windows Setup installs from.
pub(super) fn build(
    declaration: &Declaration,
    paths: &Paths,
    root: &Path,
    inputs: &Inputs<'_>,
    output: &mut impl Write,
) -> Result<(), String> {
    let media = paths.media();
    remove_tree(&media)?;
    fs::create_dir_all(&media)
        .map_err(|error| format!("cannot create {}: {error}", media.display()))?;
    writeln!(
        output,
        "building Windows Setup media; recompressing the selected edition takes several minutes"
    )
    .map_err(|error| error.to_string())?;
    let wimlib = &declaration.tools.wimlib.path;
    let image = &declaration.image;
    run_tool(
        Command::new(wimlib)
            .arg("apply")
            .arg(inputs.image)
            .arg(image.setup_media_index.to_string())
            .arg(&media),
        "wimlib apply setup media",
    )?;
    let sources = media.join("sources");
    let boot = sources.join("boot.wim");
    run_tool(
        Command::new(wimlib)
            .arg("export")
            .arg(inputs.image)
            .arg(image.preinstallation_index.to_string())
            .arg(&boot)
            .arg("--compress=LZX"),
        "wimlib export Windows PE",
    )?;
    run_tool(
        Command::new(wimlib)
            .arg("export")
            .arg(inputs.image)
            .arg(image.setup_index.to_string())
            .arg(&boot)
            .arg("--boot"),
        "wimlib export Windows Setup",
    )?;
    let install = sources.join("install.esd");
    run_tool(
        Command::new(wimlib)
            .arg("export")
            .arg(inputs.image)
            .arg(image.edition_index.to_string())
            .arg(&install)
            .args(["--compress=LZMS", "--solid", "--recompress"]),
        "wimlib export Windows edition",
    )?;
    let install_bytes = fs::metadata(&install)
        .map_err(|error| format!("cannot inspect {}: {error}", install.display()))?
        .len();
    if install_bytes >= FAT32_FILE_LIMIT {
        return Err(format!(
            "the selected edition recompresses to {install_bytes} bytes, which FAT32 media cannot hold"
        ));
    }

    let payload = media.join("spice");
    let drivers = payload.join("drivers");
    for directory in ["network", "serial"] {
        fs::create_dir_all(drivers.join(directory)).map_err(|error| {
            format!(
                "cannot create {}: {error}",
                drivers.join(directory).display()
            )
        })?;
    }
    copy_drivers(
        paths,
        inputs.drivers,
        &[
            (&declaration.drivers.network, &drivers.join("network")),
            (&declaration.drivers.serial, &drivers.join("serial")),
        ],
    )?;
    copy(inputs.openssh, &payload.join("openssh.msi"))?;
    copy(inputs.runtime, &payload.join("vc_redist.x64.exe"))?;
    copy(&paths.public_key(), &payload.join("authorized_keys"))?;
    copy(&root.join(SETUP_SCRIPT), &payload.join("setup.ps1"))?;
    write_unattend(root, &media.join("autounattend.xml"))?;
    write_image(paths, &media)
}

/// Remove the media tree and image once the base is installed.
pub(super) fn remove_build(paths: &Paths) -> Result<(), String> {
    remove_tree(&paths.media())?;
    remove_file(&paths.media_image())
}

/// Copy each `(iso directory, destination)` driver set out of the virtio ISO.
fn copy_drivers(paths: &Paths, iso: &Path, sets: &[(&str, &Path)]) -> Result<(), String> {
    let mount = paths.drivers_mount();
    remove_tree(&mount)?;
    fs::create_dir_all(&mount)
        .map_err(|error| format!("cannot create {}: {error}", mount.display()))?;
    run_tool(
        // The cache names the ISO by digest alone, so hdiutil cannot infer its
        // format from an extension.
        Command::new(HDIUTIL)
            .args([
                "attach",
                "-nobrowse",
                "-readonly",
                "-imagekey",
                "diskimage-class=CRawDiskImage",
                "-mountpoint",
            ])
            .arg(&mount)
            .arg(iso),
        "hdiutil attach drivers",
    )?;
    let copied = sets
        .iter()
        .try_for_each(|(source, destination)| copy_driver_files(&mount.join(source), destination));
    let detached = run_tool(
        Command::new(HDIUTIL).arg("detach").arg(&mount),
        "hdiutil detach drivers",
    );
    copied?;
    detached?;
    remove_tree(&mount)
}

fn copy_driver_files(source: &Path, destination: &Path) -> Result<(), String> {
    let entries = fs::read_dir(source)
        .map_err(|error| format!("cannot read driver directory {}: {error}", source.display()))?;
    let mut copied = 0;
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot read {}: {error}", source.display()))?;
        let path = entry.path();
        if !path.is_file() || path.extension().is_some_and(|extension| extension == "pdb") {
            continue;
        }
        copy(&path, &destination.join(entry.file_name()))?;
        copied += 1;
    }
    if copied == 0 {
        return Err(format!("driver directory {} is empty", source.display()));
    }
    Ok(())
}

fn write_unattend(root: &Path, destination: &Path) -> Result<(), String> {
    let path = root.join(UNATTEND_TEMPLATE);
    let template = fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if !template.contains(PASSWORD_PLACEHOLDER) {
        return Err(format!(
            "{} carries no {PASSWORD_PLACEHOLDER} placeholder",
            path.display()
        ));
    }
    let mut secret = [0_u8; 16];
    getrandom::fill(&mut secret).map_err(|error| format!("cannot draw a password: {error}"))?;
    let password = secret
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    fs::write(
        destination,
        template.replace(PASSWORD_PLACEHOLDER, &password),
    )
    .map_err(|error| format!("cannot write {}: {error}", destination.display()))
}

fn write_image(paths: &Paths, media: &Path) -> Result<(), String> {
    let image = paths.media_image();
    remove_file(&image)?;
    let bytes = (tree_bytes(media)? + MEDIA_SLACK).div_ceil(1 << 20) << 20;
    fs::File::create(&image)
        .and_then(|file| file.set_len(bytes))
        .map_err(|error| format!("cannot create {}: {error}", image.display()))?;
    let attached = run_tool(
        Command::new(HDIUTIL)
            .args([
                "attach",
                "-imagekey",
                "diskimage-class=CRawDiskImage",
                "-nomount",
            ])
            .arg(&image),
        "hdiutil attach media",
    )?;
    let device = String::from_utf8_lossy(&attached.stdout)
        .split_whitespace()
        .next()
        .filter(|device| device.starts_with("/dev/disk"))
        .map(str::to_owned)
        .ok_or_else(|| "hdiutil attach named no device for the media image".to_owned())?;
    let populated = populate(&device, media);
    let detached = run_tool(
        Command::new(HDIUTIL).arg("detach").arg(&device),
        "hdiutil detach media",
    );
    populated?;
    detached?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .open(&image)
        .map_err(|error| format!("cannot open {}: {error}", image.display()))?;
    file.seek(SeekFrom::Start(DISK_SIGNATURE_OFFSET))
        .and_then(|_| file.write_all(&DISK_SIGNATURE))
        .map_err(|error| format!("cannot sign {}: {error}", image.display()))
}

fn populate(device: &str, media: &Path) -> Result<(), String> {
    run_tool(
        Command::new(DISKUTIL).args(["eraseDisk", "FAT32", VOLUME_LABEL, "MBRFormat", device]),
        "diskutil eraseDisk",
    )?;
    let info = run_tool(
        Command::new(DISKUTIL).args(["info", &format!("{device}s1")]),
        "diskutil info",
    )?;
    let mount = String::from_utf8_lossy(&info.stdout)
        .lines()
        .find_map(|line| line.trim().strip_prefix("Mount Point:").map(str::trim))
        .filter(|mount| !mount.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| format!("{device}s1 did not mount after formatting"))?;
    run_tool(
        Command::new(DITTO)
            .args(["--norsrc", "--noextattr", "--noacl"])
            .arg(media)
            .arg(&mount),
        "ditto media",
    )?;
    Ok(())
}

fn tree_bytes(path: &Path) -> Result<u64, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    if !metadata.is_dir() {
        return Ok(metadata.len());
    }
    let mut total = 0;
    for entry in
        fs::read_dir(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?
    {
        let entry = entry.map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        total += tree_bytes(&entry.path())?;
    }
    Ok(total)
}

fn copy(source: &Path, destination: &Path) -> Result<(), String> {
    fs::copy(source, destination).map(|_| ()).map_err(|error| {
        format!(
            "cannot copy {} to {}: {error}",
            source.display(),
            destination.display()
        )
    })
}

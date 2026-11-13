//! QEMU process, QMP control, disk lifecycle, and SSH readiness.

use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::declaration::{Declaration, Machine};
use super::run_tool;

const SSH: &str = "/usr/bin/ssh";
const GUEST_USER: &str = "spice";
const INSTALL_TIMEOUT: Duration = Duration::from_secs(3 * 60 * 60);
const BOOT_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const PROGRESS_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// The virtio-serial port name the guest's announcer opens as
/// `\\.\Global\org.spice.ready`; the setup script names the same port.
const READY_PORT: &str = "org.spice.ready";
/// The line the guest's announcer writes on that port once sshd listens.
const READY_LINE: &str = "ready";

/// Machine-local guest state beneath the repository's shared Spice directory.
pub(super) struct Paths {
    pub root: PathBuf,
}

impl Paths {
    pub(super) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub(super) fn cache(&self) -> PathBuf {
        self.root.join("cache")
    }

    pub(super) fn media(&self) -> PathBuf {
        self.root.join("media")
    }

    pub(super) fn media_image(&self) -> PathBuf {
        self.root.join("media.img")
    }

    pub(super) fn drivers_mount(&self) -> PathBuf {
        self.root.join("drivers-mount")
    }

    pub(super) fn base(&self) -> PathBuf {
        self.root.join("base.qcow2")
    }

    fn base_variables(&self) -> PathBuf {
        self.root.join("base-vars.fd")
    }

    pub(super) fn overlay(&self) -> PathBuf {
        self.root.join("guest.qcow2")
    }

    fn variables(&self) -> PathBuf {
        self.root.join("vars.fd")
    }

    pub(super) fn key(&self) -> PathBuf {
        self.root.join("id_ed25519")
    }

    pub(super) fn public_key(&self) -> PathBuf {
        self.root.join("id_ed25519.pub")
    }

    fn known_hosts(&self) -> PathBuf {
        self.root.join("known_hosts")
    }

    pub(super) fn record(&self) -> PathBuf {
        self.root.join("guest.json")
    }

    pub(super) fn stage(&self) -> PathBuf {
        self.root.join("stage")
    }

    fn screen(&self) -> PathBuf {
        self.root.join("screen.png")
    }

    /// Unix socket QEMU serves for the guest's serial port; the guest writes
    /// [`READY_LINE`] on it once sshd listens.
    fn ready_socket(&self) -> PathBuf {
        self.root.join("ready.sock")
    }

    /// Remove every guest artifact except verified downloads in the cache.
    pub(super) fn remove_guest(&self) -> Result<(), String> {
        for path in [
            self.base(),
            self.base_variables(),
            self.overlay(),
            self.variables(),
            self.known_hosts(),
            self.record(),
            self.media_image(),
            self.screen(),
            self.ready_socket(),
        ] {
            remove_file(&path)?;
        }
        for path in [self.media(), self.stage()] {
            remove_tree(&path)?;
        }
        Ok(())
    }
}

/// Whether SSH learns the guest's host key or requires the recorded one.
#[derive(Clone, Copy)]
pub(super) enum HostKey {
    LearnFirst,
    Pinned,
}

/// An SSH command to the guest's loopback forward; the caller appends the
/// remote command line.
pub(super) fn ssh(paths: &Paths, machine: &Machine, host_key: HostKey) -> Command {
    let mut command = Command::new(SSH);
    let checking = match host_key {
        HostKey::LearnFirst => "StrictHostKeyChecking=accept-new",
        HostKey::Pinned => "StrictHostKeyChecking=yes",
    };
    command
        .args(["-F", "/dev/null", "-i"])
        .arg(paths.key())
        .args(["-o", "IdentitiesOnly=yes", "-o", "BatchMode=yes", "-o"])
        .arg(format!(
            "UserKnownHostsFile={}",
            paths.known_hosts().display()
        ))
        .args([
            "-o",
            checking,
            "-o",
            "ConnectTimeout=10",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "LogLevel=ERROR",
            "-p",
        ])
        .arg(machine.ssh_port.to_string())
        .arg(format!("{GUEST_USER}@127.0.0.1"));
    command
}

/// Whether a QEMU process answers on the guest's QMP port.
pub(super) fn running(port: u16) -> bool {
    Qmp::connect(port).is_ok()
}

/// Run unattended Windows Setup from the provisioning media to completion.
pub(super) fn install(
    paths: &Paths,
    declaration: &Declaration,
    output: &mut impl Write,
) -> Result<(), String> {
    let machine = &declaration.machine;
    if running(machine.qmp_port) {
        return Err(format!(
            "QMP port {} is already in use; stop the running guest first",
            machine.qmp_port
        ));
    }
    run_tool(
        Command::new(&declaration.tools.qemu_img.path)
            .args(["create", "-q", "-f", "qcow2"])
            .arg(paths.base())
            .arg(format!("{}G", machine.disk_gib)),
        "qemu-img create",
    )?;
    copy_writable(
        &declaration.tools.firmware_variables.path,
        &paths.variables(),
    )?;
    let mut child = Command::new(&declaration.tools.qemu_system.path)
        .args(qemu_arguments(paths, declaration, &paths.base(), true))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .map_err(|error| format!("cannot start QEMU: {error}"))?;
    writeln!(
        output,
        "installing Windows unattended; this takes tens of minutes (screen: {})",
        paths.screen().display()
    )
    .map_err(|error| error.to_string())?;
    let started = Instant::now();
    let (exited, exits) = mpsc::channel();
    let exit = sill::ProcessExitObserver::or_ended(child.id())
        .map_err(|error| format!("cannot observe QEMU: {error}"))?;
    thread::spawn(move || {
        let _ = exited.send(exit.wait());
    });
    loop {
        match exits
            .recv_timeout(PROGRESS_INTERVAL.min(INSTALL_TIMEOUT.saturating_sub(started.elapsed())))
        {
            Ok(Ok(())) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                let status = child
                    .wait()
                    .map_err(|error| format!("cannot wait for QEMU: {error}"))?;
                return if status.success() {
                    Ok(())
                } else {
                    Err(format!("QEMU exited with {status} during installation"))
                };
            }
            Ok(Err(error)) => return Err(format!("cannot observe QEMU: {error}")),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if started.elapsed() > INSTALL_TIMEOUT {
            capture_screen(machine.qmp_port, &paths.screen());
            let _quit =
                Qmp::connect(machine.qmp_port).and_then(|mut qmp| qmp.execute("quit", None));
            let _status = child.wait();
            return Err(format!(
                "Windows Setup did not power off within {} minutes; last screen saved to {}",
                INSTALL_TIMEOUT.as_secs() / 60,
                paths.screen().display()
            ));
        }
        capture_screen(machine.qmp_port, &paths.screen());
        writeln!(
            output,
            "still installing after {} minutes",
            started.elapsed().as_secs() / 60
        )
        .map_err(|error| error.to_string())?;
    }
}

/// Freeze the installed disk and firmware variables as the recorded clean base.
pub(super) fn seal(paths: &Paths) -> Result<(), String> {
    fs::rename(paths.variables(), paths.base_variables()).map_err(|error| {
        format!(
            "cannot record clean firmware variables {}: {error}",
            paths.base_variables().display()
        )
    })?;
    for path in [paths.base(), paths.base_variables()] {
        let mut permissions = fs::metadata(&path)
            .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?
            .permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions)
            .map_err(|error| format!("cannot seal {}: {error}", path.display()))?;
    }
    Ok(())
}

/// Replace the persistent overlay and firmware variables with fresh copies of
/// the clean base.
pub(super) fn reset(paths: &Paths, declaration: &Declaration) -> Result<(), String> {
    if running(declaration.machine.qmp_port) {
        return Err("stop the guest before resetting it".to_owned());
    }
    remove_file(&paths.overlay())?;
    run_tool(
        Command::new(&declaration.tools.qemu_img.path)
            .args([
                "create",
                "-q",
                "-f",
                "qcow2",
                "-b",
                "base.qcow2",
                "-F",
                "qcow2",
            ])
            .arg(paths.overlay()),
        "qemu-img create overlay",
    )?;
    copy_writable(&paths.base_variables(), &paths.variables())
}

/// What one wait on the guest's readiness port learned.
#[derive(Debug, Eq, PartialEq)]
enum Readiness {
    /// The guest wrote [`READY_LINE`]: sshd listens.
    Announced,
    /// QEMU closed the port: the guest exited.
    Stopped,
    /// The bound expired before any announcement.
    Silent,
}

/// Block on the readiness port until the guest announces, QEMU closes it, or
/// `arm` declines to bound another read.
///
/// `arm` runs before every read and sets that read's timeout from whatever
/// time remains; it answers false once nothing remains. Lines other than the
/// announcement are firmware or driver chatter and are read past.
fn readiness(
    reader: &mut impl BufRead,
    mut arm: impl FnMut() -> bool,
) -> Result<Readiness, String> {
    let mut line = String::new();
    loop {
        if !arm() {
            return Ok(Readiness::Silent);
        }
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return Ok(Readiness::Stopped),
            Ok(_) if line.trim_end_matches(['\r', '\n']) == READY_LINE => {
                return Ok(Readiness::Announced);
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(Readiness::Silent);
            }
            Err(error) => {
                return Err(format!("cannot read the guest's readiness port: {error}"));
            }
        }
    }
}

/// Start the persistent guest in the background and wait until it announces
/// that SSH answers.
pub(super) fn boot(
    paths: &Paths,
    declaration: &Declaration,
    host_key: HostKey,
) -> Result<(), String> {
    let machine = &declaration.machine;
    let socket = paths.ready_socket();
    remove_file(&socket)?;
    run_tool(
        Command::new(&declaration.tools.qemu_system.path)
            .args(qemu_arguments(paths, declaration, &paths.overlay(), false))
            .arg("-daemonize")
            .stdin(Stdio::null()),
        "QEMU",
    )?;
    // QEMU serves the socket before it daemonizes, so the connection is
    // open long before Windows reaches its services.
    let deadline = Instant::now() + BOOT_TIMEOUT;
    let stream = UnixStream::connect(&socket).map_err(|error| {
        format!(
            "cannot open the guest's readiness socket {}: {error}",
            socket.display()
        )
    })?;
    let mut reader = BufReader::new(&stream);
    match readiness(&mut reader, || {
        let remaining = deadline.saturating_duration_since(Instant::now());
        !remaining.is_zero() && stream.set_read_timeout(Some(remaining)).is_ok()
    })? {
        Readiness::Announced => {}
        Readiness::Stopped => {
            return Err("the guest stopped before SSH became reachable".to_owned());
        }
        Readiness::Silent => {
            capture_screen(machine.qmp_port, &paths.screen());
            return Err(format!(
                "SSH did not answer within {} minutes; the guest is still running and its screen is saved to {}",
                BOOT_TIMEOUT.as_secs() / 60,
                paths.screen().display()
            ));
        }
    }
    let reachable = ssh(paths, machine, host_key)
        .args(["exit", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| format!("cannot start ssh: {error}"))?
        .success();
    if reachable {
        Ok(())
    } else {
        Err("the guest announced SSH readiness but refused the SSH connection".to_owned())
    }
}

/// Shut Windows down cleanly, forcing QEMU off only if it does not stop.
pub(super) fn stop(
    paths: &Paths,
    machine: &Machine,
    output: &mut impl Write,
) -> Result<(), String> {
    if !running(machine.qmp_port) {
        writeln!(output, "guest is stopped").map_err(|error| error.to_string())?;
        return Ok(());
    }
    // The connection may drop as Windows goes down; the QMP port is the
    // authority on whether the machine stopped.
    let _status = ssh(paths, machine, HostKey::Pinned)
        .args([r"C:\Windows\System32\shutdown.exe", "/s", "/t", "0", "/f"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if wait_stopped(machine.qmp_port, SHUTDOWN_TIMEOUT) {
        writeln!(output, "guest is stopped").map_err(|error| error.to_string())?;
        return Ok(());
    }
    let _quit = Qmp::connect(machine.qmp_port).and_then(|mut qmp| qmp.execute("quit", None));
    if wait_stopped(machine.qmp_port, Duration::from_secs(30)) {
        writeln!(
            output,
            "Windows did not shut down within {} minutes; QEMU was stopped forcibly",
            SHUTDOWN_TIMEOUT.as_secs() / 60
        )
        .map_err(|error| error.to_string())?;
        Ok(())
    } else {
        Err("QEMU did not stop".to_owned())
    }
}

/// Whether QEMU stopped within `timeout`: a connection it holds open closes
/// when it exits, so the wait is its end of the QMP stream.
fn wait_stopped(port: u16, timeout: Duration) -> bool {
    match Qmp::connect(port) {
        Ok(qmp) => qmp.closed_within(timeout) || !running(port),
        Err(_) => true,
    }
}

fn qemu_arguments(
    paths: &Paths,
    declaration: &Declaration,
    disk: &Path,
    install: bool,
) -> Vec<OsString> {
    let machine = &declaration.machine;
    let mut arguments: Vec<OsString> = [
        "-name",
        "spice-windows-guest",
        "-machine",
        "virt",
        "-accel",
        "hvf",
        "-cpu",
        "host",
        "-smp",
    ]
    .map(OsString::from)
    .into();
    arguments.push(machine.cpus.to_string().into());
    arguments.push("-m".into());
    arguments.push(machine.memory_mib.to_string().into());
    arguments.push("-drive".into());
    arguments.push(drive(
        "if=pflash,format=raw,readonly=on",
        &declaration.tools.firmware.path,
    ));
    arguments.push("-drive".into());
    arguments.push(drive("if=pflash,format=raw", &paths.variables()));
    for device in ["ramfb", "qemu-xhci", "usb-kbd", "usb-tablet"] {
        arguments.push("-device".into());
        arguments.push(device.into());
    }
    arguments.push("-drive".into());
    arguments.push(drive("if=none,id=disk,format=qcow2,discard=unmap", disk));
    arguments.push("-device".into());
    arguments.push("nvme,drive=disk,serial=spice-guest".into());
    if install {
        arguments.push("-drive".into());
        arguments.push(drive(
            "if=none,id=media,format=raw,readonly=on",
            &paths.media_image(),
        ));
        arguments.push("-device".into());
        arguments.push("usb-storage,drive=media".into());
    }
    arguments.push("-netdev".into());
    arguments.push(
        format!(
            "user,id=net,restrict=on,hostfwd=tcp:127.0.0.1:{}-:22",
            machine.ssh_port
        )
        .into(),
    );
    arguments.push("-device".into());
    arguments.push("virtio-net-pci,netdev=net".into());
    arguments.push("-qmp".into());
    arguments.push(format!("tcp:127.0.0.1:{},server=on,wait=off", machine.qmp_port).into());
    // The guest announces on a named virtio-serial port, which the pinned
    // vioserial driver exposes as \\.\Global\<name>. Windows ARM64 drives
    // neither the virt machine's PL011 (`-serial`) nor a PCI 16550. The
    // port's host end is the readiness socket.
    arguments.push("-chardev".into());
    arguments.push(with_path(
        "socket,id=ready,server=on,wait=off",
        "path",
        &paths.ready_socket(),
    ));
    arguments.push("-device".into());
    arguments.push("virtio-serial-pci".into());
    arguments.push("-device".into());
    arguments.push(format!("virtserialport,chardev=ready,name={READY_PORT}").into());
    for option in ["-display", "none", "-monitor", "none", "-serial", "none"] {
        arguments.push(option.into());
    }
    arguments
}

fn drive(options: &str, file: &Path) -> OsString {
    with_path(options, "file", file)
}

/// `options` with a `key=` path appended, escaped as QEMU option values are.
fn with_path(options: &str, key: &str, path: &Path) -> OsString {
    let mut value = OsString::from(options);
    value.push(",");
    value.push(key);
    value.push("=");
    // QEMU option values escape a literal comma by doubling it.
    value.push(path.to_string_lossy().replace(',', ",,"));
    value
}

fn capture_screen(port: u16, path: &Path) {
    let _captured = Qmp::connect(port).and_then(|mut qmp| {
        qmp.execute(
            "screendump",
            Some(json!({ "filename": path, "format": "png" })),
        )
    });
}

fn copy_writable(source: &Path, destination: &Path) -> Result<(), String> {
    remove_file(destination)?;
    fs::copy(source, destination).map_err(|error| {
        format!(
            "cannot copy {} to {}: {error}",
            source.display(),
            destination.display()
        )
    })?;
    let mut permissions = fs::metadata(destination)
        .map_err(|error| format!("cannot inspect {}: {error}", destination.display()))?
        .permissions();
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "the working firmware variables are the guest's writable NVRAM"
    )]
    permissions.set_readonly(false);
    fs::set_permissions(destination, permissions)
        .map_err(|error| format!("cannot make {} writable: {error}", destination.display()))
}

pub(super) fn remove_file(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("cannot remove {}: {error}", path.display())),
    }
}

pub(super) fn remove_tree(path: &Path) -> Result<(), String> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("cannot remove {}: {error}", path.display())),
    }
}

/// Minimal QMP client over the guest's loopback control port.
struct Qmp {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

impl Qmp {
    fn connect(port: u16) -> std::io::Result<Self> {
        let address = SocketAddr::from(([127, 0, 0, 1], port));
        let stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        let mut qmp = Self {
            reader: BufReader::new(stream.try_clone()?),
            writer: stream,
        };
        let greeting = qmp.message()?;
        if greeting.get("QMP").is_none() {
            return Err(std::io::Error::other("port does not speak QMP"));
        }
        qmp.execute("qmp_capabilities", None)?;
        Ok(qmp)
    }

    fn execute(&mut self, command: &str, arguments: Option<Value>) -> std::io::Result<Value> {
        let mut request = json!({ "execute": command });
        if let Some(arguments) = arguments {
            request["arguments"] = arguments;
        }
        let mut line = serde_json::to_vec(&request).map_err(std::io::Error::other)?;
        line.push(b'\n');
        self.writer.write_all(&line)?;
        loop {
            let message = self.message()?;
            if let Some(result) = message.get("return") {
                return Ok(result.clone());
            }
            if let Some(error) = message.get("error") {
                return Err(std::io::Error::other(error.to_string()));
            }
        }
    }

    fn message(&mut self) -> std::io::Result<Value> {
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        serde_json::from_str(&line).map_err(std::io::Error::other)
    }

    /// Whether QEMU closed this stream within `timeout`, reading past the
    /// events it emits while shutting down.
    fn closed_within(mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero()
                || self
                    .reader
                    .get_ref()
                    .set_read_timeout(Some(remaining))
                    .is_err()
            {
                return false;
            }
            match self.message() {
                Ok(_) => {}
                Err(error) => return error.kind() == std::io::ErrorKind::UnexpectedEof,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Read};

    use super::*;

    /// A port whose reads time out after its scripted bytes run out.
    struct Port {
        bytes: io::Cursor<Vec<u8>>,
        then_closes: bool,
    }

    impl Read for Port {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            match self.bytes.read(buffer)? {
                0 if self.then_closes => Ok(0),
                0 => Err(io::ErrorKind::TimedOut.into()),
                read => Ok(read),
            }
        }
    }

    fn port(bytes: &str, then_closes: bool) -> BufReader<Port> {
        BufReader::new(Port {
            bytes: io::Cursor::new(bytes.as_bytes().to_vec()),
            then_closes,
        })
    }

    #[test]
    fn the_announcement_is_found_past_earlier_chatter() {
        let mut reader = port("firmware noise\r\nready\r\nlater\r\n", false);
        assert_eq!(readiness(&mut reader, || true), Ok(Readiness::Announced));
    }

    #[test]
    fn a_closed_port_means_the_guest_stopped() {
        let mut reader = port("partial", true);
        assert_eq!(readiness(&mut reader, || true), Ok(Readiness::Stopped));
    }

    #[test]
    fn a_timed_out_read_means_the_guest_stayed_silent() {
        let mut reader = port("not yet\n", false);
        assert_eq!(readiness(&mut reader, || true), Ok(Readiness::Silent));
    }

    #[test]
    fn an_expired_bound_stops_before_another_read() {
        let mut reader = port("ready\n", false);
        assert_eq!(readiness(&mut reader, || false), Ok(Readiness::Silent));
    }

    #[test]
    fn qemu_option_paths_escape_commas() {
        assert_eq!(
            with_path("socket,id=ready", "path", Path::new("/a,b/ready.sock")),
            OsString::from("socket,id=ready,path=/a,,b/ready.sock")
        );
    }
}

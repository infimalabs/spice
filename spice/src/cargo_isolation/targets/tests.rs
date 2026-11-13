use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;

use super::*;

const WINDOWS: &str = "x86_64-pc-windows-msvc";
const WASM: &str = "wasm32-unknown-unknown";

/// A rustup-shaped sysroot shipping release 1.97.1 with both targets available.
struct Sysroot {
    root: PathBuf,
    rustc: PathBuf,
    components: RefCell<Vec<String>>,
}

impl Sysroot {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "spice-tracked-targets-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(root.join("bin")).expect("create sysroot bin");
        fs::create_dir_all(root.join("lib/rustlib")).expect("create rustlib");
        let root = fs::canonicalize(&root).expect("canonical sysroot");
        let rustc = root.join("bin/rustc");
        fs::write(&rustc, b"").expect("write rustc");
        let sysroot = Self {
            root,
            rustc,
            components: RefCell::new(Vec::new()),
        };
        sysroot.channel_manifest("1.97.1 (8bab26f4f 2026-07-14)", true);
        sysroot.write_configuration();
        sysroot
    }

    fn rustlib(&self) -> PathBuf {
        self.root.join("lib/rustlib")
    }

    fn channel_manifest(&self, version: &str, windows_available: bool) {
        fs::write(
            self.rustlib().join("multirust-channel-manifest.toml"),
            format!(
                "manifest-version = \"2\"\n\n[pkg.rustc]\nversion = {version:?}\n\n\
                 [pkg.rust-std]\nversion = {version:?}\n\n\
                 [pkg.rust-std.target.{WASM}]\navailable = true\n\n\
                 [pkg.rust-std.target.{WINDOWS}]\navailable = {windows_available}\n"
            ),
        )
        .expect("write channel manifest");
    }

    fn write_configuration(&self) {
        let mut text = String::from("config_version = \"1\"\n");
        for target in self.components.borrow().iter() {
            text.push_str(&format!(
                "\n[[components]]\npkg = \"rust-std\"\ntarget = {target:?}\nis_extension = false\n"
            ));
        }
        fs::write(self.rustlib().join("multirust-config.toml"), text)
            .expect("write component configuration");
    }

    /// Install `target`'s standard library the way rustup records it.
    fn install(&self, target: &str) {
        let library = self.rustlib().join(target).join("lib");
        fs::create_dir_all(&library).expect("create target library");
        fs::write(library.join("libstd-0123.rlib"), b"rlib").expect("write rlib");
        fs::write(
            self.rustlib().join(format!("manifest-rust-std-{target}")),
            format!("file:lib/rustlib/{target}/lib/libstd-0123.rlib\n"),
        )
        .expect("write component manifest");
        self.components.borrow_mut().push(target.to_owned());
        self.write_configuration();
    }

    fn admit(&self, rustup: &FakeRustup<'_>) -> Result<(), IsolationError> {
        admit(
            "1.97.1",
            &[WASM.to_owned(), WINDOWS.to_owned()],
            &self.rustc,
            rustup,
        )
    }
}

impl Drop for Sysroot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Rustup bound to one sysroot: it selects `selects` and installs into `sysroot`.
struct FakeRustup<'a> {
    sysroot: &'a Sysroot,
    selects: PathBuf,
    added: RefCell<Vec<Vec<String>>>,
}

impl<'a> FakeRustup<'a> {
    fn new(sysroot: &'a Sysroot) -> Self {
        Self {
            sysroot,
            selects: sysroot.rustc.clone(),
            added: RefCell::new(Vec::new()),
        }
    }
}

impl Rustup for FakeRustup<'_> {
    fn which_rustc(&self, channel: &str) -> Result<PathBuf, IsolationError> {
        assert_eq!(channel, "1.97.1");
        Ok(self.selects.clone())
    }

    fn add_targets(&self, channel: &str, targets: &[&str]) -> Result<(), IsolationError> {
        assert_eq!(channel, "1.97.1");
        for target in targets {
            self.sysroot.install(target);
        }
        self.added
            .borrow_mut()
            .push(targets.iter().map(|target| (*target).to_owned()).collect());
        Ok(())
    }
}

#[test]
fn only_missing_declared_targets_are_installed_into_the_admitted_toolchain() {
    let sysroot = Sysroot::new();
    sysroot.install(WASM);
    let rustup = FakeRustup::new(&sysroot);

    sysroot.admit(&rustup).expect("missing target materializes");
    assert_eq!(*rustup.added.borrow(), [vec![WINDOWS.to_owned()]]);

    sysroot.admit(&rustup).expect("installed targets admit");
    assert_eq!(
        rustup.added.borrow().len(),
        1,
        "a whole install is not refreshed"
    );
}

#[test]
fn an_incomplete_installed_standard_library_is_corrupt_and_not_reinstalled() {
    let sysroot = Sysroot::new();
    sysroot.install(WASM);
    sysroot.install(WINDOWS);
    fs::remove_file(sysroot.rustlib().join(WINDOWS).join("lib/libstd-0123.rlib"))
        .expect("remove recorded file");
    let rustup = FakeRustup::new(&sysroot);

    let error = sysroot.admit(&rustup).expect_err("corrupt component");
    assert_eq!(error.reason(), "cargo-isolation::tracked-target-corrupt");
    assert!(rustup.added.borrow().is_empty());
}

#[test]
fn a_sysroot_from_another_release_is_refused_before_any_install() {
    let sysroot = Sysroot::new();
    sysroot.channel_manifest("1.96.0 (0000000 2026-05-01)", true);
    sysroot.install(WASM);
    sysroot.install(WINDOWS);
    let rustup = FakeRustup::new(&sysroot);

    let error = sysroot.admit(&rustup).expect_err("wrong channel");
    assert_eq!(
        error.reason(),
        "cargo-isolation::tracked-target-wrong-channel"
    );
    assert!(rustup.added.borrow().is_empty());
}

#[test]
fn a_target_the_release_does_not_ship_is_refused_before_any_install() {
    let sysroot = Sysroot::new();
    sysroot.channel_manifest("1.97.1 (8bab26f4f 2026-07-14)", false);
    sysroot.install(WASM);
    let rustup = FakeRustup::new(&sysroot);

    let error = sysroot.admit(&rustup).expect_err("unavailable target");
    assert_eq!(
        error.reason(),
        "cargo-isolation::tracked-target-unavailable"
    );
    assert!(rustup.added.borrow().is_empty());
}

#[test]
fn rustup_selecting_another_compiler_is_refused_before_any_install() {
    let sysroot = Sysroot::new();
    sysroot.install(WASM);
    let mut rustup = FakeRustup::new(&sysroot);
    let other = sysroot.root.join("bin/other-rustc");
    fs::write(&other, b"").expect("write foreign rustc");
    rustup.selects = other;

    let error = sysroot.admit(&rustup).expect_err("foreign toolchain");
    assert_eq!(
        error.reason(),
        "cargo-isolation::tracked-target-foreign-toolchain"
    );
    assert!(rustup.added.borrow().is_empty());
}

#[test]
fn a_sysroot_without_rustup_records_is_refused() {
    let sysroot = Sysroot::new();
    fs::remove_file(sysroot.rustlib().join("multirust-config.toml"))
        .expect("remove component configuration");
    let rustup = FakeRustup::new(&sysroot);

    let error = sysroot.admit(&rustup).expect_err("no provenance");
    assert_eq!(
        error.reason(),
        "cargo-isolation::tracked-target-provenance-unavailable"
    );
    assert!(rustup.added.borrow().is_empty());
}

#[test]
fn a_target_that_is_not_one_path_component_is_refused() {
    let sysroot = Sysroot::new();
    let rustup = FakeRustup::new(&sysroot);
    let error = admit("1.97.1", &["../escape".to_owned()], &sysroot.rustc, &rustup)
        .expect_err("path-shaped target");
    assert_eq!(error.reason(), "cargo-isolation::tracked-toolchain-invalid");
}

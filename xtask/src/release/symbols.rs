use std::ffi::{CStr, CString, OsStr, OsString};
use std::fs::{File, Metadata, OpenOptions};
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use wire::Digest;
use wire::manifest::{
    MAC_SYMBOL_ARTIFACT_VERSION, MAC_SYMBOL_MEMBER_BYTES_LIMIT, MAC_SYMBOL_RAW_BYTES_LIMIT,
    MacSymbolArtifact,
};

use super::BuildError;

const INHERITED_DESCRIPTOR_MINIMUM: libc::c_int = 10;

pub(super) struct SymbolBundle {
    bundle: PathBuf,
    name: OsString,
    root: File,
    contents: File,
    resources: File,
    dwarf_root: File,
    info: SymbolMember,
    dwarf: SymbolMember,
}

impl SymbolBundle {
    pub(super) fn capture(bundle: &Path, executable: &Path) -> Result<Self, BuildError> {
        let name = executable
            .file_name()
            .ok_or_else(|| BuildError::new("linked executable has no filename"))?;
        let root = open_directory(bundle)?;
        require_entries(&root, bundle, &[OsStr::new("Contents")])?;
        let contents_path = bundle.join("Contents");
        let contents = open_directory_at(&root, OsStr::new("Contents"), &contents_path)?;
        require_entries(
            &contents,
            &contents_path,
            &[OsStr::new("Info.plist"), OsStr::new("Resources")],
        )?;
        let resources_path = contents_path.join("Resources");
        let resources = open_directory_at(&contents, OsStr::new("Resources"), &resources_path)?;
        validate_resources(&resources, &resources_path, name)?;
        let dwarf_path = resources_path.join("DWARF");
        let dwarf_root = open_directory_at(&resources, OsStr::new("DWARF"), &dwarf_path)?;
        require_entries(&dwarf_root, &dwarf_path, &[name])?;

        let info_path = contents_path.join("Info.plist");
        let info = SymbolMember::open(&contents, OsStr::new("Info.plist"), info_path)?;
        let dwarf = SymbolMember::open(&dwarf_root, name, dwarf_path.join(name))?;
        if !raw_symbol_bytes_admitted(info.identity.length, dwarf.identity.length) {
            return Err(BuildError::new(
                "dSYM bundle exceeds its fixed raw byte limit",
            ));
        }

        Ok(Self {
            bundle: bundle.to_path_buf(),
            name: name.to_os_string(),
            root,
            contents,
            resources,
            dwarf_root,
            info,
            dwarf,
        })
    }

    pub(super) fn uuid_input(&self) -> Result<InheritedMember, BuildError> {
        self.dwarf.inherit()
    }

    pub(super) fn encode(
        self,
        uuid: &str,
        executable_digest: Digest,
        commit: &str,
    ) -> Result<Vec<u8>, BuildError> {
        // Release refuses dirty checkouts before building, so the linked
        // executable always derives from the clean commit named here.
        let artifact = MacSymbolArtifact {
            version: MAC_SYMBOL_ARTIFACT_VERSION,
            uuid: uuid.to_owned(),
            executable: executable_digest,
            commit: commit.to_owned(),
            dirty: false,
            info: self.info.read()?,
            dwarf: self.dwarf.read()?,
        }
        .encode()
        .map_err(|error| BuildError::new(error.to_string()))?;
        let contents_path = self.bundle.join("Contents");
        let resources_path = contents_path.join("Resources");
        let dwarf_path = resources_path.join("DWARF");
        require_same_path(&self.bundle, &self.root)?;
        require_child(
            &self.root,
            OsStr::new("Contents"),
            &self.contents,
            &contents_path,
            true,
        )?;
        require_child(
            &self.contents,
            OsStr::new("Resources"),
            &self.resources,
            &resources_path,
            true,
        )?;
        require_child(
            &self.resources,
            OsStr::new("DWARF"),
            &self.dwarf_root,
            &dwarf_path,
            true,
        )?;
        self.info
            .require_current(&self.contents, OsStr::new("Info.plist"))?;
        self.dwarf.require_current(&self.dwarf_root, &self.name)?;
        Ok(artifact)
    }
}

pub(super) struct InheritedMember {
    _file: File,
    path: PathBuf,
}

impl InheritedMember {
    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

struct SymbolMember {
    path: PathBuf,
    file: File,
    identity: FileIdentity,
}

impl SymbolMember {
    fn open(parent: &File, name: &OsStr, path: PathBuf) -> Result<Self, BuildError> {
        let file = open_file_at(parent, name, &path)?;
        let metadata = file
            .metadata()
            .map_err(|error| path_error("inspect", &path, error))?;
        if !metadata.is_file() {
            return Err(BuildError::new(format!(
                "dSYM member {} is not a regular file",
                path.display()
            )));
        }
        let identity = FileIdentity::from_metadata(&metadata);
        if identity.length > MAC_SYMBOL_MEMBER_BYTES_LIMIT {
            return Err(BuildError::new(format!(
                "dSYM member {} exceeds its fixed byte limit",
                path.display()
            )));
        }
        Ok(Self {
            path,
            file,
            identity,
        })
    }

    fn read(&self) -> Result<Vec<u8>, BuildError> {
        let mut file = self
            .file
            .try_clone()
            .map_err(|error| path_error("duplicate", &self.path, error))?;
        let mut bytes = Vec::with_capacity(
            usize::try_from(self.identity.length).expect("bounded dSYM member length fits usize"),
        );
        use std::io::{Seek as _, SeekFrom};
        file.seek(SeekFrom::Start(0))
            .map_err(|error| path_error("seek", &self.path, error))?;
        file.by_ref()
            .take(self.identity.length.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|error| path_error("read", &self.path, error))?;
        if bytes.len() as u64 != self.identity.length
            || FileIdentity::capture(&self.file, &self.path)? != self.identity
        {
            return Err(moved(&self.path));
        }
        Ok(bytes)
    }

    fn require_current(&self, parent: &File, name: &OsStr) -> Result<(), BuildError> {
        require_child(parent, name, &self.file, &self.path, false)
    }

    fn inherit(&self) -> Result<InheritedMember, BuildError> {
        // SAFETY: F_DUPFD duplicates this live descriptor, and the successful
        // result is transferred exactly once into File below.
        let descriptor = unsafe {
            libc::fcntl(
                self.file.as_raw_fd(),
                libc::F_DUPFD,
                INHERITED_DESCRIPTOR_MINIMUM,
            )
        };
        if descriptor < 0 {
            return Err(path_error(
                "duplicate",
                &self.path,
                std::io::Error::last_os_error(),
            ));
        }
        Ok(InheritedMember {
            // SAFETY: fcntl returned this newly owned descriptor.
            _file: unsafe { File::from_raw_fd(descriptor) },
            path: PathBuf::from(format!("/dev/fd/{descriptor}")),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FileIdentity {
    length: u64,
    modified: Option<std::time::SystemTime>,
    device: u64,
    inode: u64,
}

impl FileIdentity {
    fn capture(file: &File, path: &Path) -> Result<Self, BuildError> {
        file.metadata()
            .map(|metadata| Self::from_metadata(&metadata))
            .map_err(|error| path_error("inspect", path, error))
    }

    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            length: metadata.len(),
            modified: metadata.modified().ok(),
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

fn raw_symbol_bytes_admitted(info: u64, dwarf: u64) -> bool {
    info <= MAC_SYMBOL_MEMBER_BYTES_LIMIT
        && dwarf <= MAC_SYMBOL_MEMBER_BYTES_LIMIT
        && info
            .checked_add(dwarf)
            .is_some_and(|bytes| bytes <= MAC_SYMBOL_RAW_BYTES_LIMIT)
}

fn open_directory(path: &Path) -> Result<File, BuildError> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| path_error("open", path, error))
}

fn open_directory_at(parent: &File, name: &OsStr, path: &Path) -> Result<File, BuildError> {
    open_at(
        parent,
        name,
        path,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW,
    )
}

fn open_file_at(parent: &File, name: &OsStr, path: &Path) -> Result<File, BuildError> {
    open_at(
        parent,
        name,
        path,
        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
    )
}

fn validate_resources(resources: &File, path: &Path, executable: &OsStr) -> Result<(), BuildError> {
    let mut entries = directory_entries(resources, path, 2)?;
    entries.sort();
    let has_relocations = match entries.as_slice() {
        [dwarf] if dwarf == "DWARF" => false,
        [dwarf, relocations] if dwarf == "DWARF" && relocations == "Relocations" => true,
        _ => {
            return Err(BuildError::new(format!(
                "dSYM directory {} contains {entries:?}, expected [\"DWARF\"] or \
                 [\"DWARF\", \"Relocations\"]",
                path.display()
            )));
        }
    };
    if !has_relocations {
        return Ok(());
    }

    let relocations = OsStr::new("Relocations");
    let relocations_path = path.join(relocations);
    let relocations_root = open_directory_at(resources, relocations, &relocations_path)?;
    let architectures = directory_entries(&relocations_root, &relocations_path, 1)?;
    let architecture = architectures.first().ok_or_else(|| {
        BuildError::new(format!(
            "dSYM directory {} contains no architecture",
            relocations_path.display()
        ))
    })?;
    let architecture_path = relocations_path.join(architecture);
    let architecture_root = open_directory_at(&relocations_root, architecture, &architecture_path)?;
    let mut map = executable.to_os_string();
    map.push(".yml");
    require_entries(&architecture_root, &architecture_path, &[&map])?;
    let map_path = architecture_path.join(&map);
    let map_file = open_file_at(&architecture_root, &map, &map_path)?;
    let metadata = map_file
        .metadata()
        .map_err(|error| path_error("inspect", &map_path, error))?;
    if metadata.is_file() {
        Ok(())
    } else {
        Err(BuildError::new(format!(
            "dSYM member {} is not a regular file",
            map_path.display()
        )))
    }
}

fn directory_entries(
    directory: &File,
    path: &Path,
    limit: usize,
) -> Result<Vec<OsString>, BuildError> {
    let mut directory = Directory::open(directory, path)?;
    let mut entries = Vec::with_capacity(limit);
    while let Some(name) = directory.next(path)? {
        if matches!(name.as_bytes(), b"." | b"..") {
            continue;
        }
        if entries.len() == limit {
            return Err(BuildError::new(format!(
                "dSYM directory {} exceeds its fixed entry count",
                path.display()
            )));
        }
        entries.push(name);
    }
    Ok(entries)
}

fn require_entries(directory: &File, path: &Path, expected: &[&OsStr]) -> Result<(), BuildError> {
    let mut entries = directory_entries(directory, path, expected.len())?;
    entries.sort();
    let mut expected = expected
        .iter()
        .map(|name| (*name).to_os_string())
        .collect::<Vec<_>>();
    expected.sort();
    if entries == expected {
        Ok(())
    } else {
        Err(BuildError::new(format!(
            "dSYM directory {} contains {entries:?}, expected {expected:?}",
            path.display()
        )))
    }
}

fn open_at(parent: &File, name: &OsStr, path: &Path, flags: i32) -> Result<File, BuildError> {
    let name = CString::new(name.as_bytes())
        .map_err(|_| BuildError::new(format!("dSYM path {} contains NUL", path.display())))?;
    // SAFETY: parent and name remain valid for the call, flags never create a
    // file, and a successful descriptor is transferred exactly once to File.
    let descriptor = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if descriptor < 0 {
        Err(path_error("open", path, std::io::Error::last_os_error()))
    } else {
        // SAFETY: openat returned this owned descriptor and no other owner exists.
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
}

struct Directory(*mut libc::DIR);

impl Directory {
    fn open(file: &File, path: &Path) -> Result<Self, BuildError> {
        // SAFETY: dup only reads the live descriptor and returns new ownership.
        let descriptor = unsafe { libc::dup(file.as_raw_fd()) };
        if descriptor < 0 {
            return Err(path_error(
                "duplicate",
                path,
                std::io::Error::last_os_error(),
            ));
        }
        // SAFETY: descriptor is a duplicate of an opened directory; fdopendir
        // takes ownership on success.
        let directory = unsafe { libc::fdopendir(descriptor) };
        if directory.is_null() {
            // SAFETY: fdopendir failed, so ownership remains with this scope.
            unsafe { libc::close(descriptor) };
            Err(path_error("read", path, std::io::Error::last_os_error()))
        } else {
            Ok(Self(directory))
        }
    }

    fn next(&mut self, path: &Path) -> Result<Option<OsString>, BuildError> {
        // SAFETY: errno_location returns this thread's live errno cell.
        unsafe { *errno_location() = 0 };
        // SAFETY: self owns a live DIR until Drop.
        let entry = unsafe { libc::readdir(self.0) };
        if entry.is_null() {
            // SAFETY: errno_location returns this thread's live errno cell.
            let errno = unsafe { *errno_location() };
            return if errno == 0 {
                Ok(None)
            } else {
                Err(path_error(
                    "read",
                    path,
                    std::io::Error::from_raw_os_error(errno),
                ))
            };
        }
        // SAFETY: readdir returned a live entry whose d_name is NUL-terminated
        // until the next call on this directory stream.
        let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        Ok(Some(OsString::from_vec(bytes.to_vec())))
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        // SAFETY: self uniquely owns the DIR returned by fdopendir.
        unsafe { libc::closedir(self.0) };
    }
}

#[cfg(target_os = "macos")]
fn errno_location() -> *mut libc::c_int {
    // SAFETY: libc exposes the current thread's errno pointer.
    unsafe { libc::__error() }
}

#[cfg(target_os = "linux")]
fn errno_location() -> *mut libc::c_int {
    // SAFETY: libc exposes the current thread's errno pointer.
    unsafe { libc::__errno_location() }
}

fn require_same_path(path: &Path, held: &File) -> Result<(), BuildError> {
    let reopened = open_directory(path)?;
    if FileIdentity::capture(held, path)? == FileIdentity::capture(&reopened, path)? {
        Ok(())
    } else {
        Err(moved(path))
    }
}

fn require_child(
    parent: &File,
    name: &OsStr,
    held: &File,
    path: &Path,
    directory: bool,
) -> Result<(), BuildError> {
    let reopened = if directory {
        open_directory_at(parent, name, path)?
    } else {
        open_file_at(parent, name, path)?
    };
    if FileIdentity::capture(held, path)? == FileIdentity::capture(&reopened, path)? {
        Ok(())
    } else {
        Err(moved(path))
    }
}

fn moved(path: &Path) -> BuildError {
    BuildError::new(format!(
        "dSYM member {} moved while captured",
        path.display()
    ))
}

fn path_error(operation: &str, path: &Path, error: std::io::Error) -> BuildError {
    BuildError::new(format!(
        "cannot {operation} dSYM member {}: {error}",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    use wire::Domain;

    use super::*;

    static NEXT: AtomicU64 = AtomicU64::new(0);
    const UUID: &str = "11111111-2222-3333-4444-555555555555";
    const COMMIT: &str = "1111111111111111111111111111111111111111";

    struct Fixture {
        root: PathBuf,
        executable: PathBuf,
        bundle: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "spice-release-symbols-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let executable = root.join("spice");
            let bundle = root.join("spice.dSYM");
            let dwarf = bundle.join("Contents/Resources/DWARF/spice");
            fs::create_dir_all(dwarf.parent().expect("DWARF parent")).expect("create fixture dSYM");
            fs::write(bundle.join("Contents/Info.plist"), b"fixture plist")
                .expect("write fixture plist");
            fs::write(dwarf, b"fixture DWARF").expect("write fixture DWARF");
            Self {
                root,
                executable,
                bundle,
            }
        }

        fn relocations(&self) -> PathBuf {
            self.bundle.join("Contents/Resources/Relocations")
        }

        fn add_relocation(&self, architecture: &str, name: &str) {
            let map = self.relocations().join(architecture).join(name);
            fs::create_dir_all(map.parent().expect("relocation parent"))
                .expect("create relocation architecture");
            fs::write(map, b"fixture relocation map").expect("write relocation map");
        }

        fn artifact(&self) -> Vec<u8> {
            SymbolBundle::capture(&self.bundle, &self.executable)
                .expect("capture fixture dSYM")
                .encode(
                    UUID,
                    Digest::compute(Domain::DistArtifact, b"fixture executable"),
                    COMMIT,
                )
                .expect("encode fixture symbols")
        }

        fn refuses(&self) {
            assert!(
                SymbolBundle::capture(&self.bundle, &self.executable).is_err(),
                "unexpectedly admitted {}",
                self.bundle.display()
            );
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).expect("remove symbol fixture");
        }
    }

    #[test]
    fn relocation_map_is_admitted_without_entering_the_symbol_artifact() {
        let plain = Fixture::new();
        let expected = plain.artifact();
        let relocated = Fixture::new();
        relocated.add_relocation("aarch64", "spice.yml");
        assert_eq!(relocated.artifact(), expected);
    }

    #[test]
    fn relocation_map_shape_remains_exact() {
        let empty = Fixture::new();
        fs::create_dir_all(empty.relocations()).expect("create empty relocations");
        empty.refuses();

        let multiple_architectures = Fixture::new();
        multiple_architectures.add_relocation("aarch64", "spice.yml");
        multiple_architectures.add_relocation("x86_64", "spice.yml");
        multiple_architectures.refuses();

        let wrong_name = Fixture::new();
        wrong_name.add_relocation("aarch64", "other.yml");
        wrong_name.refuses();

        let extra_map = Fixture::new();
        extra_map.add_relocation("aarch64", "spice.yml");
        extra_map.add_relocation("aarch64", "other.yml");
        extra_map.refuses();

        let directory_map = Fixture::new();
        fs::create_dir_all(
            directory_map
                .relocations()
                .join("aarch64")
                .join("spice.yml"),
        )
        .expect("create non-file relocation map");
        directory_map.refuses();
    }

    #[test]
    fn raw_symbol_size_bound_admits_its_edge_only() {
        assert!(raw_symbol_bytes_admitted(0, MAC_SYMBOL_RAW_BYTES_LIMIT));
        assert!(!raw_symbol_bytes_admitted(
            0,
            MAC_SYMBOL_RAW_BYTES_LIMIT + 1
        ));
        assert!(raw_symbol_bytes_admitted(
            MAC_SYMBOL_RAW_BYTES_LIMIT / 2,
            MAC_SYMBOL_RAW_BYTES_LIMIT - MAC_SYMBOL_RAW_BYTES_LIMIT / 2,
        ));
        assert!(!raw_symbol_bytes_admitted(
            MAC_SYMBOL_RAW_BYTES_LIMIT / 2 + 1,
            MAC_SYMBOL_RAW_BYTES_LIMIT - MAC_SYMBOL_RAW_BYTES_LIMIT / 2,
        ));
    }
}

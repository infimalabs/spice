use std::ffi::{OsStr, OsString, c_void};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::path::{Component, Path, PathBuf, Prefix};
use std::ptr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::{
    GENERIC_ALL, GENERIC_EXECUTE, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
    LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, GetSecurityInfo, SE_FILE_OBJECT, SetSecurityInfo,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_FLAGS, ACE_HEADER, ACL, ACL_REVISION, AddAccessAllowedAceEx,
    CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetLengthSid,
    GetTokenInformation, INHERIT_ONLY_ACE, InitializeAcl, InitializeSecurityDescriptor,
    OBJECT_INHERIT_ACE, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR,
    SID_AND_ATTRIBUTES, SetSecurityDescriptorControl, SetSecurityDescriptorDacl, TOKEN_GROUPS,
    TOKEN_INFORMATION_CLASS, TOKEN_OWNER, TOKEN_QUERY, TOKEN_USER, TokenGroups, TokenOwner,
    TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateDirectoryW, CreateFileW, CreateHardLinkW, DELETE, FILE_APPEND_DATA,
    FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT, FILE_BASIC_INFO, FILE_DELETE_CHILD,
    FILE_EXECUTE, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_EXECUTE,
    FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_ID_INFO, FILE_INFO_BY_HANDLE_CLASS,
    FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FILE_STANDARD_INFO, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, FileBasicInfo,
    FileIdInfo, FileStandardInfo, GetFileInformationByHandleEx, MOVEFILE_REPLACE_EXISTING,
    MOVEFILE_WRITE_THROUGH, MoveFileExW, READ_CONTROL, SYNCHRONIZE, WRITE_DAC, WRITE_OWNER,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::FSCTL_SET_REPARSE_POINT;
use windows_sys::Win32::System::SystemServices::{
    ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE, IO_REPARSE_TAG_MOUNT_POINT, SE_GROUP_ENABLED,
    SE_GROUP_OWNER, SECURITY_DESCRIPTOR_REVISION,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use super::{Entry, EntryKind, FileIdentity, FileObject, Occupancy};

/// Access mapped to one owner permission bit (`r`, `w`, or `x`).
const READ_BITS: u32 = 0o4;
const WRITE_BITS: u32 = 0o2;
const EXECUTE_BITS: u32 = 0o1;
const ALL_BITS: u32 = 0o7;

/// Rights every owner grant carries so metadata and security stay readable.
const BASE_RIGHTS: u32 = READ_CONTROL | SYNCHRONIZE | FILE_READ_ATTRIBUTES;

/// The NT object-manager prefix a junction's substitute name requires.
const NT_PREFIX: &str = r"\??\";

/// The four 16-bit name fields that precede a mount-point path buffer.
const MOUNT_POINT_HEADER_BYTES: usize = 8;

/// Windows file times count these ticks from 1601-01-01.
const TICKS_PER_SECOND: u64 = 10_000_000;
const NANOSECONDS_PER_TICK: u32 = 100;

/// The Unix epoch as a Windows file time.
const UNIX_EPOCH_TICKS: i64 = 116_444_736_000_000_000;

pub(super) fn owner_mode(path: &Path) -> io::Result<u32> {
    file_mode(&open(path, READ_CONTROL)?)
}

pub(super) fn file_mode(file: &File) -> io::Result<u32> {
    let principal = Principal::current()?;
    let descriptor = Descriptor::read(file)?;
    let mut owner = 0;
    let mut other = 0;
    if !principal.controls(descriptor.owner) {
        other = ALL_BITS;
    }
    if descriptor.dacl.is_null() {
        return Ok(0o777);
    }
    // SAFETY: a non-null DACL returned by `GetSecurityInfo` lives inside the
    // descriptor this function still owns.
    let count = unsafe { (*descriptor.dacl).AceCount };
    for index in 0..u32::from(count) {
        let mut ace: *mut c_void = ptr::null_mut();
        // SAFETY: `index` is below the ACL's own count, and `ace` receives a
        // pointer into the same owned descriptor.
        if unsafe { GetAce(descriptor.dacl, index, &raw mut ace) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: every ACE begins with an `ACE_HEADER`.
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        if u32::from(header.AceFlags) & INHERIT_ONLY_ACE != 0 {
            continue;
        }
        match u32::from(header.AceType) {
            ACCESS_DENIED_ACE_TYPE => {}
            ACCESS_ALLOWED_ACE_TYPE => {
                // SAFETY: the header names this ACE an access-allowed ACE,
                // whose SID begins at `SidStart`.
                let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
                let sid: PSID = (&raw const allowed.SidStart).cast_mut().cast();
                let granted = bits(allowed.Mask);
                // SAFETY: both SIDs are valid for the duration of the call.
                if unsafe { EqualSid(sid, principal.user()) } != 0 {
                    owner |= granted;
                } else {
                    other |= granted;
                }
            }
            _ => other = ALL_BITS,
        }
    }
    Ok(owner << 6 | other << 3 | other)
}

pub(super) fn set_owner_mode(path: &Path, mode: u32) -> io::Result<()> {
    protect(
        &open(path, READ_CONTROL | WRITE_DAC | FILE_READ_ATTRIBUTES)?,
        mode,
    )
}

pub(super) fn set_file_mode(file: &File, mode: u32) -> io::Result<()> {
    if mode & !0o700 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Windows expresses only owner-only modes, not {mode:#o}"),
        ));
    }
    protect(file, mode)
}

/// Replace the DACL of the object `file` holds open with `WRITE_DAC` by one
/// protected owner-only grant of `mode`'s owner bits.
fn protect(file: &File, mode: u32) -> io::Result<()> {
    let directory = file.metadata()?.is_dir();
    let principal = Principal::current()?;
    let acl = owner_acl(principal.user(), mode >> 6, directory)?;
    apply_protected_dacl(file, &acl)
}

/// Replace the DACL of the object `file` holds open with `WRITE_DAC` by `acl`,
/// protected from inheritance.
fn apply_protected_dacl(file: &File, acl: &[u32]) -> io::Result<()> {
    // SAFETY: the handle is open with `WRITE_DAC`, and `acl` is an initialized
    // ACL that outlives the call.
    let status = unsafe {
        SetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            acl.as_ptr().cast::<ACL>(),
            ptr::null(),
        )
    };
    win32_status(status)
}

/// Deny every principal `FILE_DELETE_CHILD` on the directory at `path` ahead
/// of its owner's full grant, the rule some synchronized folders inherit.
#[cfg(test)]
pub(super) fn deny_everyone_delete_child(path: &Path) -> io::Result<()> {
    use windows_sys::Win32::Security::{
        ACCESS_DENIED_ACE, AddAccessDeniedAceEx, CreateWellKnownSid, SECURITY_MAX_SID_SIZE,
        WinWorldSid,
    };

    let principal = Principal::current()?;
    let mut everyone = vec![0_u8; SECURITY_MAX_SID_SIZE as usize];
    let mut everyone_bytes = SECURITY_MAX_SID_SIZE;
    // SAFETY: `everyone` holds `everyone_bytes` writable bytes for the SID.
    if unsafe {
        CreateWellKnownSid(
            WinWorldSid,
            ptr::null_mut(),
            everyone.as_mut_ptr().cast(),
            &raw mut everyone_bytes,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let everyone: PSID = everyone.as_mut_ptr().cast();
    let grants = [
        (0, rights(ALL_BITS, true)),
        (
            CONTAINER_INHERIT_ACE | INHERIT_ONLY_ACE,
            rights(ALL_BITS, true),
        ),
        (
            OBJECT_INHERIT_ACE | INHERIT_ONLY_ACE,
            rights(READ_BITS | WRITE_BITS, false),
        ),
    ];
    // SAFETY: both SIDs are valid.
    let (user_bytes, world_bytes) =
        unsafe { (GetLengthSid(principal.user()), GetLengthSid(everyone)) };
    let header = size_of::<ACCESS_ALLOWED_ACE>() - size_of::<u32>();
    let acl_bytes = size_of::<ACL>() + size_of::<ACCESS_DENIED_ACE>() - size_of::<u32>()
        + world_bytes as usize
        + (header + user_bytes as usize) * grants.len();
    let mut buffer = vec![0_u32; acl_bytes.div_ceil(size_of::<u32>())];
    let acl = buffer.as_mut_ptr().cast::<ACL>();
    // SAFETY: `buffer` is DWORD-aligned and `acl_bytes` long, enough for the
    // header and every ACE added below; both SIDs stay valid.
    unsafe {
        if InitializeAcl(acl, acl_bytes as u32, ACL_REVISION) == 0
            || AddAccessDeniedAceEx(acl, ACL_REVISION, 0, FILE_DELETE_CHILD, everyone) == 0
        {
            return Err(io::Error::last_os_error());
        }
        for (flags, mask) in grants {
            if AddAccessAllowedAceEx(acl, ACL_REVISION, flags, mask, principal.user()) == 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    apply_protected_dacl(&open(path, READ_CONTROL | WRITE_DAC)?, &buffer)
}

/// The calling thread impersonating this process's token without backup and
/// restore privileges, which let an elevated administrator pass the very
/// access checks a test means to exercise. Dropping it reverts the thread.
#[cfg(test)]
pub(super) struct WithoutBackupPrivileges;

#[cfg(test)]
impl WithoutBackupPrivileges {
    pub(super) fn enter() -> io::Result<Self> {
        use windows_sys::Win32::Security::{
            AdjustTokenPrivileges, DuplicateTokenEx, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW,
            SE_PRIVILEGE_REMOVED, SecurityImpersonation, TOKEN_ADJUST_PRIVILEGES, TOKEN_DUPLICATE,
            TOKEN_IMPERSONATE, TOKEN_PRIVILEGES, TokenImpersonation,
        };
        use windows_sys::Win32::System::Threading::SetThreadToken;

        let mut process: HANDLE = ptr::null_mut();
        // SAFETY: the pseudo-handle is valid and `process` receives one handle.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_DUPLICATE, &raw mut process) } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `OpenProcessToken` returned one owned handle.
        let process = unsafe { OwnedHandle::from_raw_handle(process) };
        let mut duplicate: HANDLE = ptr::null_mut();
        // SAFETY: `process` is open for duplication; `duplicate` receives one
        // handle.
        if unsafe {
            DuplicateTokenEx(
                process.as_raw_handle(),
                TOKEN_ADJUST_PRIVILEGES | TOKEN_IMPERSONATE | TOKEN_QUERY,
                ptr::null(),
                SecurityImpersonation,
                TokenImpersonation,
                &raw mut duplicate,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `DuplicateTokenEx` returned one owned handle.
        let token = unsafe { OwnedHandle::from_raw_handle(duplicate) };
        for name in ["SeBackupPrivilege", "SeRestorePrivilege"] {
            let name = OsStr::new(name)
                .encode_wide()
                .chain([0])
                .collect::<Vec<_>>();
            let mut removal = TOKEN_PRIVILEGES {
                PrivilegeCount: 1,
                Privileges: [LUID_AND_ATTRIBUTES {
                    Luid: windows_sys::Win32::Foundation::LUID {
                        LowPart: 0,
                        HighPart: 0,
                    },
                    Attributes: SE_PRIVILEGE_REMOVED,
                }],
            };
            // SAFETY: `name` is NUL-terminated and the LUID out-pointer lives
            // in `removal`; the token is open for adjustment.
            let removed = unsafe {
                LookupPrivilegeValueW(
                    ptr::null(),
                    name.as_ptr(),
                    &raw mut removal.Privileges[0].Luid,
                ) != 0
                    && AdjustTokenPrivileges(
                        token.as_raw_handle(),
                        0,
                        &raw const removal,
                        0,
                        ptr::null_mut(),
                        ptr::null_mut(),
                    ) != 0
            };
            if !removed {
                return Err(io::Error::last_os_error());
            }
        }
        // SAFETY: a null thread names the calling thread; the token is an
        // impersonation token open for impersonation, and the thread keeps its
        // own reference once this handle closes.
        if unsafe { SetThreadToken(ptr::null(), token.as_raw_handle()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self)
    }
}

#[cfg(test)]
impl Drop for WithoutBackupPrivileges {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Threading::SetThreadToken;

        // SAFETY: a null token reverts the calling thread to its process token.
        unsafe { SetThreadToken(ptr::null(), ptr::null_mut()) };
    }
}

pub(super) fn create_private_directory(path: &Path) -> io::Result<()> {
    let mut security = ProtectedSecurity::new(ALL_BITS, true)?;
    let attributes = security.attributes();
    let path = wide(path)?;
    // SAFETY: `path` is NUL-terminated and `security` keeps every pointer in
    // `attributes` alive through the call.
    if unsafe { CreateDirectoryW(path.as_ptr(), &raw const attributes) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn rename_no_replace(source: &Path, destination: &Path) -> io::Result<()> {
    let source = wide(source)?;
    let destination = wide(destination)?;
    // SAFETY: both paths are NUL-terminated. Without
    // `MOVEFILE_REPLACE_EXISTING`, an existing destination refuses the move.
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn directory_link_form(target: &Path) -> io::Result<PathBuf> {
    drive_form(&std::path::absolute(target)?).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("a directory junction needs an absolute local target: {target:?}"),
        )
    })
}

pub(super) fn link_directory(target: &Path, link: &Path) -> io::Result<()> {
    let target = directory_link_form(target)?;
    let buffer = mount_point(&target)?;
    let wide_link = wide(link)?;
    // SAFETY: `wide_link` is NUL-terminated; the new directory inherits its
    // parent's protection.
    if unsafe { CreateDirectoryW(wide_link.as_ptr(), ptr::null()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let result = open(link, FILE_WRITE_DATA | FILE_WRITE_ATTRIBUTES).and_then(|file| {
        let mut returned = 0;
        // SAFETY: the handle is open for writing, the input buffer is a
        // complete mount-point reparse buffer of the stated length, and the
        // call completes synchronously.
        let installed = unsafe {
            DeviceIoControl(
                file.as_raw_handle(),
                FSCTL_SET_REPARSE_POINT,
                buffer.as_ptr().cast(),
                buffer.len() as u32,
                ptr::null_mut(),
                0,
                &raw mut returned,
                ptr::null_mut(),
            )
        };
        if installed == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    });
    if result.is_err() {
        let _ = fs::remove_dir(link);
    }
    result
}

pub(super) fn read_directory_link(link: &Path) -> io::Result<PathBuf> {
    let target = fs::read_link(link)?;
    Ok(drive_form(&target).unwrap_or(target))
}

pub(super) fn allocated_bytes(path: &Path, _metadata: &Metadata) -> io::Result<u64> {
    let standard: FILE_STANDARD_INFO =
        information(&open(path, FILE_READ_ATTRIBUTES)?, FileStandardInfo)?;
    Ok(u64::try_from(standard.AllocationSize).unwrap_or(0))
}

pub(super) fn occupancy(path: &Path, _metadata: &Metadata) -> io::Result<Occupancy> {
    let file = open(path, FILE_READ_ATTRIBUTES)?;
    let standard: FILE_STANDARD_INFO = information(&file, FileStandardInfo)?;
    let basic: FILE_BASIC_INFO = information(&file, FileBasicInfo)?;
    let linked = if standard.NumberOfLinks > 1 {
        let id: FILE_ID_INFO = information(&file, FileIdInfo)?;
        Some(FileObject {
            volume: id.VolumeSerialNumber,
            index: id.FileId.Identifier,
        })
    } else {
        None
    };
    Ok(Occupancy {
        allocated: u64::try_from(standard.AllocationSize).unwrap_or(0),
        linked,
        changed: instant(basic.LastWriteTime.max(basic.ChangeTime)),
    })
}

/// A file time as an instant; one before the Unix epoch reads as that epoch.
fn instant(ticks: i64) -> SystemTime {
    let Some(since) = ticks
        .checked_sub(UNIX_EPOCH_TICKS)
        .and_then(|ticks| u64::try_from(ticks).ok())
    else {
        return UNIX_EPOCH;
    };
    let nanoseconds = u32::try_from(since % TICKS_PER_SECOND).unwrap_or(0) * NANOSECONDS_PER_TICK;
    UNIX_EPOCH + Duration::new(since / TICKS_PER_SECOND, nanoseconds)
}

pub(super) fn is_link_like(metadata: &Metadata) -> bool {
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

pub(super) fn file_identity(file: &File) -> io::Result<FileIdentity> {
    let id: FILE_ID_INFO = information(file, FileIdInfo)?;
    let basic: FILE_BASIC_INFO = information(file, FileBasicInfo)?;
    let standard: FILE_STANDARD_INFO = information(file, FileStandardInfo)?;
    Ok(FileIdentity {
        object: FileObject {
            volume: id.VolumeSerialNumber,
            index: id.FileId.Identifier,
        },
        bytes: u64::try_from(standard.EndOfFile).unwrap_or(0),
        modified: i128::from(basic.LastWriteTime),
        changed: i128::from(basic.ChangeTime),
    })
}

pub(super) fn path_identity(path: &Path) -> io::Result<FileIdentity> {
    file_identity(&open(path, FILE_READ_ATTRIBUTES)?)
}

pub(super) fn owned_by_current_user(path: &Path) -> io::Result<bool> {
    let descriptor = Descriptor::read(&open(path, READ_CONTROL)?)?;
    Ok(Principal::current()?.controls(descriptor.owner))
}

pub(super) fn owner_identity(path: &Path) -> io::Result<String> {
    sid_text(Descriptor::read(&open(path, READ_CONTROL)?)?.owner)
}

pub(super) fn process_owners() -> io::Result<super::ProcessOwners> {
    let token = process_token()?;
    let user = token_information(&token, TokenUser)?;
    let owner = token_information(&token, TokenOwner)?;
    // SAFETY: each buffer holds the structure its class names, whose SID
    // points into the same allocation.
    let (user, owner) = unsafe {
        (
            (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid,
            (*owner.as_ptr().cast::<TOKEN_OWNER>()).Owner,
        )
    };
    Ok(super::ProcessOwners {
        user: sid_text(user)?,
        default_owner: sid_text(owner)?,
    })
}

/// One fixed-size `GetFileInformationByHandleEx` record for `file`.
fn information<T: Default>(file: &File, class: FILE_INFO_BY_HANDLE_CLASS) -> io::Result<T> {
    let mut record = T::default();
    // SAFETY: the handle is open, and the output buffer is exactly one `T`,
    // the record type `class` names.
    let read = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            class,
            (&raw mut record).cast(),
            size_of::<T>() as u32,
        )
    };
    if read == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(record)
}

pub(super) fn open_unfollowed(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

/// Hold a directory without delete sharing: while the handle is open, the
/// directory can be neither renamed nor removed, so its path keeps naming it.
///
/// Share modes bind only handles holding data access, so the hold lists the
/// directory; attribute access alone would leave a rename unobstructed.
pub(super) fn hold_directory(path: &Path) -> io::Result<File> {
    let directory = OpenOptions::new()
        .access_mode(FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = directory.metadata()?;
    if is_link_like(&metadata) {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            format!("a held directory cannot be a link or reparse point: {path:?}"),
        ));
    }
    if !metadata.is_dir() {
        return Err(io::Error::from(io::ErrorKind::NotADirectory));
    }
    Ok(directory)
}

pub(super) fn open_directory_entry(
    _directory: &File,
    path: &Path,
    name: &OsStr,
) -> io::Result<File> {
    hold_directory(&path.join(name))
}

/// The listing reads each entry's attributes from the directory itself, so a
/// reparse point reports as one without being opened or followed.
pub(super) struct Entries(fs::ReadDir);

pub(super) fn entries(_directory: &File, path: &Path) -> io::Result<Entries> {
    fs::read_dir(path).map(Entries)
}

impl Iterator for Entries {
    type Item = io::Result<(OsString, EntryKind)>;

    fn next(&mut self) -> Option<Self::Item> {
        let entry = match self.0.next()? {
            Ok(entry) => entry,
            Err(error) => return Some(Err(error)),
        };
        Some(entry.metadata().map(|metadata| {
            let kind = if is_link_like(&metadata) {
                EntryKind::Other
            } else if metadata.is_dir() {
                EntryKind::Directory
            } else if metadata.is_file() {
                EntryKind::File
            } else {
                EntryKind::Other
            };
            (entry.file_name(), kind)
        }))
    }
}

pub(super) fn create_private_file(path: &Path) -> io::Result<File> {
    let mut security = ProtectedSecurity::new(READ_BITS | WRITE_BITS, false)?;
    let attributes = security.attributes();
    let path_wide = wide(path)?;
    // SAFETY: `path_wide` is NUL-terminated and `security` keeps the protected
    // owner DACL alive through object creation. `CREATE_NEW` is the exclusive
    // no-replace boundary.
    let handle = unsafe {
        CreateFileW(
            path_wide.as_ptr(),
            FILE_GENERIC_READ | FILE_GENERIC_WRITE | WRITE_DAC,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            &raw const attributes,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        let error = io::Error::last_os_error();
        // An existing directory or directory link reports access denied rather
        // than the collision that CREATE_NEW reports for a regular file.
        if error.kind() == io::ErrorKind::PermissionDenied && fs::symlink_metadata(path).is_ok() {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, error));
        }
        return Err(error);
    }
    // SAFETY: `CreateFileW` returned one newly owned file handle.
    Ok(unsafe { File::from_raw_handle(handle) })
}

pub(super) fn create_private_entry(
    _directory: &File,
    path: &Path,
    name: &OsStr,
) -> io::Result<File> {
    create_private_file(&path.join(name))
}

pub(super) fn open_entry(
    _directory: &File,
    path: &Path,
    name: &OsStr,
    write: bool,
) -> io::Result<Entry> {
    let opened = OpenOptions::new()
        .read(true)
        .write(write)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path.join(name));
    let file = match opened {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Entry::Missing),
        Err(error) => return Err(error),
    };
    if is_link_like(&file.metadata()?) {
        return Ok(Entry::Link);
    }
    Ok(Entry::Opened(file))
}

pub(super) fn replace_entry(
    _directory: &File,
    path: &Path,
    source: &OsStr,
    destination: &OsStr,
) -> io::Result<()> {
    let source = wide(&path.join(source))?;
    let destination = wide(&path.join(destination))?;
    // SAFETY: both paths are NUL-terminated and outlive the call.
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn link_entry(
    _directory: &File,
    path: &Path,
    existing: &OsStr,
    new: &OsStr,
) -> io::Result<()> {
    let existing = wide(&path.join(existing))?;
    let new = wide(&path.join(new))?;
    // SAFETY: both paths are NUL-terminated and outlive the call; a null
    // security descriptor leaves the object's own protection in force.
    if unsafe { CreateHardLinkW(new.as_ptr(), existing.as_ptr(), ptr::null()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn remove_entry(_directory: &File, path: &Path, name: &OsStr) -> io::Result<()> {
    fs::remove_file(path.join(name))
}

/// Flush the held directory. `FlushFileBuffers` needs write access, which the
/// held handle deliberately lacks; the path still names the held directory.
pub(super) fn sync_directory(_directory: &File, path: &Path) -> io::Result<()> {
    OpenOptions::new()
        .write(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?
        .sync_all()
}

/// Open the entry itself, including a directory or a reparse point.
fn open(path: &Path, access: u32) -> io::Result<File> {
    OpenOptions::new()
        .access_mode(access)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

/// Security attributes whose protected owner DACL is valid for this value's
/// lifetime.
pub(crate) struct ProtectedSecurity {
    descriptor: Box<SECURITY_DESCRIPTOR>,
    _acl: Vec<u32>,
}

impl ProtectedSecurity {
    fn new(bits: u32, directory: bool) -> io::Result<Self> {
        let principal = Principal::current()?;
        Self::with_acl(owner_acl(principal.user(), bits, directory)?)
    }

    /// Security for a named kernel object that grants exactly `access` to
    /// this token's user and nothing to anyone else.
    pub(crate) fn object(access: u32) -> io::Result<Self> {
        let principal = Principal::current()?;
        Self::with_acl(grant_acl(principal.user(), &[(0, access)])?)
    }

    fn with_acl(acl: Vec<u32>) -> io::Result<Self> {
        let mut descriptor = Box::new(SECURITY_DESCRIPTOR::default());
        let raw: PSECURITY_DESCRIPTOR = (&raw mut *descriptor).cast();
        // SAFETY: `raw` addresses a writable descriptor-sized allocation, and
        // `acl` remains allocated in the returned value.
        let prepared = unsafe {
            InitializeSecurityDescriptor(raw, SECURITY_DESCRIPTOR_REVISION) != 0
                && SetSecurityDescriptorDacl(raw, 1, acl.as_ptr().cast::<ACL>(), 0) != 0
                && SetSecurityDescriptorControl(raw, SE_DACL_PROTECTED, SE_DACL_PROTECTED) != 0
        };
        if !prepared {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            descriptor,
            _acl: acl,
        })
    }

    pub(crate) fn attributes(&mut self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: (&raw mut *self.descriptor).cast(),
            bInheritHandle: 0,
        }
    }
}

/// The current token's user and groups.
struct Principal {
    user: Vec<usize>,
    groups: Vec<usize>,
}

impl Principal {
    fn current() -> io::Result<Self> {
        let token = process_token()?;
        Ok(Self {
            user: token_information(&token, TokenUser)?,
            groups: token_information(&token, TokenGroups)?,
        })
    }

    fn user(&self) -> PSID {
        // SAFETY: `user` holds a `TOKEN_USER` whose SID points into itself.
        unsafe { (*self.user.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }

    /// Whether `owner` is a SID this token may assign as an object's owner:
    /// its user, or an enabled group the token holds for ownership, such as
    /// Administrators in an elevated token. A token's default owner is always
    /// one of these, whatever default the launching runtime selected, so an
    /// object created by any process of this principal stays controlled.
    fn controls(&self, owner: PSID) -> bool {
        let assignable = (SE_GROUP_ENABLED | SE_GROUP_OWNER) as u32;
        // SAFETY: `groups` holds a `TOKEN_GROUPS` whose `GroupCount` entries
        // follow it inside the same allocation.
        let groups = unsafe {
            let groups = self.groups.as_ptr().cast::<TOKEN_GROUPS>();
            std::slice::from_raw_parts(
                (&raw const (*groups).Groups).cast::<SID_AND_ATTRIBUTES>(),
                (*groups).GroupCount as usize,
            )
        };
        // SAFETY: all SIDs are valid for the duration of the calls.
        !owner.is_null()
            && (unsafe { EqualSid(owner, self.user()) } != 0
                || groups.iter().any(|group| {
                    group.Attributes & assignable == assignable
                        && unsafe { EqualSid(owner, group.Sid) } != 0
                }))
    }
}

/// This process's token, open for query.
fn process_token() -> io::Result<OwnedHandle> {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: the pseudo-handle names this process, and `token` receives a
    // new handle that `OwnedHandle` closes.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `OpenProcessToken` succeeded, so `token` is owned here.
    Ok(unsafe { OwnedHandle::from_raw_handle(token) })
}

/// `sid` in its `S-1-…` string form.
fn sid_text(sid: PSID) -> io::Result<String> {
    let mut text = ptr::null_mut();
    // SAFETY: `sid` is a valid SID and `text` receives a `LocalAlloc` string.
    if unsafe { ConvertSidToStringSidW(sid, &raw mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the system returned a NUL-terminated wide string.
    let length = (0..)
        .take_while(|&index| unsafe { *text.add(index) } != 0)
        .count();
    // SAFETY: `length` units precede the terminator.
    let sid = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) });
    // SAFETY: the system allocated `text` with `LocalAlloc`.
    unsafe { LocalFree(text.cast()) };
    Ok(sid)
}

fn token_information(
    token: &OwnedHandle,
    class: TOKEN_INFORMATION_CLASS,
) -> io::Result<Vec<usize>> {
    let mut needed = 0;
    // SAFETY: a null buffer of length zero asks only for the required length.
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            class,
            ptr::null_mut(),
            0,
            &raw mut needed,
        );
    }
    let mut buffer = vec![0_usize; (needed as usize).div_ceil(size_of::<usize>())];
    // SAFETY: `buffer` is pointer-aligned and at least `needed` bytes long.
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            class,
            buffer.as_mut_ptr().cast(),
            needed,
            &raw mut needed,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(buffer)
}

/// An owned security descriptor with its owner and DACL pointers.
struct Descriptor {
    owner: PSID,
    dacl: *mut ACL,
    raw: PSECURITY_DESCRIPTOR,
}

impl Descriptor {
    fn read(file: &File) -> io::Result<Self> {
        let mut descriptor = Self {
            owner: ptr::null_mut(),
            dacl: ptr::null_mut(),
            raw: ptr::null_mut(),
        };
        // SAFETY: the handle is open with `READ_CONTROL`; the out-pointers
        // receive pointers into one allocation that `Drop` frees.
        let status = unsafe {
            GetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &raw mut descriptor.owner,
                ptr::null_mut(),
                &raw mut descriptor.dacl,
                ptr::null_mut(),
                &raw mut descriptor.raw,
            )
        };
        win32_status(status)?;
        Ok(descriptor)
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            // SAFETY: `GetSecurityInfo` allocated `raw` with `LocalAlloc`.
            unsafe { LocalFree(self.raw) };
        }
    }
}

/// A protected owner-only DACL granting `bits` to `user`.
///
/// A directory also carries inherit-only grants: subdirectories receive
/// `rwx` and files receive `rw-`, the equivalent of a `077` umask.
fn owner_acl(user: PSID, bits: u32, directory: bool) -> io::Result<Vec<u32>> {
    let object = (0, rights(bits & ALL_BITS, directory));
    let entries: &[(ACE_FLAGS, u32)] = if directory {
        &[
            object,
            (
                CONTAINER_INHERIT_ACE | INHERIT_ONLY_ACE,
                rights(ALL_BITS, true),
            ),
            (
                OBJECT_INHERIT_ACE | INHERIT_ONLY_ACE,
                rights(READ_BITS | WRITE_BITS, false),
            ),
        ]
    } else {
        &[object]
    };
    grant_acl(user, entries)
}

/// A DACL whose only entries allow `user` each flagged access mask.
fn grant_acl(user: PSID, entries: &[(ACE_FLAGS, u32)]) -> io::Result<Vec<u32>> {
    // SAFETY: `user` is a valid SID.
    let sid_bytes = unsafe { GetLengthSid(user) } as usize;
    let ace_bytes = size_of::<ACCESS_ALLOWED_ACE>() - size_of::<u32>() + sid_bytes;
    let acl_bytes = size_of::<ACL>() + ace_bytes * entries.len();
    let mut buffer = vec![0_u32; acl_bytes.div_ceil(size_of::<u32>())];
    let acl = buffer.as_mut_ptr().cast::<ACL>();
    // SAFETY: `buffer` is DWORD-aligned and `acl_bytes` long, enough for the
    // header and every ACE added below.
    if unsafe { InitializeAcl(acl, acl_bytes as u32, ACL_REVISION) } == 0 {
        return Err(io::Error::last_os_error());
    }
    for &(flags, mask) in entries {
        // SAFETY: as above; `user` stays valid for the call.
        if unsafe { AddAccessAllowedAceEx(acl, ACL_REVISION, flags, mask, user) } == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(buffer)
}

/// Rights for owner permission `bits`.
///
/// Write includes `DELETE`. Windows lets a rename or replacement remove an
/// entry through the entry's own `DELETE` grant or its parent's
/// `FILE_DELETE_CHILD`, and an inherited deny can close the parent route, so a
/// private object the owner may write must also be one the owner may replace.
fn rights(bits: u32, directory: bool) -> u32 {
    let mut mask = BASE_RIGHTS;
    if bits & READ_BITS != 0 {
        mask |= FILE_GENERIC_READ;
    }
    if bits & WRITE_BITS != 0 {
        mask |= FILE_GENERIC_WRITE | DELETE;
        if directory {
            mask |= FILE_DELETE_CHILD;
        }
    }
    if bits & EXECUTE_BITS != 0 {
        mask |= FILE_GENERIC_EXECUTE;
    }
    mask
}

/// Permission bits one grant confers, counting any right that can alter the
/// object or its protection as write.
fn bits(mask: u32) -> u32 {
    let mut bits = 0;
    if mask & (GENERIC_ALL | GENERIC_READ | FILE_READ_DATA) != 0 {
        bits |= READ_BITS;
    }
    if mask
        & (GENERIC_ALL
            | GENERIC_WRITE
            | FILE_WRITE_DATA
            | FILE_APPEND_DATA
            | FILE_DELETE_CHILD
            | DELETE
            | WRITE_DAC
            | WRITE_OWNER)
        != 0
    {
        bits |= WRITE_BITS;
    }
    if mask & (GENERIC_ALL | GENERIC_EXECUTE | FILE_EXECUTE) != 0 {
        bits |= EXECUTE_BITS;
    }
    bits
}

/// A complete `IO_REPARSE_TAG_MOUNT_POINT` reparse buffer naming `target`.
fn mount_point(target: &Path) -> io::Result<Vec<u8>> {
    let print = target.as_os_str().encode_wide().collect::<Vec<_>>();
    let substitute = OsStr::new(NT_PREFIX)
        .encode_wide()
        .chain(print.iter().copied())
        .collect::<Vec<_>>();
    let unit = size_of::<u16>();
    let substitute_bytes = substitute.len() * unit;
    let print_bytes = print.len() * unit;
    let names = substitute_bytes + unit + print_bytes + unit;
    let too_long = || io::Error::new(io::ErrorKind::InvalidInput, "junction target is too long");
    let data = u16::try_from(MOUNT_POINT_HEADER_BYTES + names).map_err(|_| too_long())?;
    let substitute_length = u16::try_from(substitute_bytes).map_err(|_| too_long())?;
    let print_offset = u16::try_from(substitute_bytes + unit).map_err(|_| too_long())?;
    let print_length = u16::try_from(print_bytes).map_err(|_| too_long())?;
    let mut buffer = Vec::with_capacity(MOUNT_POINT_HEADER_BYTES + usize::from(data));
    buffer.extend_from_slice(&IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
    buffer.extend_from_slice(&data.to_le_bytes());
    buffer.extend_from_slice(&0_u16.to_le_bytes());
    for field in [0, substitute_length, print_offset, print_length] {
        buffer.extend_from_slice(&field.to_le_bytes());
    }
    for name in [&substitute, &print] {
        for unit in name.iter().chain([&0]) {
            buffer.extend_from_slice(&unit.to_le_bytes());
        }
    }
    Ok(buffer)
}

/// `path` as a `C:\...` drive path, or `None` for any other prefix.
fn drive_form(path: &Path) -> Option<PathBuf> {
    match path.components().next()? {
        Component::Prefix(prefix) => match prefix.kind() {
            Prefix::Disk(_) => Some(path.to_path_buf()),
            Prefix::VerbatimDisk(_) => {
                let units = path.as_os_str().encode_wide().skip(4).collect::<Vec<_>>();
                Some(PathBuf::from(OsString::from_wide(&units)))
            }
            _ => None,
        },
        _ => None,
    }
}

/// A NUL-terminated extended-length form of `path` for direct Win32 calls.
fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let absolute = std::path::absolute(path)?;
    let text = absolute.as_os_str().encode_wide().collect::<Vec<_>>();
    let verbatim = OsStr::new(r"\\?\").encode_wide().collect::<Vec<_>>();
    let mut converted = match absolute.components().next() {
        Some(Component::Prefix(prefix)) if prefix.kind().is_verbatim() => text,
        Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::UNC(..)) => verbatim
            .into_iter()
            .chain(OsStr::new("UNC").encode_wide())
            .chain(text.into_iter().skip(1))
            .collect(),
        _ => verbatim.into_iter().chain(text).collect(),
    };
    if converted.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains NUL",
        ));
    }
    converted.push(0);
    Ok(converted)
}

fn win32_status(status: u32) -> io::Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(status as i32))
    }
}

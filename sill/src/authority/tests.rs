use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use super::{
    Directory, Entry, EntryKind, create_private_directories, create_private_directory,
    create_private_file, directory_link_form, file_identity, file_mode, is_link_like,
    link_directory, open_unfollowed, owned_by_current_user, owner_mode, path_identity,
    read_directory_link, rename_no_replace, set_file_mode, set_owner_mode,
};

struct Scratch(PathBuf);

impl Scratch {
    fn new(stem: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock follows the epoch")
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("sill-authority-{stem}-{}-{nanos}", process::id()));
        create_private_directory(&root).expect("create scratch root");
        Self(root)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        reopen(&self.0);
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn reopen(path: &Path) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if is_link_like(&metadata) {
        return;
    }
    if metadata.is_dir() {
        let _ = set_owner_mode(path, 0o700);
        for entry in fs::read_dir(path).into_iter().flatten().flatten() {
            reopen(&entry.path());
        }
    } else {
        let _ = set_owner_mode(path, 0o600);
    }
}

#[test]
fn no_replace_rename_refuses_any_existing_destination() {
    let scratch = Scratch::new("rename");
    let source = scratch.path("source");
    let file = scratch.path("file");
    let directory = scratch.path("directory");
    fs::create_dir(&source).expect("create source");
    fs::write(&file, b"kept").expect("create file destination");
    fs::create_dir(&directory).expect("create empty directory destination");

    for destination in [&file, &directory] {
        let refused = rename_no_replace(&source, destination).expect_err("destination exists");
        assert_eq!(refused.kind(), io::ErrorKind::AlreadyExists);
    }
    assert!(source.is_dir(), "a refused rename leaves the source");
    assert_eq!(fs::read(&file).expect("read file"), b"kept");

    let published = scratch.path("published");
    rename_no_replace(&source, &published).expect("absent destination publishes");
    assert!(published.is_dir());
    assert!(!source.exists());
}

#[test]
fn private_directories_are_owner_only_and_never_reused() {
    let scratch = Scratch::new("private");
    let directory = scratch.path("private");
    create_private_directory(&directory).expect("create private directory");
    assert_eq!(owner_mode(&directory).expect("read mode"), 0o700);

    let again = create_private_directory(&directory).expect_err("name exists");
    assert_eq!(again.kind(), io::ErrorKind::AlreadyExists);
    let file = scratch.path("file");
    fs::write(&file, b"").expect("create file");
    let over_file = create_private_directory(&file).expect_err("name exists");
    assert_eq!(over_file.kind(), io::ErrorKind::AlreadyExists);
}

#[test]
fn private_directory_trees_create_every_missing_level_owner_only() {
    let scratch = Scratch::new("private-tree");
    let existing = scratch.path("existing");
    fs::create_dir(&existing).expect("create existing level");
    let before = owner_mode(&existing).expect("existing mode");
    let leaf = existing.join("a").join("b");

    create_private_directories(&leaf).expect("create missing levels");
    for level in [existing.join("a"), leaf.clone()] {
        assert_eq!(owner_mode(&level).expect("level mode"), 0o700);
    }
    assert_eq!(
        owner_mode(&existing).expect("existing mode"),
        before,
        "an existing level is kept as it is"
    );
    create_private_directories(&leaf).expect("an existing tree is kept");
}

#[test]
fn private_files_are_owner_only_from_creation_and_never_reuse_an_entry() {
    let scratch = Scratch::new("private-file");
    fs::create_dir(scratch.path("outside")).expect("create outside");
    link_directory(&scratch.path("outside"), &scratch.path("link")).expect("create link");
    let path = scratch.path("file");

    let mut created = create_private_file(&path).expect("create private file");
    assert_eq!(owner_mode(&path).expect("created mode"), 0o600);
    io::Write::write_all(&mut created, b"secret").expect("write private file");
    drop(created);
    for existing in [&path, &scratch.path("link")] {
        let refused = create_private_file(existing).expect_err("existing entry refused");
        assert_eq!(refused.kind(), io::ErrorKind::AlreadyExists, "{existing:?}");
    }
    assert_eq!(fs::read(&path).expect("read private file"), b"secret");
    assert!(
        fs::read_dir(scratch.path("outside"))
            .expect("list outside")
            .next()
            .is_none()
    );
}

#[test]
fn owner_modes_round_trip_and_refuse_shared_access() {
    let scratch = Scratch::new("modes");
    let directory = scratch.path("directory");
    let file = scratch.path("file");
    fs::create_dir(&directory).expect("create directory");
    fs::write(&file, b"bytes").expect("create file");

    for mode in [0o700, 0o500] {
        set_owner_mode(&directory, mode).expect("set directory mode");
        assert_eq!(owner_mode(&directory).expect("read mode"), mode);
    }
    for mode in [0o700, 0o600, 0o500, 0o400] {
        set_owner_mode(&file, mode).expect("set file mode");
        assert_eq!(owner_mode(&file).expect("read mode"), mode);
    }
    for shared in [0o755, 0o640, 0o4700] {
        let refused = set_owner_mode(&file, shared).expect_err("shared mode refused");
        assert_eq!(refused.kind(), io::ErrorKind::InvalidInput);
    }
    assert_eq!(owner_mode(&file).expect("read mode"), 0o400);
}

#[test]
fn a_sealed_directory_refuses_new_entries() {
    let scratch = Scratch::new("sealed");
    #[cfg(windows)]
    let _unprivileged =
        super::windows::WithoutBackupPrivileges::enter().expect("drop backup privileges");
    let directory = scratch.path("sealed");
    create_private_directory(&directory).expect("create directory");
    fs::write(directory.join("kept"), b"kept").expect("create entry");
    set_owner_mode(&directory.join("kept"), 0o400).expect("seal entry");
    set_owner_mode(&directory, 0o500).expect("seal directory");

    let refused = fs::write(directory.join("added"), b"").expect_err("sealed directory");
    assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
    assert!(fs::remove_file(directory.join("kept")).is_err());
    assert_eq!(
        fs::read(directory.join("kept")).expect("read entry"),
        b"kept"
    );
}

#[test]
fn directory_links_record_their_target_and_read_as_links() {
    let scratch = Scratch::new("link");
    let target = scratch.path("target");
    let link = scratch.path("link");
    fs::create_dir(&target).expect("create target");
    fs::write(target.join("entry"), b"through").expect("create entry");

    link_directory(&target, &link).expect("create directory link");
    assert_eq!(
        read_directory_link(&link).expect("read link"),
        directory_link_form(&target).expect("link form")
    );
    assert!(is_link_like(
        &fs::symlink_metadata(&link).expect("link metadata")
    ));
    assert!(!is_link_like(
        &fs::symlink_metadata(&target).expect("target metadata")
    ));
    assert_eq!(
        fs::read(link.join("entry")).expect("read through link"),
        b"through"
    );

    let occupied = link_directory(&target, &link).expect_err("link name exists");
    assert_eq!(occupied.kind(), io::ErrorKind::AlreadyExists);
}

#[test]
fn identity_tracks_one_object_and_detects_change_and_replacement() {
    let scratch = Scratch::new("identity");
    let path = scratch.path("file");
    fs::write(&path, b"first").expect("create file");
    let observed = path_identity(&path).expect("path identity");
    let opened = file_identity(&fs::File::open(&path).expect("open file")).expect("identity");
    assert_eq!(
        observed, opened,
        "path and handle observe one unchanged object"
    );
    assert_eq!(observed.bytes(), 5);

    fs::write(&path, b"changed").expect("rewrite file");
    let changed = path_identity(&path).expect("changed identity");
    assert_ne!(changed, observed, "a write changes the observed state");
    assert_eq!(
        changed.object(),
        observed.object(),
        "a write keeps the object"
    );

    let replacement = scratch.path("replacement");
    fs::write(&replacement, b"changed").expect("create replacement");
    fs::rename(&replacement, &path).expect("replace file");
    let replaced = path_identity(&path).expect("replaced identity");
    assert!(
        replaced.object() != changed.object(),
        "an equal-length replacement is another object"
    );
    assert!(owned_by_current_user(&path).expect("observe owner"));
}

#[test]
fn identity_and_reads_never_follow_a_final_link() {
    let scratch = Scratch::new("identity-link");
    let target = scratch.path("target");
    let link = scratch.path("link");
    fs::create_dir(&target).expect("create target");
    link_directory(&target, &link).expect("create link");

    let through = path_identity(&link).expect("link identity");
    let direct = path_identity(&target).expect("target identity");
    assert_ne!(through.object(), direct.object());
    assert!(owned_by_current_user(&link).expect("observe link owner"));
    if let Ok(opened) = open_unfollowed(&link) {
        let opened = file_identity(&opened).expect("opened identity");
        assert_ne!(
            opened.object(),
            direct.object(),
            "the open reached the target"
        );
    }
}

#[test]
fn held_directory_entries_are_private_exclusive_and_never_followed() {
    let scratch = Scratch::new("held");
    let root = scratch.path("root");
    fs::create_dir(&root).expect("create root");
    fs::create_dir(scratch.path("outside")).expect("create outside");
    link_directory(&scratch.path("outside"), &root.join("link")).expect("create link");
    let held = Directory::open(&root).expect("hold directory");

    let mut staged = held
        .create_private("staged".as_ref())
        .expect("create staged");
    io::Write::write_all(&mut staged, b"next").expect("write staged");
    drop(staged);
    assert_eq!(
        owner_mode(&root.join("staged")).expect("staged mode"),
        0o600
    );
    for existing in ["staged", "link"] {
        let refused = held
            .create_private(existing.as_ref())
            .expect_err("creation refuses an existing entry");
        assert_eq!(refused.kind(), io::ErrorKind::AlreadyExists, "{existing}");
    }
    assert!(
        fs::read_dir(scratch.path("outside"))
            .expect("list outside")
            .next()
            .is_none()
    );

    assert!(matches!(
        held.open_entry("absent".as_ref(), false).expect("absent"),
        Entry::Missing
    ));
    assert!(matches!(
        held.open_entry("link".as_ref(), true).expect("link"),
        Entry::Link
    ));

    fs::write(root.join("current"), b"before").expect("create current");
    held.replace("staged".as_ref(), "current".as_ref())
        .expect("replace current");
    held.sync().expect("sync held directory");
    let Entry::Opened(mut current) = held.open_entry("current".as_ref(), false).expect("current")
    else {
        panic!("the replaced entry opens as itself");
    };
    let mut bytes = Vec::new();
    io::Read::read_to_end(&mut current, &mut bytes).expect("read current");
    assert_eq!(bytes, b"next");
    assert!(!root.join("staged").exists());

    drop(current);
    held.remove_file("current".as_ref())
        .expect("remove current");
    assert!(matches!(
        held.open_entry("current".as_ref(), false).expect("removed"),
        Entry::Missing
    ));
    for invalid in ["", ".", "..", "a/b"] {
        let refused = held
            .open_entry(invalid.as_ref(), false)
            .expect_err("not one entry name");
        assert_eq!(refused.kind(), io::ErrorKind::InvalidInput, "{invalid:?}");
    }
}

#[test]
fn held_directory_operations_never_land_in_a_directory_that_took_its_name() {
    for renamed in ["held", "parent"] {
        let scratch = Scratch::new("held-rename");
        let parent = scratch.path("parent");
        let held_path = parent.join("held");
        fs::create_dir_all(&held_path).expect("create held directory");
        let held = Directory::open(&held_path).expect("hold directory");
        let object = held.identity().expect("held identity").object();

        let (from, to) = match renamed {
            "held" => (held_path.clone(), parent.join("moved")),
            _ => (parent.clone(), scratch.path("moved")),
        };
        let moved_held = if renamed == "held" {
            to.clone()
        } else {
            to.join("held")
        };
        if fs::rename(&from, &to).is_ok() {
            fs::create_dir_all(&held_path).expect("create impostor");
        }
        drop(held.create_private("entry".as_ref()).expect("create entry"));

        let holder = [&held_path, &moved_held]
            .into_iter()
            .find(|path| path_identity(path).is_ok_and(|identity| identity.object() == object))
            .expect("the held directory is still named");
        assert!(holder.join("entry").is_file(), "{renamed}");
        let other = if holder == &held_path {
            &moved_held
        } else {
            &held_path
        };
        assert!(!other.join("entry").exists(), "{renamed}");
    }
}

#[test]
fn held_directories_refuse_links_and_files() {
    let scratch = Scratch::new("held-refusal");
    fs::create_dir(scratch.path("target")).expect("create target");
    link_directory(&scratch.path("target"), &scratch.path("link")).expect("create link");
    fs::write(scratch.path("file"), b"").expect("create file");
    for refused in ["link", "file"] {
        let error = Directory::open(&scratch.path(refused)).expect_err("not held");
        assert_eq!(error.kind(), io::ErrorKind::NotADirectory, "{refused}");
    }
}

#[test]
fn held_directories_list_and_descend_without_following_links() {
    let scratch = Scratch::new("held-listing");
    let root = scratch.path("root");
    fs::create_dir_all(root.join("child")).expect("create child");
    fs::create_dir(scratch.path("outside")).expect("create outside");
    fs::write(root.join("file"), b"").expect("create file");
    fs::write(root.join("child").join("nested"), b"nested").expect("create nested");
    link_directory(&scratch.path("outside"), &root.join("link")).expect("create link");
    let held = Directory::open(&root).expect("hold root");

    let mut listed = held
        .entries()
        .expect("list root")
        .collect::<io::Result<Vec<_>>>()
        .expect("list every entry");
    listed.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(
        listed,
        [
            ("child".into(), EntryKind::Directory),
            ("file".into(), EntryKind::File),
            ("link".into(), EntryKind::Other),
        ]
    );

    let child = held.open_directory("child".as_ref()).expect("hold child");
    assert_eq!(child.path(), root.join("child"));
    let Entry::Opened(mut nested) = child.open_entry("nested".as_ref(), false).expect("nested")
    else {
        panic!("the nested file opens");
    };
    let mut bytes = Vec::new();
    io::Read::read_to_end(&mut nested, &mut bytes).expect("read nested");
    assert_eq!(bytes, b"nested");
    for refused in ["link", "file"] {
        let error = held
            .open_directory(refused.as_ref())
            .expect_err("only a real directory is held");
        assert_eq!(error.kind(), io::ErrorKind::NotADirectory, "{refused}");
    }
    let missing = held
        .open_directory("missing".as_ref())
        .expect_err("nothing is named");
    assert_eq!(missing.kind(), io::ErrorKind::NotFound);
}

#[test]
fn file_modes_round_trip_through_the_open_handle() {
    let scratch = Scratch::new("file-mode");
    let path = scratch.path("file");
    let created = create_private_file(&path).expect("create private file");
    assert_eq!(file_mode(&created).expect("created mode"), 0o600);
    for mode in [0o400, 0o700, 0o600] {
        set_file_mode(&created, mode).expect("owner-only modes cross every platform");
        assert_eq!(file_mode(&created).expect("handle mode"), mode);
    }
    let outside = set_file_mode(&created, 0o20000).expect_err("outside the POSIX mask");
    assert_eq!(outside.kind(), io::ErrorKind::InvalidInput);

    let shared = set_file_mode(&created, 0o644);
    if cfg!(windows) {
        let refused = shared.expect_err("Windows refuses a mode it cannot express");
        assert_eq!(refused.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(file_mode(&created).expect("unchanged mode"), 0o600);
    } else {
        shared.expect("Unix applies the exact mode");
        assert_eq!(file_mode(&created).expect("shared mode"), 0o644);
    }
    drop(created);
    set_owner_mode(&path, 0o400).expect("seal file");
    let reopened = open_unfollowed(&path).expect("open sealed file");
    assert_eq!(file_mode(&reopened).expect("sealed mode"), 0o400);
    drop(reopened);
    set_owner_mode(&path, 0o600).expect("unseal file");
}

#[cfg(windows)]
#[test]
fn entries_created_in_a_private_directory_are_private() {
    let scratch = Scratch::new("inherit");
    let file = scratch.path("file");
    let directory = scratch.path("directory");
    fs::write(&file, b"").expect("create file");
    fs::create_dir(&directory).expect("create directory");
    fs::write(directory.join("nested"), b"").expect("create nested file");

    assert_eq!(owner_mode(&file).expect("file mode"), 0o600);
    assert_eq!(owner_mode(&directory).expect("directory mode"), 0o700);
    assert_eq!(
        owner_mode(&directory.join("nested")).expect("nested mode"),
        0o600
    );
}

#[cfg(windows)]
#[test]
fn junction_targets_must_be_local_drive_paths() {
    let scratch = Scratch::new("junction");
    let refused = link_directory(Path::new(r"\\server\share\x"), &scratch.path("link"))
        .expect_err("network target refused");
    assert_eq!(refused.kind(), io::ErrorKind::InvalidInput);
    assert!(!scratch.path("link").exists());
}

/// A parent denying every principal `FILE_DELETE_CHILD` leaves replacement to
/// the destination's and source's own `DELETE` grants, which every private
/// file carries, through both the path and the held-directory routes.
#[cfg(windows)]
#[test]
fn private_files_replace_beneath_a_parent_denying_delete_child() {
    use super::windows::{WithoutBackupPrivileges, deny_everyone_delete_child};

    let scratch = Scratch::new("replace-denied");
    let _unprivileged = WithoutBackupPrivileges::enter().expect("drop backup privileges");
    let parent = scratch.path("parent");
    create_private_directory(&parent).expect("create parent");
    deny_everyone_delete_child(&parent).expect("deny delete-child");

    let destination = parent.join("destination");
    drop(create_private_file(&destination).expect("create destination"));
    let staged = parent.join("staged");
    drop(create_private_file(&staged).expect("create staged"));
    fs::rename(&staged, &destination).expect("replace destination");
    assert_eq!(owner_mode(&destination).expect("destination mode"), 0o600);

    drop(create_private_file(&staged).expect("create staged for held route"));
    let held = Directory::open(&parent).expect("hold parent");
    held.replace(
        std::ffi::OsStr::new("staged"),
        std::ffi::OsStr::new("destination"),
    )
    .expect("held replacement");
    assert!(!staged.exists());
}

#[cfg(windows)]
#[test]
fn verbatim_and_drive_targets_record_one_form() {
    let scratch = Scratch::new("verbatim");
    let target = scratch.path("target");
    fs::create_dir(&target).expect("create target");
    let verbatim = fs::canonicalize(&target).expect("verbatim target");
    assert!(verbatim.as_os_str().to_string_lossy().starts_with(r"\\?\"));
    let link = scratch.path("link");

    link_directory(&verbatim, &link).expect("link verbatim target");
    let recorded = read_directory_link(&link).expect("read link");
    assert_eq!(recorded, directory_link_form(&target).expect("drive form"));
    assert_eq!(
        recorded,
        directory_link_form(&verbatim).expect("verbatim form")
    );
}

#[test]
fn created_entries_belong_to_the_process_default_owner() {
    use super::{owner_identity, process_owners};

    let scratch = Scratch::new("default-owner");
    let file = scratch.path("file");
    let directory = scratch.path("directory");
    fs::write(&file, b"").expect("create file");
    fs::create_dir(&directory).expect("create directory");

    let owners = process_owners().expect("read process owners");
    assert!(!owners.user.is_empty(), "{owners:?}");
    for path in [&file, &directory] {
        assert_eq!(
            owner_identity(path).expect("read owner"),
            owners.default_owner,
            "{path:?}"
        );
    }
}

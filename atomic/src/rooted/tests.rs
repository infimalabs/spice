use std::fs;
use std::path::Path;

use super::{RootedFiles, RootedPublish};
use crate::tests::TestDirectory;
use crate::{Error, Refusal};

const DATA_MODE: u32 = 0o644;
const EXECUTABLE_MODE: u32 = 0o755;

#[test]
fn publication_creates_the_missing_tree_with_exact_bytes_and_modes() {
    let directory = TestDirectory::new();
    let root = directory.path().join("stage");
    let mut files = RootedFiles::new(&root);

    files
        .publish(Path::new("objects/nested/tool"), EXECUTABLE_MODE, b"exec")
        .unwrap();
    files
        .publish(Path::new("objects/nested/data"), DATA_MODE, b"old")
        .unwrap();
    files
        .publish(Path::new("objects/nested/data"), DATA_MODE, b"new")
        .unwrap();

    assert_eq!(fs::read(root.join("objects/nested/tool")).unwrap(), b"exec");
    assert_eq!(fs::read(root.join("objects/nested/data")).unwrap(), b"new");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        let mode = |name: &str| {
            fs::metadata(root.join("objects/nested").join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777
        };
        assert_eq!(mode("tool"), EXECUTABLE_MODE);
        assert_eq!(mode("data"), DATA_MODE);
    }
    let mut names = fs::read_dir(root.join("objects/nested"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, ["data", "tool"]);
}

#[test]
fn paths_that_are_not_wholly_beneath_the_root_are_refused_before_mutation() {
    let directory = TestDirectory::new();
    let root = directory.path().join("stage");
    let mut files = RootedFiles::new(&root);

    for path in [
        "",
        "../escape",
        "/escape",
        "./escape",
        "nested/../../escape",
    ] {
        assert!(
            matches!(
                files.publish(Path::new(path), DATA_MODE, b"escape"),
                Err(Error::Refused(Refusal::PathNotRooted { path: refused }))
                    if refused == Path::new(path)
            ),
            "admitted {path:?}"
        );
    }
    assert!(directory.entries().is_empty());
}

#[test]
fn a_file_in_the_ancestry_is_refused_and_left_intact() {
    let directory = TestDirectory::new();
    fs::write(directory.path().join("objects"), b"not a directory").unwrap();
    let mut files = RootedFiles::new(directory.path());

    assert!(matches!(
        files.publish(Path::new("objects/artifact"), DATA_MODE, b"bytes"),
        Err(Error::Refused(Refusal::AncestorNotDirectory { path }))
            if path == directory.path().join("objects")
    ));
    assert_eq!(
        fs::read(directory.path().join("objects")).unwrap(),
        b"not a directory"
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_ancestor_is_refused_without_writing_through_it() {
    let directory = TestDirectory::new();
    let outside = directory.path().join("outside");
    let root = directory.path().join("stage");
    fs::create_dir(&outside).unwrap();
    fs::create_dir(&root).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("objects")).unwrap();
    let mut files = RootedFiles::new(&root);

    assert!(matches!(
        files.publish(Path::new("objects/artifact"), DATA_MODE, b"bytes"),
        Err(Error::Refused(Refusal::AncestorNotDirectory { path }))
            if path == root.join("objects")
    ));
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
}

#[test]
fn a_destination_that_is_not_a_regular_file_is_never_replaced() {
    let directory = TestDirectory::new();
    fs::create_dir(directory.path().join("occupied")).unwrap();
    let mut files = RootedFiles::new(directory.path());

    assert!(matches!(
        files.publish(Path::new("occupied"), DATA_MODE, b"bytes"),
        Err(Error::Refused(Refusal::DestinationNotRegularFile { .. }))
    ));
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("occupied", directory.path().join("link")).unwrap();
        assert!(matches!(
            files.publish(Path::new("link"), DATA_MODE, b"bytes"),
            Err(Error::Refused(Refusal::DestinationNotRegularFile { .. }))
        ));
        assert!(
            fs::symlink_metadata(directory.path().join("link"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
    assert!(
        directory
            .entries()
            .iter()
            .all(|name| !crate::is_temporary_path(Path::new(name)))
    );
}

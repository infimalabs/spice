use super::{Acquisition, LockFile, LockName};
use std::fs;
use std::path::PathBuf;
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn possession_is_exclusive_and_release_is_automatic() {
    let path = unique_lock_path("exclusive");
    let lock = LockFile::new(LockName::new("exclusive").expect("valid name"), &path);
    let first = acquired(lock.try_acquire().expect("first acquisition"));

    assert!(matches!(
        lock.try_acquire().expect("contending acquisition"),
        Acquisition::Contended
    ));

    drop(first);
    let second = acquired(lock.try_acquire().expect("acquisition after release"));
    drop(second);
    fs::remove_file(path).expect("remove test lock file");
}

#[test]
fn lock_names_are_single_safe_path_components() {
    for name in [
        "",
        ".",
        "..",
        "../outside",
        "with/slash",
        "with\\backslash",
        "nul\0byte",
    ] {
        assert!(LockName::new(name).is_err(), "{name:?} must be refused");
    }
    for name in ["worker", "a.b", "two-words", "UPPER", "9"] {
        assert_eq!(
            LockName::new(name).expect("normal path component").as_str(),
            name
        );
    }
}

/// The lock file, opened the way every synchronized path opens it.
///
/// The caller keeps this descriptor open across the whole answer, because
/// it is what the owner record was read from, and probes with a clone of
/// it. That is the arrangement the probe has to give possession back
/// under, so it is the one tested.
fn opened(path: &std::path::Path) -> fs::File {
    super::platform::open(path, true).expect("lock file opens")
}

/// A probe answers a question; it must not keep what it took to answer it.
#[test]
fn an_availability_probe_gives_back_the_possession_it_took() {
    let path = unique_lock_path("probe-release");
    let lock = LockFile::new(LockName::new("probe-release").expect("valid name"), &path);
    let file = opened(&path);

    assert_eq!(
        lock.availability_opened(file.try_clone().expect("probe clone"))
            .expect("probe answers"),
        super::Availability::Available
    );

    // Closing the clone cannot be what released it: the clone shared an
    // open file description (Windows: file object) with `file`, which is
    // still open here, and the lock binds to it. Only an explicit unlock
    // gets here.
    acquired(lock.try_acquire().expect("acquisition after a probe"));

    drop(file);
    fs::remove_file(path).expect("remove test lock file");
}

fn acquired(acquisition: Acquisition) -> super::Possession {
    match acquisition {
        Acquisition::Acquired(possession) => possession,
        Acquisition::Contended => panic!("lock should have been acquired"),
    }
}

pub(crate) fn unique_lock_path(stem: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time must follow Unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("sill-{stem}-{}-{nonce}.lock", process::id()))
}

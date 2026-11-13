//! Failures inside one seal: every step that can fail leaves the plaintext
//! readable, publishes nothing the readers would trust, and lets the next
//! seal finish the rotation.

use std::fs;
use std::path::Path;

use super::{FAIL, Step};
use crate::archive::FRAME_BYTES;
use crate::tests::Scratch;
use crate::{Error, Locus, Refusal, STAGED_PREFIX, Seal, Tape, temporary_name};

/// Bytes in each line of generated plaintext, its newline included.
const LINE_BYTES: usize = 97;

/// Plaintext spanning two whole archive frames and part of a third.
fn plaintext() -> Vec<u8> {
    let length = 2 * FRAME_BYTES as usize + 4099;
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    (0..length)
        .map(|position| {
            if (position + 1) % LINE_BYTES == 0 {
                return b'\n';
            }
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

fn staged(root: &Path) -> Vec<String> {
    fs::read_dir(root)
        .expect("the tape directory lists")
        .map(|entry| entry.expect("an entry lists").file_name())
        .filter_map(|name| name.into_string().ok())
        .filter(|name| name.starts_with(STAGED_PREFIX))
        .collect()
}

/// Capture one member and run a seal whose `step` fails.
fn failed_seal(label: &str, step: Step) -> (Scratch, Tape, Locus, Vec<u8>) {
    let scratch = Scratch::new(label);
    let tape = Tape::open(scratch.path()).expect("tape opens");
    let bytes = plaintext();
    let locus = tape.tee(&bytes).expect("bytes are captured");

    FAIL.set(Some(step));
    let failed = tape.seal();
    FAIL.set(None);

    failed.expect_err("the forced step fails the seal");
    assert_eq!(
        fs::read(scratch.path().join("000000000001.plain")).expect("plaintext survives"),
        bytes,
        "a failed seal never touches the plaintext"
    );
    assert_eq!(tape.read(locus).expect("the locus resolves"), bytes);
    assert!(
        staged(scratch.path()).is_empty(),
        "a failed seal removes what it staged"
    );
    (scratch, tape, locus, bytes)
}

#[test]
fn failing_before_the_archive_is_published_leaves_the_member_active() {
    for (label, step) in [
        ("fail-prepare", Step::Prepare),
        ("fail-archive", Step::PublishArchive),
    ] {
        let (scratch, tape, locus, bytes) = failed_seal(label, step);
        assert!(!scratch.path().join("000000000001.zst").exists());
        assert!(tape.index().expect("index reads").members().is_empty());

        let later = tape.tee(b"later").expect("capture continues");
        assert_eq!(
            later,
            Locus::new(1, bytes.len() as u64, 5),
            "appends continue in the member that failed to seal"
        );
        assert!(matches!(
            tape.seal().expect("the retried seal completes"),
            Seal::Sealed { member: 1, .. }
        ));
        assert_eq!(tape.read(locus).expect("old locus resolves"), bytes);
        assert_eq!(tape.read(later).expect("new locus resolves"), b"later");
        scratch.remove();
    }
}

#[test]
fn an_archive_published_without_its_index_is_never_read_and_is_replaced() {
    let (scratch, tape, locus, bytes) = failed_seal("fail-index", Step::PublishIndex);
    assert!(
        scratch.path().join("000000000001.zst").exists(),
        "the archive was published before the index failed"
    );
    assert!(tape.index().expect("index reads").members().is_empty());

    let later = tape.tee(b"later").expect("capture continues");
    assert_eq!(later.member, 1, "the unreferenced archive did not seal it");
    assert_eq!(tape.read(later).expect("appended bytes read"), b"later");

    assert_eq!(
        tape.seal().expect("the retried seal completes"),
        Seal::Sealed {
            member: 1,
            plaintext: bytes.len() as u64 + 5,
            compressed: fs::metadata(scratch.path().join("000000000001.zst"))
                .expect("the replacement archive is published")
                .len(),
        }
    );
    assert!(!scratch.path().join("000000000001.plain").exists());
    assert_eq!(tape.read(locus).expect("old locus resolves"), bytes);
    assert_eq!(tape.read(later).expect("new locus resolves"), b"later");
    scratch.remove();
}

#[test]
fn residue_of_a_failed_cleanup_is_removed_only_by_the_next_seal() {
    let (scratch, tape, locus, bytes) = failed_seal("fail-cleanup", Step::RemovePlaintext);
    assert_eq!(tape.index().expect("index reads").members().len(), 1);

    let later = tape.tee(b"later").expect("capture continues past residue");
    assert_eq!(later, Locus::new(2, 0, 5));
    assert!(
        scratch.path().join("000000000001.plain").exists(),
        "an append never verifies or removes residue"
    );

    assert!(matches!(
        tape.seal().expect("the next seal recovers and rotates"),
        Seal::Sealed { member: 2, .. }
    ));
    assert!(!scratch.path().join("000000000001.plain").exists());
    assert_eq!(tape.read(locus).expect("archived locus resolves"), bytes);
    assert_eq!(tape.read(later).expect("new locus resolves"), b"later");
    scratch.remove();
}

#[test]
fn staging_left_by_an_interrupted_preparation_is_removed_by_the_next_seal() {
    let scratch = Scratch::new("interrupted-prepare");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    let locus = tape.tee(b"captured").expect("bytes are captured");
    let abandoned = scratch.path().join(temporary_name());
    fs::write(&abandoned, b"half an archive").expect("staging residue is planted");

    assert!(matches!(
        tape.seal().expect("the seal completes"),
        Seal::Sealed { member: 1, .. }
    ));
    assert!(staged(scratch.path()).is_empty());
    assert_eq!(tape.read(locus).expect("locus resolves"), b"captured");
    scratch.remove();
}

#[test]
fn verification_refuses_an_archive_that_does_not_reproduce_its_plaintext() {
    let scratch = Scratch::new("verify");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    let bytes = plaintext();
    tape.tee(&bytes).expect("bytes are captured");
    let length = bytes.len() as u64;

    let corrupt = scratch.path().join(temporary_name());
    tape.prepare_archive(1, length, &corrupt)
        .expect("the archive is prepared");
    let mut archive = fs::read(&corrupt).expect("staged archive reads");
    archive[64] ^= 0x40;
    fs::write(&corrupt, archive).expect("staged archive is corrupted");
    let verified = tape.verify_archive(1, length, &corrupt);
    assert!(
        matches!(
            verified,
            Err(Error::Io {
                operation: crate::Operation::VerifyArchive,
                ..
            })
        ),
        "a frame that fails its checksum is not equivalent, got {verified:?}"
    );

    let short = scratch.path().join(temporary_name());
    tape.prepare_archive(1, length - 1, &short)
        .expect("a prefix archive is prepared");
    assert!(
        matches!(
            tape.verify_archive(1, length - 1, &short),
            Err(Error::Refused(Refusal::ArchiveMismatch { member: 1 }))
        ),
        "plaintext continuing past the archive is not equivalent"
    );
    scratch.remove();
}

#[test]
fn bounded_plaintext_inspection_refuses_an_oversized_index() {
    let scratch = Scratch::new("bounded-plaintext");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    fs::write(tape.index_path(), b"more than four bytes").expect("oversized index plants");

    let error = tape
        .plaintext_bytes_bounded(4, 1)
        .expect_err("bounded inspection refuses");
    assert!(matches!(
        error.refusal(),
        Some(Refusal::IndexReadLimit {
            limit: 4,
            observed
        }) if *observed > 4
    ));

    scratch.remove();
}

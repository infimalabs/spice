//! Capture fixtures: append, rotation, interrupted sealing, indexed reads, and
//! concurrent reader visibility.
//!
//! Interrupted sealing is exercised by reconstructing the on-disk states a
//! crash leaves behind rather than by mocking the filesystem. A crash between
//! two publications is precisely a directory holding one of them, so the
//! fixture builds that directory and then asks tape to carry on.

use std::fs;
use std::io::{self};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

use tape::{Extent, Index, Locus, Refusal, Seal, Tape, Window};

/// Bytes chosen to contain a NUL, a newline, and invalid UTF-8, so that any
/// layer tempted to treat capture as text is caught by the fixture.
const VERBATIM: &[u8] = b"prose\x00\xff\xfe line\ntool call\r\n{\"partial\": ";

/// Plaintext bytes in every archive frame but the last.
const FRAME: u64 = 256 * 1024;

/// Bytes in each line of [`noise`], its newline included.
const LINE_BYTES: u64 = 89;

/// Deterministic plaintext that compresses poorly, of any length.
fn noise(length: u64) -> Vec<u8> {
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
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

/// Read a range of `extent`, returning the window and the work it charged.
fn charged(tape: &Tape, locus: Locus, extent: Extent) -> (Window, u64) {
    let mut work = None;
    let window = tape
        .read_window(locus, extent, |bytes| {
            work = Some(bytes);
            Ok::<(), tape::Error>(())
        })
        .expect("the range reads");
    (window, work.expect("every read charges once"))
}

#[test]
fn archived_reads_charge_only_the_frames_their_range_touches() {
    let scratch = Scratch::new("frame-charges");
    let tape = Tape::open(scratch.path()).unwrap();
    let length = 2 * FRAME + 1000;
    let bytes = noise(length);
    let locus = tape.tee(&bytes).unwrap();
    assert_eq!(locus, Locus::new(1, 0, length));
    let active = tape.extents().unwrap()[0];

    let suffix = Locus::new(1, length - 1, 1);
    let (window, work) = charged(&tape, suffix, active);
    assert_eq!(
        (window.offset, work),
        (length - 1, 1),
        "plaintext pays its range"
    );
    assert_eq!(window.bytes, &bytes[bytes.len() - 1..]);

    tape.seal().unwrap();
    let sealed = tape.extents().unwrap()[0];
    assert!(sealed.sealed);
    for (law, range, offset, expected) in [
        (
            "a full range decodes every frame",
            Locus::new(1, 0, length),
            0,
            length,
        ),
        (
            "a same-frame range decodes its frame",
            Locus::new(1, FRAME + 10, 20),
            FRAME,
            FRAME,
        ),
        (
            "a cross-frame range decodes both frames",
            Locus::new(1, FRAME - 3, 6),
            0,
            2 * FRAME,
        ),
        (
            "an end range decodes only the short last frame",
            Locus::new(1, length - 1, 1),
            2 * FRAME,
            1000,
        ),
        (
            "an empty range decodes nothing",
            Locus::new(1, length, 0),
            length,
            0,
        ),
    ] {
        // An observation from before the seal reads the archive too.
        for extent in [active, sealed] {
            let (window, work) = charged(&tape, range, extent);
            assert_eq!((window.offset, work), (offset, expected), "{law}");
            let end = usize::try_from(range.offset + range.length).unwrap();
            assert_eq!(
                window.bytes,
                &bytes[usize::try_from(offset).unwrap()..end],
                "{law}"
            );
            assert_eq!(
                tape.read_extent(range, extent, |_| Ok::<(), tape::Error>(()))
                    .unwrap(),
                &bytes[usize::try_from(range.offset).unwrap()..end],
                "{law}"
            );
            assert_eq!(
                tape.read(range).unwrap(),
                window.bytes[usize::try_from(range.offset - offset).unwrap()..]
            );
        }
    }

    let refused = tape.read_extent(suffix, sealed, |work| {
        Err(tape::Error::Refused(Refusal::MemberVanished {
            member: work,
        }))
    });
    assert!(
        matches!(
            refused,
            Err(tape::Error::Refused(Refusal::MemberVanished {
                member: 1000
            }))
        ),
        "the charge's refusal is the read's answer"
    );
    let mut understated = sealed;
    understated.length = 1;
    assert!(
        tape.read_extent(Locus::new(1, 0, 1), understated, |_| Ok::<(), tape::Error>(
            ()
        ))
        .is_err(),
        "an archive disagreeing with an understated sealed extent is refused"
    );
    scratch.remove();
}

#[test]
fn appends_after_archival_leave_every_earlier_locus_intact() {
    let scratch = Scratch::new("append-after");
    let tape = Tape::open(scratch.path()).unwrap();
    let first = tape.tee(&noise(FRAME + 7)).unwrap();
    tape.seal().unwrap();
    let second = tape.tee(VERBATIM).unwrap();
    assert_eq!(second, Locus::new(2, 0, VERBATIM.len() as u64));
    tape.seal().unwrap();
    let third = tape.tee(b"tail").unwrap();

    assert_eq!(tape.read(first).unwrap(), noise(FRAME + 7));
    assert_eq!(tape.read(second).unwrap(), VERBATIM);
    assert_eq!(tape.read(third).unwrap(), b"tail");
    assert_eq!(
        tape.extents()
            .unwrap()
            .iter()
            .map(|extent| (extent.member, extent.sealed))
            .collect::<Vec<_>>(),
        [(1, true), (2, true), (3, false)]
    );
    scratch.remove();
}

#[test]
fn streamed_reads_decode_archives_forward_from_the_frame_they_need() {
    let scratch = Scratch::new("streamed");
    let tape = Tape::open(scratch.path()).unwrap();
    let bytes = noise(3 * FRAME + 11);
    tape.tee(&bytes).unwrap();
    tape.seal().unwrap();
    let extent = tape.extents().unwrap()[0];

    for range in [
        Locus::new(1, 0, extent.length),
        Locus::new(1, FRAME + 5, FRAME),
        Locus::new(1, extent.length, 0),
    ] {
        let mut streamed = Vec::new();
        let mut buffer = [0; 1000];
        tape.stream_extent(range, extent, &mut buffer, |chunk| {
            streamed.extend_from_slice(chunk);
            Ok::<(), tape::Error>(())
        })
        .unwrap();
        let start = usize::try_from(range.offset).unwrap();
        assert_eq!(
            streamed,
            &bytes[start..start + usize::try_from(range.length).unwrap()]
        );
    }
    scratch.remove();
}

#[test]
fn a_corrupted_archive_is_an_error_rather_than_an_absence() {
    let scratch = Scratch::new("corrupted");
    let tape = Tape::open(scratch.path()).unwrap();
    let bytes = noise(FRAME + 100);
    let locus = tape.tee(&bytes).unwrap();
    tape.seal().unwrap();
    let extent = tape.extents().unwrap()[0];
    let path = scratch.path().join("000000000001.zst");
    let mut archive = fs::read(&path).unwrap();
    archive[100] ^= 0x10;
    fs::write(&path, archive).unwrap();

    let first_frame = Locus::new(1, 0, 10);
    for error in [
        tape.read(first_frame).unwrap_err(),
        tape.read_extent(first_frame, extent, |_| Ok::<(), tape::Error>(()))
            .unwrap_err(),
        tape.stream_extent(first_frame, extent, &mut [0; 64], |_| {
            Ok::<(), tape::Error>(())
        })
        .unwrap_err(),
    ] {
        assert!(
            matches!(
                error,
                tape::Error::Io {
                    operation: tape::Operation::ReadArchive,
                    ..
                }
            ),
            "got {error}"
        );
    }
    assert_eq!(
        tape.read(Locus::new(1, FRAME, 100)).unwrap(),
        &bytes[usize::try_from(FRAME).unwrap()..],
        "an intact frame still reads"
    );
    assert!(tape.read(locus).is_err());
    scratch.remove();
}

#[cfg(unix)]
#[test]
fn cleanup_requires_the_storage_normal_readers_select() {
    use std::os::unix::fs::symlink;

    let scratch = Scratch::new("reader-selected-cleanup");
    let tape = Tape::open(scratch.path()).unwrap();
    let locus = tape.tee(VERBATIM).unwrap();
    tape.seal().unwrap();
    let plain = scratch.path().join("000000000001.plain");
    let archive = scratch.path().join("000000000001.zst");
    fs::write(&plain, VERBATIM).unwrap();
    fs::remove_file(&archive).unwrap();
    symlink("000000000001.zst", &archive).unwrap();
    assert_ne!(
        fs::File::open(&archive).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );

    if tape.seal().is_err() {
        assert_eq!(fs::read(&plain).unwrap(), VERBATIM);
    }
    assert_eq!(tape.read(locus).unwrap(), VERBATIM);
    scratch.remove();
}

#[test]
fn one_source_identity_survives_reopen_and_member_sealing() {
    let scratch = Scratch::new("source-stable");
    let path = scratch.path().join("owned");
    let tape = Tape::open(&path).expect("tape opens and publishes its source");
    let source = tape.source_id().expect("source identity reads");
    let locus = tape.tee(VERBATIM).expect("bytes are captured");

    tape.seal().expect("the member seals");
    let reopened = Tape::open_existing(&path).expect("observer reopens the tape");

    assert_eq!(reopened.source_id().expect("source reopens"), source);
    assert_eq!(
        reopened.read(locus).expect("locus survives sealing"),
        VERBATIM
    );
    assert_private(&path.join("source"));
    scratch.remove();
}

#[test]
fn replacing_a_tape_at_the_same_path_mints_another_source_identity() {
    let scratch = Scratch::new("source-replaced");
    let path = scratch.path().join("selected");
    let first = Tape::open(&path)
        .expect("first tape opens")
        .source_id()
        .expect("first source reads");

    fs::remove_dir_all(&path).expect("first generation is removed");
    let second = Tape::open(&path)
        .expect("replacement tape opens")
        .source_id()
        .expect("replacement source reads");

    assert_ne!(second, first, "a path is not a source-generation identity");
    scratch.remove();
}

#[test]
fn the_observer_door_never_creates_or_follows_a_tape() {
    let scratch = Scratch::new("observer-door");
    let missing = scratch.path().join("missing");
    assert!(Tape::open_existing(&missing).is_err());
    assert!(!missing.exists(), "opening a missing tape is read-only");

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let real = scratch.path().join("real");
        fs::create_dir(&real).expect("real tape directory exists");
        let alias = scratch.path().join("alias");
        symlink(&real, &alias).expect("fixture symlink exists");
        assert!(
            Tape::open_existing(&alias).is_err(),
            "an observer does not cross a selected symlink"
        );
    }

    scratch.remove();
}

#[test]
fn an_append_is_readable_before_anything_interprets_it() {
    let scratch = Scratch::new("append");
    let tape = Tape::open(scratch.path()).expect("tape opens");

    let locus = tape.tee(VERBATIM).expect("bytes are captured");

    assert_eq!(locus.member, 1, "capture begins at member one");
    assert_eq!(locus.offset, 0);
    assert_eq!(locus.length, VERBATIM.len() as u64);
    assert_eq!(tape.read(locus).expect("locus resolves"), VERBATIM);
    scratch.remove();
}

#[test]
fn each_append_names_only_its_own_bytes() {
    let scratch = Scratch::new("ranges");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    let chunks: [&[u8]; 4] = [b"alpha", b"", b"gamma-gamma", b"d"];

    let loci: Vec<Locus> = chunks
        .iter()
        .map(|chunk| tape.tee(chunk).expect("bytes are captured"))
        .collect();

    let mut expected_offset = 0;
    for (locus, chunk) in loci.iter().zip(chunks) {
        assert_eq!(locus.offset, expected_offset, "appends do not overlap");
        assert_eq!(tape.read(*locus).expect("locus resolves"), chunk);
        expected_offset += chunk.len() as u64;
    }
    scratch.remove();
}

#[test]
fn a_locus_survives_the_rotation_of_its_member() {
    let scratch = Scratch::new("rotation");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    let first = tape.tee(VERBATIM).expect("bytes are captured");
    let second = tape.tee(b"after").expect("bytes are captured");

    let sealed = tape.seal().expect("the member seals");

    assert!(
        matches!(sealed, Seal::Sealed { member: 1, .. }),
        "the active member rotated, got {sealed:?}"
    );
    assert_eq!(
        tape.read(first).expect("the locus still resolves"),
        VERBATIM,
        "a locus means the same bytes after its member is sealed"
    );
    assert_eq!(
        tape.read(second).expect("the locus still resolves"),
        b"after"
    );
    scratch.remove();
}

#[test]
fn sealing_publishes_an_indexed_private_seekable_archive() {
    let scratch = Scratch::new("sealed");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    tape.tee(VERBATIM).expect("bytes are captured");

    tape.seal().expect("the member seals");

    let member = scratch.path().join("000000000001.zst");
    let bytes = fs::read(&member).expect("the sealed member is published");
    assert_eq!(
        &bytes[..4],
        b"\x28\xb5\x2f\xfd",
        "a sealed member opens with a zstd frame"
    );
    assert_eq!(
        &bytes[bytes.len() - 4..],
        b"\xb1\xea\x92\x8f",
        "a sealed member ends with the standard seek table"
    );
    assert_eq!(
        bytes[4] & 0x04,
        0x04,
        "every frame carries its Zstandard content checksum"
    );
    assert!(
        !scratch.path().join("000000000001.plain").exists(),
        "plaintext is removed once its bytes are readable elsewhere"
    );

    let index = tape.index().expect("the index parses");
    assert_eq!(index.members().len(), 1);
    assert_eq!(index.members()[0].member, 1);
    assert_eq!(
        index.members()[0].length,
        VERBATIM.len() as u64,
        "the index records plaintext length, not storage length"
    );

    assert_private(&member);
    assert_private(&scratch.path().join("index"));
    scratch.remove();
}

#[test]
fn a_seal_interrupted_before_its_index_republishes_identical_bytes() {
    let scratch = Scratch::new("interrupted-index");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    let locus = tape.tee(VERBATIM).expect("bytes are captured");
    tape.seal().expect("the member seals");
    let published = fs::read(scratch.path().join("000000000001.zst")).expect("member readable");

    // A crash after the member was published and before the index named it:
    // the member is on disk, unreferenced, and the plaintext never went away.
    fs::remove_file(scratch.path().join("index")).expect("index removed");
    fs::write(scratch.path().join("000000000001.plain"), VERBATIM).expect("plaintext restored");

    let resumed = tape.seal().expect("the interrupted seal completes");

    assert!(matches!(resumed, Seal::Sealed { member: 1, .. }));
    assert_eq!(
        fs::read(scratch.path().join("000000000001.zst")).expect("member readable"),
        published,
        "re-sealing the same plaintext republishes byte-identical bytes"
    );
    assert_eq!(tape.read(locus).expect("locus resolves"), VERBATIM);
    scratch.remove();
}

#[test]
fn residue_beside_an_unfaithful_archive_is_archived_again_before_removal() {
    let scratch = Scratch::new("residue-repair");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    let bytes = noise(FRAME + 3);
    let locus = tape.tee(&bytes).expect("bytes are captured");
    tape.seal().expect("the member seals");
    let archive = scratch.path().join("000000000001.zst");
    let published = fs::read(&archive).expect("member readable");

    // Residue of an interrupted cleanup, beside an archive that no longer
    // reproduces it: the residue holds the only faithful copy.
    fs::write(scratch.path().join("000000000001.plain"), &bytes).expect("residue restored");
    let mut damaged = published.clone();
    damaged[40] ^= 0x01;
    fs::write(&archive, damaged).expect("archive damaged");

    assert_eq!(
        tape.seal().expect("recovery repairs and finishes"),
        Seal::Empty
    );
    assert_eq!(fs::read(&archive).expect("member readable"), published);
    assert!(!scratch.path().join("000000000001.plain").exists());
    assert_eq!(tape.read(locus).expect("locus resolves"), bytes);
    scratch.remove();
}

#[test]
fn residue_disagreeing_with_the_index_is_refused_and_kept() {
    let scratch = Scratch::new("residue-mismatch");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    let locus = tape.tee(VERBATIM).expect("bytes are captured");
    tape.seal().expect("the member seals");
    let residue = scratch.path().join("000000000001.plain");
    fs::write(&residue, b"other").expect("foreign residue planted");

    assert!(matches!(
        tape.seal(),
        Err(tape::Error::Refused(Refusal::ResidueMismatch {
            member: 1,
            indexed,
            observed: 5,
        })) if indexed == VERBATIM.len() as u64
    ));
    assert_eq!(fs::read(&residue).expect("residue kept"), b"other");
    assert!(
        tape.read(locus).is_err(),
        "plaintext disagreeing with the index is not served as the member"
    );
    scratch.remove();
}

#[test]
fn residue_of_an_interrupted_cleanup_stays_readable_until_the_next_seal() {
    let scratch = Scratch::new("interrupted-cleanup");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    let locus = tape.tee(VERBATIM).expect("bytes are captured");
    tape.seal().expect("the member seals");

    // A crash after the index named the member and before the plaintext was
    // removed: both copies exist at once.
    let residue = scratch.path().join("000000000001.plain");
    fs::write(&residue, VERBATIM).expect("plaintext restored");

    let next = tape.tee(b"later").expect("capture continues");
    assert_eq!(
        next,
        Locus::new(2, 0, 5),
        "residue of a completed seal does not reopen its member"
    );
    assert!(residue.exists(), "capture leaves recovery to the next seal");
    assert_eq!(
        tape.read(locus).expect("the residue serves the locus"),
        VERBATIM
    );

    assert!(matches!(
        tape.seal().expect("the next seal recovers and rotates"),
        Seal::Sealed { member: 2, .. }
    ));
    assert!(!residue.exists(), "the interrupted seal was completed");
    assert_eq!(
        tape.read(locus).expect("the sealed locus resolves"),
        VERBATIM,
        "no captured byte was lost by finishing the seal"
    );
    assert_eq!(tape.read(next).expect("the new locus resolves"), b"later");
    scratch.remove();
}

#[test]
fn an_empty_member_seals_nothing() {
    let scratch = Scratch::new("empty");
    let tape = Tape::open(scratch.path()).expect("tape opens");

    assert_eq!(tape.seal().expect("sealing is attempted"), Seal::Empty);

    tape.tee(b"").expect("an empty append is accepted");
    assert_eq!(
        tape.seal().expect("sealing is attempted"),
        Seal::Empty,
        "a member holding no bytes is not worth an archive"
    );
    scratch.remove();
}

#[test]
fn a_locus_past_the_end_of_its_member_is_refused() {
    let scratch = Scratch::new("range");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    let locus = tape.tee(b"eight!!!").expect("bytes are captured");
    let past = Locus::new(locus.member, locus.offset, locus.length + 1);

    let active = tape.read(past).expect_err("an overlong locus is refused");
    assert!(
        matches!(active.refusal(), Some(Refusal::OutOfRange { .. })),
        "got {active}"
    );

    tape.seal().expect("the member seals");
    let sealed = tape.read(past).expect_err("an overlong locus is refused");
    assert!(
        matches!(sealed.refusal(), Some(Refusal::OutOfRange { .. })),
        "the same locus is refused once sealed, got {sealed}"
    );

    let overflowing = Locus::new(locus.member, u64::MAX, 1);
    assert!(
        matches!(
            tape.read(overflowing).expect_err("refused").refusal(),
            Some(Refusal::OutOfRange { .. })
        ),
        "a locus whose end overflows is refused rather than wrapping"
    );
    scratch.remove();
}

#[test]
fn a_locus_naming_an_unissued_member_is_refused() {
    let scratch = Scratch::new("unissued");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    tape.tee(b"present").expect("bytes are captured");

    for member in [2, 9_999] {
        let error = tape
            .read(Locus::new(member, 0, 1))
            .expect_err("an unissued member is refused");
        assert!(
            matches!(error.refusal(), Some(Refusal::UnknownMember { .. })),
            "got {error}"
        );
    }
    scratch.remove();
}

#[test]
fn the_index_round_trips_through_its_byte_stable_form() {
    let scratch = Scratch::new("index-form");
    let tape = Tape::open(scratch.path()).expect("tape opens");
    for chunk in [b"one".as_slice(), b"two", b"three"] {
        tape.tee(chunk).expect("bytes are captured");
        tape.seal().expect("the member seals");
    }

    let text = fs::read_to_string(scratch.path().join("index")).expect("index readable");
    let parsed = Index::parse(&text).expect("the index parses");

    assert_eq!(parsed.render(), text, "the index has one spelling");
    assert_eq!(parsed.members().len(), 3);
    assert_eq!(parsed.next_member(), 4);
    assert_eq!(
        text.lines().next(),
        Some("tape 1"),
        "the index names its own format first"
    );
    scratch.remove();
}

/// The accept half of "parse admits exactly what render emits".
///
/// The refusal fixture below is the other half. Together they pin the index to
/// one spelling: these forms parse and render back to the identical bytes, and
/// every departure from them named below is refused rather than repaired.
#[test]
fn every_rendered_index_is_admitted_and_renders_back_unchanged() {
    for (sealed, text) in [
        (0, "tape 1\n"),
        (1, "tape 1\nmember 000000000001 3\n"),
        (
            3,
            "tape 1\nmember 000000000001 1\nmember 000000000002 65535\nmember 000000000003 12\n",
        ),
    ] {
        let index = Index::parse(text).expect("a rendered index is admitted");
        assert_eq!(
            index.members().len(),
            sealed,
            "{text:?} named other members"
        );
        assert_eq!(index.render(), text, "{text:?} has a second spelling");
        assert_eq!(
            index.next_member(),
            sealed as u64 + 1,
            "{text:?} appends to the wrong member",
        );
    }
}

#[test]
fn an_index_that_is_not_the_current_form_is_refused_by_line() {
    for (law, broken) in [
        ("text naming no format at all", ""),
        ("a format this build does not write", "tape 2\n"),
        ("a carriage return the renderer never emits", "tape 1\r\n"),
        // A terminal newline is what distinguishes a whole index from one
        // truncated mid-write. `str::lines` yields identical segments either
        // way, so accepting both would accept a partial file as current.
        ("a format line with no terminal newline", "tape 1"),
        (
            "a member line with no terminal newline",
            "tape 1\nmember 000000000001 5",
        ),
        ("a misspelled keyword", "tape 1\nmembers 000000000001 5\n"),
        ("a blank line between members", "tape 1\n\n"),
        ("a member number that is not padded", "tape 1\nmember 1 5\n"),
        (
            "a member number padded to another width",
            "tape 1\nmember 0000000000001 5\n",
        ),
        (
            "a member line carrying no length",
            "tape 1\nmember 000000000001\n",
        ),
        (
            "a length that is not a number",
            "tape 1\nmember 000000000001 five\n",
        ),
        // Rendered lengths are plain decimals, so a leading zero is a second
        // spelling of a number that already has one.
        (
            "a length spelled with a leading zero",
            "tape 1\nmember 000000000001 05\n",
        ),
        (
            "text trailing the length",
            "tape 1\nmember 000000000001 5 5\n",
        ),
        // An empty member seals as `Seal::Empty` and is never named, so a
        // sealed member of no bytes is a member that never existed.
        (
            "a sealed member holding no bytes",
            "tape 1\nmember 000000000001 0\n",
        ),
        // Members are numbered from one and sealed one after another, so the
        // run is contiguous and a locus can name every member it lists.
        ("member zero", "tape 1\nmember 000000000000 5\n"),
        (
            "a run starting past member one",
            "tape 1\nmember 000000000002 5\n",
        ),
        (
            "a gap in the run",
            "tape 1\nmember 000000000001 5\nmember 000000000003 5\n",
        ),
        (
            "one member given two lengths",
            "tape 1\nmember 000000000001 5\nmember 000000000001 6\n",
        ),
        (
            "members out of order",
            "tape 1\nmember 000000000002 5\nmember 000000000001 5\n",
        ),
    ] {
        let error = Index::parse(broken).expect_err(law);
        assert!(
            matches!(error.refusal(), Some(Refusal::MalformedIndex { .. })),
            "{law}: {broken:?} produced {error}",
        );
    }
}

#[test]
fn readers_resolve_loci_while_a_sealer_publishes_underneath_them() {
    let scratch = Scratch::new("concurrent");
    let tape = Tape::open(scratch.path()).expect("tape opens");

    let mut loci = Vec::new();
    let mut expected = Vec::new();
    for index in 0..64_u32 {
        let chunk = format!("chunk-{index:04}\n").into_bytes();
        loci.push(tape.tee(&chunk).expect("bytes are captured"));
        expected.push(chunk);
    }

    let barrier = Arc::new(Barrier::new(2));
    let reader = {
        let tape = tape.clone();
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            barrier.wait();
            for _ in 0..64 {
                for (locus, want) in loci.iter().zip(&expected) {
                    let got = tape
                        .read(*locus)
                        .expect("a locus resolves while a sealer runs");
                    assert_eq!(&got, want, "a reader never observes a partial member");
                }
            }
        })
    };

    barrier.wait();
    tape.seal().expect("the member seals beneath the reader");
    reader
        .join()
        .expect("no reader observed a torn or missing member");
    scratch.remove();
}

/// Assert a published file is readable and writable only by its owner.
fn assert_private(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = fs::metadata(path)
            .expect("published file is readable")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "{} is not owner-only", path.display());
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Names tried before a scratch root is declared unobtainable.
const CREATE_ATTEMPTS: usize = 128;

/// The first candidate this call creates, never one it merely found.
fn create_scratch_root(candidates: impl Iterator<Item = PathBuf>) -> PathBuf {
    for root in candidates.take(CREATE_ATTEMPTS) {
        match fs::create_dir(&root) {
            Ok(()) => return root,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => panic!("create a scratch root at {}: {error}", root.display()),
        }
    }
    panic!("no scratch root was free within {CREATE_ATTEMPTS} attempts");
}

/// A scratch directory explicitly removed when the fixture ends.
struct Scratch {
    root: PathBuf,
    removed: bool,
}

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

impl Scratch {
    fn new(label: &str) -> Self {
        let directory = std::env::temp_dir();
        let process = std::process::id();
        let root = create_scratch_root(std::iter::repeat_with(|| {
            let sequence = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
            directory.join(format!("spice-tape-{label}-{process}-{sequence}"))
        }));
        Self {
            root,
            removed: false,
        }
    }

    fn path(&self) -> &Path {
        &self.root
    }

    fn remove(mut self) {
        fs::remove_dir_all(&self.root).unwrap_or_else(|error| {
            panic!(
                "remove the scratch root at {}: {error}",
                self.root.display()
            )
        });
        assert!(
            !self.root.exists(),
            "the scratch root outlived its removal at {}",
            self.root.display()
        );
        self.removed = true;
    }
}

impl Drop for Scratch {
    /// Attempt cleanup during unwinding without replacing the first panic.
    fn drop(&mut self) {
        if !self.removed {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

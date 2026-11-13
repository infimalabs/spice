use super::{Error, Rack};

/// One ZIP member as another writer might store it.
struct Raw<'a> {
    path: &'a str,
    method: u16,
    bytes: &'a [u8],
    /// Extra field bytes in the local header only, as many writers emit.
    local_extra: &'a [u8],
}

/// A ZIP with nonzero timestamps, local extra fields, central entries in
/// input order, and an archive comment.
fn foreign_zip(members: &[Raw<'_>], comment: &[u8]) -> Vec<u8> {
    let mut archive = Vec::new();
    let mut directory = Vec::new();
    for member in members {
        let crc = crc32fast::hash(member.bytes);
        let offset = archive.len() as u32;
        let fields = |out: &mut Vec<u8>, extra: u16| {
            for value in [20_u16, 0, member.method, 0x6000, 0x5821] {
                out.extend_from_slice(&value.to_le_bytes());
            }
            for value in [crc, member.bytes.len() as u32, member.bytes.len() as u32] {
                out.extend_from_slice(&value.to_le_bytes());
            }
            out.extend_from_slice(&(member.path.len() as u16).to_le_bytes());
            out.extend_from_slice(&extra.to_le_bytes());
        };
        archive.extend_from_slice(&0x0403_4b50_u32.to_le_bytes());
        fields(&mut archive, member.local_extra.len() as u16);
        archive.extend_from_slice(member.path.as_bytes());
        archive.extend_from_slice(member.local_extra);
        archive.extend_from_slice(member.bytes);
        directory.extend_from_slice(&0x0201_4b50_u32.to_le_bytes());
        directory.extend_from_slice(&0x031e_u16.to_le_bytes());
        fields(&mut directory, 0);
        for value in [0_u16, 0, 1] {
            directory.extend_from_slice(&value.to_le_bytes());
        }
        directory.extend_from_slice(&0x81a4_0000_u32.to_le_bytes());
        directory.extend_from_slice(&offset.to_le_bytes());
        directory.extend_from_slice(member.path.as_bytes());
    }
    let start = archive.len() as u32;
    archive.extend_from_slice(&directory);
    archive.extend_from_slice(&0x0605_4b50_u32.to_le_bytes());
    let count = members.len() as u16;
    for value in [0, 0, count, count] {
        archive.extend_from_slice(&value.to_le_bytes());
    }
    archive.extend_from_slice(&(directory.len() as u32).to_le_bytes());
    archive.extend_from_slice(&start.to_le_bytes());
    archive.extend_from_slice(&(comment.len() as u16).to_le_bytes());
    archive.extend_from_slice(comment);
    archive
}

/// A stored member at `path` holding `bytes`.
const fn stored<'a>(path: &'a str, bytes: &'a [u8]) -> Raw<'a> {
    Raw {
        path,
        method: 0,
        bytes,
        local_extra: &[],
    }
}

fn refused_member(result: Result<Rack<'_>, Error>) -> String {
    match result {
        Err(Error::Member { path, .. }) => path,
        other => panic!("expected a member refusal, got {other:?}"),
    }
}

#[test]
fn admission_reads_exact_members_another_writer_produced() {
    let guide = b"# Guide\n".repeat(40);
    let bytes = foreign_zip(
        &[
            Raw {
                local_extra: b"UT\x05\0\x01\0\0\0\0",
                ..stored("docs/guide.md", &guide)
            },
            stored("app/index.html.gz", b"named .gz, never parsed"),
            stored("SKILL.md", b"skill"),
        ],
        b"written elsewhere",
    );

    let rack = Rack::admit(&bytes).expect("a foreign rack admits");

    let members = rack
        .members()
        .iter()
        .map(|member| (member.path(), member.bytes()))
        .collect::<Vec<_>>();
    assert_eq!(
        members,
        [
            ("SKILL.md", b"skill".as_slice()),
            ("app/index.html.gz", b"named .gz, never parsed"),
            ("docs/guide.md", &guide),
        ]
    );
    assert_eq!(rack.get("app/index.html"), None);
    assert_eq!(rack.get("docs/absent.md"), None);
}

#[test]
fn beneath_lists_one_directory_by_relative_path() {
    let bytes = foreign_zip(
        &[
            stored("docs/b.md", b"b"),
            stored("docs-extra/c.md", b"c"),
            stored("docs/a/nested.md", b"n"),
            stored("doc", b"d"),
            stored("readme.md", b"r"),
        ],
        b"",
    );
    let rack = Rack::admit(&bytes).expect("rack admits");

    let docs = rack
        .beneath("docs")
        .map(|(file, _)| file)
        .collect::<Vec<_>>();
    assert_eq!(docs, ["a/nested.md", "b.md"]);
    assert_eq!(rack.beneath("").count(), 5);
    assert_eq!(
        rack.beneath("docs/a")
            .map(|(file, _)| file)
            .collect::<Vec<_>>(),
        ["nested.md"]
    );
    assert_eq!(rack.beneath("absent").count(), 0);
}

#[test]
fn admission_refuses_broken_structure() {
    let bytes = foreign_zip(&[stored("a.md", b"a"), stored("b.md", b"b")], b"");
    let mut displaced = bytes.clone();
    let end = displaced.len() - 22;
    displaced[end + 16..end + 20].copy_from_slice(&u32::MAX.to_le_bytes());
    let mut spanning = bytes.clone();
    spanning[end + 4..end + 6].copy_from_slice(&1_u16.to_le_bytes());
    let mut commented = bytes.clone();
    commented[end + 20..end + 22].copy_from_slice(&5_u16.to_le_bytes());
    for crafted in [
        &bytes[..bytes.len() - 1],
        &bytes[..21],
        &displaced[..],
        &spanning[..],
        &commented[..],
    ] {
        assert!(Rack::admit(crafted).is_err());
    }

    let mut renamed = bytes;
    renamed[30] = b'z';
    assert!(Rack::admit(&renamed).is_err());

    let deflated = foreign_zip(
        &[Raw {
            method: 8,
            ..stored("a.md", b"a")
        }],
        b"",
    );
    assert_eq!(refused_member(Rack::admit(&deflated)), "a.md");

    let repeated = foreign_zip(&[stored("a.md", b"a"), stored("a.md", b"b")], b"");
    assert!(Rack::admit(&repeated).is_err());

    for path in ["a//b", "./a", "a/../b", "a\\b", "a\0b", "/a", "a/"] {
        let unnormalized = foreign_zip(&[stored(path, b"x")], b"");
        assert_eq!(refused_member(Rack::admit(&unnormalized)), path);
    }
}

#[test]
fn local_metadata_cannot_change_the_interpretation_of_stored_bytes() {
    let bytes = foreign_zip(&[stored("a.md", b"exact original bytes")], b"");
    for (offset, value) in [(6, 1_u16), (8, 8)] {
        let mut changed = bytes.clone();
        changed[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        assert!(Rack::admit(&changed).is_err(), "local field at {offset}");
    }
    let mut changed = bytes;
    changed[18..22].copy_from_slice(&1_u32.to_le_bytes());
    assert!(Rack::admit(&changed).is_err());
}

#[test]
fn borrowed_members_cover_exact_bytes_including_empty_content() {
    let binary = (0..=255).collect::<Vec<u8>>();
    let entries = [
        stored("empty", b""),
        stored("binary", &binary),
        stored("non-ascii/caf\u{e9}.txt", b"coffee"),
    ];
    let bytes = foreign_zip(&entries, b"archive metadata");
    let rack = Rack::admit(&bytes).expect("foreign members admit");
    let archive = bytes.as_ptr_range();
    for entry in &entries {
        let member = rack.get(entry.path).expect("member is indexed");
        assert_eq!(member.bytes(), entry.bytes);
        let stored = member.bytes().as_ptr_range();
        assert!(archive.start <= stored.start && stored.end <= archive.end);
    }
}

#[cfg(feature = "write")]
mod write {
    use std::io::Read as _;

    use super::super::{Error, Rack, write};

    #[test]
    fn writing_is_deterministic_and_round_trips_exact_members() {
        let image = vec![7_u8; 4096];
        let entries = [
            ("docs/guide.md", b"# Guide\n".as_slice()),
            ("app/image.png", &image),
            ("tape.gz", b"named .gz, never parsed"),
            ("empty", b""),
        ];
        let bytes = write(entries).expect("entries write");
        let mut reversed = entries;
        reversed.reverse();
        assert_eq!(write(reversed).expect("entries write"), bytes);

        let rack = Rack::admit(&bytes).expect("written rack admits");
        let mut expected = entries.to_vec();
        expected.sort_unstable();
        let members = rack
            .members()
            .iter()
            .map(|member| (member.path(), member.bytes()))
            .collect::<Vec<_>>();
        assert_eq!(members, expected);

        let copied = write(
            rack.members()
                .iter()
                .map(|member| (member.path(), member.bytes())),
        )
        .expect("admitted members write");
        assert_eq!(copied, bytes);
    }

    #[test]
    fn a_generic_zip_reader_extracts_every_member_exactly() {
        let entries = [
            ("docs/guide.md", b"# Guide\n".as_slice()),
            ("tape.gz", b"named .gz, never parsed"),
        ];
        let bytes = write(entries).expect("entries write");
        let mut archive =
            zip::ZipArchive::new(std::io::Cursor::new(&bytes)).expect("a generic reader opens");
        for (path, original) in entries {
            let mut extracted = Vec::new();
            archive
                .by_name(path)
                .expect("entry reads")
                .read_to_end(&mut extracted)
                .expect("entry extracts");
            assert_eq!(extracted, original);
        }
        assert_eq!(archive.len(), entries.len());
    }

    #[test]
    fn writing_refuses_repeated_and_unnormalized_paths() {
        assert!(matches!(
            write([("a.md", b"a".as_slice()), ("a.md", b"b".as_slice())]),
            Err(Error::Member { path, .. }) if path == "a.md"
        ));
        for path in ["", "a//b", "./a", "a/../b", "a\\b", "/a", "a/"] {
            assert!(
                matches!(
                    write([(path, b"x".as_slice())]),
                    Err(Error::Member { path: refused, .. }) if refused == path
                ),
                "{path:?}"
            );
        }
    }
}

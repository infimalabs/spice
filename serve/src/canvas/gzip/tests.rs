use std::io::{Read as _, Write as _};

use flate2::Compression;

use super::{Stream, beneath, compress, get, stored_name, write};

/// A gzip stream as the `gzip` tool writes it: a timestamp, a stored file
/// name, and a Unix OS byte.
fn named_gzip(name: &str, bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::GzBuilder::new()
        .filename(name)
        .mtime(1_700_000_000)
        .operating_system(3)
        .write(Vec::new(), Compression::fast());
    encoder.write_all(bytes).expect("deflating into memory");
    encoder.finish().expect("deflating into memory")
}

fn refused_path<T: std::fmt::Debug>(result: Result<T, rack::Error>) -> String {
    match result {
        Err(rack::Error::Member { path, .. }) => path,
        other => panic!("expected a member refusal, got {other:?}"),
    }
}

#[test]
fn every_file_is_stored_as_one_deterministic_gzip_stream_under_its_path_plus_gz() {
    let files = [
        ("docs/guide.md", b"# Guide\n".repeat(40)),
        ("tape.gz", b"a logical .gz path".to_vec()),
        ("empty", Vec::new()),
    ];
    let entries = || files.iter().map(|(path, bytes)| (*path, bytes.as_slice()));
    let bytes = write(entries()).expect("files write");
    assert_eq!(write(entries().rev()).expect("files write"), bytes);

    let rack = rack::Rack::admit(&bytes).expect("a Spice rack is a rack");
    let names = rack
        .members()
        .iter()
        .map(|member| member.path())
        .collect::<Vec<_>>();
    assert_eq!(names, ["docs/guide.md.gz", "empty.gz", "tape.gz.gz"]);
    for (path, original) in &files {
        let stream = get(&rack, path)
            .expect("member is one stream")
            .expect("member is stored");
        assert_eq!(stream.path(), *path);
        assert_eq!(stream.size() as usize, original.len());
        assert_eq!(stream.inflate().expect("stream inflates"), *original);
        let stored = rack.get(&stored_name(path)).expect("stored name");
        assert_eq!(stream.gzip(), stored.bytes());
        // The bytes a generic ZIP tool extracts are an ordinary gzip file.
        let mut inflated = Vec::new();
        flate2::read::GzDecoder::new(stored.bytes())
            .read_to_end(&mut inflated)
            .expect("stored bytes are one gzip file");
        assert_eq!(inflated, *original);
    }
    assert_eq!(get(&rack, "tape.gz.gz").expect("absent"), None);
    assert_eq!(
        beneath(&rack, "docs")
            .map(|entry| entry.expect("one stream").0)
            .collect::<Vec<_>>(),
        ["guide.md"]
    );
    assert_eq!(compress(b"same"), compress(b"same"));
}

#[test]
fn a_foreign_gzip_header_reads_as_one_stream() {
    let guide = b"# Guide\n".repeat(40);
    let gzip = named_gzip("guide.md", &guide);
    let bytes = rack::write([("guide.md.gz", gzip.as_slice())]).expect("rack writes");
    let rack = rack::Rack::admit(&bytes).expect("rack admits");
    let stream = get(&rack, "guide.md").expect("one stream").expect("stored");
    assert_eq!(stream.gzip(), gzip);
    assert_eq!(stream.inflate().expect("inflates"), guide);
}

#[test]
fn members_outside_the_convention_are_refused() {
    let gzip = compress(b"content");
    let unsuffixed = rack::write([("a.md", gzip.as_slice())]).expect("rack writes");
    let rack = rack::Rack::admit(&unsuffixed).expect("rack admits");
    assert_eq!(
        refused_path(Stream::parse(rack.members()[0])),
        "a.md",
        "a stored name without the suffix is refused by that name"
    );
    assert_eq!(
        refused_path(beneath(&rack, "").collect::<Result<Vec<_>, _>>()),
        "a.md"
    );

    let plain = rack::write([("a.md.gz", b"not gzip".as_slice())]).expect("rack writes");
    let rack = rack::Rack::admit(&plain).expect("rack admits");
    assert_eq!(refused_path(get(&rack, "a.md")), "a.md");
}

#[test]
fn inflation_refuses_a_stream_that_disagrees_with_its_trailer() {
    for corrupt in [
        |gzip: &mut Vec<u8>| {
            let at = gzip.len() - 8;
            gzip[at] ^= 1;
        },
        |gzip: &mut Vec<u8>| {
            let at = gzip.len() - 4;
            gzip[at] = gzip[at].wrapping_sub(1);
        },
        |gzip: &mut Vec<u8>| {
            let at = gzip.len() - 4;
            gzip[at] = gzip[at].wrapping_add(1);
        },
    ] {
        let mut gzip = compress(b"original bytes");
        corrupt(&mut gzip);
        let bytes = rack::write([("a.md.gz", gzip.as_slice())]).expect("rack writes");
        let rack = rack::Rack::admit(&bytes).expect("rack admits");
        let stream = get(&rack, "a.md")
            .expect("structure parses")
            .expect("stored");
        assert_eq!(refused_path(stream.inflate()), "a.md");
    }
}

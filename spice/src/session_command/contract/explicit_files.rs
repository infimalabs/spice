use super::*;
use flate2::Compression;
use flate2::write::GzEncoder;
use std::fs::File;
use std::io::Write;

#[test]
fn a_repeated_compressed_file_contributes_one_source() {
    let scratch = Scratch::new("explicit-files");
    let plain = scratch.path().join("omp-session.jsonl");
    let bytes = write_jsonl(&plain, &omp_rows(THREAD));
    let gzip_path = scratch.path().join("omp-session.jsonl.gz");
    let mut encoder = GzEncoder::new(
        File::create(&gzip_path).expect("gzip fixture"),
        Compression::default(),
    );
    encoder.write_all(&bytes).expect("gzip fixture bytes");
    encoder.finish().expect("gzip fixture finishes");
    let gzip = gzip_path.to_string_lossy().into_owned();

    let deduped = reply_forms(
        &strings(&["session", "turns", &gzip, &gzip, "--limit", "4"]),
        &[],
        "turns",
    );
    assert_eq!(deduped["sources"].as_array().map(Vec::len), Some(1));
    assert_eq!(deduped["sources"][0]["compressed"], true);
    assert_eq!(deduped["rows"].as_array().map(Vec::len), Some(3));

    scratch.remove();
}

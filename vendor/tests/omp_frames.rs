//! OMP transport boundaries, independent of a running provider.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use vendor::omp::frame::{FrameDecoder, MAX_FRAME_BYTES, MAX_REASSEMBLED_BYTES};

const CHUNK: usize = 256 * 1024;

fn frame(index: usize, count: usize, length: usize, bytes: &[u8]) -> Value {
    json!({"type":"rpc_chunk", "chunkId":"one", "index":index,
        "count":count, "byteLength":length, "data":STANDARD.encode(bytes)})
}

fn push(decoder: &mut FrameDecoder, frame: &Value) -> Result<Option<Vec<u8>>, String> {
    decoder
        .push(&serde_json::to_vec(frame).unwrap())
        .map(|record| record.map(|bytes| bytes.into_owned()))
        .map_err(|error| error.to_string())
}

fn record(length: usize) -> Vec<u8> {
    let mut bytes = br#"{"text":""#.to_vec();
    bytes.resize(length - 2, b'x');
    bytes.extend_from_slice(b"\"}");
    bytes
}

#[test]
fn chunked_json_is_byte_exact_and_the_next_record_is_independent() {
    let bytes = record(MAX_FRAME_BYTES + 31);
    let mut decoder = FrameDecoder::default();
    let count = bytes.len().div_ceil(CHUNK);
    for (index, part) in bytes.chunks(CHUNK).enumerate() {
        let result = push(&mut decoder, &frame(index, count, bytes.len(), part)).unwrap();
        if index + 1 == count {
            let mut expected = bytes.clone();
            expected.push(b'\n');
            assert_eq!(result.unwrap(), expected);
        } else {
            assert!(result.is_none());
        }
    }
    decoder.finish().unwrap();
    let plain = b"{\"type\":\"ready\"}\n";
    assert_eq!(decoder.push(plain).unwrap().unwrap().as_ref(), plain);
}

#[test]
fn invalid_metadata_refuses_before_an_assembly_can_be_admitted() {
    let valid = frame(0, 5, MAX_FRAME_BYTES + 1, b"x");
    for (field, value) in [
        ("chunkId", json!("")),
        ("chunkId", json!("x".repeat(129))),
        ("index", json!(-1)),
        ("index", json!(1)),
        ("index", json!(5)),
        ("count", json!(1)),
        ("count", json!(257)),
        ("count", json!(2.5)),
        ("byteLength", json!(MAX_FRAME_BYTES - 1)),
        ("byteLength", json!(MAX_REASSEMBLED_BYTES + 1)),
    ] {
        let mut changed = valid.clone();
        changed[field] = value;
        assert!(
            push(&mut FrameDecoder::default(), &changed).is_err(),
            "{field}: {changed}"
        );
    }
}

#[test]
fn base64_and_payload_bounds_refuse_without_completing_a_record() {
    let valid = frame(0, 5, MAX_FRAME_BYTES + 1, b"x");
    for data in [
        "".to_owned(),
        "eA".to_owned(),
        "eA==\n".to_owned(),
        "eB==".to_owned(),
        "_A==".to_owned(),
        STANDARD.encode(vec![b'x'; CHUNK + 1]),
    ] {
        let mut changed = valid.clone();
        changed["data"] = json!(data);
        assert!(push(&mut FrameDecoder::default(), &changed).is_err());
    }
}

#[test]
fn sequence_identity_order_and_metadata_cannot_change_mid_record() {
    let first = frame(0, 5, MAX_FRAME_BYTES + 1, b"x");
    let second = frame(1, 5, MAX_FRAME_BYTES + 1, b"y");
    for (field, value) in [
        ("chunkId", json!("other")),
        ("index", json!(0)),
        ("index", json!(2)),
        ("count", json!(6)),
        ("byteLength", json!(MAX_FRAME_BYTES + 2)),
    ] {
        let mut decoder = FrameDecoder::default();
        assert!(push(&mut decoder, &first).unwrap().is_none());
        let mut changed = second.clone();
        changed[field] = value;
        assert!(push(&mut decoder, &changed).is_err(), "{field}");
    }
}

#[test]
fn interrupted_and_incomplete_sequences_never_become_logical_records() {
    let mut decoder = FrameDecoder::default();
    assert!(
        push(&mut decoder, &frame(0, 5, MAX_FRAME_BYTES + 1, b"x"))
            .unwrap()
            .is_none()
    );
    assert!(decoder.finish().is_err());
    assert!(decoder.push(b"{\"type\":\"ready\"}\n").is_err());
}

#[test]
fn declared_length_must_match_even_when_count_and_order_match() {
    for length in [MAX_FRAME_BYTES, MAX_FRAME_BYTES + 2] {
        let bytes = record(MAX_FRAME_BYTES + 1);
        let count = bytes.len().div_ceil(CHUNK);
        let mut decoder = FrameDecoder::default();
        for (index, part) in bytes.chunks(CHUNK).enumerate() {
            let result = push(&mut decoder, &frame(index, count, length, part));
            if index + 1 == count {
                assert!(result.is_err());
            } else {
                assert!(result.unwrap().is_none());
            }
        }
    }
}

#[test]
fn assembled_bytes_must_be_one_utf8_json_object() {
    for ending in *b"\xff!]\n" {
        let mut bytes = record(MAX_FRAME_BYTES);
        if ending == b']' {
            bytes[0] = b'[';
        }
        *bytes.last_mut().unwrap() = ending;
        let count = bytes.len().div_ceil(CHUNK);
        let mut decoder = FrameDecoder::default();
        for (index, part) in bytes.chunks(CHUNK).enumerate() {
            let result = push(&mut decoder, &frame(index, count, bytes.len(), part));
            if index + 1 == count {
                assert!(result.is_err());
            } else {
                assert!(result.unwrap().is_none());
            }
        }
    }
}

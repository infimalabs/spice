use std::io::{Read, Write};
use std::thread;

use blake3::hazmat::{
    ChainingValue, HasherExt as _, Mode, left_subtree_len, merge_subtrees_non_root,
    merge_subtrees_root,
};
use blake3::{CHUNK_LEN, Hash, Hasher};

use super::Domain;

const PARALLEL_INPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_HASH_WORKERS: usize = 16;
const READER_BUFFER_BYTES: usize = 64 * 1024;

// Preserve the standard BLAKE3 tree and domain framing while distributing only
// the large executable inputs that dominate installed-hook authentication.
pub(super) fn compute(domain: Domain, bytes: &[u8]) -> Hash {
    if !matches!(domain, Domain::InstallArtifact | Domain::GateTool)
        || bytes.len() < PARALLEL_INPUT_BYTES
    {
        return sequential(domain, bytes);
    }
    let mut prefix = domain.tag().as_bytes().to_vec();
    prefix.push(0);
    let framed = FramedInput {
        prefix: &prefix,
        payload: bytes,
    };
    let workers = thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .min(MAX_HASH_WORKERS);
    parallel_root(framed, workers)
}

fn sequential(domain: Domain, bytes: &[u8]) -> Hash {
    let mut hasher = framed_hasher(domain);
    hasher.update(bytes);
    hasher.finalize()
}

pub(super) fn compute_reader(
    domain: Domain,
    reader: impl Read,
    limit: u64,
) -> std::io::Result<(u64, Hash)> {
    consume_reader(domain, reader, limit, |_| Ok(()))
}

pub(super) fn copy_reader(
    domain: Domain,
    reader: impl Read,
    writer: &mut dyn Write,
    limit: u64,
) -> std::io::Result<(u64, Hash)> {
    consume_reader(domain, reader, limit, |bytes| writer.write_all(bytes))
}

fn consume_reader(
    domain: Domain,
    mut reader: impl Read,
    limit: u64,
    mut consume: impl FnMut(&[u8]) -> std::io::Result<()>,
) -> std::io::Result<(u64, Hash)> {
    if limit == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "digest reader byte limit must be positive",
        ));
    }
    let mut hasher = framed_hasher(domain);
    let mut buffer = [0_u8; READER_BUFFER_BYTES];
    let mut length = 0_u64;
    loop {
        let remaining = limit - length;
        if remaining == 0 {
            return match reader.read(&mut buffer[..1]) {
                Ok(0) => Ok((length, hasher.finalize())),
                Ok(_) => Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("digest reader exceeds its {limit}-byte limit"),
                )),
                Err(error) => Err(error),
            };
        }
        let capacity = usize::try_from(remaining.min(READER_BUFFER_BYTES as u64))
            .expect("reader buffer capacity fits usize");
        let read = reader.read(&mut buffer[..capacity])?;
        if read == 0 {
            return Ok((length, hasher.finalize()));
        }
        consume(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        length += read as u64;
    }
}

/// Forwards writes to `inner` and hashes exactly the bytes it accepted.
pub(super) struct HashingWriter<W> {
    inner: W,
    hasher: Hasher,
    length: u64,
}

impl<W: Write> HashingWriter<W> {
    pub(super) fn new(domain: Domain, inner: W) -> Self {
        Self {
            inner,
            hasher: framed_hasher(domain),
            length: 0,
        }
    }

    pub(super) fn finish(self) -> (W, u64, Hash) {
        (self.inner, self.length, self.hasher.finalize())
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(bytes)?;
        self.hasher.update(&bytes[..written]);
        self.length += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn framed_hasher(domain: Domain) -> Hasher {
    let mut hasher = Hasher::new();
    hasher.update(domain.tag().as_bytes());
    hasher.update(&[0]);
    hasher
}

#[derive(Clone, Copy)]
struct FramedInput<'a> {
    prefix: &'a [u8],
    payload: &'a [u8],
}

impl FramedInput<'_> {
    fn len(self) -> usize {
        self.prefix.len() + self.payload.len()
    }

    fn update(self, hasher: &mut Hasher, start: usize, length: usize) {
        let end = start + length;
        if start < self.prefix.len() {
            hasher.update(&self.prefix[start..end.min(self.prefix.len())]);
        }
        if end > self.prefix.len() {
            let payload_start = start.saturating_sub(self.prefix.len());
            let payload_end = end - self.prefix.len();
            hasher.update(&self.payload[payload_start..payload_end]);
        }
    }
}

fn parallel_root(input: FramedInput<'_>, workers: usize) -> Hash {
    if workers <= 1 || input.len() <= CHUNK_LEN {
        let mut hasher = Hasher::new();
        input.update(&mut hasher, 0, input.len());
        return hasher.finalize();
    }
    let left_len = left_subtree_len(input.len() as u64) as usize;
    let (left_workers, right_workers) = divide_workers(workers);
    let (left, right) = thread::scope(|scope| {
        let left = scope.spawn(|| subtree(input, 0, left_len, 0, left_workers));
        let right = subtree(
            input,
            left_len,
            input.len() - left_len,
            left_len as u64,
            right_workers,
        );
        (left.join().expect("scoped digest worker completes"), right)
    });
    merge_subtrees_root(&left, &right, Mode::Hash)
}

fn subtree(
    input: FramedInput<'_>,
    start: usize,
    length: usize,
    offset: u64,
    workers: usize,
) -> ChainingValue {
    if workers <= 1 || length <= CHUNK_LEN {
        let mut hasher = Hasher::new();
        hasher.set_input_offset(offset);
        input.update(&mut hasher, start, length);
        return hasher.finalize_non_root();
    }
    let left_len = left_subtree_len(length as u64) as usize;
    let (left_workers, right_workers) = divide_workers(workers);
    let (left, right) = thread::scope(|scope| {
        let left = scope.spawn(|| subtree(input, start, left_len, offset, left_workers));
        let right = subtree(
            input,
            start + left_len,
            length - left_len,
            offset + left_len as u64,
            right_workers,
        );
        (left.join().expect("scoped digest worker completes"), right)
    });
    merge_subtrees_non_root(&left, &right, Mode::Hash)
}

fn divide_workers(workers: usize) -> (usize, usize) {
    let left = workers / 2;
    (left, workers - left)
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    struct FailingReader;

    impl Read for FailingReader {
        fn read(&mut self, _bytes: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("fixture read failure"))
        }
    }

    #[test]
    fn parallel_tree_matches_blake3_for_irregular_boundaries() {
        for length in [
            CHUNK_LEN + 1,
            2 * CHUNK_LEN,
            2 * CHUNK_LEN + 1,
            17 * CHUNK_LEN - 1,
            17 * CHUNK_LEN,
            17 * CHUNK_LEN + 1,
            1024 * CHUNK_LEN + 37,
        ] {
            let input = vec![0x5a; length];
            for workers in 1..=MAX_HASH_WORKERS {
                let framed = FramedInput {
                    prefix: &input[..17.min(input.len())],
                    payload: &input[17.min(input.len())..],
                };
                assert_eq!(
                    parallel_root(framed, workers),
                    blake3::hash(&input),
                    "length={length} workers={workers}",
                );
            }
        }
        let body = b"bounded reader";
        let domain = Domain::DistArtifact;
        let limit = body.len() as u64;
        let expected = (limit, compute(domain, body));
        let computed = compute_reader(domain, body.as_slice(), limit).unwrap();
        assert_eq!(computed, expected);
        let mut copied = Vec::new();
        let streamed = copy_reader(domain, body.as_slice(), &mut copied, limit).unwrap();
        assert_eq!(streamed, expected);
        assert_eq!(copied, body);
        for (invalid, kind) in [
            (0, io::ErrorKind::InvalidInput),
            (limit - 1, io::ErrorKind::InvalidData),
        ] {
            let computed = compute_reader(domain, body.as_slice(), invalid).unwrap_err();
            let copied =
                copy_reader(domain, body.as_slice(), &mut io::sink(), invalid).unwrap_err();
            assert_eq!((computed.kind(), copied.kind()), (kind, kind),);
        }
        let read_error = compute_reader(domain, FailingReader, 1).unwrap_err();
        assert_eq!(read_error.kind(), io::ErrorKind::Other);
        let mut empty = io::Cursor::new(&mut [] as &mut [u8]);
        let write_error = copy_reader(domain, body.as_slice(), &mut empty, limit).unwrap_err();
        assert_eq!(write_error.kind(), io::ErrorKind::WriteZero);
    }
}

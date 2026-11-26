/// The command-line routes by which omp, or the `gh` that omp's gist share
/// store drives, uploads a session, screen, recording, skill, or tool report.
/// Each is denied with every argument that follows it.
pub(super) const COMMANDS: [&str; 7] = [
    "omp share",
    "omp clip",
    "omp stream",
    "omp skill publish",
    "omp skills publish",
    "omp grievances push",
    "gh gist create",
];

/// Name a publishing route when it appears where the shell will start one
/// command, including after a compound-command separator.
///
/// This is deliberately a narrow gate over the commands a seat promises to
/// refuse, not a shell parser or network sandbox. It dequotes literal command
/// words so quoting cannot evade the gate, but ignores quoted arguments after
/// any other executable so admitted commands may print these spellings.
pub(super) fn command(command: &str) -> Option<&'static str> {
    const EXECUTABLES: [&[u8]; 2] = [b"omp", b"gh"];
    const OMP_COMMANDS: [&[u8]; 6] = [
        b"share",
        b"clip",
        b"stream",
        b"skill",
        b"skills",
        b"grievances",
    ];
    const PUBLISH: [&[u8]; 1] = [b"publish"];
    const PUSH: [&[u8]; 1] = [b"push"];
    const GIST: [&[u8]; 1] = [b"gist"];
    const CREATE: [&[u8]; 1] = [b"create"];

    let bytes = command.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        while index < bytes.len()
            && (bytes[index].is_ascii_whitespace() || is_separator(bytes[index]))
        {
            index += 1;
        }
        if index == bytes.len() {
            break;
        }

        match take_word(bytes, &mut index, &EXECUTABLES) {
            Some(0) => match next_word(bytes, &mut index, &OMP_COMMANDS) {
                Some(route @ 0..=2) => return Some(COMMANDS[route]),
                Some(3) if next_word(bytes, &mut index, &PUBLISH) == Some(0) => {
                    return Some(COMMANDS[3]);
                }
                Some(4) if next_word(bytes, &mut index, &PUBLISH) == Some(0) => {
                    return Some(COMMANDS[4]);
                }
                Some(5) if next_word(bytes, &mut index, &PUSH) == Some(0) => {
                    return Some(COMMANDS[5]);
                }
                _ => {}
            },
            Some(1)
                if next_word(bytes, &mut index, &GIST) == Some(0)
                    && next_word(bytes, &mut index, &CREATE) == Some(0) =>
            {
                return Some(COMMANDS[6]);
            }
            _ => {}
        }

        while index < bytes.len() && !is_separator(bytes[index]) {
            if bytes[index].is_ascii_whitespace() {
                index += 1;
            } else {
                take_word(bytes, &mut index, &[]);
            }
        }
    }
    None
}

fn is_separator(byte: u8) -> bool {
    matches!(byte, b';' | b'|' | b'&' | b'\n' | b'(' | b')' | b'{' | b'}')
}

fn next_word(bytes: &[u8], index: &mut usize, candidates: &[&[u8]]) -> Option<usize> {
    while *index < bytes.len() && bytes[*index].is_ascii_whitespace() && bytes[*index] != b'\n' {
        *index += 1;
    }
    (*index < bytes.len() && !is_separator(bytes[*index]))
        .then(|| take_word(bytes, index, candidates))
        .flatten()
}

fn take_word(bytes: &[u8], index: &mut usize, candidates: &[&[u8]]) -> Option<usize> {
    debug_assert!(candidates.len() <= u8::BITS as usize);
    let mut possible = ((1_u16 << candidates.len()) - 1) as u8;
    let mut normalized_len = 0;
    let mut quote = None;
    let mut saw_word = false;
    let compare = |actual: u8, possible: &mut u8, normalized_len: &mut usize| {
        for (candidate, expected) in candidates.iter().enumerate() {
            if expected.get(*normalized_len).copied() != Some(actual) {
                *possible &= !(1 << candidate);
            }
        }
        *normalized_len += 1;
    };

    while *index < bytes.len() {
        let byte = bytes[*index];
        if let Some(delimiter) = quote {
            if byte == delimiter {
                quote = None;
                *index += 1;
            } else if delimiter == b'`' {
                *index += 1;
            } else if delimiter == b'"' && byte == b'\\' && *index + 1 < bytes.len() {
                let escaped = bytes[*index + 1];
                if escaped == b'\n' {
                    *index += 2;
                } else if matches!(escaped, b'$' | b'`' | b'"' | b'\\') {
                    compare(escaped, &mut possible, &mut normalized_len);
                    *index += 2;
                } else {
                    compare(byte, &mut possible, &mut normalized_len);
                    *index += 1;
                }
            } else {
                compare(byte, &mut possible, &mut normalized_len);
                *index += 1;
            }
            continue;
        }
        if byte.is_ascii_whitespace() || is_separator(byte) {
            break;
        }
        saw_word = true;
        if matches!(byte, b'\'' | b'"' | b'`') {
            quote = Some(byte);
            *index += 1;
        } else if byte == b'\\' && *index + 1 < bytes.len() {
            let escaped = bytes[*index + 1];
            if escaped != b'\n' {
                compare(escaped, &mut possible, &mut normalized_len);
            }
            *index += 2;
        } else {
            compare(byte, &mut possible, &mut normalized_len);
            *index += 1;
        }
    }

    if !saw_word {
        return None;
    }
    candidates
        .iter()
        .enumerate()
        .find_map(|(candidate, expected)| {
            ((possible & (1 << candidate) != 0) && expected.len() == normalized_len)
                .then_some(candidate)
        })
}

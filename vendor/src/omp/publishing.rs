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
/// command, including after a compound-command separator or through ordinary
/// command substitution, command/environment prefixes, and shell `-c`
/// scripts.
///
/// This is deliberately a narrow gate over the commands a seat promises to
/// refuse, not a shell parser or network sandbox. It dequotes literal command
/// words and executable nested scripts while leaving quoted data passed to
/// other executables alone.
pub(super) fn command(command: &str) -> Option<&'static str> {
    command_bytes(command.as_bytes())
}

fn command_bytes(bytes: &[u8]) -> Option<&'static str> {
    const EXECUTABLES: [&[u8]; 8] = [
        b"omp", b"gh", b"command", b"env", b"sh", b"bash", b"dash", b"zsh",
    ];
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

    if let Some(route) = substitution_command(bytes) {
        return Some(route);
    }

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
        skip_assignments(bytes, &mut index);

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
            Some(2) => {
                if let Some(route) = command_prefix(bytes, index) {
                    return Some(route);
                }
            }
            Some(3) => {
                if let Some(route) = env_prefix(bytes, index) {
                    return Some(route);
                }
            }
            Some(4..=7) => {
                if let Some(route) = shell_script(bytes, index) {
                    return Some(route);
                }
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

fn skip_assignments(bytes: &[u8], index: &mut usize) {
    loop {
        while *index < bytes.len() && bytes[*index].is_ascii_whitespace() && bytes[*index] != b'\n'
        {
            *index += 1;
        }
        let Some(end) = assignment_end(bytes, *index) else {
            return;
        };
        *index = end;
    }
}

fn assignment_end(bytes: &[u8], mut index: usize) -> Option<usize> {
    let first = *bytes.get(index)?;
    if first != b'_' && !first.is_ascii_alphabetic() {
        return None;
    }
    index += 1;
    while bytes
        .get(index)
        .is_some_and(|byte| *byte == b'_' || byte.is_ascii_alphanumeric())
    {
        index += 1;
    }
    if bytes.get(index) != Some(&b'=') {
        return None;
    }
    index += 1;

    let mut quote = None;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(delimiter) = quote {
            if byte == delimiter {
                quote = None;
            } else if byte == b'\\' && delimiter != b'\'' {
                index += 1;
            }
        } else if byte.is_ascii_whitespace() || is_separator(byte) {
            break;
        } else if matches!(byte, b'\'' | b'"') {
            quote = Some(byte);
        } else if byte == b'\\' {
            index += 1;
        }
        index += 1;
    }
    Some(index)
}

fn command_prefix(bytes: &[u8], mut index: usize) -> Option<&'static str> {
    loop {
        let (start, word) = next_value(bytes, &mut index)?;
        if word == b"--" || word == b"-p" {
            continue;
        }
        if word.starts_with(b"-") {
            return None;
        }
        return command_bytes(&bytes[start..]);
    }
}

fn env_prefix(bytes: &[u8], mut index: usize) -> Option<&'static str> {
    loop {
        let (start, word) = next_value(bytes, &mut index)?;
        if word == b"--" || matches!(word.as_slice(), b"-i" | b"--ignore-environment") {
            continue;
        }
        if matches!(word.as_slice(), b"-u" | b"--unset" | b"-C" | b"--chdir") {
            next_value(bytes, &mut index)?;
            continue;
        }
        if word == b"-S" || word == b"--split-string" {
            let (_, script) = next_value(bytes, &mut index)?;
            return command_bytes(&script);
        }
        if word.starts_with(b"--unset=") || word.starts_with(b"--chdir=") || word.contains(&b'=') {
            continue;
        }
        if word.starts_with(b"-") {
            return None;
        }
        return command_bytes(&bytes[start..]);
    }
}

fn shell_script(bytes: &[u8], mut index: usize) -> Option<&'static str> {
    let mut takes_script = false;
    while let Some((_, word)) = next_value(bytes, &mut index) {
        if takes_script {
            return command_bytes(&word);
        }
        if word == b"--" {
            continue;
        }
        if word.starts_with(b"-") {
            takes_script = word[1..].contains(&b'c');
            continue;
        }
        return None;
    }
    None
}

fn substitution_command(bytes: &[u8]) -> Option<&'static str> {
    let mut index = 0;
    let mut quote = None;
    while index < bytes.len() {
        let byte = bytes[index];
        if quote == Some(b'\'') {
            if byte == b'\'' {
                quote = None;
            }
            index += 1;
            continue;
        }
        if byte == b'\\' {
            index = (index + 2).min(bytes.len());
            continue;
        }
        if quote == Some(b'"') && byte == b'"' {
            quote = None;
            index += 1;
            continue;
        }
        if quote.is_none() && matches!(byte, b'\'' | b'"') {
            quote = Some(byte);
            index += 1;
            continue;
        }
        if byte == b'$'
            && bytes.get(index + 1) == Some(&b'(')
            && let Some(end) = substitution_end(bytes, index + 1)
        {
            if let Some(route) = command_bytes(&bytes[index + 2..end]) {
                return Some(route);
            }
            index = end + 1;
            continue;
        }
        if byte == b'`'
            && let Some(end) = backtick_end(bytes, index + 1)
        {
            if let Some(route) = command_bytes(&bytes[index + 1..end]) {
                return Some(route);
            }
            index = end + 1;
            continue;
        }
        index += 1;
    }
    None
}

fn substitution_end(bytes: &[u8], mut index: usize) -> Option<usize> {
    let mut depth = 1_usize;
    let mut quote = None;
    index += 1;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(delimiter) = quote {
            if byte == delimiter {
                quote = None;
            } else if byte == b'\\' && delimiter != b'\'' {
                index += 1;
            } else if delimiter == b'"' && byte == b'$' && bytes.get(index + 1) == Some(&b'(') {
                depth += 1;
                index += 1;
            }
        } else if byte == b'\\' {
            index += 1;
        } else if matches!(byte, b'\'' | b'"' | b'`') {
            quote = Some(byte);
        } else if byte == b'(' {
            depth += 1;
        } else if byte == b')' {
            depth -= 1;
            if depth == 0 {
                return Some(index);
            }
        }
        index += 1;
    }
    None
}

fn backtick_end(bytes: &[u8], mut index: usize) -> Option<usize> {
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            index += 2;
        } else if bytes[index] == b'`' {
            return Some(index);
        } else {
            index += 1;
        }
    }
    None
}

fn next_value(bytes: &[u8], index: &mut usize) -> Option<(usize, Vec<u8>)> {
    while *index < bytes.len() && bytes[*index].is_ascii_whitespace() && bytes[*index] != b'\n' {
        *index += 1;
    }
    (*index < bytes.len() && !is_separator(bytes[*index])).then(|| {
        let start = *index;
        (start, take_value(bytes, index))
    })
}

fn take_value(bytes: &[u8], index: &mut usize) -> Vec<u8> {
    let mut value = Vec::new();
    let mut quote = None;
    while *index < bytes.len() {
        let byte = bytes[*index];
        if let Some(delimiter) = quote {
            if byte == delimiter {
                quote = None;
                *index += 1;
            } else if delimiter == b'"' && byte == b'\\' && *index + 1 < bytes.len() {
                let escaped = bytes[*index + 1];
                if escaped == b'\n' {
                    *index += 2;
                } else if matches!(escaped, b'$' | b'`' | b'"' | b'\\') {
                    value.push(escaped);
                    *index += 2;
                } else {
                    value.push(byte);
                    *index += 1;
                }
            } else {
                value.push(byte);
                *index += 1;
            }
            continue;
        }
        if byte.is_ascii_whitespace() || is_separator(byte) {
            break;
        }
        if matches!(byte, b'\'' | b'"') {
            quote = Some(byte);
            *index += 1;
        } else if byte == b'\\' && *index + 1 < bytes.len() {
            let escaped = bytes[*index + 1];
            if escaped != b'\n' {
                value.push(escaped);
            }
            *index += 2;
        } else {
            value.push(byte);
            *index += 1;
        }
    }
    value
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

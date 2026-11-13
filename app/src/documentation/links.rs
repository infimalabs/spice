use wire::intent::RACK_PATH_BYTES_MAX;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) path: String,
    pub(crate) fragment: String,
}

impl Target {
    pub(crate) fn href(&self) -> String {
        format!("#docs/{}#{}", encode(&self.path), encode(&self.fragment))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Link {
    Internal(Target),
    External(String),
    Inert,
}

pub(crate) fn resolve(base: &str, directory: bool, destination: &str) -> Link {
    let destination = destination.trim();
    if destination.chars().any(char::is_control) || destination.contains('\\') {
        return Link::Inert;
    }
    let lower = destination.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        return Link::External(destination.to_owned());
    }
    let (rooted, destination) = if let Some(path) = destination.strip_prefix("#docs/") {
        (true, path)
    } else if let Some(path) = destination.strip_prefix("spice://docs") {
        if !path.is_empty() && !path.starts_with(['/', '#']) {
            return Link::Inert;
        }
        (true, path.strip_prefix('/').unwrap_or(path))
    } else {
        if destination.starts_with('/')
            || destination
                .split(['/', '#', '?'])
                .next()
                .is_some_and(|head| head.contains(':'))
        {
            return Link::Inert;
        }
        (false, destination)
    };
    let (path, fragment) = destination.split_once('#').unwrap_or((destination, ""));
    if path.contains('?') {
        return Link::Inert;
    }
    let (Some(path), Some(fragment)) = (decode(path), decode(fragment)) else {
        return Link::Inert;
    };
    if path.len() > RACK_PATH_BYTES_MAX
        || path.contains(['\\', ':', '?', '#'])
        || path.chars().any(char::is_control)
        || fragment.chars().any(char::is_control)
    {
        return Link::Inert;
    }
    let mut parts: Vec<&str> = if rooted {
        Vec::new()
    } else {
        base.split('/').filter(|part| !part.is_empty()).collect()
    };
    if !rooted && !path.is_empty() && !directory {
        parts.pop();
    }
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Link::Inert;
                }
            }
            part => parts.push(part),
        }
    }
    let path = parts.join("/");
    if path.len() > RACK_PATH_BYTES_MAX {
        return Link::Inert;
    }
    Link::Internal(Target { path, fragment })
}

fn decode(text: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut input = text.bytes();
    while let Some(byte) = input.next() {
        if byte == b'%' {
            let high = char::from(input.next()?).to_digit(16)?;
            let low = char::from(input.next()?).to_digit(16)?;
            bytes.push(u8::try_from(high * 16 + low).ok()?);
        } else {
            bytes.push(byte);
        }
    }
    String::from_utf8(bytes).ok()
}

fn encode(text: &str) -> String {
    use std::fmt::Write as _;
    let mut encoded = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

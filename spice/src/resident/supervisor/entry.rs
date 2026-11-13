//! The hidden entries this executable runs under: a lane supervisor, the
//! keeper of one of a lane's jobs, or one read of a lane's rendered inbox.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::Path;

use spice::ambient::Ambient;

use super::command::{current, inbox};
use super::terminal::job;

/// One request-path entry, run against the environment `main` captured.
type RequestEntry = fn(&Path, &Ambient) -> io::Result<()>;

/// Dispatch a hidden entry before normal CLI parsing.
pub(crate) fn entry(
    arguments: impl IntoIterator<Item = OsString>,
    ambient: &Ambient,
) -> Option<i32> {
    let mut arguments = arguments.into_iter();
    let _executable = arguments.next()?;
    let role = arguments.next()?;
    if let Some(code) = current::entry(&role, &mut arguments, ambient) {
        return Some(code);
    }
    if role == OsStr::new(inbox::ENTRY) {
        let (Some(lane), Some(projection), None) =
            (arguments.next(), arguments.next(), arguments.next())
        else {
            eprintln!("lane inbox takes a lane and a projection state path");
            return Some(2);
        };
        return Some(match inbox::run(&lane, Path::new(&projection), ambient) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("lane inbox failed: {error}");
                1
            }
        });
    }
    let (name, run): (&str, RequestEntry) = if role == OsStr::new(super::ENTRY) {
        ("lane supervisor", super::run)
    } else if role == OsStr::new(job::ENTRY) {
        ("lane job", |request, _| job::keep(request))
    } else {
        return None;
    };
    let Some(request) = arguments.next() else {
        eprintln!("{name} request path is missing");
        return Some(2);
    };
    if arguments.next().is_some() {
        eprintln!("{name} received unexpected arguments");
        return Some(2);
    }
    Some(match run(Path::new(&request), ambient) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{name} failed: {error}");
            1
        }
    })
}

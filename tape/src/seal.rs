//! Rotation of plaintext into indexed, immutable seekable archives.
//!
//! Sealing one member is three separable steps over a staged file: prepare
//! streams the plaintext into a [seekable Zstandard archive](crate::archive),
//! verify reopens the staged archive and the plaintext independently and
//! compares every decoded byte, and publish renames the verified archive into
//! place. None of the three touches the plaintext or the index, and a failure
//! in any of them removes only the staged file.
//!
//! A seal publishes in one order and that order is the crash contract: the
//! verified archive first, then the index that names it, then the removal of
//! the plaintext it was made from. Every prefix of that order is recoverable.
//! Dying before the archive is published leaves at most a staged file, which
//! the next seal removes. Dying between the archive and the index leaves an
//! unreferenced archive beside plaintext that is still accepting appends;
//! readers never consult it, and the next seal replaces it. Dying between the
//! index and the removal leaves plaintext whose member the index already
//! names. Readers prefer that plaintext, and the next seal's recovery removes
//! it only after the current published archive is proven to reproduce it byte
//! for byte; a missing, unreadable or unfaithful archive is prepared again
//! first. Capture never recovers, because recovery verifies an archive.
//!
//! Plaintext is therefore never removed before the bytes it holds are
//! verifiably readable from somewhere else, and every step runs under the
//! writer lock, so no append can land in a member being sealed.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;

use crate::archive::{self, Archive};
use crate::{Error, Operation, Refusal, STAGED_PREFIX, STAGED_SUFFIX, Tape, create_private};

/// First line of the index, naming the one format tape writes.
const FORMAT: &str = "tape 1";

/// Digits in a rendered member number.
///
/// Member numbers are zero-padded so that a directory listing sorts members
/// into the order they were captured in.
pub(crate) const DIGITS: usize = 12;

/// One sealed member.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Member {
    /// Member number, assigned when its first byte was appended.
    pub member: u64,
    /// Plaintext bytes the member carries, before compression.
    pub length: u64,
}

/// Observable bounds of one durable tape member.
///
/// A sealed extent cannot grow. An active extent may be longer on the next
/// observation, which is the cursor comparison a daemonless listener uses to
/// heal notifications it never received.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Extent {
    /// Member number.
    pub member: u64,
    /// Plaintext bytes currently readable from the member.
    pub length: u64,
    /// Whether the member has crossed the immutable index boundary.
    pub sealed: bool,
}

/// Complete readable extents admitted by caller-owned discovery bounds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundedExtents {
    /// Readable members in ascending order.
    pub extents: Vec<Extent>,
    /// Exact index bytes read and parsed.
    pub index_bytes: u64,
}

/// Every sealed member of one tape, in ascending member order.
///
/// The index records plaintext lengths rather than compressed lengths because
/// a locus is a position in the bytes that were captured, not in their
/// storage. Compression is a storage decision and must not be visible to a
/// reader resolving a position.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Index {
    members: Vec<Member>,
}

impl Index {
    /// Every sealed member, ascending.
    #[must_use]
    pub fn members(&self) -> &[Member] {
        &self.members
    }

    /// The highest sealed member, if any member has been sealed.
    #[must_use]
    pub fn last(&self) -> Option<Member> {
        self.members.last().copied()
    }

    /// The sealed member with this number, if it is sealed.
    #[must_use]
    pub fn find(&self, member: u64) -> Option<Member> {
        let index = usize::try_from(member.checked_sub(1)?).ok()?;
        self.members
            .get(index)
            .copied()
            .filter(|candidate| candidate.member == member)
    }

    /// The member currently accepting appends.
    ///
    /// Members are numbered from one, so a tape that has sealed nothing is
    /// appending to member one.
    #[must_use]
    pub fn next_member(&self) -> u64 {
        self.last().map_or(1, |member| member.member + 1)
    }

    /// Render the one byte-stable form of this index.
    #[must_use]
    pub fn render(&self) -> String {
        let mut text = String::from(FORMAT);
        text.push('\n');
        for member in &self.members {
            text.push_str(&format!(
                "member {:0width$} {}\n",
                member.member,
                member.length,
                width = DIGITS
            ));
        }
        text
    }

    /// Parse an index, refusing anything that is not the current form.
    ///
    /// This admits exactly what [`Index::render`] emits. The index is the map
    /// from a locus to the bytes it names, so a reading that is not the one
    /// writing produced is a reading of something else: every spelling this
    /// accepts is a spelling some writer could have meant, and only one writer
    /// exists. Anything else is refused by the line it appeared on rather than
    /// repaired, because tape cannot know which of two meanings was intended.
    ///
    /// # Errors
    ///
    /// Returns a refusal naming the first line that is not current.
    pub fn parse(text: &str) -> Result<Self, Error> {
        // Every rendered line is terminated, so text that stops without a
        // newline is truncated rather than current. `str::lines` yields the
        // same segments either way and would admit the truncation silently.
        let Some(body) = text.strip_suffix('\n') else {
            return Err(malformed(text.lines().next_back().unwrap_or("")));
        };

        let mut lines = body.split('\n');
        match lines.next() {
            Some(FORMAT) => {}
            Some(other) => return Err(malformed(other)),
            None => return Err(malformed("")),
        }

        let mut members: Vec<Member> = Vec::new();
        for line in lines {
            let member = parse_member(line)?;

            // Members are sealed one after another starting at one, so the
            // index is a contiguous run and its position states its number.
            // A gap would name a member whose bytes no locus can reach, and a
            // repeat or a descent would give one number two lengths.
            let expected = members.len() as u64 + 1;
            if member.member != expected {
                return Err(malformed(line));
            }
            members.push(member);
        }

        Ok(Self { members })
    }
}

/// Parse one `member` line.
fn parse_member(line: &str) -> Result<Member, Error> {
    let rest = line
        .strip_prefix("member ")
        .ok_or_else(|| malformed(line))?;
    let (number, length) = rest.split_once(' ').ok_or_else(|| malformed(line))?;

    if number.len() != DIGITS || !is_digits(number) || !is_length(length) {
        return Err(malformed(line));
    }
    let member = number.parse().map_err(|_| malformed(line))?;
    let length = length.parse().map_err(|_| malformed(line))?;

    Ok(Member { member, length })
}

/// True when every byte is an ASCII digit and there is at least one.
fn is_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// True when the text is the one spelling [`Index::render`] gives a length.
///
/// Lengths are rendered plainly, so a leading zero is a second spelling of a
/// number that already has one, and two spellings of one index would give the
/// same tape two byte-stable forms. Zero is refused outright rather than as a
/// spelling: [`Tape::seal`] reports an empty member as [`Seal::Empty`] and
/// seals nothing, so a sealed member of no bytes never existed to be named.
fn is_length(text: &str) -> bool {
    is_digits(text) && !text.starts_with('0')
}

/// A refusal naming the line that is not the current index form.
fn malformed(line: &str) -> Error {
    Error::Refused(Refusal::MalformedIndex {
        line: line.to_owned(),
    })
}

/// What one seal did.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Seal {
    /// No member was pending, so nothing rotated.
    Empty,
    /// One member rotated into an immutable seekable archive.
    Sealed {
        /// The member that rotated.
        member: u64,
        /// Plaintext bytes it carried.
        plaintext: u64,
        /// Bytes the archive occupies.
        compressed: u64,
    },
}

impl fmt::Display for Seal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("nothing to seal"),
            Self::Sealed {
                member,
                plaintext,
                compressed,
            } => write!(
                formatter,
                "sealed member {member}: {plaintext} plaintext byte(s) into {compressed}"
            ),
        }
    }
}

impl Tape {
    /// Every sealed member of this tape.
    ///
    /// # Errors
    ///
    /// Returns a refusal when the index on disk is not the current form, or an
    /// I/O failure when it cannot be read.
    pub fn index(&self) -> Result<Index, Error> {
        self.read_index()
    }

    /// Plaintext still retained by issued members, including interrupted seal
    /// cleanup. Reads only the current index and file metadata, not tape bytes.
    ///
    /// This is a snapshot, not a writer reservation. Callers scheduling
    /// maintenance must independently establish capture quiescence; `seal`
    /// still takes the exclusive writer lock and verifies replacement bytes.
    ///
    /// # Errors
    ///
    /// Refuses an invalid index, non-file member, unreadable metadata, or a
    /// total that cannot be represented. Only missing plaintext is absent.
    pub fn plaintext_bytes(&self) -> Result<u64, Error> {
        let index = self.read_index()?;
        self.plaintext_bytes_for(&index)
    }

    /// Plaintext still retained, after bounding the index bytes and extents
    /// needed to identify every issued member.
    ///
    /// # Errors
    ///
    /// Returns a typed limit refusal or the same index and metadata failures
    /// as [`Self::plaintext_bytes`].
    pub fn plaintext_bytes_bounded(
        &self,
        max_index_bytes: u64,
        max_extents: usize,
    ) -> Result<u64, Error> {
        let path = self.index_path();
        let index = match File::open(&path) {
            Ok(file) => read_bounded_index(file, &path, max_index_bytes, max_extents)?.0,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Index::default(),
            Err(error) => return Err(Error::io(Operation::ReadIndex, path, error)),
        };
        self.plaintext_bytes_for(&index)
    }

    fn plaintext_bytes_for(&self, index: &Index) -> Result<u64, Error> {
        let mut total = 0_u64;
        for member in index
            .members()
            .iter()
            .map(|member| member.member)
            .chain(std::iter::once(index.next_member()))
        {
            let path = self.plain_path(member);
            match fs::metadata(&path) {
                Ok(metadata) if metadata.is_file() => {
                    total = total.checked_add(metadata.len()).ok_or_else(|| {
                        Error::io(
                            Operation::InspectExtents,
                            &path,
                            io::Error::new(
                                io::ErrorKind::InvalidData,
                                "tape plaintext size overflow",
                            ),
                        )
                    })?;
                }
                Ok(_) => {
                    return Err(Error::io(
                        Operation::InspectExtents,
                        path,
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "tape plaintext is not a regular file",
                        ),
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(Error::io(Operation::InspectExtents, path, error)),
            }
        }
        Ok(total)
    }

    /// Snapshot every currently readable member bound in ascending order.
    ///
    /// The snapshot reads only metadata and the index. A seal racing this read
    /// may make the active member disappear after an older index was read; in
    /// that case the next snapshot observes the published index and the
    /// caller's durable cursor heals the missed transition.
    ///
    /// # Errors
    ///
    /// Returns a refusal when the index is not current, or an I/O failure when
    /// the index or active member metadata cannot be read.
    pub fn extents(&self) -> Result<Vec<Extent>, Error> {
        let index = self.read_index()?;
        let mut extents = index
            .members()
            .iter()
            .map(|member| Extent {
                member: member.member,
                length: member.length,
                sealed: true,
            })
            .collect::<Vec<_>>();

        let active = index.next_member();
        let path = self.plain_path(active);
        match fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => extents.push(Extent {
                member: active,
                length: metadata.len(),
                sealed: false,
            }),
            Ok(_) => {
                return Err(Error::io(
                    Operation::InspectExtents,
                    path,
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "active tape member is not a regular file",
                    ),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(Error::io(Operation::InspectExtents, path, error)),
        }

        Ok(extents)
    }

    /// Snapshot readable members without reading an oversized index or
    /// materializing more extents than the caller admitted.
    ///
    /// # Errors
    ///
    /// Returns a typed limit refusal or the same index and metadata failures
    /// as [`Self::extents`].
    pub fn extents_bounded(
        &self,
        max_index_bytes: u64,
        max_extents: usize,
    ) -> Result<BoundedExtents, Error> {
        let path = self.index_path();
        let (index, index_bytes) = match File::open(&path) {
            Ok(file) => read_bounded_index(file, &path, max_index_bytes, max_extents)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => (Index::default(), 0),
            Err(error) => return Err(Error::io(Operation::ReadIndex, path, error)),
        };
        let mut extents = index
            .members()
            .iter()
            .map(|member| Extent {
                member: member.member,
                length: member.length,
                sealed: true,
            })
            .collect::<Vec<_>>();
        let active = index.next_member();
        let active_path = self.plain_path(active);
        match fs::metadata(&active_path) {
            Ok(metadata) if metadata.is_file() => {
                if extents.len() == max_extents {
                    return Err(Error::Refused(Refusal::ExtentReadLimit {
                        limit: max_extents,
                        observed: max_extents.saturating_add(1),
                        index_bytes,
                    }));
                }
                extents.push(Extent {
                    member: active,
                    length: metadata.len(),
                    sealed: false,
                });
            }
            Ok(_) => {
                return Err(Error::io(
                    Operation::InspectExtents,
                    active_path,
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "active tape member is not a regular file",
                    ),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(Error::io(Operation::InspectExtents, active_path, error)),
        }
        Ok(BoundedExtents {
            extents,
            index_bytes,
        })
    }

    /// Read the index, treating an absent one as an empty tape.
    pub(crate) fn read_index(&self) -> Result<Index, Error> {
        let path = self.index_path();
        match fs::read_to_string(&path) {
            Ok(text) => Index::parse(&text),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Index::default()),
            Err(error) => Err(Error::io(Operation::ReadIndex, path, error)),
        }
    }

    /// Rotate the member currently accepting appends into a seekable archive.
    ///
    /// This is the whole member-level primitive and it owns no policy: any
    /// holder of the tape may call it at any time, and it seals whatever the
    /// active member holds. Sealing an empty or absent member is not a
    /// failure; it reports [`Seal::Empty`] and leaves the tape alone. A sealed
    /// member is never rewritten afterwards, so every locus already issued
    /// against it keeps resolving to the same bytes, and the next append
    /// starts the following member.
    ///
    /// The writer lock is held throughout, and the residue of any earlier
    /// interrupted seal is recovered first. Memory stays independent of the
    /// member's size: the plaintext is streamed through compression and
    /// verification, never held.
    ///
    /// # Errors
    ///
    /// Refuses when another writer holds the tape, when earlier residue
    /// disagrees with the index, or when the prepared archive does not
    /// reproduce the plaintext. Returns an I/O or publication failure when a
    /// step cannot complete. In every case the plaintext stays readable.
    pub fn seal(&self) -> Result<Seal, Error> {
        let _possession = self.possess()?;
        let index = self.read_index()?;
        self.recover(&index)?;

        let member = index.next_member();
        let plain = self.plain_path(member);
        let plaintext = match fs::metadata(&plain) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Seal::Empty),
            Err(error) => return Err(Error::io(Operation::ReadPlaintext, plain, error)),
        };
        if plaintext == 0 {
            return Ok(Seal::Empty);
        }

        let compressed = self.archive_member(member, plaintext)?;

        let mut sealed = index;
        sealed.members.push(Member {
            member,
            length: plaintext,
        });
        fault(Step::PublishIndex)?;
        self.publish(&self.index_path(), sealed.render().as_bytes())?;
        self.remove_plaintext(member)?;

        Ok(Seal::Sealed {
            member,
            plaintext,
            compressed,
        })
    }

    /// Finish a seal that published its index and stopped before removing
    /// the plaintext, and discard staged files no holder can still own.
    ///
    /// Plaintext whose member the index names cannot be accepting appends,
    /// so it is residue. It is removed only once the current published archive
    /// reproduces it exactly; otherwise that archive is prepared, verified and
    /// published again from the residue first. Only
    /// the last indexed member can hold residue, because every seal recovers
    /// before it rotates.
    ///
    /// # Errors
    ///
    /// Refuses residue whose length disagrees with the index, and returns
    /// any failure to inspect, archive, or remove; the residue then stays.
    fn recover(&self, index: &Index) -> Result<(), Error> {
        self.remove_staged()?;
        let Some(last) = index.last() else {
            return Ok(());
        };
        let residue = self.plain_path(last.member);
        let observed = match fs::metadata(&residue) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(Error::io(Operation::ReadPlaintext, residue, error)),
        };
        if observed != last.length {
            return Err(Error::Refused(Refusal::ResidueMismatch {
                member: last.member,
                indexed: last.length,
                observed,
            }));
        }
        if !self.published_reproduces(last.member) {
            self.archive_member(last.member, last.length)?;
        }
        self.remove_plaintext(last.member)
    }

    /// Prepare, verify, and publish one member's archive from its plaintext.
    ///
    /// The plaintext and the index are left untouched; a failure removes the
    /// staged file and publishes nothing.
    fn archive_member(&self, member: u64, plaintext: u64) -> Result<u64, Error> {
        let staged = self.root.join(crate::temporary_name());
        let prepared = self
            .prepare_archive(member, plaintext, &staged)
            .and_then(|compressed| {
                self.verify_archive(member, plaintext, &staged)?;
                fault(Step::PublishArchive)?;
                Ok(compressed)
            });
        let compressed = match prepared {
            Ok(compressed) => compressed,
            Err(error) => {
                let _ = fs::remove_file(&staged);
                return Err(error);
            }
        };
        self.install(&staged, &self.archive_path(member))?;
        Ok(compressed)
    }

    /// Stream `plaintext` bytes of a member into a synchronized staged archive.
    fn prepare_archive(&self, member: u64, plaintext: u64, staged: &Path) -> Result<u64, Error> {
        let plain = self.plain_path(member);
        let mut source = File::open(&plain)
            .map_err(|error| Error::io(Operation::ReadPlaintext, &plain, error))?
            .take(plaintext);
        let mut file = create_private(staged)
            .map_err(|error| Error::io(Operation::PrepareArchive, staged, error))?;
        let compressed = archive::write(&mut source, &mut file)
            .map_err(|error| Error::io(Operation::PrepareArchive, staged, error))?;
        fault(Step::Prepare)?;
        file.sync_all()
            .map_err(|error| Error::io(Operation::PrepareArchive, staged, error))?;
        Ok(compressed)
    }

    /// Prove a staged archive reproduces the member's plaintext exactly.
    ///
    /// Both files are reopened, so the comparison reads what the disk holds
    /// rather than anything the compressor kept, and it decodes through the
    /// same validated frames every reader uses. The plaintext must end where
    /// the archive does.
    fn verify_archive(&self, member: u64, plaintext: u64, staged: &Path) -> Result<(), Error> {
        let failed = |error| Error::io(Operation::VerifyArchive, staged, error);
        let archive = File::open(staged).and_then(Archive::open).map_err(failed)?;
        if archive.length() != plaintext {
            return Err(Error::Refused(Refusal::ArchiveMismatch { member }));
        }
        let plain = self.plain_path(member);
        let mut source = File::open(&plain)
            .map_err(|error| Error::io(Operation::ReadPlaintext, &plain, error))?;
        let mut frames = archive.reader_from(0).map_err(failed)?;
        if archive::identical(&mut frames, &mut source).map_err(failed)? {
            Ok(())
        } else {
            Err(Error::Refused(Refusal::ArchiveMismatch { member }))
        }
    }

    /// Whether published storage reproduces a member's plaintext exactly.
    ///
    /// Any failure answers no: the plaintext is still present, so the
    /// remedy for an unreadable or unfaithful archive is to prepare it again,
    /// never to trust it.
    fn published_reproduces(&self, member: u64) -> bool {
        File::open(self.archive_path(member))
            .and_then(Archive::open)
            .and_then(|archive| archive.reader_from(0))
            .and_then(|mut frames| {
                let mut plain = File::open(self.plain_path(member))?;
                archive::identical(&mut frames, &mut plain)
            })
            .unwrap_or(false)
    }

    /// Remove a sealed member's plaintext.
    fn remove_plaintext(&self, member: u64) -> Result<(), Error> {
        fault(Step::RemovePlaintext)?;
        let plain = self.plain_path(member);
        fs::remove_file(&plain).map_err(|error| Error::io(Operation::RemovePlaintext, plain, error))
    }

    /// Remove staged files left by a holder that stopped before publishing.
    ///
    /// Every staging happens under the writer lock, which the caller holds,
    /// so no staged file can belong to work still in progress.
    fn remove_staged(&self) -> Result<(), Error> {
        let entries = fs::read_dir(&self.root)
            .map_err(|error| Error::io(Operation::RemoveStaged, &self.root, error))?;
        for entry in entries {
            let entry =
                entry.map_err(|error| Error::io(Operation::RemoveStaged, &self.root, error))?;
            let name = entry.file_name();
            let staged = name.to_str().is_some_and(|name| {
                name.starts_with(STAGED_PREFIX) && name.ends_with(STAGED_SUFFIX)
            });
            if !staged {
                continue;
            }
            match fs::remove_file(entry.path()) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(Error::io(Operation::RemoveStaged, entry.path(), error)),
            }
        }
        Ok(())
    }
}

fn read_bounded_index(
    file: File,
    path: &std::path::Path,
    max_bytes: u64,
    max_extents: usize,
) -> Result<(Index, u64), Error> {
    let observed = file
        .metadata()
        .map_err(|error| Error::io(Operation::ReadIndex, path, error))?
        .len();
    if observed > max_bytes {
        return Err(Error::Refused(Refusal::IndexReadLimit {
            limit: max_bytes,
            observed,
        }));
    }
    let mut text = String::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_string(&mut text)
        .map_err(|error| Error::io(Operation::ReadIndex, path, error))?;
    let index_bytes = u64::try_from(text.len()).expect("a string length fits u64");
    if index_bytes > max_bytes {
        return Err(Error::Refused(Refusal::IndexReadLimit {
            limit: max_bytes,
            observed: index_bytes,
        }));
    }
    let observed_extents = text.lines().count().saturating_sub(1);
    if observed_extents > max_extents {
        return Err(Error::Refused(Refusal::ExtentReadLimit {
            limit: max_extents,
            observed: max_extents.saturating_add(1),
            index_bytes,
        }));
    }
    Ok((Index::parse(&text)?, index_bytes))
}

/// The seal steps whose failure the in-process tests force.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Step {
    /// Writing the staged archive.
    Prepare,
    /// Renaming the verified archive into place.
    PublishArchive,
    /// Publishing the index naming the sealed member.
    PublishIndex,
    /// Removing the sealed member's plaintext.
    RemovePlaintext,
}

#[cfg(test)]
thread_local! {
    /// The step the current test thread forces to fail.
    static FAIL: std::cell::Cell<Option<Step>> = const { std::cell::Cell::new(None) };
}

/// Fail `step` when the current test forces it to; always succeed otherwise.
#[cfg_attr(
    not(test),
    expect(
        clippy::unnecessary_wraps,
        reason = "the test build fails steps through the same signature"
    )
)]
fn fault(step: Step) -> Result<(), Error> {
    #[cfg(test)]
    if FAIL.get() == Some(step) {
        return Err(Error::io(
            Operation::SyncDirectory,
            format!("{step:?}"),
            io::Error::other("forced failure"),
        ));
    }
    #[cfg(not(test))]
    let _ = step;
    Ok(())
}

#[cfg(test)]
mod tests;

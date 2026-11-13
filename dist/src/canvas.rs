//! The spice rack linked into one appliance executable.
//!
//! The release target build and `spice dev xtask -- install` link the object
//! [`rack::link::object`] emits. A build reads that section back out of its
//! final image, after symbols, stripping, and signing, and refuses unless it
//! is exactly the archive it assembled.

use wire::manifest::ProductVersion;
use wire::{Digest, Domain};

const IMAGE_FILE_MACHINE_AMD64: u16 = 0x8664;
const IMAGE_NT_OPTIONAL_HDR64_MAGIC: u16 = 0x20b;
const COFF_HEADER_BYTES: usize = 20;
const PE32_PLUS_OPTIONAL_HEADER_BYTES: usize = 112;
const DATA_DIRECTORY_BYTES: usize = 8;
const SECURITY_DIRECTORY: usize = 4;

/// The assembled rack one build links: app generation G and its exact bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Canvas {
    generation: Digest,
    archive: Digest,
    bytes: u64,
}

impl Canvas {
    /// Name the archive bytes a build admitted as `generation`.
    #[must_use]
    pub fn new(generation: Digest, archive: &[u8]) -> Self {
        Self {
            generation,
            archive: Digest::compute(Domain::DistArtifact, archive),
            bytes: archive.len() as u64,
        }
    }

    /// The app generation the archive carries.
    #[must_use]
    pub const fn generation(&self) -> &Digest {
        &self.generation
    }

    /// Identity of the exact archive bytes.
    #[must_use]
    pub const fn archive(&self) -> &Digest {
        &self.archive
    }

    /// Length of the archive, and so of the executable growth it causes.
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Refuse a final executable whose rack section is not exactly this archive.
    ///
    /// # Errors
    ///
    /// Names an image that yields no rack section, or different bytes.
    pub fn verify_linked(&self, executable: &[u8]) -> Result<(), CanvasError> {
        let observed = Digest::compute(Domain::DistArtifact, rack::link::extract(executable)?);
        if observed == self.archive {
            Ok(())
        } else {
            Err(CanvasError::Mismatch {
                expected: self.archive.clone(),
                observed,
            })
        }
    }

    /// Refuse a final executable whose version report does not name this generation.
    ///
    /// # Errors
    ///
    /// Names the report when its last word is not `app G`.
    pub fn verify_report(&self, report: &str) -> Result<(), CanvasError> {
        let named = report
            .trim_end()
            .rsplit_once(" app ")
            .is_some_and(|(_, generation)| generation == self.generation.as_str());
        if named {
            Ok(())
        } else {
            Err(CanvasError::Report(report.to_owned()))
        }
    }

    /// Refuse a release executable whose report differs from its complete
    /// declared product identity.
    ///
    /// One ordinary line terminator is transport, not identity. Every other
    /// byte must name the admitted product version and this app generation.
    ///
    /// # Errors
    ///
    /// Returns the expected and observed reports when either identity moved.
    pub fn verify_release_report(
        &self,
        version: &ProductVersion,
        report: &str,
    ) -> Result<(), CanvasError> {
        let expected = format!("spice {} app {}", version.as_str(), self.generation);
        let observed = report.strip_suffix('\n').unwrap_or(report);
        let observed = observed.strip_suffix('\r').unwrap_or(observed);
        if observed == expected {
            Ok(())
        } else {
            Err(CanvasError::ReleaseReport {
                expected,
                observed: report.to_owned(),
            })
        }
    }

    /// Enforce the Windows release policy: the PE image carries no embedded
    /// Authenticode certificate. Release authenticity is supplied by the
    /// detached public GitHub Sigstore proof after all image bytes are final.
    ///
    /// # Errors
    ///
    /// Refuses a non-x86-64 PE32+ image, malformed certificate directory, or
    /// any embedded Authenticode certificate.
    pub fn verify_windows_authenticode(&self, executable: &[u8]) -> Result<(), CanvasError> {
        require_unsigned(executable)
    }
}

/// Why a final executable was refused.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CanvasError {
    #[error(transparent)]
    Link(#[from] rack::link::Error),
    #[error("executable is not an x86-64 PE32+ image")]
    NotPe,
    #[error("executable {0} is malformed")]
    Malformed(&'static str),
    #[error(
        "executable spice rack section {observed} differs from the assembled archive {expected}"
    )]
    Mismatch { expected: Digest, observed: Digest },
    #[error("Windows release policy forbids an embedded Authenticode certificate")]
    Authenticode,
    #[error("executable version report does not name its app generation: {0:?}")]
    Report(String),
    #[error(
        "release executable version report differs from expected {expected:?}: observed {observed:?}"
    )]
    ReleaseReport { expected: String, observed: String },
}

fn require_unsigned(image: &[u8]) -> Result<(), CanvasError> {
    if field(image, 0, 2, "DOS header")? != b"MZ" {
        return Err(CanvasError::NotPe);
    }
    let pe = read_u32(image, 0x3c, "DOS header")? as usize;
    if field(image, pe, 4, "PE signature")? != b"PE\0\0" {
        return Err(CanvasError::Malformed("PE signature"));
    }
    let coff = pe
        .checked_add(4)
        .ok_or(CanvasError::Malformed("PE header offset"))?;
    if read_u16(image, coff, "PE file header")? != IMAGE_FILE_MACHINE_AMD64 {
        return Err(CanvasError::NotPe);
    }
    let optional_bytes = usize::from(read_u16(image, coff + 16, "PE file header")?);
    if optional_bytes < PE32_PLUS_OPTIONAL_HEADER_BYTES {
        return Err(CanvasError::Malformed("PE32+ optional header"));
    }
    let optional = coff
        .checked_add(COFF_HEADER_BYTES)
        .ok_or(CanvasError::Malformed("PE optional header offset"))?;
    field(image, optional, optional_bytes, "PE32+ optional header")?;
    if read_u16(image, optional, "PE32+ optional header")? != IMAGE_NT_OPTIONAL_HDR64_MAGIC {
        return Err(CanvasError::NotPe);
    }
    let directories = read_u32(image, optional + 108, "PE32+ data directory count")? as usize;
    let declared = directories
        .checked_mul(DATA_DIRECTORY_BYTES)
        .and_then(|bytes| bytes.checked_add(PE32_PLUS_OPTIONAL_HEADER_BYTES))
        .ok_or(CanvasError::Malformed("PE32+ data directories"))?;
    if declared > optional_bytes {
        return Err(CanvasError::Malformed("PE32+ data directories"));
    }
    if directories <= SECURITY_DIRECTORY {
        return Ok(());
    }
    let entry =
        optional + PE32_PLUS_OPTIONAL_HEADER_BYTES + SECURITY_DIRECTORY * DATA_DIRECTORY_BYTES;
    let offset = read_u32(image, entry, "Authenticode directory")? as usize;
    let bytes = read_u32(image, entry + 4, "Authenticode directory")? as usize;
    match (offset, bytes) {
        (0, 0) => Ok(()),
        (0, _) | (_, 0) => Err(CanvasError::Malformed("Authenticode directory")),
        _ => {
            field(image, offset, bytes, "Authenticode certificate table")?;
            Err(CanvasError::Authenticode)
        }
    }
}

fn field<'a>(
    image: &'a [u8],
    offset: usize,
    bytes: usize,
    label: &'static str,
) -> Result<&'a [u8], CanvasError> {
    offset
        .checked_add(bytes)
        .and_then(|end| image.get(offset..end))
        .ok_or(CanvasError::Malformed(label))
}

fn read_u16(image: &[u8], offset: usize, label: &'static str) -> Result<u16, CanvasError> {
    let bytes = field(image, offset, 2, label)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32(image: &[u8], offset: usize, label: &'static str) -> Result<u32, CanvasError> {
    let bytes = field(image, offset, 4, label)?;
    Ok(u32::from_le_bytes(
        bytes.try_into().expect("the field is four bytes"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARCHIVE: &[u8] = b"PK\x03\x04object layout boundary";
    const PE_HEADER: usize = 0x80;
    const OPTIONAL: usize = PE_HEADER + 4 + COFF_HEADER_BYTES;
    const SECURITY: usize =
        OPTIONAL + PE32_PLUS_OPTIONAL_HEADER_BYTES + SECURITY_DIRECTORY * DATA_DIRECTORY_BYTES;

    /// A PE32+ x86-64 header with sixteen empty data directories.
    fn pe() -> Vec<u8> {
        let optional_bytes = PE32_PLUS_OPTIONAL_HEADER_BYTES + 16 * DATA_DIRECTORY_BYTES;
        let mut image = vec![0; OPTIONAL + optional_bytes];
        image[0..2].copy_from_slice(b"MZ");
        image[0x3c..0x40].copy_from_slice(&(PE_HEADER as u32).to_le_bytes());
        image[PE_HEADER..PE_HEADER + 4].copy_from_slice(b"PE\0\0");
        let coff = PE_HEADER + 4;
        image[coff..coff + 2].copy_from_slice(&IMAGE_FILE_MACHINE_AMD64.to_le_bytes());
        image[coff + 16..coff + 18].copy_from_slice(&(optional_bytes as u16).to_le_bytes());
        image[OPTIONAL..OPTIONAL + 2].copy_from_slice(&IMAGE_NT_OPTIONAL_HDR64_MAGIC.to_le_bytes());
        image[OPTIONAL + 108..OPTIONAL + 112].copy_from_slice(&16_u32.to_le_bytes());
        image
    }

    fn put_u32(image: &mut [u8], offset: usize, value: u32) {
        image[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn canvas_metadata_and_generation_report_are_exact() {
        let generation = Digest::compute(Domain::DistArtifact, b"generation");
        let canvas = Canvas::new(generation.clone(), ARCHIVE);

        assert_eq!(canvas.generation(), &generation);
        assert_eq!(
            canvas.archive(),
            &Digest::compute(Domain::DistArtifact, ARCHIVE)
        );
        assert_eq!(canvas.bytes(), ARCHIVE.len() as u64);
        assert_eq!(
            canvas.verify_report(&format!("spice development app {generation}\n")),
            Ok(())
        );
        assert!(matches!(
            canvas.verify_report(&format!("spice development app {}\n", "0".repeat(64))),
            Err(CanvasError::Report(_))
        ));
    }

    #[test]
    fn a_linked_image_must_carry_exactly_the_assembled_archive() {
        let canvas = Canvas::new(Digest::compute(Domain::DistArtifact, b"g"), ARCHIVE);
        let target = rack::link::Target::LinuxX86_64;
        assert_eq!(
            canvas.verify_linked(&rack::link::object(ARCHIVE, target)),
            Ok(())
        );
        assert!(matches!(
            canvas.verify_linked(&rack::link::object(b"PK\x03\x04different", target)),
            Err(CanvasError::Mismatch { .. })
        ));
        assert_eq!(
            canvas.verify_linked(b"neither image"),
            Err(CanvasError::Link(rack::link::Error::Format))
        );
    }

    #[test]
    fn windows_release_policy_refuses_embedded_authenticode() {
        let unsigned = pe();
        assert_eq!(require_unsigned(&unsigned), Ok(()));

        let mut signed = unsigned;
        let certificate = signed.len();
        signed.extend_from_slice(b"WIN_CERTIFICATE");
        put_u32(&mut signed, SECURITY, certificate as u32);
        put_u32(&mut signed, SECURITY + 4, b"WIN_CERTIFICATE".len() as u32);
        assert_eq!(require_unsigned(&signed), Err(CanvasError::Authenticode));

        put_u32(&mut signed, SECURITY, 0);
        assert_eq!(
            require_unsigned(&signed),
            Err(CanvasError::Malformed("Authenticode directory"))
        );

        let mut overflowing = pe();
        put_u32(&mut overflowing, OPTIONAL + 108, 17);
        assert_eq!(
            require_unsigned(&overflowing),
            Err(CanvasError::Malformed("PE32+ data directories"))
        );
        assert_eq!(require_unsigned(b"ELF"), Err(CanvasError::NotPe));
    }
}

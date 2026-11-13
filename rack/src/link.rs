//! Linking a rack into an executable.
//!
//! A build links one relocatable object that places the archive in its own
//! retained read-only section, with its length beside it, both named by the
//! global data symbols the `linked` feature declares. The archive inside the
//! section is a standard ZIP whose offsets are relative to its own start; the
//! executable around it is not, so [`extract`] reads the section back out of
//! a final image after symbols, stripping, and signing.
mod coff;

use std::fmt;

/// Segment holding the rack section on Mach-O.
const MACHO_SEGMENT: &[u8] = b"__TEXT";
/// Rack section on Mach-O: lowercase after two underscores, as Apple's
/// Mach-O File Format Reference names sections.
const MACHO_SECTION: &[u8] = b"__spicezip";
/// Rack section on ELF: undotted, since the System V gABI reserves
/// dot-prefixed section names for the system. The same word names the PE32+
/// section, exactly filling its eight-byte limit.
const ELF_SECTION: &[u8] = b"spicezip";
/// First rack byte, as the `linked` feature declares it.
const ARCHIVE_SYMBOL: &[u8] = b"spicezip";
/// The rack length as a little-endian `u64`, as the `linked` feature declares it.
const LENGTH_SYMBOL: &[u8] = b"spicezip_len";

const MACHO_MAGIC_64: u32 = 0xfeed_facf;
const MACHO_HEADER_BYTES: usize = 32;
const LC_SEGMENT_64: u32 = 0x19;
const LC_SYMTAB: u32 = 0x2;
const LC_DYSYMTAB: u32 = 0xb;
const LC_BUILD_VERSION: u32 = 0x32;
const SEGMENT_COMMAND_BYTES: usize = 72;
const SECTION_64_BYTES: usize = 80;
const ELF_MAGIC: &[u8] = b"\x7fELF";
const ELF_CLASS_64: u8 = 2;
const ELF_LITTLE_ENDIAN: u8 = 1;
const ELF_SECTION_HEADER_BYTES: u64 = 64;
const SHT_PROGBITS: u32 = 1;
const SHT_SYMTAB: u32 = 2;
const SHT_STRTAB: u32 = 3;
const SHT_NOBITS: u32 = 8;
const SHF_ALLOC: u64 = 2;

/// An executable target the rack object is written for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Target {
    MacAarch64,
    MacX86_64,
    LinuxAarch64,
    LinuxX86_64,
    WindowsX86_64,
}

impl Target {
    /// The target `os` and `arch` name, spelled as [`std::env::consts`] spells them.
    #[must_use]
    pub fn new(os: &str, arch: &str) -> Option<Self> {
        match (os, arch) {
            ("macos", "aarch64") => Some(Self::MacAarch64),
            ("macos", "x86_64") => Some(Self::MacX86_64),
            ("linux", "aarch64") => Some(Self::LinuxAarch64),
            ("linux", "x86_64") => Some(Self::LinuxX86_64),
            ("windows", "x86_64") => Some(Self::WindowsX86_64),
            _ => None,
        }
    }

    /// The name this target gives its object file.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::MacAarch64 => "macos-aarch64",
            Self::MacX86_64 => "macos-x86_64",
            Self::LinuxAarch64 => "linux-aarch64",
            Self::LinuxX86_64 => "linux-x86_64",
            Self::WindowsX86_64 => "windows-x86_64",
        }
    }

    /// The object-file extension this target's linker expects.
    #[must_use]
    pub const fn object_extension(self) -> &'static str {
        match self {
            Self::WindowsX86_64 => "obj",
            _ => "o",
        }
    }
}

/// The relocatable object that links `archive` into one executable for `target`.
///
/// The rack section holds the archive bytes alone, so the final image's
/// section is byte-for-byte the archive; its length sits in a read-only
/// constant section. Both are named by global data symbols, which is what
/// keeps the linker from discarding them.
#[must_use]
pub fn object(archive: &[u8], target: Target) -> Vec<u8> {
    match target {
        // The oldest macOS each architecture's Rust target supports, so the
        // linker never sees an object newer than the executable it links.
        Target::MacAarch64 => macho_object(archive, 0x0100_000c, 0, 0x000b_0000),
        Target::MacX86_64 => macho_object(archive, 0x0100_0007, 3, 0x000a_0c00),
        Target::LinuxAarch64 => elf_object(archive, 183),
        Target::LinuxX86_64 => elf_object(archive, 62),
        Target::WindowsX86_64 => coff::object(archive),
    }
}

fn macho_object(archive: &[u8], cpu: u32, subtype: u32, minos: u32) -> Vec<u8> {
    const COMMANDS: usize = SEGMENT_COMMAND_BYTES + 2 * SECTION_64_BYTES + 24 + 24 + 80;
    let data = MACHO_HEADER_BYTES + COMMANDS;
    let length_at = archive.len().next_multiple_of(8);
    let segment = length_at + 8;
    let symbols = data + segment;
    let strings = [&b"\0_"[..], ARCHIVE_SYMBOL, b"\0_", LENGTH_SYMBOL, b"\0"].concat();
    let strings_at = symbols + 2 * 16;
    let string_bytes = strings.len().next_multiple_of(8);

    let mut out = Vec::with_capacity(strings_at + string_bytes);
    put32(
        &mut out,
        &[MACHO_MAGIC_64, cpu, subtype, 1, 4, word(COMMANDS), 0, 0],
    );
    put32(
        &mut out,
        &[
            LC_SEGMENT_64,
            word(SEGMENT_COMMAND_BYTES + 2 * SECTION_64_BYTES),
        ],
    );
    out.extend_from_slice(&[0; 16]);
    put64(&mut out, &[0, wide(segment), wide(data), wide(segment)]);
    put32(&mut out, &[7, 7, 2, 0]);
    for (name, address, size, align) in [
        (MACHO_SECTION, 0, archive.len(), 0),
        (&b"__const"[..], length_at, 8, 3),
    ] {
        out.extend_from_slice(&padded_name(name));
        out.extend_from_slice(&padded_name(MACHO_SEGMENT));
        put64(&mut out, &[wide(address), wide(size)]);
        put32(&mut out, &[word(data + address), align, 0, 0, 0, 0, 0, 0]);
    }
    put32(&mut out, &[LC_BUILD_VERSION, 24, 1, minos, 0, 0]);
    put32(
        &mut out,
        &[
            LC_SYMTAB,
            24,
            word(symbols),
            2,
            word(strings_at),
            word(string_bytes),
        ],
    );
    put32(&mut out, &[LC_DYSYMTAB, 80, 0, 0, 0, 2, 2, 0]);
    out.extend_from_slice(&[0; 48]);
    out.extend_from_slice(archive);
    out.resize(data + length_at, 0);
    put64(&mut out, &[wide(archive.len())]);
    // N_SECT | N_EXT, in section 1 at its start and section 2 at its address.
    for (name, section, value) in [(1, 1_u8, 0), (ARCHIVE_SYMBOL.len() + 3, 2, length_at)] {
        put32(&mut out, &[word(name)]);
        out.extend_from_slice(&[0x0f, section, 0, 0]);
        put64(&mut out, &[wide(value)]);
    }
    out.extend_from_slice(&strings);
    out.resize(strings_at + string_bytes, 0);
    out
}

fn elf_object(archive: &[u8], machine: u16) -> Vec<u8> {
    let (section_names, section_offsets) = string_table(&[
        ELF_SECTION,
        b".rodata.spicezip_len",
        b".note.GNU-stack",
        b".symtab",
        b".strtab",
        b".shstrtab",
    ]);
    let (symbol_names, symbol_offsets) = string_table(&[ARCHIVE_SYMBOL, LENGTH_SYMBOL]);
    let length_at = (64 + archive.len()).next_multiple_of(8);
    let symbols_at = length_at + 8;
    let symbol_names_at = symbols_at + 3 * 24;
    let section_names_at = symbol_names_at + symbol_names.len();
    let headers_at = (section_names_at + section_names.len()).next_multiple_of(8);

    let mut out = Vec::with_capacity(headers_at + 7 * 64);
    out.extend_from_slice(ELF_MAGIC);
    out.extend_from_slice(&[ELF_CLASS_64, ELF_LITTLE_ENDIAN, 1]);
    out.resize(16, 0);
    // A relocatable object for `machine`.
    for half in [1, machine] {
        out.extend_from_slice(&half.to_le_bytes());
    }
    put32(&mut out, &[1]);
    put64(&mut out, &[0, 0, wide(headers_at)]);
    put32(&mut out, &[0]);
    // Header, no program headers, seven section headers, names in the last.
    for half in [64_u16, 0, 0, 64, 7, 6] {
        out.extend_from_slice(&half.to_le_bytes());
    }
    out.extend_from_slice(archive);
    out.resize(length_at, 0);
    put64(&mut out, &[wide(archive.len())]);
    out.resize(symbols_at + 24, 0);
    for (name, section, size) in [
        (symbol_offsets[0], 1_u16, archive.len()),
        (symbol_offsets[1], 2, 8),
    ] {
        put32(&mut out, &[name]);
        // STB_GLOBAL, STT_OBJECT, default visibility.
        out.extend_from_slice(&[0x11, 0]);
        out.extend_from_slice(&section.to_le_bytes());
        put64(&mut out, &[0, wide(size)]);
    }
    out.extend_from_slice(&symbol_names);
    out.extend_from_slice(&section_names);
    out.resize(headers_at + 64, 0);
    let sections = [
        (SHT_PROGBITS, SHF_ALLOC, 64, archive.len(), 0, 0, 1, 0),
        (SHT_PROGBITS, SHF_ALLOC, length_at, 8, 0, 0, 8, 0),
        // Without this note GNU linkers infer an executable stack.
        (SHT_PROGBITS, 0, symbols_at, 0, 0, 0, 1, 0),
        (SHT_SYMTAB, 0, symbols_at, 3 * 24, 5, 1, 8, 24),
        (
            SHT_STRTAB,
            0,
            symbol_names_at,
            symbol_names.len(),
            0,
            0,
            1,
            0,
        ),
        (
            SHT_STRTAB,
            0,
            section_names_at,
            section_names.len(),
            0,
            0,
            1,
            0,
        ),
    ];
    for ((kind, flags, offset, size, link, info, align, entry), name) in
        sections.into_iter().zip(section_offsets)
    {
        put32(&mut out, &[name, kind]);
        put64(&mut out, &[flags, 0, wide(offset), wide(size)]);
        put32(&mut out, &[link, info]);
        put64(&mut out, &[align, entry]);
    }
    out
}

/// A NUL-led string table and each name's offset in it.
fn string_table<const N: usize>(names: &[&[u8]; N]) -> (Vec<u8>, [u32; N]) {
    let mut table = vec![0];
    let offsets = names.map(|name| {
        let offset = word(table.len());
        table.extend_from_slice(name);
        table.push(0);
        offset
    });
    (table, offsets)
}

fn padded_name(name: &[u8]) -> [u8; 16] {
    let mut field = [0; 16];
    field[..name.len()].copy_from_slice(name);
    field
}

fn put32(out: &mut Vec<u8>, values: &[u32]) {
    for value in values {
        out.extend_from_slice(&value.to_le_bytes());
    }
}

fn put64(out: &mut Vec<u8>, values: &[u64]) {
    for value in values {
        out.extend_from_slice(&value.to_le_bytes());
    }
}

fn word(value: usize) -> u32 {
    u32::try_from(value).expect("a rack object offset fits in 32 bits")
}

fn wide(value: usize) -> u64 {
    value as u64
}

/// Why an executable image yields no rack.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    /// The image is neither a supported 64-bit Mach-O, ELF, nor PE image.
    Format,
    /// The named header or record is malformed.
    Malformed(&'static str),
    /// The image carries no rack section.
    Absent,
    /// The image carries more than one rack section.
    Repeated,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Format => formatter
                .write_str("executable is neither a supported 64-bit Mach-O, ELF, nor PE image"),
            Self::Malformed(what) => write!(formatter, "executable {what} is malformed"),
            Self::Absent => formatter.write_str("executable carries no rack section"),
            Self::Repeated => formatter.write_str("executable carries more than one rack section"),
        }
    }
}

impl std::error::Error for Error {}

/// The archive bytes inside a final executable's rack section.
///
/// # Errors
///
/// Names an image that is not a supported 64-bit Mach-O, ELF, or PE image, a
/// malformed header, or an absent or repeated section.
pub fn extract(executable: &[u8]) -> Result<&[u8], Error> {
    if read_u32(executable, 0).ok() == Some(MACHO_MAGIC_64) {
        return macho(executable);
    }
    if executable.starts_with(ELF_MAGIC) {
        return elf(executable);
    }
    if executable.starts_with(b"MZ") {
        return coff::pe_archive(executable);
    }
    Err(Error::Format)
}

fn macho(image: &[u8]) -> Result<&[u8], Error> {
    let commands = read_u32(image, 16)?;
    let mut command = MACHO_HEADER_BYTES;
    let mut found = None;
    for _ in 0..commands {
        let kind = read_u32(image, command)?;
        let size = read_u32(image, command + 4)? as usize;
        if size < 8 {
            return Err(Error::Malformed("load command"));
        }
        if kind == LC_SEGMENT_64 {
            let sections = read_u32(image, command + 64)? as usize;
            let span = sections
                .checked_mul(SECTION_64_BYTES)
                .and_then(|bytes| bytes.checked_add(SEGMENT_COMMAND_BYTES))
                .filter(|span| *span <= size)
                .ok_or(Error::Malformed("segment command"))?;
            for header in
                (command + SEGMENT_COMMAND_BYTES..command + span).step_by(SECTION_64_BYTES)
            {
                if name(image, header)? == MACHO_SECTION
                    && name(image, header + 16)? == MACHO_SEGMENT
                {
                    let bytes = read_u64(image, header + 40)?;
                    let offset = u64::from(read_u32(image, header + 48)?);
                    record(&mut found, range(image, offset, bytes)?)?;
                }
            }
        }
        command = command
            .checked_add(size)
            .ok_or(Error::Malformed("load command"))?;
    }
    found.ok_or(Error::Absent)
}

fn elf(image: &[u8]) -> Result<&[u8], Error> {
    if image.get(4) != Some(&ELF_CLASS_64) || image.get(5) != Some(&ELF_LITTLE_ENDIAN) {
        return Err(Error::Format);
    }
    let table = read_u64(image, 0x28)?;
    let entry = u64::from(read_u16(image, 0x3a)?);
    let count = u64::from(read_u16(image, 0x3c)?);
    let names = u64::from(read_u16(image, 0x3e)?);
    if entry != ELF_SECTION_HEADER_BYTES || count == 0 || names >= count {
        return Err(Error::Malformed("section header table"));
    }
    let header = |index: u64| -> Result<usize, Error> {
        index
            .checked_mul(ELF_SECTION_HEADER_BYTES)
            .and_then(|offset| offset.checked_add(table))
            .and_then(|offset| usize::try_from(offset).ok())
            .ok_or(Error::Malformed("section header table"))
    };
    let names = header(names)?;
    let names = range(
        image,
        read_u64(image, names + 24)?,
        read_u64(image, names + 32)?,
    )?;
    let mut found = None;
    for index in 0..count {
        let section = header(index)?;
        let start = read_u32(image, section)? as usize;
        let name = names
            .get(start..)
            .and_then(|tail| tail.split(|byte| *byte == 0).next())
            .ok_or(Error::Malformed("section name"))?;
        if name == ELF_SECTION {
            if read_u32(image, section + 4)? == SHT_NOBITS {
                return Err(Error::Malformed("rack section holds no file bytes"));
            }
            let offset = read_u64(image, section + 24)?;
            let bytes = read_u64(image, section + 32)?;
            record(&mut found, range(image, offset, bytes)?)?;
        }
    }
    found.ok_or(Error::Absent)
}

fn record<'a>(found: &mut Option<&'a [u8]>, section: &'a [u8]) -> Result<(), Error> {
    if found.replace(section).is_some() {
        Err(Error::Repeated)
    } else {
        Ok(())
    }
}

/// One fixed 16-byte Mach-O name, without its NUL padding.
fn name(image: &[u8], offset: usize) -> Result<&[u8], Error> {
    let field = field(image, offset, 16)?;
    Ok(field.split(|byte| *byte == 0).next().unwrap_or(field))
}

fn range(image: &[u8], offset: u64, bytes: u64) -> Result<&[u8], Error> {
    let start = usize::try_from(offset).map_err(|_| Error::Malformed("section range"))?;
    let length = usize::try_from(bytes).map_err(|_| Error::Malformed("section range"))?;
    start
        .checked_add(length)
        .and_then(|end| image.get(start..end))
        .ok_or(Error::Malformed("section range"))
}

fn field(image: &[u8], offset: usize, length: usize) -> Result<&[u8], Error> {
    offset
        .checked_add(length)
        .and_then(|end| image.get(offset..end))
        .ok_or(Error::Malformed("header"))
}

fn read_u16(image: &[u8], offset: usize) -> Result<u16, Error> {
    let bytes = field(image, offset, 2)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32(image: &[u8], offset: usize) -> Result<u32, Error> {
    let bytes = field(image, offset, 4)?;
    Ok(u32::from_le_bytes(
        bytes.try_into().expect("the field is four bytes"),
    ))
}

fn read_u64(image: &[u8], offset: usize) -> Result<u64, Error> {
    let bytes = field(image, offset, 8)?;
    Ok(u64::from_le_bytes(
        bytes.try_into().expect("the field is eight bytes"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARCHIVE: &[u8] = b"PK\x03\x04object layout boundary";

    #[test]
    fn object_targets_name_their_linker_artifacts() {
        for (target, name, extension) in [
            (Target::MacAarch64, "macos-aarch64", "o"),
            (Target::MacX86_64, "macos-x86_64", "o"),
            (Target::LinuxAarch64, "linux-aarch64", "o"),
            (Target::LinuxX86_64, "linux-x86_64", "o"),
            (Target::WindowsX86_64, "windows-x86_64", "obj"),
        ] {
            assert_eq!(target.name(), name);
            assert_eq!(target.object_extension(), extension);
        }
    }

    #[test]
    fn executable_parsers_refuse_format_boundary_violations() {
        let mut macho = object(ARCHIVE, Target::MacAarch64);
        macho[16..20].copy_from_slice(&1_u32.to_le_bytes());
        macho[MACHO_HEADER_BYTES..MACHO_HEADER_BYTES + 4].copy_from_slice(&0_u32.to_le_bytes());
        macho[MACHO_HEADER_BYTES + 4..MACHO_HEADER_BYTES + 8].copy_from_slice(&7_u32.to_le_bytes());
        assert_eq!(extract(&macho), Err(Error::Malformed("load command")));
        macho[MACHO_HEADER_BYTES + 4..MACHO_HEADER_BYTES + 8].copy_from_slice(&8_u32.to_le_bytes());
        assert_eq!(extract(&macho), Err(Error::Absent));

        let elf = object(ARCHIVE, Target::LinuxX86_64);
        let mut wrong_class = elf.clone();
        wrong_class[4] = 1;
        assert_eq!(extract(&wrong_class), Err(Error::Format));

        let mut wrong_header_size = elf.clone();
        wrong_header_size[0x3a..0x3c].copy_from_slice(&63_u16.to_le_bytes());
        assert_eq!(
            extract(&wrong_header_size),
            Err(Error::Malformed("section header table"))
        );

        let mut no_headers = elf.clone();
        no_headers[0x3c..0x3e].copy_from_slice(&0_u16.to_le_bytes());
        assert_eq!(
            extract(&no_headers),
            Err(Error::Malformed("section header table"))
        );

        let mut names_outside_table = elf.clone();
        names_outside_table[0x3e..0x40].copy_from_slice(&7_u16.to_le_bytes());
        assert_eq!(
            extract(&names_outside_table),
            Err(Error::Malformed("section header table"))
        );

        let headers = u64_at(&elf, 0x28) as usize;
        let archive_header = headers + ELF_SECTION_HEADER_BYTES as usize;
        let mut no_file_bytes = elf;
        no_file_bytes[archive_header + 4..archive_header + 8]
            .copy_from_slice(&SHT_NOBITS.to_le_bytes());
        assert_eq!(
            extract(&no_file_bytes),
            Err(Error::Malformed("rack section holds no file bytes"))
        );
    }

    #[test]
    fn mach_o_objects_expose_exact_sections_symbols_and_target_identity() {
        for (target, cpu, subtype, minos) in [
            (Target::MacAarch64, 0x0100_000c, 0, 0x000b_0000),
            (Target::MacX86_64, 0x0100_0007, 3, 0x000a_0c00),
        ] {
            let object = object(ARCHIVE, target);
            let data =
                MACHO_HEADER_BYTES + SEGMENT_COMMAND_BYTES + 2 * SECTION_64_BYTES + 24 + 24 + 80;
            let length_at = ARCHIVE.len().next_multiple_of(8);
            let segment_bytes = length_at + 8;
            let symbols = data + segment_bytes;
            let strings_at = symbols + 2 * 16;
            let string_table = b"\0_spicezip\0_spicezip_len\0";
            let string_bytes = string_table.len().next_multiple_of(8);

            assert_eq!(u32_at(&object, 0), MACHO_MAGIC_64);
            assert_eq!(u32_at(&object, 4), cpu);
            assert_eq!(u32_at(&object, 8), subtype);
            assert_eq!(u32_at(&object, 12), 1);
            assert_eq!(u32_at(&object, 16), 4);
            assert_eq!(u32_at(&object, 20) as usize, data - MACHO_HEADER_BYTES);

            let segment = MACHO_HEADER_BYTES;
            assert_eq!(u32_at(&object, segment), LC_SEGMENT_64);
            assert_eq!(
                u32_at(&object, segment + 4) as usize,
                SEGMENT_COMMAND_BYTES + 2 * SECTION_64_BYTES
            );
            assert_eq!(u64_at(&object, segment + 32) as usize, segment_bytes);
            assert_eq!(u64_at(&object, segment + 40) as usize, data);
            assert_eq!(u64_at(&object, segment + 48) as usize, segment_bytes);
            assert_eq!(u32_at(&object, segment + 64), 2);

            let archive = segment + SEGMENT_COMMAND_BYTES;
            assert_eq!(name(&object, archive), Ok(MACHO_SECTION));
            assert_eq!(name(&object, archive + 16), Ok(MACHO_SEGMENT));
            assert_eq!(u64_at(&object, archive + 32), 0);
            assert_eq!(u64_at(&object, archive + 40) as usize, ARCHIVE.len());
            assert_eq!(u32_at(&object, archive + 48) as usize, data);
            assert_eq!(u32_at(&object, archive + 52), 0);

            let length = archive + SECTION_64_BYTES;
            assert_eq!(name(&object, length), Ok(&b"__const"[..]));
            assert_eq!(name(&object, length + 16), Ok(MACHO_SEGMENT));
            assert_eq!(u64_at(&object, length + 32) as usize, length_at);
            assert_eq!(u64_at(&object, length + 40), 8);
            assert_eq!(u32_at(&object, length + 48) as usize, data + length_at);
            assert_eq!(u32_at(&object, length + 52), 3);

            let build = segment + SEGMENT_COMMAND_BYTES + 2 * SECTION_64_BYTES;
            assert_eq!(u32_at(&object, build), LC_BUILD_VERSION);
            assert_eq!(u32_at(&object, build + 8), 1);
            assert_eq!(u32_at(&object, build + 12), minos);

            let symtab = build + 24;
            assert_eq!(u32_at(&object, symtab), LC_SYMTAB);
            assert_eq!(u32_at(&object, symtab + 8) as usize, symbols);
            assert_eq!(u32_at(&object, symtab + 12), 2);
            assert_eq!(u32_at(&object, symtab + 16) as usize, strings_at);
            assert_eq!(u32_at(&object, symtab + 20) as usize, string_bytes);

            assert_eq!(&object[data..data + ARCHIVE.len()], ARCHIVE);
            assert_eq!(u64_at(&object, data + length_at), ARCHIVE.len() as u64);
            assert_eq!(u32_at(&object, symbols), 1);
            assert_eq!(&object[symbols + 4..symbols + 8], &[0x0f, 1, 0, 0]);
            assert_eq!(u64_at(&object, symbols + 8), 0);
            assert_eq!(
                u32_at(&object, symbols + 16),
                ARCHIVE_SYMBOL.len() as u32 + 3
            );
            assert_eq!(&object[symbols + 20..symbols + 24], &[0x0f, 2, 0, 0]);
            assert_eq!(u64_at(&object, symbols + 24) as usize, length_at);
            assert_eq!(
                &object[strings_at..strings_at + string_table.len()],
                string_table
            );
            assert_eq!(object.len(), strings_at + string_bytes);
            assert_eq!(extract(&object), Ok(ARCHIVE));
        }
    }

    #[test]
    fn elf_objects_expose_exact_sections_symbols_and_target_identity() {
        let section_names =
            b"\0spicezip\0.rodata.spicezip_len\0.note.GNU-stack\0.symtab\0.strtab\0.shstrtab\0";
        let symbol_names = b"\0spicezip\0spicezip_len\0";
        for (target, machine) in [(Target::LinuxAarch64, 183), (Target::LinuxX86_64, 62)] {
            let object = object(ARCHIVE, target);
            let length_at = (64 + ARCHIVE.len()).next_multiple_of(8);
            let symbols = length_at + 8;
            let symbol_names_at = symbols + 3 * 24;
            let section_names_at = symbol_names_at + symbol_names.len();
            let headers = (section_names_at + section_names.len()).next_multiple_of(8);

            assert_eq!(&object[..4], ELF_MAGIC);
            assert_eq!(&object[4..7], &[ELF_CLASS_64, ELF_LITTLE_ENDIAN, 1]);
            assert_eq!(u16_at(&object, 16), 1);
            assert_eq!(u16_at(&object, 18), machine);
            assert_eq!(u32_at(&object, 20), 1);
            assert_eq!(u64_at(&object, 40) as usize, headers);
            assert_eq!(u16_at(&object, 52), 64);
            assert_eq!(u16_at(&object, 58), 64);
            assert_eq!(u16_at(&object, 60), 7);
            assert_eq!(u16_at(&object, 62), 6);

            assert_eq!(&object[64..64 + ARCHIVE.len()], ARCHIVE);
            assert_eq!(u64_at(&object, length_at), ARCHIVE.len() as u64);
            assert_eq!(&object[symbols..symbols + 24], &[0; 24]);
            assert_elf_symbol(&object, symbols + 24, 1, 1, ARCHIVE.len() as u64);
            assert_elf_symbol(
                &object,
                symbols + 48,
                section_name(symbol_names, LENGTH_SYMBOL),
                2,
                8,
            );
            assert_eq!(
                &object[symbol_names_at..symbol_names_at + symbol_names.len()],
                symbol_names
            );
            assert_eq!(
                &object[section_names_at..section_names_at + section_names.len()],
                section_names
            );

            let archive = headers + ELF_SECTION_HEADER_BYTES as usize;
            assert_eq!(
                ElfSectionHeader::read(&object, archive),
                ElfSectionHeader {
                    name_offset: section_name(section_names, b"spicezip"),
                    kind: SHT_PROGBITS,
                    flags: SHF_ALLOC,
                    data_offset: 64,
                    bytes: ARCHIVE.len() as u64,
                    link: 0,
                    info: 0,
                    align: 1,
                    entry_bytes: 0,
                }
            );
            let length = archive + ELF_SECTION_HEADER_BYTES as usize;
            assert_eq!(
                ElfSectionHeader::read(&object, length),
                ElfSectionHeader {
                    name_offset: section_name(section_names, b".rodata.spicezip_len"),
                    kind: SHT_PROGBITS,
                    flags: SHF_ALLOC,
                    data_offset: length_at as u64,
                    bytes: 8,
                    link: 0,
                    info: 0,
                    align: 8,
                    entry_bytes: 0,
                }
            );
            let symtab = archive + 3 * ELF_SECTION_HEADER_BYTES as usize;
            assert_eq!(
                ElfSectionHeader::read(&object, symtab),
                ElfSectionHeader {
                    name_offset: section_name(section_names, b".symtab"),
                    kind: SHT_SYMTAB,
                    flags: 0,
                    data_offset: symbols as u64,
                    bytes: 3 * 24,
                    link: 5,
                    info: 1,
                    align: 8,
                    entry_bytes: 24,
                }
            );
            assert_eq!(
                object.len(),
                headers + 7 * ELF_SECTION_HEADER_BYTES as usize
            );
            assert_eq!(extract(&object), Ok(ARCHIVE));
        }
    }

    fn assert_elf_symbol(object: &[u8], offset: usize, name_offset: u32, section: u16, bytes: u64) {
        assert_eq!(u32_at(object, offset), name_offset);
        assert_eq!(&object[offset + 4..offset + 6], &[0x11, 0]);
        assert_eq!(u16_at(object, offset + 6), section);
        assert_eq!(u64_at(object, offset + 8), 0);
        assert_eq!(u64_at(object, offset + 16), bytes);
    }

    /// The fields of one ELF64 section header, decoded for whole-record
    /// comparison.
    #[derive(Debug, PartialEq, Eq)]
    struct ElfSectionHeader {
        name_offset: u32,
        kind: u32,
        flags: u64,
        data_offset: u64,
        bytes: u64,
        link: u32,
        info: u32,
        align: u64,
        entry_bytes: u64,
    }

    impl ElfSectionHeader {
        fn read(object: &[u8], offset: usize) -> Self {
            Self {
                name_offset: u32_at(object, offset),
                kind: u32_at(object, offset + 4),
                flags: u64_at(object, offset + 8),
                data_offset: u64_at(object, offset + 24),
                bytes: u64_at(object, offset + 32),
                link: u32_at(object, offset + 40),
                info: u32_at(object, offset + 44),
                align: u64_at(object, offset + 48),
                entry_bytes: u64_at(object, offset + 56),
            }
        }
    }

    fn section_name(table: &[u8], expected: &[u8]) -> u32 {
        table
            .windows(expected.len() + 1)
            .position(|window| window[..expected.len()] == *expected && window[expected.len()] == 0)
            .expect("section name is present") as u32
    }

    fn u16_at(bytes: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes(bytes[offset..offset + 2].try_into().expect("u16 field"))
    }

    fn u32_at(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("u32 field"))
    }

    fn u64_at(bytes: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("u64 field"))
    }
}

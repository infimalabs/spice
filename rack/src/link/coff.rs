//! x86-64 Windows COFF object emission and final PE32+ archive discovery.

use super::{ARCHIVE_SYMBOL, Error, LENGTH_SYMBOL};

/// Rack section in a PE image: the one word every target shares, exactly the
/// eight bytes a PE32+ section name holds.
const SECTION: &[u8; 8] = b"spicezip";

const IMAGE_FILE_MACHINE_AMD64: u16 = 0x8664;
const IMAGE_NT_OPTIONAL_HDR64_MAGIC: u16 = 0x20b;
const COFF_HEADER_BYTES: usize = 20;
const COFF_SECTION_HEADER_BYTES: usize = 40;
const COFF_SYMBOL_BYTES: usize = 18;
const PE_SECTION_HEADER_BYTES: usize = 40;
const PE32_PLUS_OPTIONAL_HEADER_BYTES: usize = 112;
/// Initialized, readable data with byte alignment.
const ARCHIVE_CHARACTERISTICS: u32 = 0x4010_0040;
/// Initialized, readable data with eight-byte alignment.
const LENGTH_CHARACTERISTICS: u32 = 0x4040_0040;
const IMAGE_SYM_CLASS_EXTERNAL: u8 = 2;

pub(super) fn object(archive: &[u8]) -> Vec<u8> {
    const SECTIONS: usize = 2;
    const SYMBOLS: usize = 2;

    let data_at = COFF_HEADER_BYTES + SECTIONS * COFF_SECTION_HEADER_BYTES;
    let archive_bytes = u32::try_from(archive.len()).expect("a rack fits in one COFF section");
    let length_at = data_at
        .checked_add(archive.len())
        .expect("the COFF object size fits in memory")
        .next_multiple_of(std::mem::align_of::<u64>());
    let symbols_at = length_at + std::mem::size_of::<u64>();
    let strings_at = symbols_at + SYMBOLS * COFF_SYMBOL_BYTES;
    let strings_bytes = 4 + ARCHIVE_SYMBOL.len() + 1 + LENGTH_SYMBOL.len() + 1;
    let mut output = vec![0; strings_at + strings_bytes];

    put_u16(&mut output, 0, IMAGE_FILE_MACHINE_AMD64);
    put_u16(&mut output, 2, SECTIONS as u16);
    put_u32(
        &mut output,
        8,
        u32::try_from(symbols_at).expect("the COFF symbol table offset fits in 32 bits"),
    );
    put_u32(&mut output, 12, SYMBOLS as u32);

    let archive_header = COFF_HEADER_BYTES;
    output[archive_header..archive_header + SECTION.len()].copy_from_slice(SECTION);
    put_u32(&mut output, archive_header + 16, archive_bytes);
    put_u32(&mut output, archive_header + 20, data_at as u32);
    put_u32(&mut output, archive_header + 36, ARCHIVE_CHARACTERISTICS);

    let length_header = archive_header + COFF_SECTION_HEADER_BYTES;
    output[length_header..length_header + 6].copy_from_slice(b".rdata");
    put_u32(
        &mut output,
        length_header + 16,
        std::mem::size_of::<u64>() as u32,
    );
    put_u32(
        &mut output,
        length_header + 20,
        u32::try_from(length_at).expect("the COFF length section offset fits in 32 bits"),
    );
    put_u32(&mut output, length_header + 36, LENGTH_CHARACTERISTICS);

    output[data_at..data_at + archive.len()].copy_from_slice(archive);
    put_u64(&mut output, length_at, archive.len() as u64);

    let archive_name = 4_u32;
    let length_name = archive_name + ARCHIVE_SYMBOL.len() as u32 + 1;
    write_symbol(&mut output, symbols_at, archive_name, 1);
    write_symbol(&mut output, symbols_at + COFF_SYMBOL_BYTES, length_name, 2);

    put_u32(&mut output, strings_at, strings_bytes as u32);
    let archive_name_at = strings_at + archive_name as usize;
    output[archive_name_at..archive_name_at + ARCHIVE_SYMBOL.len()].copy_from_slice(ARCHIVE_SYMBOL);
    let length_name_at = strings_at + length_name as usize;
    output[length_name_at..length_name_at + LENGTH_SYMBOL.len()].copy_from_slice(LENGTH_SYMBOL);
    output
}

fn write_symbol(output: &mut [u8], offset: usize, name: u32, section: u16) {
    put_u32(output, offset + 4, name);
    put_u16(output, offset + 12, section);
    output[offset + 16] = IMAGE_SYM_CLASS_EXTERNAL;
}

pub(super) fn pe_archive(image: &[u8]) -> Result<&[u8], Error> {
    let (table, sections) = pe_section_table(image)?;
    let mut archive = None;
    for index in 0..sections {
        let header = table + index * PE_SECTION_HEADER_BYTES;
        let Some(data) = archive_section(image, header)? else {
            continue;
        };
        if archive.replace(data).is_some() {
            return Err(Error::Repeated);
        }
    }
    archive.ok_or(Error::Absent)
}

fn pe_section_table(image: &[u8]) -> Result<(usize, usize), Error> {
    if field(image, 0, 2, "DOS header")? != b"MZ" {
        return Err(Error::Format);
    }
    let pe = usize::try_from(read_u32(image, 0x3c, "DOS header")?)
        .map_err(|_| Error::Malformed("PE header offset"))?;
    if field(image, pe, 4, "PE signature")? != b"PE\0\0" {
        return Err(Error::Malformed("PE signature"));
    }
    let coff = pe
        .checked_add(4)
        .ok_or(Error::Malformed("PE header offset"))?;
    if read_u16(image, coff, "PE file header")? != IMAGE_FILE_MACHINE_AMD64 {
        return Err(Error::Format);
    }
    let sections = usize::from(read_u16(image, coff + 2, "PE file header")?);
    let optional_bytes = usize::from(read_u16(image, coff + 16, "PE file header")?);
    let table = pe_optional_table(image, coff, optional_bytes)?;
    let table_bytes = sections
        .checked_mul(PE_SECTION_HEADER_BYTES)
        .ok_or(Error::Malformed("PE section table"))?;
    field(image, table, table_bytes, "PE section table")?;
    Ok((table, sections))
}

fn pe_optional_table(image: &[u8], coff: usize, optional_bytes: usize) -> Result<usize, Error> {
    if optional_bytes < PE32_PLUS_OPTIONAL_HEADER_BYTES {
        return Err(Error::Malformed("PE32+ optional header"));
    }
    let optional = coff
        .checked_add(COFF_HEADER_BYTES)
        .ok_or(Error::Malformed("PE optional header offset"))?;
    let table = optional
        .checked_add(optional_bytes)
        .ok_or(Error::Malformed("PE section table offset"))?;
    field(image, optional, optional_bytes, "PE32+ optional header")?;
    if read_u16(image, optional, "PE32+ optional header")? != IMAGE_NT_OPTIONAL_HDR64_MAGIC {
        return Err(Error::Format);
    }
    let directories = usize::try_from(read_u32(
        image,
        optional + 108,
        "PE32+ data directory count",
    )?)
    .map_err(|_| Error::Malformed("PE32+ data directory count"))?;
    let declared = directories
        .checked_mul(8)
        .and_then(|bytes| bytes.checked_add(PE32_PLUS_OPTIONAL_HEADER_BYTES))
        .ok_or(Error::Malformed("PE32+ data directories"))?;
    if declared > optional_bytes {
        return Err(Error::Malformed("PE32+ data directories"));
    }
    Ok(table)
}

fn archive_section(image: &[u8], header: usize) -> Result<Option<&[u8]>, Error> {
    if field(image, header, SECTION.len(), "PE section name")? != SECTION {
        return Ok(None);
    }
    let size = usize::try_from(read_u32(image, header + 8, "rack section size")?)
        .map_err(|_| Error::Malformed("rack section size"))?;
    let raw_bytes = usize::try_from(read_u32(image, header + 16, "rack raw section size")?)
        .map_err(|_| Error::Malformed("rack raw section size"))?;
    let raw_at = usize::try_from(read_u32(image, header + 20, "rack section offset")?)
        .map_err(|_| Error::Malformed("rack section offset"))?;
    if size == 0 || raw_bytes < size {
        return Err(Error::Malformed("rack section range"));
    }
    field(image, raw_at, raw_bytes, "rack section range")?;
    field(image, raw_at, size, "rack section range").map(Some)
}

fn field<'a>(
    image: &'a [u8],
    offset: usize,
    bytes: usize,
    label: &'static str,
) -> Result<&'a [u8], Error> {
    offset
        .checked_add(bytes)
        .and_then(|end| image.get(offset..end))
        .ok_or(Error::Malformed(label))
}

fn read_u16(image: &[u8], offset: usize, label: &'static str) -> Result<u16, Error> {
    let bytes = field(image, offset, 2, label)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32(image: &[u8], offset: usize, label: &'static str) -> Result<u32, Error> {
    let bytes = field(image, offset, 4, label)?;
    Ok(u32::from_le_bytes(
        bytes.try_into().expect("the field is four bytes"),
    ))
}

fn put_u16(image: &mut [u8], offset: usize, value: u16) {
    image[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(image: &mut [u8], offset: usize, value: u32) {
    image[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(image: &mut [u8], offset: usize, value: u64) {
    image[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::Target;

    const ARCHIVE: &[u8] = b"PK\x03\x04embedded app";

    #[test]
    fn coff_object_exposes_the_archive_and_runtime_symbols() {
        let target = Target::new("windows", "x86_64").expect("Windows target is supported");
        assert_eq!(target, Target::WindowsX86_64);
        assert_eq!(target.object_extension(), "obj");
        let bytes = object(ARCHIVE);
        assert_eq!(read_u16(&bytes, 0, "machine"), Ok(IMAGE_FILE_MACHINE_AMD64));
        assert_eq!(read_u16(&bytes, 2, "sections"), Ok(2));

        let archive_header = COFF_HEADER_BYTES;
        assert_eq!(&bytes[archive_header..archive_header + 8], SECTION);
        let archive_at = read_u32(&bytes, archive_header + 20, "archive offset").unwrap() as usize;
        let archive_bytes = read_u32(&bytes, archive_header + 16, "archive size").unwrap() as usize;
        assert_eq!(&bytes[archive_at..archive_at + archive_bytes], ARCHIVE);
        assert_eq!(
            archive_at,
            COFF_HEADER_BYTES + 2 * COFF_SECTION_HEADER_BYTES
        );
        assert_eq!(
            read_u32(&bytes, archive_header + 36, "archive characteristics"),
            Ok(0x4010_0040)
        );

        let length_header = archive_header + COFF_SECTION_HEADER_BYTES;
        let length_at = read_u32(&bytes, length_header + 20, "length offset").unwrap() as usize;
        assert_eq!(
            u64::from_le_bytes(bytes[length_at..length_at + 8].try_into().unwrap()),
            ARCHIVE.len() as u64
        );
        assert_eq!(
            read_u32(&bytes, length_header + 16, "length bytes"),
            Ok(std::mem::size_of::<u64>() as u32)
        );
        assert_eq!(
            read_u32(&bytes, length_header + 36, "length characteristics"),
            Ok(0x4040_0040)
        );
        assert_eq!(
            length_at,
            (archive_at + ARCHIVE.len()).next_multiple_of(std::mem::align_of::<u64>())
        );

        let symbols = read_u32(&bytes, 8, "symbol table").unwrap() as usize;
        let strings = symbols + 2 * COFF_SYMBOL_BYTES;
        assert_eq!(symbols, length_at + std::mem::size_of::<u64>());
        let strings_bytes = 4 + ARCHIVE_SYMBOL.len() + 1 + LENGTH_SYMBOL.len() + 1;
        assert_eq!(
            read_u32(&bytes, strings, "string table bytes"),
            Ok(strings_bytes as u32)
        );
        assert_eq!(bytes.len(), strings + strings_bytes);
        assert_eq!(symbol_name(&bytes, symbols, strings), ARCHIVE_SYMBOL);
        assert_eq!(
            symbol_name(&bytes, symbols + COFF_SYMBOL_BYTES, strings),
            LENGTH_SYMBOL
        );
        assert_eq!(read_u16(&bytes, symbols + 12, "archive section"), Ok(1));
        assert_eq!(
            read_u16(&bytes, symbols + COFF_SYMBOL_BYTES + 12, "length section"),
            Ok(2)
        );
    }

    #[test]
    fn pe32_plus_archive_admission_enforces_one_complete_section() {
        let exact = pe(&[(SECTION, ARCHIVE)]);
        assert_eq!(pe_archive(&exact), Ok(ARCHIVE));

        let absent = pe(&[(b"another", ARCHIVE)]);
        assert_eq!(pe_archive(&absent), Err(Error::Absent));

        let malformed = pe(&[(SECTION, b"")]);
        assert_eq!(
            pe_archive(&malformed),
            Err(Error::Malformed("rack section range"))
        );

        let repeated = pe(&[(SECTION, ARCHIVE), (SECTION, b"again")]);
        assert_eq!(pe_archive(&repeated), Err(Error::Repeated));

        let archive_after_another = pe(&[(b"another", b"prefix"), (SECTION, ARCHIVE)]);
        assert_eq!(pe_archive(&archive_after_another), Ok(ARCHIVE));

        let mut truncated = exact.clone();
        truncated.pop();
        assert_eq!(
            pe_archive(&truncated),
            Err(Error::Malformed("rack section range"))
        );
    }

    #[test]
    fn pe32_plus_archive_uses_virtual_size_not_raw_padding() {
        const FIRST_SECTION_HEADER: usize = 0x80 + 4 + COFF_HEADER_BYTES + 0xf0;

        let padding = b"section alignment";
        let mut padded = pe(&[(SECTION, ARCHIVE)]);
        put_u32(
            &mut padded,
            FIRST_SECTION_HEADER + 16,
            (ARCHIVE.len() + padding.len()) as u32,
        );
        padded.extend_from_slice(padding);
        assert_eq!(pe_archive(&padded), Ok(ARCHIVE));

        let mut virtual_overrun = pe(&[(SECTION, ARCHIVE)]);
        put_u32(
            &mut virtual_overrun,
            FIRST_SECTION_HEADER + 8,
            (ARCHIVE.len() + 1) as u32,
        );
        assert_eq!(
            pe_archive(&virtual_overrun),
            Err(Error::Malformed("rack section range"))
        );
    }

    #[test]
    fn pe32_plus_optional_header_bounds_its_directory_table_exactly() {
        let coff = 0;
        let optional = COFF_HEADER_BYTES;
        let exact_bytes = PE32_PLUS_OPTIONAL_HEADER_BYTES;
        let mut exact = vec![0; optional + exact_bytes];
        put_u16(&mut exact, optional, IMAGE_NT_OPTIONAL_HDR64_MAGIC);
        assert_eq!(
            pe_optional_table(&exact, coff, exact_bytes),
            Ok(exact.len())
        );

        let short_bytes = exact_bytes - 1;
        assert_eq!(
            pe_optional_table(&exact, coff, short_bytes),
            Err(Error::Malformed("PE32+ optional header"))
        );

        put_u32(&mut exact, optional + 108, 1);
        assert_eq!(
            pe_optional_table(&exact, coff, exact_bytes),
            Err(Error::Malformed("PE32+ data directories"))
        );
    }

    fn symbol_name(image: &[u8], symbol: usize, strings: usize) -> &[u8] {
        assert_eq!(&image[symbol..symbol + 4], &[0; 4]);
        let offset = read_u32(image, symbol + 4, "symbol name").unwrap() as usize;
        let name = &image[strings + offset..];
        &name[..name.iter().position(|byte| *byte == 0).unwrap()]
    }

    fn pe(sections: &[(&[u8], &[u8])]) -> Vec<u8> {
        const PE_HEADER: usize = 0x80;
        const OPTIONAL_HEADER: usize = 0xf0;
        const SECTION_ALIGNMENT: u32 = 0x1000;
        const FILE_ALIGNMENT: usize = 0x200;

        let table = PE_HEADER + 4 + COFF_HEADER_BYTES + OPTIONAL_HEADER;
        let raw =
            (table + sections.len() * PE_SECTION_HEADER_BYTES).next_multiple_of(FILE_ALIGNMENT);
        let mut image = vec![0; raw];
        image[0..2].copy_from_slice(b"MZ");
        put_u32(&mut image, 0x3c, PE_HEADER as u32);
        image[PE_HEADER..PE_HEADER + 4].copy_from_slice(b"PE\0\0");

        let coff = PE_HEADER + 4;
        put_u16(&mut image, coff, IMAGE_FILE_MACHINE_AMD64);
        put_u16(&mut image, coff + 2, sections.len() as u16);
        put_u16(&mut image, coff + 16, OPTIONAL_HEADER as u16);
        put_u16(&mut image, coff + 18, 0x22);

        let optional = coff + COFF_HEADER_BYTES;
        put_u16(&mut image, optional, IMAGE_NT_OPTIONAL_HDR64_MAGIC);
        put_u32(&mut image, optional + 16, SECTION_ALIGNMENT);
        put_u32(&mut image, optional + 20, SECTION_ALIGNMENT);
        put_u64(&mut image, optional + 24, 0x1_4000_0000);
        put_u32(&mut image, optional + 32, SECTION_ALIGNMENT);
        put_u32(&mut image, optional + 36, FILE_ALIGNMENT as u32);
        put_u32(
            &mut image,
            optional + 56,
            (sections.len() as u32 + 1) * SECTION_ALIGNMENT,
        );
        put_u32(&mut image, optional + 60, raw as u32);
        put_u16(&mut image, optional + 68, 3);
        put_u32(&mut image, optional + 108, 16);

        for (index, (name, data)) in sections.iter().enumerate() {
            assert!(name.len() <= 8);
            let header = table + index * PE_SECTION_HEADER_BYTES;
            image[header..header + name.len()].copy_from_slice(name);
            put_u32(&mut image, header + 8, data.len() as u32);
            put_u32(
                &mut image,
                header + 12,
                (index as u32 + 1) * SECTION_ALIGNMENT,
            );
            put_u32(&mut image, header + 16, data.len() as u32);
            let data_offset = image.len() as u32;
            put_u32(&mut image, header + 20, data_offset);
            put_u32(&mut image, header + 36, 0x4000_0040);
            image.extend_from_slice(data);
        }
        image
    }
}

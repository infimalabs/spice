//! The rack linked into the running executable.
//!
//! A build links the relocatable object `link::object` emits, which
//! holds the archive in its own read-only section and its length beside it.
//! Linked before symbols, stripping, and signing, the bytes survive `strip`
//! and sit inside the signed image; linker symbols expose them as an ordinary
//! static, so nothing parses the executable at run time.

unsafe extern "C" {
    /// First byte of the rack section.
    static spicezip: u8;
    /// Byte length of the rack section.
    static spicezip_len: u64;
}

/// The rack archive bytes this executable was linked with.
///
/// Only builds that link the object resolve these symbols, so enable the
/// `linked` feature only in those builds.
#[must_use]
pub fn linked() -> &'static [u8] {
    // SAFETY: the linked object defines both symbols: an immutable section of
    // `spicezip_len` bytes starting at `spicezip`, live for the whole process.
    unsafe {
        let length = usize::try_from(spicezip_len)
            .expect("the linked archive length fits this address space");
        std::slice::from_raw_parts(&raw const spicezip, length)
    }
}

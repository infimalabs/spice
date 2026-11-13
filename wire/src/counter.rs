//! Tracked additive product-version counters.
//!
//! One registry carries every independently owned contributor as sorted
//! `name=decimal` records. Owners select their counter at compile time, while
//! the product version sums every record. Adding a contributor therefore
//! changes one tracked source and no Rust registry.

/// The sum of every counter in a canonical product-version registry.
///
/// # Panics
///
/// Refuses at compile time malformed, duplicate, or unsorted records, values
/// past `u64`, and a sum past `u64`.
#[must_use]
pub const fn registry_sum(text: &str) -> u64 {
    parse_registry(text, "").0
}

/// One named counter in a canonical product-version registry.
///
/// # Panics
///
/// Refuses at compile time the same malformed registry as [`registry_sum`],
/// an invalid or absent name, and duplicate names.
#[must_use]
pub const fn registry_counter(text: &str, name: &str) -> u64 {
    assert!(
        !name.is_empty(),
        "a product-version counter name is not empty"
    );
    let (value, found) = parse_registry(text, name);
    assert!(
        found,
        "the product-version registry contains the requested counter"
    );
    value
}

/// [`registry_counter`] narrowed to the `u32` a journal schema carries.
///
/// # Panics
///
/// Refuses at compile time a counter past `u32::MAX`.
#[must_use]
pub const fn registry_counter_u32(text: &str, name: &str) -> u32 {
    let value = registry_counter(text, name);
    assert!(
        value <= u32::MAX as u64,
        "a product-version counter fits a u32"
    );
    value as u32
}

/// [`registry_counter`] narrowed to the `u16` an install receipt carries.
///
/// # Panics
///
/// Refuses at compile time a counter past `u16::MAX`.
#[must_use]
pub const fn registry_counter_u16(text: &str, name: &str) -> u16 {
    let value = registry_counter(text, name);
    assert!(
        value <= u16::MAX as u64,
        "a product-version counter fits a u16"
    );
    value as u16
}

const fn parse_registry(text: &str, requested: &str) -> (u64, bool) {
    let bytes = text.as_bytes();
    let requested = requested.as_bytes();
    let mut index = 0;
    let mut previous_start = 0;
    let mut previous_end = 0;
    let mut first = true;
    let mut sum = 0_u64;
    let mut selected = 0_u64;
    let mut found = false;

    while index < bytes.len() {
        let key_start = index;
        while index < bytes.len() && bytes[index] != b'=' {
            assert!(
                bytes[index].is_ascii_lowercase()
                    || bytes[index].is_ascii_digit()
                    || bytes[index] == b'-',
                "a product-version counter name uses lowercase ASCII, digits, and hyphens"
            );
            index += 1;
        }
        let key_end = index;
        assert!(
            key_end > key_start && index < bytes.len(),
            "a product-version record is name=decimal followed by one LF"
        );
        if !first {
            assert!(
                key_range_less(bytes, previous_start, previous_end, key_start, key_end),
                "product-version counter names are unique and sorted"
            );
        }
        first = false;
        previous_start = key_start;
        previous_end = key_end;
        index += 1;

        let value_start = index;
        let mut value = 0_u64;
        while index < bytes.len() && bytes[index] != b'\n' {
            let digit = bytes[index];
            assert!(
                digit.is_ascii_digit(),
                "a product-version counter is decimal"
            );
            assert!(
                index == value_start || bytes[value_start] != b'0',
                "a product-version counter is canonical: no leading zero"
            );
            let Some(shifted) = value.checked_mul(10) else {
                panic!("a product-version counter fits a u64");
            };
            let Some(next) = shifted.checked_add((digit - b'0') as u64) else {
                panic!("a product-version counter fits a u64");
            };
            value = next;
            index += 1;
        }
        assert!(
            index > value_start && index < bytes.len(),
            "a product-version record is name=decimal followed by one LF"
        );
        index += 1;

        if requested.is_empty() {
            let Some(next) = sum.checked_add(value) else {
                panic!("product-version counters sum to a u64");
            };
            sum = next;
        } else if range_equals(bytes, key_start, key_end, requested) {
            assert!(!found, "product-version counter names are unique");
            selected = value;
            found = true;
        }
    }
    assert!(
        !first,
        "the product-version registry contains at least one counter"
    );
    if requested.is_empty() {
        (sum, true)
    } else {
        (selected, found)
    }
}

const fn key_range_less(
    bytes: &[u8],
    left_start: usize,
    left_end: usize,
    right_start: usize,
    right_end: usize,
) -> bool {
    let mut offset = 0;
    while left_start + offset < left_end && right_start + offset < right_end {
        let left = bytes[left_start + offset];
        let right = bytes[right_start + offset];
        if left != right {
            return left < right;
        }
        offset += 1;
    }
    left_end - left_start < right_end - right_start
}

const fn range_equals(bytes: &[u8], start: usize, end: usize, expected: &[u8]) -> bool {
    if end - start != expected.len() {
        return false;
    }
    let mut offset = 0;
    while offset < expected.len() {
        if bytes[start + offset] != expected[offset] {
            return false;
        }
        offset += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registries_are_canonical_and_select_named_counters() {
        const REGISTRY: &str = "gap=0\ninstall=1\njournal=110\n";
        assert_eq!(registry_sum(REGISTRY), 111);
        assert_eq!(registry_counter(REGISTRY, "gap"), 0);
        assert_eq!(registry_counter_u16(REGISTRY, "install"), 1);
        assert_eq!(registry_counter_u32(REGISTRY, "journal"), 110);
        for refused in [
            "",
            "gap=0",
            "gap=\n",
            "gap=00\n",
            "gap=1a\n",
            "Gap=1\n",
            "journal=1\ninstall=1\n",
            "gap=0\ngap=1\n",
        ] {
            assert!(
                std::panic::catch_unwind(|| registry_sum(refused)).is_err(),
                "{refused:?}"
            );
        }
        assert!(std::panic::catch_unwind(|| registry_counter(REGISTRY, "missing")).is_err());
        assert!(
            std::panic::catch_unwind(|| registry_counter_u16("install=65536\n", "install"))
                .is_err()
        );
        assert!(
            std::panic::catch_unwind(|| registry_counter_u32("journal=4294967296\n", "journal"))
                .is_err()
        );
    }
}

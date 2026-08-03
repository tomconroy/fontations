//! Encoding of CFF INDEX structures.
//!
//! An INDEX is a count, an offset size, `count + 1` offsets into the data
//! that follows, and that data. The only difference between the versions is
//! the width of the count: CFF1 uses a `u16`, CFF2 a `u32`. An INDEX with no
//! items is just the zero count, so an empty CFF1 INDEX is two bytes and an
//! empty CFF2 INDEX is four.

/// The width of an INDEX's `count` field, which differs between CFF versions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CountSize {
    /// CFF1: a `u16` count, so an empty INDEX is two zero bytes.
    U16,
    /// CFF2: a `u32` count, so an empty INDEX is four zero bytes.
    U32,
}

impl CountSize {
    /// The size in bytes of the count field, and so of an empty INDEX.
    pub const fn size(self) -> usize {
        match self {
            CountSize::U16 => 2,
            CountSize::U32 => 4,
        }
    }

    fn write_count(self, out: &mut Vec<u8>, count: usize) {
        match self {
            CountSize::U16 => out.extend_from_slice(&(count as u16).to_be_bytes()),
            CountSize::U32 => out.extend_from_slice(&(count as u32).to_be_bytes()),
        }
    }
}

/// The number of bytes needed to store an offset of this size.
pub fn calc_off_size(value: u32) -> u8 {
    match value {
        0..=0xff => 1,
        0x100..=0xffff => 2,
        0x10000..=0xffffff => 3,
        _ => 4,
    }
}

/// The serialized size of an INDEX containing items of the given lengths.
pub fn index_size(item_lens: &[usize], count_size: CountSize) -> usize {
    if item_lens.is_empty() {
        return count_size.size();
    }
    let data_len: usize = item_lens.iter().sum();
    let off_size = calc_off_size(data_len as u32 + 1) as usize;
    count_size.size() + 1 + (item_lens.len() + 1) * off_size + data_len
}

/// Append an INDEX containing `items` to `out`.
pub fn write_index_to(out: &mut Vec<u8>, items: &[Vec<u8>], count_size: CountSize) {
    count_size.write_count(out, items.len());
    if items.is_empty() {
        return;
    }
    let data_len: usize = items.iter().map(|i| i.len()).sum();
    let off_size = calc_off_size(data_len as u32 + 1);
    out.push(off_size);
    let mut offset = 1u32;
    let push_offset = |out: &mut Vec<u8>, offset: u32| {
        out.extend_from_slice(&offset.to_be_bytes()[4 - off_size as usize..]);
    };
    push_offset(out, offset);
    for item in items {
        offset += item.len() as u32;
        push_offset(out, offset);
    }
    for item in items {
        out.extend_from_slice(item);
    }
}

/// The bytes of an INDEX containing `items`.
pub fn compile_index(items: &[Vec<u8>], count_size: CountSize) -> Vec<u8> {
    let mut out = Vec::new();
    write_index_to(&mut out, items, count_size);
    out
}

#[cfg(test)]
mod tests {
    use read_fonts::ps::cff::index::Index;

    use super::*;

    /// Round-trip through read-fonts, which is the authority on how many
    /// bytes an INDEX occupies (an empty one is version-dependent, and
    /// `Index::new` only recognizes it at exactly that length).
    fn round_trip(items: &[Vec<u8>], count_size: CountSize) -> Vec<u8> {
        let bytes = compile_index(items, count_size);
        assert_eq!(bytes.len(), index_size(&lens(items), count_size));
        let is_cff2 = count_size == CountSize::U32;
        let index = Index::new(&bytes, is_cff2).unwrap();
        assert_eq!(index.count() as usize, items.len());
        if !items.is_empty() {
            assert_eq!(index.size_in_bytes().unwrap(), bytes.len());
        }
        for (i, item) in items.iter().enumerate() {
            assert_eq!(index.get(i).unwrap(), &item[..]);
        }
        bytes
    }

    fn lens(items: &[Vec<u8>]) -> Vec<usize> {
        items.iter().map(|i| i.len()).collect()
    }

    #[test]
    fn empty_index_is_a_bare_count() {
        assert_eq!(round_trip(&[], CountSize::U16), [0, 0]);
        assert_eq!(round_trip(&[], CountSize::U32), [0, 0, 0, 0]);
    }

    #[test]
    fn u16_count_matches_fonttools() {
        // fontTools' CFF1 INDEX for [b"abc", b"de"]:
        // count=2, offSize=1, offsets 1,4,6, then the data
        assert_eq!(
            round_trip(&[b"abc".to_vec(), b"de".to_vec()], CountSize::U16),
            [0, 2, 1, 1, 4, 6, b'a', b'b', b'c', b'd', b'e']
        );
    }

    #[test]
    fn u32_count_matches_fonttools() {
        // the same items in a CFF2 INDEX: only the count widens
        assert_eq!(
            round_trip(&[b"abc".to_vec(), b"de".to_vec()], CountSize::U32),
            [0, 0, 0, 2, 1, 1, 4, 6, b'a', b'b', b'c', b'd', b'e']
        );
    }

    #[test]
    fn off_size_grows_with_the_data() {
        assert_eq!(calc_off_size(0xff), 1);
        assert_eq!(calc_off_size(0x100), 2);
        assert_eq!(calc_off_size(0xffff), 2);
        assert_eq!(calc_off_size(0x10000), 3);
        assert_eq!(calc_off_size(0xffffff), 3);
        assert_eq!(calc_off_size(0x1000000), 4);

        // an item one byte short of needing two-byte offsets, and one that
        // needs them: the last offset is data_len + 1
        let short = vec![vec![0u8; 254]];
        assert_eq!(round_trip(&short, CountSize::U32)[4], 1);
        let long = vec![vec![0u8; 255]];
        assert_eq!(round_trip(&long, CountSize::U32)[4], 2);
    }
}

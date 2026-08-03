//! CFF version 2.

include!("../../../generated/generated_cff2.rs");

pub mod charstring;
pub mod dict;
pub mod font;
pub mod varstore;

pub use charstring::{Cff2Charstring, Cff2CharstringBuilder};
pub use dict::Blend;
pub use font::{Cff2, Cff2FontBuilder, Cff2GlyphData, Cff2PrivateDictValues, Cff2TopDictValues};

impl Index {
    /// Construct an `Index` from a list of byte items.
    ///
    /// Note that an empty INDEX comes out as six bytes (a zero count, an
    /// offset size and one offset) rather than the bare four-byte zero count
    /// the CFF2 spec calls for, because this type always writes `off_size`
    /// and `offsets`. A `CFF2` table cannot use that form — everything after
    /// an empty INDEX would be two bytes out — so the table writer emits
    /// INDEXes with [`crate::ps::cff::index::write_index_to`] instead, and
    /// this constructor is only for INDEXes that stand alone in their own
    /// subtable, like `VARC`'s.
    pub fn from_items(items: Vec<Vec<u8>>) -> Self {
        if items.is_empty() {
            return Index::new(0, 1, vec![1], vec![]);
        }

        let count = items.len() as u32;

        // Calculate offsets (1-based per CFF2 spec)
        let mut offset_values = Vec::with_capacity(items.len() + 1);
        let mut current_offset = 1u32;
        offset_values.push(current_offset);
        for item in &items {
            current_offset += item.len() as u32;
            offset_values.push(current_offset);
        }

        // Determine off_size (minimum bytes needed for largest offset)
        let max_offset = *offset_values.last().unwrap();
        let off_size = if max_offset <= 0xFF {
            1u8
        } else if max_offset <= 0xFFFF {
            2u8
        } else if max_offset <= 0xFFFFFF {
            3u8
        } else {
            4u8
        };

        // Pack offsets based on off_size
        let mut offsets = Vec::with_capacity(offset_values.len() * off_size as usize);
        for offset in &offset_values {
            match off_size {
                1 => offsets.push(*offset as u8),
                2 => offsets.extend((*offset as u16).to_be_bytes()),
                3 => {
                    let bytes = offset.to_be_bytes();
                    offsets.extend(&bytes[1..4]);
                }
                4 => offsets.extend(offset.to_be_bytes()),
                _ => unreachable!(),
            }
        }

        // Concatenate item data
        let data: Vec<u8> = items.into_iter().flatten().collect();

        Index::new(count, off_size, offsets, data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_index2_from_items() {
        let items = vec![vec![1, 2, 3], vec![4, 5]];
        let index = Index::from_items(items);
        assert_eq!(index.count, 2);
    }
}

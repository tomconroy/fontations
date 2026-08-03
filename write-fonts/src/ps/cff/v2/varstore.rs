//! The `ItemVariationStore` a CFF2 table carries.
//!
//! CFF2 uses the variation store differently from every other table: its
//! subtables hold no deltas at all (the deltas live in the charstrings and in
//! the Private DICT), so a subtable is nothing but a list of region indices,
//! and a charstring's `vsindex` picks the subtable. varLib builds these lists
//! as it merges glyphs — a subtable per distinct set of participating
//! masters, in the order the sets are first seen, with each subtable's region
//! indices in its own sub-model's support order (`varLib/cff.py:290-305`).
//!
//! So there is nothing here to derive, and both the region list and each
//! subtable's indices are written exactly as handed in. That is why
//! [`VariationStoreBuilder`] cannot be used: it derives region order from the
//! deltas added to it, prunes regions no delta references, and sorts each
//! subtable's region indices by delta width — all of which are no-ops or
//! actively wrong when there are no deltas. It is also why the bytes are
//! written here rather than through [`dump_table`]: the table packer
//! deduplicates identical subtables, and two variation store indices with
//! the same region list have to stay two subtables.
//!
//! [`VariationStoreBuilder`]: crate::tables::variations::ivs_builder::VariationStoreBuilder
//! [`dump_table`]: crate::write::dump_table

use crate::{
    ps::cff::CffBuildError,
    tables::variations::{
        ItemVariationData, ItemVariationStore, VariationRegion, VariationRegionList,
    },
};

/// Build the variation store for a CFF2 table.
///
/// `subtables` gives one region-index list per variation store index, in
/// `vsindex` order; the indices point into `regions`.
pub fn build_var_store(
    axis_count: u16,
    regions: Vec<VariationRegion>,
    subtables: &[Vec<u16>],
) -> Result<ItemVariationStore, CffBuildError> {
    if regions.len() > u16::MAX as usize || subtables.len() > u16::MAX as usize {
        return Err(CffBuildError::VarStoreTooBig);
    }
    for region in &regions {
        if region.region_axes.len() != axis_count as usize {
            return Err(CffBuildError::WrongAxisCount {
                expected: axis_count as usize,
                got: region.region_axes.len(),
            });
        }
    }
    let var_data = subtables
        .iter()
        .map(|region_indexes| {
            for index in region_indexes {
                if *index as usize >= regions.len() {
                    return Err(CffBuildError::RegionOutOfRange { region: *index });
                }
            }
            // no deltas: item_count and word_delta_count are both zero, and
            // there are no delta set rows. This is `buildVarData(indices,
            // None, False)` (`varLib/cff.py:299`).
            Ok(Some(ItemVariationData::new(
                0,
                0,
                region_indexes.clone(),
                vec![],
            )))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ItemVariationStore::new(
        VariationRegionList::new(axis_count, regions),
        var_data,
    ))
}

/// Serialize a variation store, subtable for subtable and region for region.
///
/// The result is what a CFF2 table embeds, without the two-byte length
/// prefix the table writes in front of it.
pub fn dump_var_store(store: &ItemVariationStore) -> Vec<u8> {
    let regions = &store.variation_region_list;
    let mut out = Vec::new();
    out.extend_from_slice(&1u16.to_be_bytes()); // format
                                                // format, the region list offset, the count, and one offset per subtable
    let region_list_offset = 2 + 4 + 2 + 4 * store.item_variation_data.len();
    out.extend_from_slice(&(region_list_offset as u32).to_be_bytes());
    out.extend_from_slice(&(store.item_variation_data.len() as u16).to_be_bytes());

    let axis_count = regions.axis_count as usize;
    let region_list_len = 4 + regions.variation_regions.len() * axis_count * 6;
    let mut offset = region_list_offset + region_list_len;
    let mut subtables = Vec::new();
    for data in &store.item_variation_data {
        let Some(data) = data.as_ref() else {
            // a null offset: the store says this subtable is not there
            out.extend_from_slice(&0u32.to_be_bytes());
            continue;
        };
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&data.item_count.to_be_bytes());
        bytes.extend_from_slice(&data.word_delta_count.to_be_bytes());
        bytes.extend_from_slice(&(data.region_indexes.len() as u16).to_be_bytes());
        for index in &data.region_indexes {
            bytes.extend_from_slice(&index.to_be_bytes());
        }
        bytes.extend_from_slice(&data.delta_sets);
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        offset += bytes.len();
        subtables.push(bytes);
    }

    out.extend_from_slice(&regions.axis_count.to_be_bytes());
    out.extend_from_slice(&(regions.variation_regions.len() as u16).to_be_bytes());
    for region in &regions.variation_regions {
        for axis in &region.region_axes {
            out.extend_from_slice(&axis.start_coord.to_be_bytes());
            out.extend_from_slice(&axis.peak_coord.to_be_bytes());
            out.extend_from_slice(&axis.end_coord.to_be_bytes());
        }
    }
    for bytes in subtables {
        out.extend_from_slice(&bytes);
    }
    out
}

#[cfg(test)]
mod tests {
    use font_types::F2Dot14;
    use read_fonts::{tables::variations::ItemVariationStore as ReadStore, FontRead};

    use super::*;
    use crate::tables::variations::{ivs_builder::VariationStoreBuilder, RegionAxisCoordinates};

    fn region(axes: &[(f32, f32, f32)]) -> VariationRegion {
        VariationRegion::new(
            axes.iter()
                .map(|(start, peak, end)| {
                    RegionAxisCoordinates::new(
                        F2Dot14::from_f32(*start),
                        F2Dot14::from_f32(*peak),
                        F2Dot14::from_f32(*end),
                    )
                })
                .collect(),
        )
    }

    fn bytes(axis_count: u16, regions: Vec<VariationRegion>, subtables: &[Vec<u16>]) -> Vec<u8> {
        let store = build_var_store(axis_count, regions, subtables).unwrap();
        let bytes = dump_var_store(&store);
        // whatever we write, read-fonts has to agree it is a store
        let parsed = ReadStore::read(bytes.as_slice().into()).unwrap();
        assert_eq!(parsed.item_variation_data_count() as usize, subtables.len());
        for (index, expected) in subtables.iter().enumerate() {
            let data = parsed.item_variation_data().get(index).unwrap().unwrap();
            assert_eq!(data.item_count(), 0);
            assert_eq!(
                data.region_indexes()
                    .iter()
                    .map(|i| i.get())
                    .collect::<Vec<_>>(),
                *expected
            );
        }
        bytes
    }

    /// The store of the three-master `Vf3` fixture, byte for byte as
    /// `fontmake -o variable-cff2 --optimize-cff 1` writes it: three regions,
    /// and three subtables because three different sets of masters take part
    /// in its glyphs.
    #[test]
    fn vf3_store_matches_fontmake() {
        #[rustfmt::skip]
        let expected: &[u8] = &[
            0x00, 0x01,                                     // format 1
            0x00, 0x00, 0x00, 0x14,                         // region list at 20
            0x00, 0x03,                                     // 3 subtables
            0x00, 0x00, 0x00, 0x2a, 0x00, 0x00, 0x00, 0x32, 0x00, 0x00, 0x00, 0x3c,
            0x00, 0x01, 0x00, 0x03,                         // 1 axis, 3 regions
            0x00, 0x00, 0x40, 0x00, 0x40, 0x00,             // (0, 1, 1)
            0x00, 0x00, 0x19, 0x9a, 0x40, 0x00,             // (0, 0.4, 1)
            0x19, 0x9a, 0x40, 0x00, 0x40, 0x00,             // (0.4, 1, 1)
            0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0x00, 0x02,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01,
        ];
        assert_eq!(
            bytes(
                1,
                vec![
                    region(&[(0.0, 1.0, 1.0)]),
                    region(&[(0.0, 0.4, 1.0)]),
                    region(&[(0.4, 1.0, 1.0)]),
                ],
                &[vec![0], vec![1, 2], vec![1]],
            ),
            expected
        );
    }

    /// A subtable's region indices are neither sorted nor renumbered:
    /// `Anybody.glyphs` (2 axes, 5 regions) really does have
    /// `VarData[1].VarRegionIndex = [3, 4, 1, 2]`, and the whole 106-byte
    /// store has to come out as fontmake wrote it.
    #[test]
    fn anybody_store_keeps_its_region_order() {
        let regions = vec![
            region(&[(0.0, 1.0, 1.0), (0.0, 0.0, 0.0)]),
            region(&[(0.0, 0.0, 0.0), (0.0, 1.0, 1.0)]),
            region(&[(0.0, 1.0, 1.0), (0.0, 1.0, 1.0)]),
            region(&[(0.0, 0.5, 1.0), (0.0, 0.0, 0.0)]),
            region(&[(0.5, 1.0, 1.0), (0.0, 0.0, 0.0)]),
        ];
        let subtables = [vec![0, 1, 2], vec![3, 4, 1, 2]];
        let written = bytes(2, regions, &subtables);
        assert_eq!(written.len(), 106);
        #[rustfmt::skip]
        let tail: &[u8] = &[
            0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x01, 0x00, 0x02,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x03, 0x00, 0x04, 0x00, 0x01, 0x00, 0x02,
        ];
        assert_eq!(&written[80..], tail);
    }

    /// Why this module exists: the shared [`VariationStoreBuilder`] cannot
    /// produce the store above. Given the same five regions and a delta set
    /// that references four of them, it renumbers the regions into the order
    /// it first saw them and sorts each subtable's indices — so the second
    /// subtable's `[3, 4, 1, 2]` comes back as an ascending run.
    #[test]
    fn the_shared_builder_would_reorder_this() {
        let regions = [
            region(&[(0.0, 1.0, 1.0), (0.0, 0.0, 0.0)]),
            region(&[(0.0, 0.0, 0.0), (0.0, 1.0, 1.0)]),
            region(&[(0.0, 1.0, 1.0), (0.0, 1.0, 1.0)]),
            region(&[(0.0, 0.5, 1.0), (0.0, 0.0, 0.0)]),
            region(&[(0.5, 1.0, 1.0), (0.0, 0.0, 0.0)]),
        ];
        let mut builder = VariationStoreBuilder::new(2);
        // one delta set over regions 3, 4, 1, 2, in that order
        builder.add_deltas(
            [3usize, 4, 1, 2]
                .iter()
                .map(|i| (regions[*i].clone(), 1i16))
                .collect(),
        );
        let (store, _) = builder.build();
        let data = store.item_variation_data[0].as_ref().unwrap();
        assert_ne!(data.region_indexes, vec![3, 4, 1, 2]);
        assert_eq!(data.region_indexes, vec![0, 1, 2, 3]);
        // and it only knows about the four regions it was given deltas for
        assert_eq!(store.variation_region_list.variation_regions.len(), 4);
    }

    /// The two-master `Probe` fixture's store, which is what a CFF2 with a
    /// single region looks like: 30 bytes, and the length the table's
    /// two-byte prefix announces.
    #[test]
    fn probe_store_matches_fontmake() {
        #[rustfmt::skip]
        let expected: &[u8] = &[
            0x00, 0x01, 0x00, 0x00, 0x00, 0x0c, 0x00, 0x01, 0x00, 0x00, 0x00, 0x16,
            0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x40, 0x00, 0x40, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
        ];
        assert_eq!(
            bytes(1, vec![region(&[(0.0, 1.0, 1.0)])], &[vec![0]]),
            expected
        );
    }

    /// Two subtables that happen to reference the same regions stay two
    /// subtables: fontTools allocates one per participation set, and a
    /// `vsindex` has to be able to name each of them.
    #[test]
    fn identical_subtables_are_not_deduplicated() {
        let written = bytes(
            1,
            vec![region(&[(0.0, 1.0, 1.0)])],
            &[vec![0], vec![0], vec![0]],
        );
        // header (2 + 4 + 2 + 3 * 4), region list (4 + 6), 3 * 8
        assert_eq!(written.len(), 20 + 10 + 24);
        let offsets: Vec<u32> = written[8..20]
            .chunks(4)
            .map(|c| u32::from_be_bytes(c.try_into().unwrap()))
            .collect();
        assert_eq!(offsets, [30, 38, 46]);
    }

    #[test]
    fn bad_input_is_an_error() {
        let one_axis = vec![region(&[(0.0, 1.0, 1.0)])];
        assert_eq!(
            build_var_store(2, one_axis.clone(), &[vec![0]]),
            Err(CffBuildError::WrongAxisCount {
                expected: 2,
                got: 1
            })
        );
        assert_eq!(
            build_var_store(1, one_axis, &[vec![0, 1]]),
            Err(CffBuildError::RegionOutOfRange { region: 1 })
        );
    }
}

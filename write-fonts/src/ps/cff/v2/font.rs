//! Building a complete `CFF2` table.
//!
//! [`Cff2FontBuilder`] assembles the table varLib produces for a variable
//! font: one FontDict, a Private DICT whose values may blend, a variation
//! store that carries only region lists, and charstrings that blend. Layout,
//! DICT emission order and operand encoding match fontTools' `cffLib`, so the
//! output is byte-identical to `fontmake -o variable-cff2 --optimize-cff 1`
//! given the same values.
//!
//! The shape is "the default master's CFF1, minus the widths, plus blends"
//! (`fontTools/cffLib/CFFToCFF2.py:32-224`): no `name` INDEX, no String
//! INDEX, no charset, no encoding, no FontBBox, and no `defaultWidthX` or
//! `nominalWidthX`.

use font_types::Tag;
use kurbo::Rect;
use read_fonts::TopLevelTable;

use crate::{
    ps::cff::{
        dict::{self, Number, Operator},
        index::{compile_index, index_size, CountSize},
        v2::{
            dict::{write_delta, write_number, Blend},
            varstore::dump_var_store,
        },
        CffBuildError,
    },
    tables::variations::ItemVariationStore,
    validate::{Validate, ValidationCtx},
    write::{FontWrite, TableWriter},
};

/// A compiled `CFF2` table, ready to add to a font.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Cff2(Vec<u8>);

impl Cff2 {
    /// The raw bytes of the table.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl TopLevelTable for Cff2 {
    /// 'CFF2'
    const TAG: Tag = Tag::new(b"CFF2");
}

impl Validate for Cff2 {
    fn validate_impl(&self, _ctx: &mut ValidationCtx) {}
}

impl FontWrite for Cff2 {
    fn write_into(&self, writer: &mut TableWriter) {
        writer.write_slice(&self.0);
    }
}

/// Top DICT values.
///
/// A CFF2 Top DICT holds almost nothing: the offsets the builder fills in,
/// and these two. Everything a CFF1 Top DICT carries about the font's
/// identity moves to `name` and `OS/2` (`CFFToCFF2.py:143-201`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cff2TopDictValues {
    /// Emitted when not `[0.001, 0, 0, 0.001, 0, 0]`; for a font with
    /// units-per-em `u` this is `[1/u, 0, 0, 1/u, 0, 0]`.
    pub font_matrix: Option<[f64; 6]>,
    /// The deepest the interpreter's argument stack goes in this font.
    ///
    /// fontmake never sets this, and readers assume the default (193) when it
    /// is absent.
    pub max_stack: Option<u16>,
}

/// Private DICT values, each of which may blend across variation regions.
///
/// Values equal to their CFF spec defaults are omitted, as `cffLib` omits
/// them — but only when they do not vary, since a blended value is never
/// equal to a plain default.
///
/// Note that which keys are here at all is the caller's decision and is not
/// obvious: varLib blends a key only if the *default* master's Private DICT
/// has it, and `cffLib` never puts a default-valued key in a master's Private
/// DICT to begin with. So a family whose default master has the default
/// `BlueShift` of 7 has no `BlueShift` in its CFF2 at all, whatever the other
/// masters say (`varLib/cff.py:138-151`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cff2PrivateDictValues {
    /// The variation store index this Private DICT's blends belong to.
    ///
    /// fontmake never writes one: varLib blends the Private DICT against
    /// store index 0 and leaves the operator out (`varLib/cff.py:117-121`).
    pub vsindex: Option<u16>,
    pub blue_values: Vec<Blend>,
    pub other_blues: Vec<Blend>,
    pub family_blues: Vec<Blend>,
    pub family_other_blues: Vec<Blend>,
    /// CFF default: 0.039625
    pub blue_scale: Option<Blend>,
    /// CFF default: 7
    pub blue_shift: Option<Blend>,
    /// CFF default: 1
    pub blue_fuzz: Option<Blend>,
    pub std_hw: Option<Blend>,
    pub std_vw: Option<Blend>,
    pub stem_snap_h: Vec<Blend>,
    pub stem_snap_v: Vec<Blend>,
    /// CFF default: 0
    pub language_group: Option<i32>,
    /// CFF default: 0.06
    pub expansion_factor: Option<f64>,
}

/// A glyph to be included in the font.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cff2GlyphData {
    /// The charstring program, including any `vsindex` prefix, as
    /// [`Cff2Charstring`] produces it.
    ///
    /// [`Cff2Charstring`]: super::charstring::Cff2Charstring
    pub charstring: Vec<u8>,
    /// The charstring's exact bounds in the default master, which is what
    /// the default instance's metrics are measured from. Nothing in the
    /// `CFF2` table itself uses them — unlike CFF1, there is no FontBBox —
    /// but the caller needs them for `hmtx` and `head`.
    pub bounds: Option<Rect>,
}

/// Builds a `CFF2` table.
///
/// Multiple FontDicts (a CID-keyed font) are not supported: ufo2ft never
/// produces one, and every corpus font measured for this work has exactly one
/// FontDict and no FDSelect.
#[derive(Clone, Debug, Default)]
pub struct Cff2FontBuilder {
    top_dict: Cff2TopDictValues,
    private: Cff2PrivateDictValues,
    var_store: Option<ItemVariationStore>,
    glyphs: Vec<Cff2GlyphData>,
}

impl Cff2FontBuilder {
    pub fn new(top_dict: Cff2TopDictValues, private: Cff2PrivateDictValues) -> Self {
        Cff2FontBuilder {
            top_dict,
            private,
            var_store: None,
            glyphs: Vec::new(),
        }
    }

    /// Give the font a variation store.
    ///
    /// A font with no store is a valid, static CFF2; one is needed as soon as
    /// any charstring or Private DICT value blends.
    pub fn set_var_store(&mut self, var_store: ItemVariationStore) {
        self.var_store = Some(var_store);
    }

    /// Add a glyph. Glyphs must be added in glyph order, `.notdef` first.
    pub fn add_glyph(&mut self, glyph: Cff2GlyphData) {
        self.glyphs.push(glyph);
    }

    pub fn build(self) -> Result<Cff2, CffBuildError> {
        if self.glyphs.is_empty() {
            return Err(CffBuildError::NoGlyphs);
        }
        let charstrings: Vec<Vec<u8>> = self
            .glyphs
            .iter()
            .map(|glyph| glyph.charstring.clone())
            .collect();
        let charstrings_index = compile_index(&charstrings, CountSize::U32);
        let private_dict = compile_private_dict(&self.private);
        let var_store = self.var_store.as_ref().map(dump_var_store);

        // The Top DICT holds offsets whose encoded size depends on their
        // value, and the FontDict holds the Private DICT's offset, which
        // depends on the size of the FDArray INDEX that holds the FontDict.
        // Iterate until nothing moves, like fontTools.
        let mut offsets = Cff2Offsets::default();
        let (top_dict_data, font_dict) = loop {
            let font_dict = compile_font_dict(private_dict.len(), offsets.private);
            let top_dict_data = compile_top_dict(&self.top_dict, &offsets, var_store.is_some());
            let charstrings = 5
                + top_dict_data.len()
                + CountSize::U32.size()
                + var_store.as_ref().map_or(0, |bytes| 2 + bytes.len());
            let fdarray = charstrings + charstrings_index.len();
            let new_offsets = Cff2Offsets {
                var_store: 5 + top_dict_data.len() + CountSize::U32.size(),
                charstrings,
                fdarray,
                private: fdarray + index_size(&[font_dict.len()], CountSize::U32),
            };
            if new_offsets == offsets {
                break (top_dict_data, font_dict);
            }
            offsets = new_offsets;
        };

        let mut data = Vec::with_capacity(offsets.private + private_dict.len());
        // header: major, minor, headerSize, then the Top DICT's length
        data.extend_from_slice(&[2, 0, 5]);
        data.extend_from_slice(&(top_dict_data.len() as u16).to_be_bytes());
        data.extend_from_slice(&top_dict_data);
        // an empty Global Subr INDEX, which in CFF2 is a bare u32 count
        data.extend_from_slice(&[0, 0, 0, 0]);
        if let Some(var_store) = &var_store {
            debug_assert_eq!(data.len(), offsets.var_store);
            data.extend_from_slice(&(var_store.len() as u16).to_be_bytes());
            data.extend_from_slice(var_store);
        }
        debug_assert_eq!(data.len(), offsets.charstrings);
        data.extend_from_slice(&charstrings_index);
        debug_assert_eq!(data.len(), offsets.fdarray);
        data.extend_from_slice(&compile_index(&[font_dict], CountSize::U32));
        debug_assert_eq!(data.len(), offsets.private);
        data.extend_from_slice(&private_dict);
        Ok(Cff2(data))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Cff2Offsets {
    var_store: usize,
    charstrings: usize,
    fdarray: usize,
    private: usize,
}

/// The Top DICT, in `buildOrder(topDictOperators2)` order: `maxstack`,
/// `FontMatrix`, `FDSelect`, `FDArray`, `CharStrings`, `VarStore`.
fn compile_top_dict(
    values: &Cff2TopDictValues,
    offsets: &Cff2Offsets,
    has_var_store: bool,
) -> Vec<u8> {
    let mut out = Vec::new();
    if let Some(max_stack) = values.max_stack {
        dict::encode_int(&mut out, max_stack as i32);
        Operator::MAXSTACK.write(&mut out);
    }
    if let Some(matrix) = values.font_matrix {
        if matrix != [0.001, 0.0, 0.0, 0.001, 0.0, 0.0] {
            for value in matrix {
                // as in CFF1: an integral element is written as an integer
                // operand, which is what fontTools does for the zeros ufo2ft
                // passes as Python ints
                Number::conv_to_int(value).write(&mut out);
            }
            Operator::FONT_MATRIX.write(&mut out);
        }
    }
    // no FDSelect: one FontDict covers every glyph
    dict::encode_int(&mut out, offsets.fdarray as i32);
    Operator::FD_ARRAY.write(&mut out);
    dict::encode_int(&mut out, offsets.charstrings as i32);
    Operator::CHARSTRINGS.write(&mut out);
    if has_var_store {
        dict::encode_int(&mut out, offsets.var_store as i32);
        Operator::VSTORE.write(&mut out);
    }
    out
}

/// The one FontDict, which holds nothing but the Private DICT's size and
/// offset.
fn compile_font_dict(private_size: usize, private_offset: usize) -> Vec<u8> {
    let mut out = Vec::new();
    dict::encode_int(&mut out, private_size as i32);
    dict::encode_int(&mut out, private_offset as i32);
    Operator::PRIVATE.write(&mut out);
    out
}

/// The Private DICT, in `buildOrder(privateDictOperators2)` order.
///
/// The CFF1-only width keys are gone, and so is `ForceBold`: `CFFToCFF2`
/// deletes every operator that is not in `privateDictOperators2`
/// (`CFFToCFF2.py:161-171`).
fn compile_private_dict(values: &Cff2PrivateDictValues) -> Vec<u8> {
    let mut out = Vec::new();
    /// A value that does not vary and equals the CFF default is left out,
    /// like `DictCompiler.compile`'s `if value == defaults.get(name)`.
    fn is_default(value: &Blend, default: f64) -> bool {
        !value.varies() && value.default.to_f64() == default
    }
    let delta_entry = |out: &mut Vec<u8>, values: &[Blend], op: Operator| {
        if !values.is_empty() {
            write_delta(out, values);
            op.write(out);
        }
    };
    let number_entry = |out: &mut Vec<u8>, value: &Option<Blend>, default: Option<f64>, op| {
        if let Some(value) = value {
            if default.is_some_and(|default| is_default(value, default)) {
                return;
            }
            write_number(out, value);
            Operator::write(op, out);
        }
    };

    if let Some(vsindex) = values.vsindex {
        dict::encode_int(&mut out, vsindex as i32);
        Operator::VS_INDEX.write(&mut out);
    }
    delta_entry(&mut out, &values.blue_values, Operator::BLUE_VALUES);
    delta_entry(&mut out, &values.other_blues, Operator::OTHER_BLUES);
    delta_entry(&mut out, &values.family_blues, Operator::FAMILY_BLUES);
    delta_entry(
        &mut out,
        &values.family_other_blues,
        Operator::FAMILY_OTHER_BLUES,
    );
    number_entry(
        &mut out,
        &values.blue_scale,
        Some(0.039625),
        Operator::BLUE_SCALE,
    );
    number_entry(
        &mut out,
        &values.blue_shift,
        Some(7.0),
        Operator::BLUE_SHIFT,
    );
    number_entry(&mut out, &values.blue_fuzz, Some(1.0), Operator::BLUE_FUZZ);
    number_entry(&mut out, &values.std_hw, None, Operator::STD_HW);
    number_entry(&mut out, &values.std_vw, None, Operator::STD_VW);
    delta_entry(&mut out, &values.stem_snap_h, Operator::STEM_SNAP_H);
    delta_entry(&mut out, &values.stem_snap_v, Operator::STEM_SNAP_V);
    if let Some(language_group) = values.language_group {
        if language_group != 0 {
            dict::encode_int(&mut out, language_group);
            Operator::LANGUAGE_GROUP.write(&mut out);
        }
    }
    if let Some(expansion_factor) = values.expansion_factor {
        if expansion_factor != 0.06 {
            dict::encode_real(&mut out, expansion_factor);
            Operator::EXPANSION_FACTOR.write(&mut out);
        }
    }
    // no Subrs: this builder does not subroutinize
    out
}

#[cfg(test)]
mod tests {
    use font_types::{F2Dot14, GlyphId};
    use kurbo::Point;
    use read_fonts::{model::glyph::outline::OutlinePen, ps::cff::CffFontRef};

    use super::*;
    use crate::{
        ps::cff::{
            charstring::CharstringBuilder,
            v2::{charstring::Cff2CharstringBuilder, varstore::build_var_store},
        },
        tables::variations::{RegionAxisCoordinates, VariationRegion},
    };

    fn ints(values: &[i32]) -> Vec<Number> {
        values.iter().copied().map(Number::Int).collect()
    }

    fn blend(default: i32, deltas: &[i32]) -> Blend {
        Blend::new(default, ints(deltas))
    }

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

    /// Draws to a list of points so a round-tripped glyph can be measured.
    #[derive(Default)]
    struct PointPen(Vec<(f32, f32)>);

    impl OutlinePen for PointPen {
        fn move_to(&mut self, x: f32, y: f32) {
            self.0.push((x, y));
        }
        fn line_to(&mut self, x: f32, y: f32) {
            self.0.push((x, y));
        }
        fn quad_to(&mut self, _: f32, _: f32, x: f32, y: f32) {
            self.0.push((x, y));
        }
        fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, x: f32, y: f32) {
            self.0.push((x, y));
        }
        fn close(&mut self) {}
    }

    /// Draw a glyph at a location and return the box it covers.
    fn draw(table: &Cff2, gid: u32, coords: &[f32]) -> (f32, f32, f32, f32) {
        let font = CffFontRef::new_cff2(table.as_bytes(), Some(1000)).unwrap();
        let coords: Vec<F2Dot14> = coords.iter().map(|c| F2Dot14::from_f32(*c)).collect();
        let subfont = font.subfont(0, &coords).unwrap();
        let mut pen = PointPen::default();
        font.draw(&subfont, GlyphId::new(gid), &coords, None, &mut pen)
            .unwrap();
        let xs = pen.0.iter().map(|p| p.0);
        let ys = pen.0.iter().map(|p| p.1);
        (
            xs.clone().fold(f32::MAX, f32::min),
            ys.clone().fold(f32::MAX, f32::min),
            xs.fold(f32::MIN, f32::max),
            ys.fold(f32::MIN, f32::max),
        )
    }

    /// The `CFF2` table `fontmake 3.12.1` produces for the two-master `Probe`
    /// fixture with `--optimize-cff 1`; this builder must reproduce it byte
    /// for byte.
    ///
    /// Its charstrings are fed in verbatim here; the charstring builder is
    /// tested against fontmake separately, and end to end in
    /// [`vf3_matches_fontmake`].
    #[rustfmt::skip]
    const PROBE_CFF2: &[u8] = &[
        // header, then a Top DICT of 8 bytes
        0x02, 0x00, 0x05, 0x00, 0x08,
        0xf7, 0x2a, 0x0c, 0x24, // 150 FDArray
        0xbc, 0x11,             // 49 CharStrings
        0x9c, 0x18,             // 17 VarStore
        0x00, 0x00, 0x00, 0x00, // empty Global Subr INDEX
        // VarStore, length-prefixed: one axis, one region, one subtable
        0x00, 0x1e,
        0x00, 0x01, 0x00, 0x00, 0x00, 0x0c, 0x00, 0x01, 0x00, 0x00, 0x00, 0x16,
        0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x40, 0x00, 0x40, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
        // CharStrings INDEX: 5 glyphs
        0x00, 0x00, 0x00, 0x05, 0x01, 0x01, 0x1a, 0x2a, 0x38, 0x5b, 0x5b,
        0xbd, 0xfb, 0x5c, 0x15, 0xf8, 0x24, 0xfa, 0x7c, 0xfc, 0x24, 0xfe, 0x7c, 0x06,
        0xbd, 0xbd, 0x15, 0xfa, 0x18, 0xf7, 0xc0, 0xfe, 0x18, 0xfb, 0xc0, 0x07,
        0x8b, 0x96, 0x8c, 0x10, 0x16, 0xef, 0x06, 0xbd, 0xbd, 0x8b, 0xef, 0x59, 0xbd,
        0x08, 0x27, 0x06,
        0x8b, 0x16, 0x95, 0x8e, 0x8c, 0x10, 0x06, 0x8b, 0x88, 0x8c, 0x10, 0xf7, 0xc0, 0x05,
        0x8b, 0x96, 0x8c, 0x10, 0x16, 0xef, 0x06, 0xbd, 0xbd, 0x8b, 0xef, 0x59, 0xbd,
        0x08, 0x27, 0x06, 0xa9, 0x8f, 0x8c, 0x10, 0xfb, 0x34, 0x15, 0x9a, 0x93, 0x8c,
        0x10, 0x06, 0x8b, 0x86, 0x8c, 0x10, 0xf7, 0xc0, 0x05,
        // FDArray INDEX: one FontDict, "65 161 Private"
        0x00, 0x00, 0x00, 0x01, 0x01, 0x01, 0x05, 0xcc, 0xf7, 0x35, 0x12,
        // Private DICT
        0x81, 0x95, 0xf8, 0x88, 0x97, 0xf7, 0x50, 0x97, 0x87, 0x8f, 0x9f, 0x8d, 0x89,
        0x8a, 0x91, 0x17, 0x06,
        0x7f, 0x97, 0x88, 0x8b, 0x8d, 0x17, 0x07,
        0x1e, 0x37, 0x5c, 0x4f, 0x1e, 0x3c, 0x4f, 0x8c, 0x17, 0x0c, 0x09,
        0x8b, 0x8e, 0x8c, 0x17, 0x0c, 0x0b,
        0x9f, 0x8e, 0x8c, 0x17, 0x0a,
        0xa0, 0x8e, 0x8c, 0x17, 0x0b,
        0x9f, 0x9f, 0x8e, 0x8b, 0x8d, 0x17, 0x0c, 0x0c,
        0xa0, 0x8e, 0x8c, 0x17, 0x0c, 0x0d,
    ];

    fn probe_table() -> Cff2 {
        let private = Cff2PrivateDictValues {
            blue_values: vec![
                blend(-10, &[-4]),
                blend(0, &[4]),
                blend(500, &[20]),
                blend(512, &[2]),
                blend(700, &[-2]),
                blend(712, &[-1]),
            ],
            other_blues: vec![blend(-12, &[-3]), blend(0, &[0])],
            // the default master's BlueShift is the CFF default, so varLib
            // never sees the key and the other master's value is lost
            blue_shift: None,
            blue_scale: Some(Blend::new(0.0375, vec![Number::Real(0.0003)])),
            blue_fuzz: Some(blend(0, &[3])),
            std_hw: Some(blend(20, &[3])),
            std_vw: Some(blend(21, &[3])),
            stem_snap_h: vec![blend(20, &[3]), blend(40, &[0])],
            stem_snap_v: vec![blend(21, &[3])],
            language_group: Some(0),
            expansion_factor: Some(0.06),
            ..Default::default()
        };
        let mut builder = Cff2FontBuilder::new(Cff2TopDictValues::default(), private);
        builder.set_var_store(
            build_var_store(1, vec![region(&[(0.0, 1.0, 1.0)])], &[vec![0]]).unwrap(),
        );
        // the charstrings fontmake wrote, in glyph order
        // the CharStrings INDEX's data starts at 60, and its offsets are
        // 1, 26, 42, 56, 91, 91
        for charstring in [
            &PROBE_CFF2[60..85],
            &PROBE_CFF2[85..101],
            &PROBE_CFF2[101..115],
            &PROBE_CFF2[115..150],
            &PROBE_CFF2[150..150],
        ] {
            builder.add_glyph(Cff2GlyphData {
                charstring: charstring.to_vec(),
                bounds: None,
            });
        }
        builder.build().unwrap()
    }

    #[test]
    fn probe_matches_fontmake() {
        assert_eq!(probe_table().as_bytes(), PROBE_CFF2);
    }

    #[test]
    fn probe_round_trips_through_read_fonts() {
        let table = probe_table();
        let font = CffFontRef::new_cff2(table.as_bytes(), Some(1000)).unwrap();
        assert_eq!(font.num_glyphs(), 5);
        assert!(font.var_store().is_some());
        // glyph 1 blends its x-component from 0 to 11
        assert_eq!(draw(&table, 1, &[0.0]).0, 0.0);
        assert_eq!(draw(&table, 1, &[1.0]).0, 11.0);
        // and its Private DICT blends the blue zones. Note what the stored
        // blend means: each row's default value is absolute but its delta is
        // a delta of the *relative* value, so the second entry's blend of
        // `[0, 4]` moves the zone's top from 0 to -14 + (10 + 4) = 0 again,
        // which is exactly what the Bold master states.
        let zones = |location: f32| {
            let (_, hints) = font
                .subfont_hinted(0, &[F2Dot14::from_f32(location)])
                .unwrap();
            hints.blues.values()[..2]
                .iter()
                .map(|(a, b)| (a.to_f64(), b.to_f64()))
                .collect::<Vec<_>>()
        };
        assert_eq!(zones(0.0), [(-10.0, 0.0), (500.0, 512.0)]);
        assert_eq!(zones(1.0), [(-14.0, 0.0), (520.0, 534.0)]);
    }

    /// The three-master `Vf3` fixture, end to end: the charstrings are built
    /// from the masters' outlines by [`Cff2CharstringBuilder`], and the whole
    /// 261-byte table has to match fontmake.
    ///
    /// This is the fixture with three variation store subtables — its glyph
    /// order puts a two-master glyph first, so store index 0 is *not* the
    /// full model.
    fn vf3_table() -> Cff2 {
        fn full_deltas(values: &[f64]) -> Vec<f64> {
            vec![values[0], values[1] - values[0], values[2] - values[0]]
        }
        fn pair_deltas(values: &[f64]) -> Vec<f64> {
            vec![values[0], values[1] - values[0]]
        }
        fn draw_box(pen: &mut CharstringBuilder, x: f64, y: f64, w: f64, h: f64) {
            pen.move_to(Point::new(x, y));
            pen.line_to(Point::new(x + w, y));
            pen.line_to(Point::new(x + w, y + h));
            pen.line_to(Point::new(x, y + h));
            pen.close_path();
        }

        let mut charstrings = Vec::new();
        // .notdef: the same box in all three masters
        let mut notdef = Cff2CharstringBuilder::new(3);
        for master in 0..3 {
            draw_box(notdef.master(master), 50.0, -200.0, 400.0, 1000.0);
        }
        charstrings.push(notdef.build(full_deltas).unwrap());

        // Sparse: Thin and Bold only. It is the first glyph that blends, so
        // it takes store index 0 and writes no vsindex
        let mut sparse = Cff2CharstringBuilder::new(2);
        for (master, t) in [0.0, 100.0].into_iter().enumerate() {
            draw_box(sparse.master(master), 0.0, 0.0, 100.0 + t, 300.0);
        }
        charstrings.push(sparse.build(pair_deltas).unwrap().with_vsindex(0).unwrap());

        // Full: all three masters -> store index 1
        let mut full = Cff2CharstringBuilder::new(3);
        for (master, t) in [0.0, 40.0, 100.0].into_iter().enumerate() {
            draw_box(full.master(master), 0.0, 0.0, 100.0 + 3.0 * t, 301.0 + t);
        }
        charstrings.push(full.build(full_deltas).unwrap().with_vsindex(1).unwrap());

        // Sparse2: Thin and Mid -> store index 2
        let mut sparse2 = Cff2CharstringBuilder::new(2);
        for (master, t) in [0.0, 40.0].into_iter().enumerate() {
            draw_box(sparse2.master(master), 7.0, 3.0, 111.0 + t, 307.0);
        }
        charstrings.push(sparse2.build(pair_deltas).unwrap().with_vsindex(2).unwrap());

        // Static: identical everywhere, so it never blends and never gets a
        // store index however high a one it is offered
        let mut stat = Cff2CharstringBuilder::new(3);
        for master in 0..3 {
            draw_box(stat.master(master), 10.0, 10.0, 200.0, 400.0);
        }
        charstrings.push(stat.build(full_deltas).unwrap().with_vsindex(1).unwrap());

        // Curvy: a curve whose dy2 does not vary
        let mut curvy = Cff2CharstringBuilder::new(3);
        for (master, t) in [0.0, 40.0, 100.0].into_iter().enumerate() {
            let (x, y, w, h) = (20.0, 100.0, 300.0 + t, 250.0 + 2.0 * t);
            let pen = curvy.master(master);
            pen.move_to(Point::new(x, y));
            pen.curve_to(
                Point::new(x + (w as i64 / 3) as f64, y + h),
                Point::new(x + (2 * w as i64 / 3) as f64, y + h),
                Point::new(x + w, y),
            );
            pen.line_to(Point::new(x + w, y - 50.0));
            pen.close_path();
        }
        charstrings.push(curvy.build(full_deltas).unwrap().with_vsindex(1).unwrap());

        // space: empty in every master
        charstrings.push(Cff2CharstringBuilder::new(3).build(full_deltas).unwrap());

        // every master states the same blues, so they do not blend; the stem
        // widths do, against store index 0's two-master model
        let private = Cff2PrivateDictValues {
            blue_values: [-10, 0, 500, 512, 700, 712]
                .iter()
                .map(|v| Blend::scalar(*v))
                .collect(),
            other_blues: vec![Blend::scalar(-12), Blend::scalar(0)],
            blue_scale: Some(Blend::scalar(0.0375)),
            blue_fuzz: Some(Blend::scalar(0)),
            std_hw: Some(blend(20, &[10])),
            std_vw: Some(blend(21, &[10])),
            stem_snap_h: vec![blend(20, &[10]), blend(40, &[0])],
            stem_snap_v: vec![blend(21, &[10])],
            ..Default::default()
        };
        let mut builder = Cff2FontBuilder::new(Cff2TopDictValues::default(), private);
        builder.set_var_store(
            build_var_store(
                1,
                vec![
                    region(&[(0.0, 1.0, 1.0)]),
                    region(&[(0.0, 0.4, 1.0)]),
                    region(&[(0.4, 1.0, 1.0)]),
                ],
                &[vec![0], vec![1, 2], vec![1]],
            )
            .unwrap(),
        );
        for charstring in charstrings {
            builder.add_glyph(Cff2GlyphData {
                charstring: charstring.bytes,
                bounds: charstring.bounds,
            });
        }
        builder.build().unwrap()
    }

    #[test]
    fn vf3_matches_fontmake() {
        #[rustfmt::skip]
        let expected: &[u8] = &[
            0x02, 0x00, 0x05, 0x00, 0x08,
            0xf7, 0x61, 0x0c, 0x24, 0xe2, 0x11, 0x9c, 0x18,
            0x00, 0x00, 0x00, 0x00,
            0x00, 0x44,
            0x00, 0x01, 0x00, 0x00, 0x00, 0x14, 0x00, 0x03,
            0x00, 0x00, 0x00, 0x2a, 0x00, 0x00, 0x00, 0x32, 0x00, 0x00, 0x00, 0x3c,
            0x00, 0x01, 0x00, 0x03,
            0x00, 0x00, 0x40, 0x00, 0x40, 0x00,
            0x00, 0x00, 0x19, 0x9a, 0x40, 0x00,
            0x19, 0x9a, 0x40, 0x00, 0x40, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0x00, 0x02,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01,
            0x00, 0x00, 0x00, 0x07, 0x01, 0x01, 0x0c, 0x19, 0x2e, 0x40, 0x4a, 0x6a, 0x6a,
            0xbd, 0xfb, 0x5c, 0x15, 0xf8, 0x24, 0xfa, 0x7c, 0xfc, 0x24, 0x06,
            0x8b, 0x16, 0xef, 0xef, 0x8c, 0x10, 0xf7, 0xc0, 0x27, 0x27, 0x8c, 0x10, 0x06,
            0x8c, 0x0f, 0x8b, 0x16, 0xef, 0xf7, 0xc1, 0x27, 0xf7, 0x0c, 0xf7, 0xc0, 0xb3,
            0xef, 0xfb, 0x0c, 0xfb, 0xc0, 0x8e, 0x10, 0x06,
            0x8d, 0x0f, 0x92, 0x8e, 0x15, 0xf7, 0x03, 0xb3, 0x8c, 0x10, 0xf7, 0xc7, 0xfb,
            0x03, 0x63, 0x8c, 0x10, 0x06,
            0x95, 0x95, 0x15, 0xf7, 0x5c, 0xf8, 0x24, 0xfb, 0x5c, 0x06,
            0x8c, 0x0f, 0x9f, 0xef, 0x15, 0xef, 0xf7, 0x8e, 0xef, 0x98, 0xac, 0xdb, 0xf7,
            0x5c, 0x98, 0xac, 0x8e, 0x10, 0x8b, 0xef, 0xfb, 0x8e, 0x99, 0xad, 0x3b, 0xfb,
            0x5c, 0x8d, 0x10, 0x08, 0x59, 0x07,
            0x00, 0x00, 0x00, 0x01, 0x01, 0x01, 0x05, 0xb8, 0xf7, 0x6c, 0x12,
            0x81, 0x95, 0xf8, 0x88, 0x97, 0xf7, 0x50, 0x97, 0x06,
            0x7f, 0x97, 0x07,
            0x1e, 0x37, 0x5c, 0x4f, 0x0c, 0x09,
            0x8b, 0x0c, 0x0b,
            0x9f, 0x95, 0x8c, 0x17, 0x0a,
            0xa0, 0x95, 0x8c, 0x17, 0x0b,
            0x9f, 0x9f, 0x95, 0x8b, 0x8d, 0x17, 0x0c, 0x0c,
            0xa0, 0x95, 0x8c, 0x17, 0x0c, 0x0d,
        ];
        assert_eq!(vf3_table().as_bytes(), expected);
    }

    /// The blends mean what they should: `Full` draws each master's box at
    /// that master's location, including the intermediate one.
    #[test]
    fn vf3_draws_every_master() {
        let table = vf3_table();
        assert_eq!(draw(&table, 2, &[0.0]), (0.0, 0.0, 100.0, 301.0));
        assert_eq!(draw(&table, 2, &[0.4]), (0.0, 0.0, 220.0, 341.0));
        assert_eq!(draw(&table, 2, &[1.0]), (0.0, 0.0, 400.0, 401.0));
        // Sparse only has Thin and Bold, and interpolates between them
        assert_eq!(draw(&table, 1, &[0.0]), (0.0, 0.0, 100.0, 300.0));
        assert_eq!(draw(&table, 1, &[1.0]), (0.0, 0.0, 200.0, 300.0));
        // Static does not move
        assert_eq!(draw(&table, 4, &[1.0]), (10.0, 10.0, 210.0, 410.0));
    }

    /// A CFF2 without a variation store is still a CFF2: no VarStore
    /// operator, and nothing between the Global Subr INDEX and the
    /// charstrings.
    #[test]
    fn a_static_font_has_no_var_store() {
        let mut builder = Cff2FontBuilder::new(
            Cff2TopDictValues::default(),
            Cff2PrivateDictValues::default(),
        );
        builder.add_glyph(Cff2GlyphData {
            charstring: vec![0x8b, 0x16],
            bounds: None,
        });
        let table = builder.build().unwrap();
        assert_eq!(
            table.as_bytes(),
            &[
                0x02, 0x00, 0x05, 0x00, 0x05, // header, 5-byte Top DICT
                0xa2, 0x0c, 0x24, // 23 FDArray
                0x99, 0x11, // 14 CharStrings
                0x00, 0x00, 0x00, 0x00, // Global Subrs
                0x00, 0x00, 0x00, 0x01, 0x01, 0x01, 0x03, 0x8b, 0x16, // CharStrings
                // FDArray: one FontDict, "0 33 Private", and an empty
                // Private DICT at 33 -- the table ends there
                0x00, 0x00, 0x00, 0x01, 0x01, 0x01, 0x04, 0x8b, 0xac, 0x12,
            ]
        );
        assert!(CffFontRef::new_cff2(table.as_bytes(), Some(1000))
            .unwrap()
            .var_store()
            .is_none());
    }

    /// A non-1000 upem puts a FontMatrix in the Top DICT, with the zeros as
    /// integer operands.
    #[test]
    fn font_matrix_is_emitted_for_other_upems() {
        let top_dict = Cff2TopDictValues {
            font_matrix: Some([1.0 / 2048.0, 0.0, 0.0, 1.0 / 2048.0, 0.0, 0.0]),
            max_stack: Some(48),
        };
        let mut builder = Cff2FontBuilder::new(top_dict, Cff2PrivateDictValues::default());
        builder.add_glyph(Cff2GlyphData::default());
        let table = builder.build().unwrap();
        let matrix: &[u8] = &[
            0x1e, 0x48, 0x82, 0x81, 0x25, 0xc1, 0x1f, // 1/2048
            0x8b, 0x8b, // 0 0
            0x1e, 0x48, 0x82, 0x81, 0x25, 0xc1, 0x1f, //
            0x8b, 0x8b, 0x0c, 0x07,
        ];
        assert!(table.as_bytes().windows(matrix.len()).any(|w| w == matrix));
        // maxstack comes first
        assert_eq!(&table.as_bytes()[5..8], &[0xbb, 0x19, 0x1e]);
    }

    #[test]
    fn no_glyphs_is_an_error() {
        let builder = Cff2FontBuilder::new(
            Cff2TopDictValues::default(),
            Cff2PrivateDictValues::default(),
        );
        assert_eq!(builder.build(), Err(CffBuildError::NoGlyphs));
    }
}

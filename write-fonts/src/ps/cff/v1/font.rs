//! Building a complete `CFF ` table.
//!
//! [`CffFontBuilder`] assembles a name-keyed (non-CID), non-subroutinized
//! CFF version 1 font: the kind emitted by fontmake/ufo2ft before
//! subroutinization. Layout, DICT emission order, SID interning order,
//! charset format selection, and default/nominal width optimization all
//! match fontTools' `cffLib` so that output is byte-identical to fontmake's
//! at CFF optimization level 1 (specialize, don't subroutinize), given the
//! same input values.

use std::collections::HashMap;

use font_types::Tag;
use kurbo::Rect;
use read_fonts::{ps::string::STANDARD_STRINGS, TopLevelTable};

use super::{
    charstring,
    dict::{self, Operator},
};
use crate::{
    validate::{Validate, ValidationCtx},
    write::{FontWrite, TableWriter},
    OtRound,
};

/// A compiled `CFF ` table, ready to add to a font.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Cff(Vec<u8>);

impl Cff {
    /// The raw bytes of the table.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl TopLevelTable for Cff {
    /// 'CFF '
    const TAG: Tag = Tag::new(b"CFF ");
}

impl Validate for Cff {
    fn validate_impl(&self, _ctx: &mut ValidationCtx) {}
}

impl FontWrite for Cff {
    fn write_into(&self, writer: &mut TableWriter) {
        writer.write_slice(&self.0);
    }
}

/// Top DICT values.
///
/// String values that are `None` are omitted; numeric values equal to their
/// CFF spec defaults are omitted, like fontTools. Note that ufo2ft sets
/// `notice` and `copyright` to empty strings (not `None`) when the source
/// has no such data.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TopDictValues {
    pub version: Option<String>,
    pub notice: Option<String>,
    pub copyright: Option<String>,
    pub full_name: Option<String>,
    pub family_name: Option<String>,
    pub weight: Option<String>,
    pub is_fixed_pitch: bool,
    /// Emitted as a real number when nonzero, like ufo2ft.
    pub italic_angle: f64,
    /// CFF default: -100
    pub underline_position: i32,
    /// CFF default: 50
    pub underline_thickness: i32,
    /// Emitted when not `[0.001, 0, 0, 0.001, 0, 0]`; for a font with
    /// units-per-em `u` this is `[1/u, 0, 0, 1/u, 0, 0]`.
    ///
    /// Elements with integral values are written as integer operands (that
    /// is how fontTools encodes the zeros ufo2ft passes as Python ints).
    pub font_matrix: Option<[f64; 6]>,
}

impl TopDictValues {
    /// Sensible defaults matching the CFF spec defaults.
    pub fn new() -> Self {
        TopDictValues {
            underline_position: -100,
            underline_thickness: 50,
            ..Default::default()
        }
    }
}

/// Private DICT values.
///
/// Values equal to their CFF spec defaults are omitted from the output.
/// Blue and stem-snap arrays must already be in the form ufo2ft would
/// pass (sorted pairs for blues, sorted stem snaps).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PrivateDictValues {
    pub blue_values: Vec<i32>,
    pub other_blues: Vec<i32>,
    pub family_blues: Vec<i32>,
    pub family_other_blues: Vec<i32>,
    /// CFF default: 0.039625
    pub blue_scale: Option<f64>,
    /// CFF default: 7
    pub blue_shift: Option<i32>,
    /// CFF default: 1
    pub blue_fuzz: Option<i32>,
    pub std_hw: Option<i32>,
    pub std_vw: Option<i32>,
    pub stem_snap_h: Vec<i32>,
    pub stem_snap_v: Vec<i32>,
    pub force_bold: Option<bool>,
    pub language_group: Option<i32>,
    /// If `None`, computed from the glyph advances with the same
    /// optimization fontTools uses.
    pub default_width_x: Option<i32>,
    /// If `None`, computed together with `default_width_x`.
    pub nominal_width_x: Option<i32>,
}

/// A glyph to be included in the font.
#[derive(Clone, Debug)]
pub struct GlyphData {
    pub name: String,
    /// The unrounded advance width; compared against `defaultWidthX` and
    /// rounded exactly like ufo2ft.
    pub advance_width: f64,
    /// The charstring program, without a width prefix (as produced by
    /// [`CharstringBuilder::build`][super::charstring::CharstringBuilder::build]
    /// with `width: None`).
    pub charstring: Vec<u8>,
    /// The charstring's exact bounds, straight from
    /// [`Charstring::bounds`][super::charstring::Charstring::bounds]; `None`
    /// for a glyph that draws nothing.
    ///
    /// Only the FontBBox is derived from this, and it is *not* rounded: see
    /// [`CffFontBuilder::build`].
    pub bounds: Option<Rect>,
}

/// An error produced when building a [`Cff`] table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CffBuildError {
    /// No glyphs were provided
    NoGlyphs,
    /// The first glyph must be named `.notdef`
    FirstGlyphNotNotdef,
    /// A charstring operand does not fit the Type 2 integer range
    OperandOutOfRange { value: i32 },
    /// More strings than the 16-bit SID space can identify
    TooManyStrings,
}

impl std::fmt::Display for CffBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            CffBuildError::NoGlyphs => write!(f, "a CFF font requires at least one glyph"),
            CffBuildError::FirstGlyphNotNotdef => {
                write!(f, "the first glyph in a CFF font must be '.notdef'")
            }
            CffBuildError::OperandOutOfRange { value } => write!(
                f,
                "charstring operand {value} is outside the Type 2 integer range"
            ),
            CffBuildError::TooManyStrings => {
                write!(f, "a CFF font cannot hold more than 65535 strings")
            }
        }
    }
}

impl std::error::Error for CffBuildError {}

/// Builds a name-keyed CFF version 1 table.
#[derive(Clone, Debug, Default)]
pub struct CffFontBuilder {
    font_name: String,
    top_dict: TopDictValues,
    private: PrivateDictValues,
    glyphs: Vec<GlyphData>,
}

impl CffFontBuilder {
    /// `font_name` is the PostScript name, stored in the Name INDEX.
    pub fn new(font_name: String, top_dict: TopDictValues, private: PrivateDictValues) -> Self {
        CffFontBuilder {
            font_name,
            top_dict,
            private,
            glyphs: Vec::new(),
        }
    }

    /// Add a glyph. Glyphs must be added in glyph order, `.notdef` first.
    pub fn add_glyph(&mut self, glyph: GlyphData) {
        self.glyphs.push(glyph);
    }

    pub fn build(self) -> Result<Cff, CffBuildError> {
        if self.glyphs.is_empty() {
            return Err(CffBuildError::NoGlyphs);
        }
        if self.glyphs[0].name != ".notdef" {
            return Err(CffBuildError::FirstGlyphNotNotdef);
        }

        // widths: decide defaultWidthX/nominalWidthX, then finalize each
        // charstring with its width prefix.
        let rounded_widths: Vec<i32> = self
            .glyphs
            .iter()
            .map(|g| g.advance_width.ot_round())
            .collect();
        let (default_width, nominal_width) =
            match (self.private.default_width_x, self.private.nominal_width_x) {
                (Some(d), Some(n)) => (d, n),
                (d, n) => {
                    let (opt_d, opt_n) = optimize_widths(&rounded_widths);
                    (d.unwrap_or(opt_d), n.unwrap_or(opt_n))
                }
            };
        let charstrings: Vec<Vec<u8>> = self
            .glyphs
            .iter()
            .map(|g| {
                // like ufo2ft: compare the unrounded width to defaultWidthX,
                // and only round the delta that gets written
                if g.advance_width == default_width as f64 {
                    Ok(g.charstring.clone())
                } else {
                    let delta: i32 = (g.advance_width - nominal_width as f64).ot_round();
                    let mut program = Vec::with_capacity(g.charstring.len() + 3);
                    // a charstring operand, not a DICT operand: the widest
                    // form here is 3 bytes
                    charstring::encode_int(&mut program, delta)?;
                    program.extend_from_slice(&g.charstring);
                    Ok(program)
                }
            })
            .collect::<Result<_, CffBuildError>>()?;

        // SIDs: non-standard glyph names intern first (in glyph order,
        // via the charset), then Top DICT string values in operator order.
        let mut strings = StringInterner::new();
        let charset_sids: Vec<u16> = self.glyphs[1..]
            .iter()
            .map(|g| strings.get_sid(&g.name))
            .collect::<Result<_, _>>()?;
        let sid = |strings: &mut StringInterner, s: &Option<String>| match s {
            Some(s) => strings.get_sid(s).map(Some),
            None => Ok(None),
        };
        let top_dict_sids = TopDictSids {
            version: sid(&mut strings, &self.top_dict.version)?,
            notice: sid(&mut strings, &self.top_dict.notice)?,
            copyright: sid(&mut strings, &self.top_dict.copyright)?,
            full_name: sid(&mut strings, &self.top_dict.full_name)?,
            family_name: sid(&mut strings, &self.top_dict.family_name)?,
            weight: sid(&mut strings, &self.top_dict.weight)?,
        };

        let font_bbox = font_bbox(&self.glyphs);

        // a charset the reader already knows needs no block of its own
        let predefined_charset = predefined_charset(&charset_sids);
        let charset = match predefined_charset {
            Some(_) => Vec::new(),
            None => compile_charset(&charset_sids),
        };
        let charstrings_index = compile_index(&charstrings);
        let private_dict = compile_private_dict(&self.private, default_width, nominal_width);

        // fixed-size prefix: header, Name INDEX, String INDEX, GlobalSubrs
        let name_index = compile_index(&[self.font_name.as_bytes().to_vec()]);
        let string_index = compile_index(
            &strings
                .strings
                .iter()
                .map(|s| s.as_bytes().to_vec())
                .collect::<Vec<_>>(),
        );

        // iterate until the offsets stored in the Top DICT stabilize, since
        // their encoded size depends on their value (like fontTools).
        let mut offsets = TopDictOffsets::default();
        let top_dict_data = loop {
            let top_dict_data = compile_top_dict(
                &self.top_dict,
                &top_dict_sids,
                font_bbox,
                private_dict.len(),
                predefined_charset,
                &offsets,
            );
            let top_dict_index = index_size(&[top_dict_data.len()]);
            let prefix_len = 4 + name_index.len() + top_dict_index + string_index.len() + 2;
            let new_offsets = TopDictOffsets {
                charset: prefix_len,
                charstrings: prefix_len + charset.len(),
                private: prefix_len + charset.len() + charstrings_index.len(),
            };
            if new_offsets == offsets {
                break top_dict_data;
            }
            offsets = new_offsets;
        };

        let total_size = offsets.private + private_dict.len();
        let mut data = Vec::with_capacity(total_size);
        // header: major, minor, hdrSize, offSize for the whole table
        data.extend_from_slice(&[1, 0, 4, calc_off_size(total_size as u32)]);
        data.extend_from_slice(&name_index);
        write_index_to(&mut data, &[top_dict_data]);
        data.extend_from_slice(&string_index);
        data.extend_from_slice(&[0, 0]); // empty Global Subr INDEX
        debug_assert_eq!(data.len(), offsets.charset);
        data.extend_from_slice(&charset);
        debug_assert_eq!(data.len(), offsets.charstrings);
        data.extend_from_slice(&charstrings_index);
        debug_assert_eq!(data.len(), offsets.private);
        data.extend_from_slice(&private_dict);
        Ok(Cff(data))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TopDictOffsets {
    charset: usize,
    charstrings: usize,
    private: usize,
}

#[derive(Clone, Debug, Default)]
struct TopDictSids {
    version: Option<u16>,
    notice: Option<u16>,
    copyright: Option<u16>,
    full_name: Option<u16>,
    family_name: Option<u16>,
    weight: Option<u16>,
}

fn compile_top_dict(
    values: &TopDictValues,
    sids: &TopDictSids,
    font_bbox: [i32; 4],
    private_size: usize,
    predefined_charset: Option<u8>,
    offsets: &TopDictOffsets,
) -> Vec<u8> {
    let mut out = Vec::new();
    let sid_entry = |out: &mut Vec<u8>, sid: Option<u16>, op: Operator| {
        if let Some(sid) = sid {
            dict::encode_int(out, sid as i32);
            op.write(out);
        }
    };
    // operator order matches fontTools' topDictOperators
    sid_entry(&mut out, sids.version, Operator::VERSION);
    sid_entry(&mut out, sids.notice, Operator::NOTICE);
    sid_entry(&mut out, sids.copyright, Operator::COPYRIGHT);
    sid_entry(&mut out, sids.full_name, Operator::FULL_NAME);
    sid_entry(&mut out, sids.family_name, Operator::FAMILY_NAME);
    sid_entry(&mut out, sids.weight, Operator::WEIGHT);
    if values.is_fixed_pitch {
        dict::encode_int(&mut out, 1);
        Operator::IS_FIXED_PITCH.write(&mut out);
    }
    if values.italic_angle != 0.0 {
        dict::encode_real(&mut out, values.italic_angle);
        Operator::ITALIC_ANGLE.write(&mut out);
    }
    if values.underline_position != -100 {
        dict::encode_int(&mut out, values.underline_position);
        Operator::UNDERLINE_POSITION.write(&mut out);
    }
    if values.underline_thickness != 50 {
        dict::encode_int(&mut out, values.underline_thickness);
        Operator::UNDERLINE_THICKNESS.write(&mut out);
    }
    if let Some(matrix) = values.font_matrix {
        if matrix != [0.001, 0.0, 0.0, 0.001, 0.0, 0.0] {
            for value in matrix {
                // fontTools picks the operand encoding from the Python type,
                // and ufo2ft builds the matrix as [1.0/upem, 0, 0, 1.0/upem,
                // 0, 0] -- integer literals for the four zeros. Rust has one
                // number type here, so key off the value instead: an integral
                // element is written as an integer operand, which is what
                // fontTools does for those literals (and is shorter besides).
                if value.fract() == 0.0 && (value.abs() as i64) <= i32::MAX as i64 {
                    dict::encode_int(&mut out, value as i32);
                } else {
                    dict::encode_real(&mut out, value);
                }
            }
            Operator::FONT_MATRIX.write(&mut out);
        }
    }
    if font_bbox != [0; 4] {
        for value in font_bbox {
            dict::encode_int(&mut out, value);
        }
        Operator::FONT_BBOX.write(&mut out);
    }
    // fontTools writes the predefined id here even when it is 0 (the DICT
    // default): it fills in `charset` after the pass that drops defaults
    let charset = predefined_charset.map_or(offsets.charset as i32, i32::from);
    dict::encode_int(&mut out, charset);
    Operator::CHARSET.write(&mut out);
    dict::encode_int(&mut out, private_size as i32);
    dict::encode_int(&mut out, offsets.private as i32);
    Operator::PRIVATE.write(&mut out);
    dict::encode_int(&mut out, offsets.charstrings as i32);
    Operator::CHARSTRINGS.write(&mut out);
    out
}

fn compile_private_dict(
    values: &PrivateDictValues,
    default_width: i32,
    nominal_width: i32,
) -> Vec<u8> {
    let mut out = Vec::new();
    let delta_entry = |out: &mut Vec<u8>, values: &[i32], op: Operator| {
        if !values.is_empty() {
            dict::encode_delta(out, values);
            op.write(out);
        }
    };
    // operator order matches fontTools' privateDictOperators
    delta_entry(&mut out, &values.blue_values, Operator::BLUE_VALUES);
    delta_entry(&mut out, &values.other_blues, Operator::OTHER_BLUES);
    delta_entry(&mut out, &values.family_blues, Operator::FAMILY_BLUES);
    delta_entry(
        &mut out,
        &values.family_other_blues,
        Operator::FAMILY_OTHER_BLUES,
    );
    if let Some(blue_scale) = values.blue_scale {
        if blue_scale != 0.039625 {
            dict::encode_real(&mut out, blue_scale);
            Operator::BLUE_SCALE.write(&mut out);
        }
    }
    if let Some(blue_shift) = values.blue_shift {
        if blue_shift != 7 {
            dict::encode_int(&mut out, blue_shift);
            Operator::BLUE_SHIFT.write(&mut out);
        }
    }
    if let Some(blue_fuzz) = values.blue_fuzz {
        if blue_fuzz != 1 {
            dict::encode_int(&mut out, blue_fuzz);
            Operator::BLUE_FUZZ.write(&mut out);
        }
    }
    if let Some(std_hw) = values.std_hw {
        dict::encode_int(&mut out, std_hw);
        Operator::STD_HW.write(&mut out);
    }
    if let Some(std_vw) = values.std_vw {
        dict::encode_int(&mut out, std_vw);
        Operator::STD_VW.write(&mut out);
    }
    delta_entry(&mut out, &values.stem_snap_h, Operator::STEM_SNAP_H);
    delta_entry(&mut out, &values.stem_snap_v, Operator::STEM_SNAP_V);
    if values.force_bold == Some(true) {
        dict::encode_int(&mut out, 1);
        Operator::FORCE_BOLD.write(&mut out);
    }
    if let Some(language_group) = values.language_group {
        if language_group != 0 {
            dict::encode_int(&mut out, language_group);
            Operator::LANGUAGE_GROUP.write(&mut out);
        }
    }
    if default_width != 0 {
        dict::encode_int(&mut out, default_width);
        Operator::DEFAULT_WIDTH_X.write(&mut out);
    }
    if nominal_width != 0 {
        dict::encode_int(&mut out, nominal_width);
        Operator::NOMINAL_WIDTH_X.write(&mut out);
    }
    out
}

/// Interns non-standard strings, assigning SIDs beyond the 391 standard ones.
struct StringInterner {
    strings: Vec<String>,
    lookup: HashMap<String, u16>,
}

impl StringInterner {
    fn new() -> Self {
        StringInterner {
            strings: Vec::new(),
            lookup: HashMap::new(),
        }
    }

    fn get_sid(&mut self, s: &str) -> Result<u16, CffBuildError> {
        if let Some(sid) = standard_sid(s) {
            return Ok(sid);
        }
        if let Some(sid) = self.lookup.get(s) {
            return Ok(*sid);
        }
        let sid = u16::try_from(STANDARD_STRINGS.len() + self.strings.len())
            .map_err(|_| CffBuildError::TooManyStrings)?;
        self.strings.push(s.to_string());
        self.lookup.insert(s.to_string(), sid);
        Ok(sid)
    }
}

fn standard_sid(s: &str) -> Option<u16> {
    static LOOKUP: std::sync::OnceLock<HashMap<&'static str, u16>> = std::sync::OnceLock::new();
    LOOKUP
        .get_or_init(|| {
            STANDARD_STRINGS
                .iter()
                .enumerate()
                .map(|(i, s)| (*s, i as u16))
                .collect()
        })
        .get(s)
        .copied()
}

/// The Expert charset (CFF spec appendix C), as SIDs: every name in it is a
/// standard string, and standard strings are unique, so comparing SIDs is the
/// same test fontTools' `getStdCharSet` makes on names.
#[rustfmt::skip]
const EXPERT_CHARSET: &[u16] = &[
    0, 1, 229, 230, 231, 232, 233, 234, 235, 236, 237, 238,
    13, 14, 15, 99, 239, 240, 241, 242, 243, 244, 245, 246,
    247, 248, 27, 28, 249, 250, 251, 252, 253, 254, 255, 256,
    257, 258, 259, 260, 261, 262, 263, 264, 265, 266, 109, 110,
    267, 268, 269, 270, 271, 272, 273, 274, 275, 276, 277, 278,
    279, 280, 281, 282, 283, 284, 285, 286, 287, 288, 289, 290,
    291, 292, 293, 294, 295, 296, 297, 298, 299, 300, 301, 302,
    303, 304, 305, 306, 307, 308, 309, 310, 311, 312, 313, 314,
    315, 316, 317, 318, 158, 155, 163, 319, 320, 321, 322, 323,
    324, 325, 326, 150, 164, 169, 327, 328, 329, 330, 331, 332,
    333, 334, 335, 336, 337, 338, 339, 340, 341, 342, 343, 344,
    345, 346, 347, 348, 349, 350, 351, 352, 353, 354, 355, 356,
    357, 358, 359, 360, 361, 362, 363, 364, 365, 366, 367, 368,
    369, 370, 371, 372, 373, 374, 375, 376, 377, 378,
];

/// The Expert Subset charset, as SIDs. See [`EXPERT_CHARSET`].
#[rustfmt::skip]
const EXPERT_SUBSET_CHARSET: &[u16] = &[
    0, 1, 231, 232, 235, 236, 237, 238, 13, 14, 15, 99,
    239, 240, 241, 242, 243, 244, 245, 246, 247, 248, 27, 28,
    249, 250, 251, 253, 254, 255, 256, 257, 258, 259, 260, 261,
    262, 263, 264, 265, 266, 109, 110, 267, 268, 269, 270, 272,
    300, 301, 302, 305, 314, 315, 158, 155, 163, 320, 321, 322,
    323, 324, 325, 326, 150, 164, 169, 327, 328, 329, 330, 331,
    332, 333, 334, 335, 336, 337, 338, 339, 340, 341, 342, 343,
    344, 345, 346,
];

/// The ISOAdobe charset is the first 229 standard strings, in order.
const ISO_ADOBE_CHARSET_LEN: usize = 229;

/// Port of fontTools' `getStdCharSet`: the id of the predefined charset whose
/// order the glyph names follow, if any. `sids` excludes `.notdef`, which is
/// the first entry of every predefined charset.
fn predefined_charset(sids: &[u16]) -> Option<u8> {
    let is_prefix_of = |charset: &[u16]| {
        sids.len() < charset.len() && sids.iter().zip(&charset[1..]).all(|(a, b)| a == b)
    };
    if sids.len() < ISO_ADOBE_CHARSET_LEN
        && sids
            .iter()
            .enumerate()
            .all(|(i, sid)| *sid as usize == i + 1)
    {
        Some(0)
    } else if is_prefix_of(EXPERT_CHARSET) {
        Some(1)
    } else if is_prefix_of(EXPERT_SUBSET_CHARSET) {
        Some(2)
    } else {
        None
    }
}

/// Compile the charset, choosing the smallest of format 0/1/2 like fontTools
/// (ties go to format 0). `sids` excludes `.notdef`.
fn compile_charset(sids: &[u16]) -> Vec<u8> {
    let mut format0 = Vec::with_capacity(1 + sids.len() * 2);
    format0.push(0u8);
    for sid in sids {
        format0.extend_from_slice(&sid.to_be_bytes());
    }

    // format 1/2: ranges of consecutive SIDs
    let mut ranges: Vec<(u16, u16)> = Vec::new(); // (first, n_left)
    for sid in sids {
        match ranges.last_mut() {
            Some((first, n_left)) if *sid as u32 == *first as u32 + *n_left as u32 + 1 => {
                *n_left += 1;
            }
            _ => ranges.push((*sid, 0)),
        }
    }
    let format = if ranges.iter().any(|(_, n_left)| *n_left > 255) {
        2u8
    } else {
        1u8
    };
    let mut ranged = Vec::with_capacity(1 + ranges.len() * 4);
    ranged.push(format);
    for (first, n_left) in ranges {
        ranged.extend_from_slice(&first.to_be_bytes());
        if format == 1 {
            ranged.push(n_left as u8);
        } else {
            ranged.extend_from_slice(&n_left.to_be_bytes());
        }
    }

    if ranged.len() < format0.len() {
        ranged
    } else {
        format0
    }
}

fn calc_off_size(value: u32) -> u8 {
    match value {
        0..=0xff => 1,
        0x100..=0xffff => 2,
        0x10000..=0xffffff => 3,
        _ => 4,
    }
}

/// The serialized size of an INDEX containing items of the given lengths.
fn index_size(item_lens: &[usize]) -> usize {
    if item_lens.is_empty() {
        return 2;
    }
    let data_len: usize = item_lens.iter().sum();
    let off_size = calc_off_size(data_len as u32 + 1) as usize;
    2 + 1 + (item_lens.len() + 1) * off_size + data_len
}

fn write_index_to(out: &mut Vec<u8>, items: &[Vec<u8>]) {
    out.extend_from_slice(&(items.len() as u16).to_be_bytes());
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

fn compile_index(items: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    write_index_to(&mut out, items);
    out
}

/// The FontBBox fontTools would compute for these glyphs.
///
/// fontTools recalculates the FontBBox whenever it compiles a CFF table
/// (`CFFFontSet.compile` -> `TopDict.recalcFontBBox`), discarding whatever
/// ufo2ft put there: it unions the *exact* charstring bounds and then grows
/// the result outward to integers with `intRect`, flooring the minimums and
/// ceiling the maximums. That is deliberately not the union of the rounded
/// per-glyph boxes, which can be a unit smaller on each side.
///
/// With no bounded glyph at all fontTools falls back to the DICT default,
/// which is all zeros.
fn font_bbox(glyphs: &[GlyphData]) -> [i32; 4] {
    glyphs
        .iter()
        .filter_map(|g| g.bounds)
        .reduce(|a, b| a.union(b))
        .map(|r| {
            [
                r.x0.floor() as i32,
                r.y0.floor() as i32,
                r.x1.ceil() as i32,
                r.y1.ceil() as i32,
            ]
        })
        .unwrap_or_default()
}

/// Port of fontTools' `cffLib.width.optimizeWidths`: pick the
/// `(defaultWidthX, nominalWidthX)` pair minimizing total charstring width
/// bytes. Linear in the width range + glyph count.
fn optimize_widths(widths: &[i32]) -> (i32, i32) {
    debug_assert!(!widths.is_empty());
    let mut freq: HashMap<i32, i64> = HashMap::new();
    for w in widths {
        *freq.entry(*w).or_default() += 1;
    }
    let min_w = *freq.keys().min().unwrap();
    let max_w = *freq.keys().max().unwrap();

    let cum_frq_u = CumTable::new(&freq, min_w, max_w, false, |a, b| a + b);
    let cum_max_u = CumTable::new(&freq, min_w, max_w, false, |a, b| a.max(b));
    let cum_frq_d = CumTable::new(&freq, min_w, max_w, true, |a, b| a + b);
    let cum_max_d = CumTable::new(&freq, min_w, max_w, true, |a, b| a.max(b));

    let freq_at = |x: i32| freq.get(&x).copied().unwrap_or(0);
    let nomn_cost_u =
        |x: i32| cum_frq_u.get(x) + cum_frq_u.get(x - 108) + cum_frq_u.get(x - 1132) * 3;
    let nomn_cost_d =
        |x: i32| cum_frq_d.get(x) + cum_frq_d.get(x + 108) + cum_frq_d.get(x + 1132) * 3;
    let nomn_cost = |x: i32| nomn_cost_u(x) + nomn_cost_d(x) - freq_at(x);
    let dflt_cost_u = |x: i32| {
        cum_max_u
            .get(x)
            .max(cum_max_u.get(x - 108) * 2)
            .max(cum_max_u.get(x - 1132) * 5)
    };
    let dflt_cost_d = |x: i32| {
        cum_max_d
            .get(x)
            .max(cum_max_d.get(x + 108) * 2)
            .max(cum_max_d.get(x + 1132) * 5)
    };
    let dflt_cost = |x: i32| dflt_cost_u(x).max(dflt_cost_d(x));
    let best_cost = |x: i32| nomn_cost(x) - dflt_cost(x);

    let nominal = (min_w..=max_w).min_by_key(|x| (best_cost(*x), *x)).unwrap();

    // work back the best default
    let dflt = nomn_cost(nominal) - best_cost(nominal);
    let mut ends = Vec::new();
    if dflt == dflt_cost_u(nominal) {
        for mut start in [nominal, nominal - 108, nominal - 1132] {
            while cum_max_u.get(start) != 0 && cum_max_u.get(start) == cum_max_u.get(start - 1) {
                start -= 1;
            }
            ends.push(start);
        }
    } else {
        for mut start in [nominal, nominal + 108, nominal + 1132] {
            while cum_max_d.get(start) != 0 && cum_max_d.get(start) == cum_max_d.get(start + 1) {
                start += 1;
            }
            ends.push(start);
        }
    }
    let default = *ends
        .iter()
        .min_by_key(|d| byte_cost(&freq, Some(**d), nominal))
        .unwrap();
    (default, nominal)
}

fn byte_cost(freq: &HashMap<i32, i64>, default: Option<i32>, nominal: i32) -> i64 {
    let mut cost = 0;
    for (w, count) in freq {
        if Some(*w) == default {
            continue;
        }
        let diff = (w - nominal).abs();
        cost += match diff {
            0..=107 => *count,
            108..=1131 => *count * 2,
            _ => *count * 5,
        };
    }
    cost
}

/// Cumulative sum/max over a frequency table, mirroring fontTools' `cumSum`:
/// out-of-domain lookups before the accumulation start yield 0, past the end
/// they yield the total.
struct CumTable {
    min: i32,
    max: i32,
    values: Vec<i64>,
    total: i64,
    decreasing: bool,
}

impl CumTable {
    fn new(
        freq: &HashMap<i32, i64>,
        min: i32,
        max: i32,
        decreasing: bool,
        op: fn(i64, i64) -> i64,
    ) -> Self {
        let len = (max - min + 1) as usize;
        let mut values = vec![0i64; len];
        let mut acc = 0i64;
        if decreasing {
            for x in (min..=max).rev() {
                acc = op(acc, freq.get(&x).copied().unwrap_or(0));
                values[(x - min) as usize] = acc;
            }
        } else {
            for x in min..=max {
                acc = op(acc, freq.get(&x).copied().unwrap_or(0));
                values[(x - min) as usize] = acc;
            }
        }
        CumTable {
            min,
            max,
            values,
            total: acc,
            decreasing,
        }
    }

    fn get(&self, x: i32) -> i64 {
        if x < self.min {
            if self.decreasing {
                self.total
            } else {
                0
            }
        } else if x > self.max {
            if self.decreasing {
                0
            } else {
                self.total
            }
        } else {
            self.values[(x - self.min) as usize]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brute_force(widths: &[i32]) -> (i32, i32) {
        let mut freq: HashMap<i32, i64> = HashMap::new();
        for w in widths {
            *freq.entry(*w).or_default() += 1;
        }
        let min_w = *freq.keys().min().unwrap();
        let max_w = *freq.keys().max().unwrap();
        let mut best = (i64::MAX, 0, 0);
        for nominal in min_w..=max_w {
            for default in min_w..=max_w {
                let cost = byte_cost(&freq, Some(default), nominal);
                if cost < best.0 {
                    best = (cost, default, nominal);
                }
            }
        }
        (best.1, best.2)
    }

    #[test]
    fn optimize_widths_uniform() {
        // every width identical: default == nominal == that width
        assert_eq!(optimize_widths(&[500; 5]), (500, 500));
    }

    #[test]
    fn optimize_widths_matches_bruteforce_cost() {
        for widths in [
            vec![500, 500, 600, 600, 600, 250],
            vec![1, 2, 3, 4, 5],
            vec![100, 900],
            vec![250, 250, 250, 1200, 1200, 640, 640, 640, 640],
        ] {
            let mut freq: HashMap<i32, i64> = HashMap::new();
            for w in &widths {
                *freq.entry(*w).or_default() += 1;
            }
            let (d, n) = optimize_widths(&widths);
            let (bd, bn) = brute_force(&widths);
            assert_eq!(
                byte_cost(&freq, Some(d), n),
                byte_cost(&freq, Some(bd), bn),
                "widths {widths:?}: got ({d},{n}), brute force ({bd},{bn})"
            );
        }
    }

    #[test]
    fn charset_format_selection() {
        // 4 scattered SIDs: format 0 is 9 bytes, format 1 is 13 -> format 0
        assert_eq!(
            compile_charset(&[1, 93, 12, 391]),
            [0, 0, 1, 0, 93, 0, 12, 1, 135]
        );
        // long consecutive run: format 1 wins
        let sids: Vec<u16> = (391..451).collect();
        assert_eq!(compile_charset(&sids), [1, 1, 135, 59]);
    }

    #[test]
    fn predefined_charset_ids() {
        // .notdef, space, exclam: the start of ISOAdobe
        assert_eq!(predefined_charset(&[1, 2]), Some(0));
        // .notdef, space, bar
        assert_eq!(predefined_charset(&[1, 93]), None);
        // the whole ISOAdobe charset is 229 names, .notdef included
        assert_eq!(predefined_charset(&(1..229).collect::<Vec<_>>()), Some(0));
        assert_eq!(predefined_charset(&(1..230).collect::<Vec<_>>()), None);
        // .notdef, space, exclamsmall, Hungarumlautsmall
        assert_eq!(predefined_charset(&[1, 229, 230]), Some(1));
        // .notdef, space, exclamsmall, dollaroldstyle: Expert Subset skips
        // Hungarumlautsmall, so this is not Expert
        assert_eq!(predefined_charset(&[1, 229, 231]), None);
        assert_eq!(predefined_charset(&[1, 231, 232]), Some(2));
    }

    /// A font whose glyph names start the ISOAdobe charset writes the
    /// predefined id and no charset block. These are the bytes fontTools
    /// produces for the same font.
    #[test]
    fn iso_adobe_charset_matches_fonttools() {
        use super::super::charstring::CharstringBuilder;
        use kurbo::Point;

        fn corner(size: f64) -> Vec<u8> {
            let mut pen = CharstringBuilder::new();
            pen.move_to(Point::new(0.0, 0.0));
            pen.line_to(Point::new(size, 0.0));
            pen.line_to(Point::new(size, size));
            pen.close_path();
            pen.build(None, true).unwrap().bytes
        }

        let top_dict = TopDictValues {
            version: Some("1.0".into()),
            full_name: Some("Test Regular".into()),
            family_name: Some("Test".into()),
            font_matrix: Some([0.001, 0.0, 0.0, 0.001, 0.0, 0.0]),
            ..TopDictValues::new()
        };
        let mut builder = CffFontBuilder::new(
            "Test-Regular".into(),
            top_dict,
            PrivateDictValues::default(),
        );
        for (name, size) in [(".notdef", 100.0), ("space", 0.0), ("exclam", 50.0)] {
            builder.add_glyph(GlyphData {
                name: name.into(),
                advance_width: 500.0,
                charstring: if size == 0.0 {
                    CharstringBuilder::new().build(None, true).unwrap().bytes
                } else {
                    corner(size)
                },
                bounds: (size != 0.0).then(|| Rect::new(0.0, 0.0, size, size)),
            });
        }
        let cff = builder.build().unwrap();
        let hex: String = cff.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "01000401000101010d546573742d526567756c61720001010116f81b00f81c02\
             f81d038b8befef058b0f91ea12d61100030101041014312e30546573742052\
             6567756c61725465737400000003010107080e8b16efef060e0e8b16bdbd06\
             0ef88814f88815"
        );
    }

    /// ufo2ft builds the matrix as `[1.0/upem, 0, 0, 1.0/upem, 0, 0]`, so
    /// fontTools writes the scale as a real and the zeros as integers.
    #[test]
    fn font_matrix_zeros_are_integer_operands() {
        let values = TopDictValues {
            // upem 256, as in fontc's Float-Regular.ufo fixture
            font_matrix: Some([1.0 / 256.0, 0.0, 0.0, 1.0 / 256.0, 0.0, 0.0]),
            ..TopDictValues::new()
        };
        let dict = compile_top_dict(
            &values,
            &TopDictSids::default(),
            [0; 4],
            0,
            None,
            &TopDictOffsets::default(),
        );
        // expectations produced by fontTools' encodeNumber
        let operands: &[u8] = &[
            0x1e, 0x39, 0x06, 0x25, 0xc8, 0xff, // 0.00390625
            0x8b, 0x8b, // 0, 0
            0x1e, 0x39, 0x06, 0x25, 0xc8, 0xff, // 0.00390625
            0x8b, 0x8b, // 0, 0
        ];
        let mut expected = operands.to_vec();
        Operator::FONT_MATRIX.write(&mut expected);
        assert!(
            dict.windows(expected.len()).any(|w| w == expected),
            "FontMatrix not found in {dict:02x?}"
        );
        // the default matrix is still omitted entirely
        let default_matrix = TopDictValues {
            font_matrix: Some([0.001, 0.0, 0.0, 0.001, 0.0, 0.0]),
            ..TopDictValues::new()
        };
        let dict = compile_top_dict(
            &default_matrix,
            &TopDictSids::default(),
            [0; 4],
            0,
            None,
            &TopDictOffsets::default(),
        );
        let mut font_matrix_op = Vec::new();
        Operator::FONT_MATRIX.write(&mut font_matrix_op);
        assert!(!dict
            .windows(font_matrix_op.len())
            .any(|w| w == font_matrix_op));
    }

    #[test]
    fn index_layout() {
        assert_eq!(compile_index(&[]), [0, 0]);
        assert_eq!(
            compile_index(&[b"ab".to_vec(), b"c".to_vec()]),
            [0, 2, 1, 1, 3, 4, b'a', b'b', b'c']
        );
    }

    /// The `CFF ` table fontmake 3.12.1 produces for fontc's
    /// Static-Regular.ufo fixture with `--optimize-cff 1` (specialize, no
    /// subroutinization); this builder must reproduce it byte for byte.
    #[rustfmt::skip]
    const DUCK_CFF: &[u8] = &[
        // header
        0x01, 0x00, 0x04, 0x01,
        // Name INDEX: "Duck-Regular"
        0x00, 0x01, 0x01, 0x01, 0x0d,
        0x44, 0x75, 0x63, 0x6b, 0x2d, 0x52, 0x65, 0x67, 0x75, 0x6c, 0x61, 0x72,
        // Top DICT INDEX
        0x00, 0x01, 0x01, 0x01, 0x25,
        0xf8, 0x1c, 0x00, // version SID 392
        0xf8, 0x1d, 0x01, // Notice SID 393 ("")
        0xf8, 0x1d, 0x0c, 0x00, // Copyright SID 393 ("", deduplicated)
        0xf8, 0x1e, 0x02, // FullName SID 394
        0xf8, 0x1f, 0x03, // FamilyName SID 395
        0x40, 0x0c, 0x03, // UnderlinePosition -75
        0xb6, 0xfb, 0x85, 0xf8, 0x95, 0xf9, 0xb4, 0x05, // FontBBox 43 -241 513 800
        0xf1, 0x0f, // charset 102
        0x91, 0xf7, 0x4e, 0x12, // Private 6 186
        0xf7, 0x03, 0x11, // CharStrings 111
        // String INDEX: element_of, "0.0", "", "Duck Regular", "Duck"
        0x00, 0x05, 0x01, 0x01, 0x0b, 0x0e, 0x0e, 0x1a, 0x1e,
        0x65, 0x6c, 0x65, 0x6d, 0x65, 0x6e, 0x74, 0x5f, 0x6f, 0x66,
        0x30, 0x2e, 0x30,
        0x44, 0x75, 0x63, 0x6b, 0x20, 0x52, 0x65, 0x67, 0x75, 0x6c, 0x61, 0x72,
        0x44, 0x75, 0x63, 0x6b,
        // Global Subr INDEX (empty)
        0x00, 0x00,
        // charset format 0: space, bar, plus, element_of
        0x00, 0x00, 0x01, 0x00, 0x5d, 0x00, 0x0c, 0x01, 0x87,
        // CharStrings INDEX
        0x00, 0x05, 0x01, 0x01, 0x1b, 0x1c, 0x28, 0x42, 0x43,
        // .notdef: 50 -200 rmoveto 400 1000 -400 -1000 hlineto
        //          50 50 rmoveto 900 300 -900 -300 vlineto endchar
        0xbd, 0xfb, 0x5c, 0x15, 0xf8, 0x24, 0xfa, 0x7c, 0xfc, 0x24, 0xfe, 0x7c,
        0x06, 0xbd, 0xbd, 0x15, 0xfa, 0x18, 0xf7, 0xc0, 0xfe, 0x18, 0xfb, 0xc0,
        0x07, 0x0e,
        // space: endchar
        0x0e,
        // bar: 17 222 -241 rmoveto 73 1001 -73 hlineto endchar
        0x9c, 0xf7, 0x72, 0xfb, 0x85, 0x15, 0xd4, 0xfa, 0x7d, 0x42, 0x06, 0x0e,
        // plus: 57 242 111 rmoveto
        //       72 206 199 71 -199 207 -72 -207 -199 -71 199 hlineto endchar
        0xc4, 0xf7, 0x86, 0xf7, 0x03, 0x15, 0xd3, 0xf7, 0x62, 0xf7, 0x5b, 0xd2,
        0xfb, 0x5b, 0xf7, 0x63, 0x43, 0xfb, 0x63, 0xfb, 0x5b, 0x44, 0xf7, 0x5b,
        0x06, 0x0e,
        // element_of: endchar
        0x0e,
        // Private DICT: defaultWidthX 500, nominalWidthX 500
        0xf8, 0x88, 0x14, 0xf8, 0x88, 0x15,
    ];

    /// fontTools grows the FontBBox outward to integers (`intRect`) from the
    /// union of the *exact* charstring bounds, so a curve that overshoots its
    /// on-curve points by a fraction of a unit widens the box by a whole unit
    /// on that side. Rounding each glyph box first would lose that unit.
    ///
    /// The numbers are the ones AlefAlefAlef/GveretLevin exposed: fontc used
    /// to write [-479, -418, 1131, 1217] where fontmake wrote
    /// [-480, -418, 1132, 1218].
    #[test]
    fn font_bbox_encloses_unrounded_bounds() {
        let glyph = |name: &str, bounds| GlyphData {
            name: name.into(),
            advance_width: 500.0,
            charstring: Vec::new(),
            bounds,
        };
        let glyphs = [
            glyph(".notdef", Some(Rect::new(-479.36, -320.09, 232.81, 562.42))),
            glyph("a", Some(Rect::new(-41.22, -418.0, 749.07, 1217.34))),
            glyph("b", Some(Rect::new(-13.88, -26.58, 1131.11, 687.0))),
            glyph("space", None),
        ];
        // mins floored and maxes ceiled, *not* each side rounded to nearest —
        // that would give [-479, -418, 1131, 1217]
        assert_eq!(font_bbox(&glyphs), [-480, -418, 1132, 1218]);
        // an integral side stays put in both directions
        assert_eq!(font_bbox(&glyphs)[1], -418);
        // no bounded glyph at all: the DICT default
        assert_eq!(font_bbox(&[glyph("space", None)]), [0, 0, 0, 0]);
    }

    #[test]
    fn duck_matches_fontmake_bytes() {
        use super::super::charstring::CharstringBuilder;
        use kurbo::Point;

        fn lines(points: &[(f64, f64)]) -> Vec<u8> {
            let mut pen = CharstringBuilder::new();
            let mut iter = points.iter();
            let first = iter.next().unwrap();
            pen.move_to(Point::new(first.0, first.1));
            for (x, y) in iter {
                pen.line_to(Point::new(*x, *y));
            }
            pen.close_path();
            pen.build(None, true).unwrap().bytes
        }

        fn contours(contours: &[&[(f64, f64)]]) -> Vec<u8> {
            let mut pen = CharstringBuilder::new();
            for points in contours {
                let mut iter = points.iter();
                let first = iter.next().unwrap();
                pen.move_to(Point::new(first.0, first.1));
                for (x, y) in iter {
                    pen.line_to(Point::new(*x, *y));
                }
                pen.close_path();
            }
            pen.build(None, true).unwrap().bytes
        }

        let top_dict = TopDictValues {
            version: Some("0.0".into()),
            notice: Some("".into()),
            copyright: Some("".into()),
            full_name: Some("Duck Regular".into()),
            family_name: Some("Duck".into()),
            underline_position: -75,
            underline_thickness: 50,
            font_matrix: Some([0.001, 0.0, 0.0, 0.001, 0.0, 0.0]),
            ..Default::default()
        };
        let mut builder = CffFontBuilder::new(
            "Duck-Regular".into(),
            top_dict,
            PrivateDictValues::default(),
        );
        // ufo2ft's synthesized .notdef box: both contours draw their closing
        // line explicitly
        builder.add_glyph(GlyphData {
            name: ".notdef".into(),
            advance_width: 500.0,
            charstring: contours(&[
                &[
                    (50.0, -200.0),
                    (450.0, -200.0),
                    (450.0, 800.0),
                    (50.0, 800.0),
                    (50.0, -200.0),
                ],
                &[
                    (100.0, -150.0),
                    (100.0, 750.0),
                    (400.0, 750.0),
                    (400.0, -150.0),
                    (100.0, -150.0),
                ],
            ]),
            bounds: Some(Rect::new(50.0, -200.0, 450.0, 800.0)),
        });
        builder.add_glyph(GlyphData {
            name: "space".into(),
            advance_width: 500.0,
            charstring: CharstringBuilder::new().build(None, true).unwrap().bytes,
            bounds: None,
        });
        builder.add_glyph(GlyphData {
            name: "bar".into(),
            advance_width: 517.0,
            charstring: lines(&[
                (222.0, -241.0),
                (295.0, -241.0),
                (295.0, 760.0),
                (222.0, 760.0),
            ]),
            bounds: Some(Rect::new(222.0, -241.0, 295.0, 760.0)),
        });
        builder.add_glyph(GlyphData {
            name: "plus".into(),
            advance_width: 557.0,
            charstring: lines(&[
                (242.0, 111.0),
                (314.0, 111.0),
                (314.0, 317.0),
                (513.0, 317.0),
                (513.0, 388.0),
                (314.0, 388.0),
                (314.0, 595.0),
                (242.0, 595.0),
                (242.0, 388.0),
                (43.0, 388.0),
                (43.0, 317.0),
                (242.0, 317.0),
            ]),
            bounds: Some(Rect::new(43.0, 111.0, 513.0, 595.0)),
        });
        builder.add_glyph(GlyphData {
            name: "element_of".into(),
            advance_width: 500.0,
            charstring: CharstringBuilder::new().build(None, true).unwrap().bytes,
            bounds: None,
        });

        let cff = builder.build().unwrap();
        assert_eq!(
            cff.as_bytes(),
            DUCK_CFF,
            "\nbuilt:    {}\nexpected: {}",
            cff.as_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
            DUCK_CFF
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
    }

    #[test]
    fn duck_round_trips_through_read_fonts() {
        use read_fonts::ps::cff::CffFontRef;

        let font = CffFontRef::new_cff(DUCK_CFF, 0, Some(1000)).unwrap();
        assert!(!font.is_cid());
        assert_eq!(font.charstrings().count(), 5);
        let charset = font.charset().unwrap();
        let names: Vec<_> = (0..5u32)
            .map(|gid| {
                let sid = charset.string_id(gid.into()).unwrap();
                String::from_utf8(font.string(sid).unwrap().to_vec()).unwrap()
            })
            .collect();
        assert_eq!(names, [".notdef", "space", "bar", "plus", "element_of"]);
    }

    #[test]
    fn standard_sids() {
        assert_eq!(standard_sid(".notdef"), Some(0));
        assert_eq!(standard_sid("space"), Some(1));
        assert_eq!(standard_sid("plus"), Some(12));
        assert_eq!(standard_sid("bar"), Some(93));
        assert_eq!(standard_sid("Regular"), Some(388));
        assert_eq!(standard_sid("element_of"), None);
    }
}

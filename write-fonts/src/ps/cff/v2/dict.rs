//! Encoding of CFF2 DICT values, including blends.
//!
//! A CFF2 Private DICT value can vary across the font's variation regions:
//! the operand list holds the default master's value(s) followed by one delta
//! per region, a count, and the `blend` operator. The layouts here are ports
//! of `cffLib.DictCompiler.arg_number` and `arg_delta_blend`
//! (`fontTools/cffLib/__init__.py:2233,2269`), which is what `varLib.cff`
//! ends up calling, so they byte-match fontmake's `-o variable-cff2` output
//! given the same values.
//!
//! Note that fractional operands are packed-BCD reals (operand form 30), just
//! as in CFF1 — the 16.16 fixed form (255) that CFF2 adds belongs to
//! charstrings, not DICTs. Measured on the fixture in the research doc: the
//! `BlueScale` blend of a two-master CFF2 is `1e 37 5c 4f 1e 3c 4f 8c 17`,
//! i.e. two BCD reals, a `1` and `blend`.

use crate::ps::cff::dict::{Number, Operator};

/// The most operands that may precede an operator in a CFF2 DICT.
///
/// `cffLib`'s `maxStackLimit` (`cffLib/__init__.py:44`), used as
/// `PrivateDictCompiler.maxBlendStack` (`:2454`).
pub const MAX_BLEND_STACK: usize = 513;

/// A DICT value that may vary across the variation regions of a CFF2 font.
///
/// `deltas` holds one delta per region, in region order; when it is empty the
/// value is the same in every master and is written as a plain operand.
///
/// Whether each number is written as an integer or a real is the caller's
/// choice, exactly as it is fontTools'; see [`Number::conv_to_int`].
#[derive(Clone, Debug, PartialEq)]
pub struct Blend {
    /// The default master's value.
    pub default: Number,
    /// One delta per variation region, or empty if the value does not vary.
    pub deltas: Vec<Number>,
}

impl Blend {
    /// A value that is the same in every master.
    pub fn scalar(value: impl Into<Number>) -> Self {
        Blend {
            default: value.into(),
            deltas: Vec::new(),
        }
    }

    /// A value that varies: the default master's value and one delta per
    /// region.
    pub fn new(default: impl Into<Number>, deltas: Vec<Number>) -> Self {
        Blend {
            default: default.into(),
            deltas,
        }
    }

    /// Whether this value varies across regions.
    pub fn varies(&self) -> bool {
        !self.deltas.is_empty()
    }
}

/// Write a single (possibly blended) numeric operand.
///
/// A blended value is `[default, delta…, 1, blend]`: `numBlends` is literally
/// 1, since each scalar entry blends on its own
/// (`cffLib/__init__.py:2233-2241`).
pub fn write_number(out: &mut Vec<u8>, value: &Blend) {
    value.default.write(out);
    if !value.varies() {
        return;
    }
    for delta in &value.deltas {
        delta.write(out);
    }
    Number::Int(1).write(out);
    Operator::BLEND.write(out);
}

/// Write a list operand ("delta" in cffLib's terms: `BlueValues`,
/// `StemSnapH`, …).
///
/// With no blending this is the usual cumulative-delta encoding. With
/// blending it is the layout of `arg_delta_blend`
/// (`cffLib/__init__.py:2269-2334`): every row's *default* value relative to
/// the previous row's, then each row's region deltas verbatim, then the row
/// count and `blend`.
///
/// # Panics
///
/// If the rows disagree about how many regions there are. fontTools decides
/// blended-or-not from the first row alone and then assumes the rest match,
/// which would silently produce a mis-shaped operand list.
pub fn write_delta(out: &mut Vec<u8>, values: &[Blend]) {
    let Some(first) = values.first() else {
        return;
    };
    assert!(
        values.iter().all(|v| v.deltas.len() == first.deltas.len()),
        "every row of a blended DICT list must have the same number of deltas"
    );
    if !first.varies() {
        // plain cumulative deltas, like `arg_delta`'s non-blend branch
        let mut last = Number::Int(0);
        for value in values {
            value.default.sub(last).write(out);
            last = value.default;
        }
        return;
    }
    write_delta_blend(out, values, first.deltas.len() + 1);
}

/// `arg_delta_blend`, including its chunking of long lists.
///
/// `num_masters` is the default master plus one per region; it is passed down
/// so the chunk size is computed once.
fn write_delta_blend(out: &mut Vec<u8>, values: &[Blend], num_masters: usize) {
    if values.len() * num_masters + 1 > MAX_BLEND_STACK {
        // Split into chunks that fit, each of which is a complete
        // `… numBlends blend` group of its own. Note that each chunk starts
        // its running default value from zero again, so the first default
        // value of a chunk is absolute rather than relative to the last value
        // of the previous chunk (`cffLib/__init__.py:2301-2314`, where the
        // recursive call re-enters with `prevVal = 0`).
        let rows_per_chunk = (MAX_BLEND_STACK - 1) / num_masters;
        assert!(
            rows_per_chunk > 0,
            "a single blended row does not fit the operand stack"
        );
        for chunk in values.chunks(rows_per_chunk) {
            write_delta_blend(out, chunk, num_masters);
        }
        return;
    }

    let mut prev = Number::Int(0);
    for value in values {
        value.default.sub(prev).write(out);
        prev = value.default;
    }
    for value in values {
        for delta in &value.deltas {
            delta.write(out);
        }
    }
    Number::Int(values.len() as i32).write(out);
    Operator::BLEND.write(out);
}

#[cfg(test)]
mod tests {
    use read_fonts::ps::cff::{
        dict::{tokens, Operator as ReadOperator, Token},
        stack::Number as ReadNumber,
    };

    use super::*;

    fn ints(values: &[i32]) -> Vec<Number> {
        values.iter().copied().map(Number::Int).collect()
    }

    fn number_bytes(value: &Blend) -> Vec<u8> {
        let mut out = Vec::new();
        write_number(&mut out, value);
        out
    }

    fn delta_bytes(values: &[Blend]) -> Vec<u8> {
        let mut out = Vec::new();
        write_delta(&mut out, values);
        out
    }

    #[test]
    fn scalar_blend_matches_fonttools() {
        // fontTools: PrivateDictCompiler.arg_number([20, 10])
        assert_eq!(
            number_bytes(&Blend::new(20, ints(&[10]))),
            [0x9f, 0x95, 0x8c, 0x17]
        );
        // an unvarying value is just the number
        assert_eq!(number_bytes(&Blend::scalar(20)), [0x9f]);
    }

    #[test]
    fn blended_reals_encode_as_bcd() {
        // Probe's BlueScale: fontmake writes `[0.0375, 0.0003]` as two BCD
        // reals, a 1 and `blend` — CFF2 DICTs have no 16.16 operand form
        assert_eq!(
            number_bytes(&Blend::new(0.0375, vec![Number::Real(0.0003)])),
            [0x1e, 0x37, 0x5c, 0x4f, 0x1e, 0x3c, 0x4f, 0x8c, 0x17]
        );
    }

    #[test]
    fn unvarying_list_is_a_plain_delta() {
        // fontTools: arg_delta([-10, 0, 500, 512, 700, 712])
        let values: Vec<_> = [-10, 0, 500, 512, 700, 712]
            .iter()
            .map(|v| Blend::scalar(*v))
            .collect();
        assert_eq!(
            delta_bytes(&values),
            [0x81, 0x95, 0xf8, 0x88, 0x97, 0xf7, 0x50, 0x97]
        );
        assert!(delta_bytes(&[]).is_empty());
    }

    #[test]
    fn blended_list_matches_fonttools() {
        // Probe's BlueValues, straight out of the CFF2 fontmake produced:
        // defaults [-10, 0, 500, 512, 700, 712] and one delta each
        let rows = [(-10, -4), (0, 4), (500, 20), (512, 2), (700, -2), (712, -1)];
        let values: Vec<_> = rows
            .iter()
            .map(|(v, d)| Blend::new(*v, ints(&[*d])))
            .collect();
        assert_eq!(
            delta_bytes(&values),
            [
                // defaults, delta-encoded against the previous default
                0x81, 0x95, 0xf8, 0x88, 0x97, 0xf7, 0x50, 0x97,
                // then each row's region deltas, verbatim
                0x87, 0x8f, 0x9f, 0x8d, 0x89, 0x8a, //
                0x91, 0x17, // 6 blend
            ]
        );
    }

    #[test]
    fn mixed_int_and_real_defaults_promote_like_python() {
        // `defaultValue - prevVal` in Python: int - int stays an int, but
        // anything touching a float becomes one, so the 1.0 difference here
        // is written as a real, not as an integer operand
        let values = [
            Blend::new(0.5, vec![Number::Int(1)]),
            Blend::new(1.5, vec![Number::Int(2)]),
        ];
        assert_eq!(
            delta_bytes(&values),
            [
                0x1e, 0xa5, 0xff, // .5
                0x1e, 0x1f, // 1
                0x8c, 0x8d, // deltas 1, 2
                0x8d, 0x17, // 2 blend
            ]
        );
    }

    /// The whole Private DICT of the two-master `Probe.designspace` fixture,
    /// as `fontmake -o variable-cff2 --optimize-cff 1` writes it: 65 bytes at
    /// offset 161 of the CFF2 table. Every blend form appears in it — a
    /// blended list, a blended list with an all-zero delta row, blended
    /// reals, and blended scalars — and the entries are in
    /// `buildOrder(privateDictOperators2)` order.
    #[test]
    fn probe_private_dict_matches_fontmake() {
        let mut out = Vec::new();
        let entry_list = |out: &mut Vec<u8>, rows: &[(i32, i32)], op: Operator| {
            let values: Vec<_> = rows
                .iter()
                .map(|(v, d)| Blend::new(*v, ints(&[*d])))
                .collect();
            write_delta(out, &values);
            op.write(out);
        };
        let entry_num = |out: &mut Vec<u8>, value: Blend, op: Operator| {
            write_number(out, &value);
            op.write(out);
        };

        entry_list(
            &mut out,
            &[(-10, -4), (0, 4), (500, 20), (512, 2), (700, -2), (712, -1)],
            Operator::BLUE_VALUES,
        );
        entry_list(&mut out, &[(-12, -3), (0, 0)], Operator::OTHER_BLUES);
        entry_num(
            &mut out,
            Blend::new(0.0375, vec![Number::Real(0.0003)]),
            Operator::BLUE_SCALE,
        );
        // note: no BlueShift — the default master's value is the CFF default,
        // so cffLib never sees the key and the other master's value is lost
        entry_num(&mut out, Blend::new(0, ints(&[3])), Operator::BLUE_FUZZ);
        entry_num(&mut out, Blend::new(20, ints(&[3])), Operator::STD_HW);
        entry_num(&mut out, Blend::new(21, ints(&[3])), Operator::STD_VW);
        entry_list(&mut out, &[(20, 3), (40, 0)], Operator::STEM_SNAP_H);
        entry_list(&mut out, &[(21, 3)], Operator::STEM_SNAP_V);

        #[rustfmt::skip]
        let expected: &[u8] = &[
            0x81, 0x95, 0xf8, 0x88, 0x97, 0xf7, 0x50, 0x97,
            0x87, 0x8f, 0x9f, 0x8d, 0x89, 0x8a, 0x91, 0x17, 0x06,
            0x7f, 0x97, 0x88, 0x8b, 0x8d, 0x17, 0x07,
            0x1e, 0x37, 0x5c, 0x4f, 0x1e, 0x3c, 0x4f, 0x8c, 0x17, 0x0c, 0x09,
            0x8b, 0x8e, 0x8c, 0x17, 0x0c, 0x0b,
            0x9f, 0x8e, 0x8c, 0x17, 0x0a,
            0xa0, 0x8e, 0x8c, 0x17, 0x0b,
            0x9f, 0x9f, 0x8e, 0x8b, 0x8d, 0x17, 0x0c, 0x0c,
            0xa0, 0x8e, 0x8c, 0x17, 0x0c, 0x0d,
        ];
        assert_eq!(out, expected);
    }

    /// A list too long for the operand stack is split into chunks, and each
    /// chunk's first default value is absolute again.
    #[test]
    fn long_blended_list_is_chunked() {
        // 14 rows across 40 masters: 14 * 40 + 1 = 561 operands, over the
        // 513 limit, so cffLib emits chunks of int(512 / 40) = 12 rows
        let regions = 39;
        let values: Vec<_> = (0..14)
            .map(|i| Blend::new(i * 10, ints(&vec![i + 1; regions])))
            .collect();
        let bytes = delta_bytes(&values);

        // decode with read-fonts and check the shape
        let mut operands = Vec::new();
        let mut groups = Vec::new();
        for token in tokens(&bytes) {
            match token.unwrap() {
                Token::Operand(ReadNumber::I32(n), _) => operands.push(n),
                Token::Operand(n, _) => panic!("unexpected non-integer operand {n}"),
                Token::Operator(op) => {
                    assert_eq!(op, ReadOperator::Blend);
                    groups.push(std::mem::take(&mut operands));
                }
            }
        }
        assert!(operands.is_empty());
        assert_eq!(groups.len(), 2, "12 rows then 2");

        let expect_chunk = |rows: std::ops::Range<i32>| {
            let mut expected = Vec::new();
            // defaults, relative to the previous *within the chunk*
            let mut prev = 0;
            for i in rows.clone() {
                expected.push(i * 10 - prev);
                prev = i * 10;
            }
            for i in rows.clone() {
                expected.extend(std::iter::repeat_n(i + 1, regions));
            }
            expected.push(rows.len() as i32);
            expected
        };
        assert_eq!(groups[0], expect_chunk(0..12));
        assert_eq!(groups[1], expect_chunk(12..14));
        // the second chunk starts from zero again: 120, not 10
        assert_eq!(groups[1][0], 120);
        // fontTools produces this many bytes for the same input
        assert_eq!(bytes.len(), 565);
    }

    #[test]
    fn cff2_operators() {
        let mut out = Vec::new();
        for op in [
            Operator::MAXSTACK,
            Operator::FONT_MATRIX,
            Operator::FD_SELECT,
            Operator::FD_ARRAY,
            Operator::CHARSTRINGS,
            Operator::VSTORE,
            Operator::VS_INDEX,
            Operator::BLEND,
        ] {
            op.write(&mut out);
        }
        assert_eq!(
            out,
            [25, 12, 7, 12, 37, 12, 36, 17, 24, 22, 23],
            "buildOrder(topDictOperators2) then vsindex, blend"
        );
    }
}

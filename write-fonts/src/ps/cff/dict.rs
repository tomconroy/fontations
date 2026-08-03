//! Encoding of CFF DICT structures (operands and operators).
//!
//! A DICT is a sequence of operand(s)-then-operator tokens. Operand number
//! encoding is defined in section 4 of the CFF spec (Adobe TN #5176); real
//! numbers use the packed BCD ("nibble") representation.
//!
//! The encoding here matches fontTools' `cffLib`, including its AFDKO-derived
//! `%.8G` formatting of real numbers, so that fonts produced through this
//! module byte-match fonts produced by fontTools given the same values.

/// A CFF DICT operator, including two-byte escaped operators.
///
/// Two-byte operators (escape prefix 12) are represented as `0x0c00 | op`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Operator(u16);

impl Operator {
    pub const VERSION: Operator = Operator(0);
    pub const NOTICE: Operator = Operator(1);
    pub const COPYRIGHT: Operator = Operator(0x0c00);
    pub const FULL_NAME: Operator = Operator(2);
    pub const FAMILY_NAME: Operator = Operator(3);
    pub const WEIGHT: Operator = Operator(4);
    pub const IS_FIXED_PITCH: Operator = Operator(0x0c01);
    pub const ITALIC_ANGLE: Operator = Operator(0x0c02);
    pub const UNDERLINE_POSITION: Operator = Operator(0x0c03);
    pub const UNDERLINE_THICKNESS: Operator = Operator(0x0c04);
    pub const PAINT_TYPE: Operator = Operator(0x0c05);
    pub const CHARSTRING_TYPE: Operator = Operator(0x0c06);
    pub const FONT_MATRIX: Operator = Operator(0x0c07);
    pub const UNIQUE_ID: Operator = Operator(13);
    pub const FONT_BBOX: Operator = Operator(5);
    pub const STROKE_WIDTH: Operator = Operator(0x0c08);
    pub const XUID: Operator = Operator(14);
    pub const CHARSET: Operator = Operator(15);
    pub const ENCODING: Operator = Operator(16);
    pub const CHARSTRINGS: Operator = Operator(17);
    pub const PRIVATE: Operator = Operator(18);

    // Private DICT operators
    pub const BLUE_VALUES: Operator = Operator(6);
    pub const OTHER_BLUES: Operator = Operator(7);
    pub const FAMILY_BLUES: Operator = Operator(8);
    pub const FAMILY_OTHER_BLUES: Operator = Operator(9);
    pub const BLUE_SCALE: Operator = Operator(0x0c09);
    pub const BLUE_SHIFT: Operator = Operator(0x0c0a);
    pub const BLUE_FUZZ: Operator = Operator(0x0c0b);
    pub const STD_HW: Operator = Operator(10);
    pub const STD_VW: Operator = Operator(11);
    pub const STEM_SNAP_H: Operator = Operator(0x0c0c);
    pub const STEM_SNAP_V: Operator = Operator(0x0c0d);
    pub const FORCE_BOLD: Operator = Operator(0x0c0e);
    pub const LANGUAGE_GROUP: Operator = Operator(0x0c11);
    pub const EXPANSION_FACTOR: Operator = Operator(0x0c12);
    pub const INITIAL_RANDOM_SEED: Operator = Operator(0x0c13);
    pub const DEFAULT_WIDTH_X: Operator = Operator(20);
    pub const NOMINAL_WIDTH_X: Operator = Operator(21);
    pub const SUBRS: Operator = Operator(19);

    // CFF2 operators. `FDArray`/`FDSelect` exist in CFF1 too, but only for
    // CID-keyed fonts, which this crate does not write.
    pub const MAXSTACK: Operator = Operator(25);
    pub const FD_ARRAY: Operator = Operator(0x0c24);
    pub const FD_SELECT: Operator = Operator(0x0c25);
    pub const VS_INDEX: Operator = Operator(22);
    pub const BLEND: Operator = Operator(23);
    pub const VSTORE: Operator = Operator(24);

    pub fn write(self, out: &mut Vec<u8>) {
        if self.0 & 0x0c00 == 0x0c00 {
            out.push(12);
            out.push((self.0 & 0xff) as u8);
        } else {
            out.push(self.0 as u8);
        }
    }
}

/// A DICT operand.
///
/// fontTools encodes Python ints as integers and Python floats as packed-BCD
/// reals regardless of whether the float is integral; mirror that by choosing
/// the variant deliberately.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Number {
    Int(i32),
    Real(f64),
}

impl Number {
    pub fn write(self, out: &mut Vec<u8>) {
        match self {
            Number::Int(v) => encode_int(out, v),
            Number::Real(v) => encode_real(out, v),
        }
    }

    /// The value as a float, whichever form it is stored in.
    pub fn to_f64(self) -> f64 {
        match self {
            Number::Int(v) => v as f64,
            Number::Real(v) => v,
        }
    }

    /// Choose a form for `value` the way varLib's `conv_to_int` does: a value
    /// that is mathematically an integer becomes an integer operand, and
    /// everything else a real.
    ///
    /// varLib applies this to every blended Private DICT value before
    /// handing it to `cffLib`, which then picks the encoding by Python type
    /// (`fontTools/varLib/cff.py:60-63`, `cffLib/__init__.py:2337-2341`).
    pub fn conv_to_int(value: f64) -> Number {
        // matches Python `float.is_integer()`: infinities and NaN are not
        if value.fract() == 0.0
            && value.is_finite()
            && (i32::MIN as f64..=i32::MAX as f64).contains(&value)
        {
            Number::Int(value as i32)
        } else {
            Number::Real(value)
        }
    }

    /// `self - other`, choosing the result's form the way Python does: the
    /// difference of two integers is an integer, and anything involving a
    /// float is a float (and so encodes as a real, even when integral).
    pub(crate) fn sub(self, other: Number) -> Number {
        match (self, other) {
            (Number::Int(a), Number::Int(b)) => Number::Int(a - b),
            (a, b) => Number::Real(a.to_f64() - b.to_f64()),
        }
    }
}

impl From<i32> for Number {
    fn from(v: i32) -> Self {
        Number::Int(v)
    }
}

impl From<f64> for Number {
    fn from(v: f64) -> Self {
        Number::Real(v)
    }
}

/// Encode a DICT integer operand.
pub fn encode_int(out: &mut Vec<u8>, value: i32) {
    match value {
        -107..=107 => out.push((value + 139) as u8),
        108..=1131 => {
            let v = value - 108;
            out.push(((v >> 8) + 247) as u8);
            out.push((v & 0xff) as u8);
        }
        -1131..=-108 => {
            let v = -value - 108;
            out.push(((v >> 8) + 251) as u8);
            out.push((v & 0xff) as u8);
        }
        -32768..=32767 => {
            out.push(28);
            out.extend_from_slice(&(value as i16).to_be_bytes());
        }
        _ => {
            out.push(29);
            out.extend_from_slice(&value.to_be_bytes());
        }
    }
}

/// The number of bytes [`encode_int`] produces for `value`.
pub fn int_size(value: i32) -> usize {
    match value {
        -107..=107 => 1,
        -1131..=-108 | 108..=1131 => 2,
        -32768..=32767 => 3,
        _ => 5,
    }
}

/// Encode a DICT real operand using packed BCD, like fontTools.
///
/// The digit string is produced with C `%.8G` formatting (8 significant
/// digits) plus fontTools' AFDKO-compatible rewrites, so the emitted bytes
/// match fontTools' `encodeFloat` for the same value.
pub fn encode_real(out: &mut Vec<u8>, value: f64) {
    out.push(30);
    if value == 0.0 {
        out.push(0x0f);
        return;
    }
    let mut s = format_g8(value);
    // fontTools' rewrites of the %.8G output.
    if let Some(stripped) = s.strip_prefix("0.") {
        s = format!(".{stripped}");
    } else if let Some(stripped) = s.strip_prefix("-0.") {
        s = format!("-.{stripped}");
    } else if s.ends_with("000") {
        let significant = s.trim_end_matches('0');
        s = format!("{}E{}", significant, s.len() - significant.len());
    } else if let (Some(dot), Some(e)) = (s.find('.'), s.find('E')) {
        let integer = &s[..dot];
        let fraction = &s[dot + 1..e];
        let exponent: i32 = s[e + 1..].parse().unwrap();
        let new_exponent = exponent - fraction.len() as i32;
        if new_exponent == 1 {
            s = format!("{integer}{fraction}0");
        } else {
            s = format!("{integer}{fraction}E{new_exponent}");
        }
    }
    if s.starts_with(".0") || s.starts_with("-.0") {
        let (sign, rest) = s.split_once('.').unwrap();
        s = format!("{}{}E-{}", sign, rest.trim_start_matches('0'), rest.len());
    }

    // Nibble encoding: digits 0-9 are themselves, '.' is 0xa, 'E' is 0xb,
    // 'E-' is 0xc, '-' is 0xe, 0xf terminates.
    let mut nibbles = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        let nibble = match c {
            '0'..='9' => c as u8 - b'0',
            '.' => 0xa,
            '-' => 0xe,
            'E' => {
                let mut nib = 0xb;
                if chars.peek() == Some(&'-') {
                    chars.next();
                    nib = 0xc;
                } else if chars.peek() == Some(&'+') {
                    chars.next();
                }
                // drop a single leading zero of the exponent, like fontTools
                if chars.peek() == Some(&'0') {
                    chars.next();
                }
                nib
            }
            _ => unreachable!("unexpected char {c:?} in %.8G output {s:?}"),
        };
        nibbles.push(nibble);
    }
    nibbles.push(0xf);
    if nibbles.len() % 2 != 0 {
        nibbles.push(0xf);
    }
    for pair in nibbles.chunks(2) {
        out.push(pair[0] << 4 | pair[1]);
    }
}

/// The value a real operand comes back as once it has been through the DICT
/// encoder and read again.
///
/// A DICT real keeps only eight significant digits, so this is lossy — and
/// the loss is observable. varLib blends a CFF2 Private DICT against the
/// *default* master's `CFF ` table, which `convertCFFToCFF2` has already
/// re-read from bytes, while the region masters' values are whatever ufo2ft
/// computed; a caller reproducing those blends has to quantize the default
/// master's values the same way or it will find them equal when fontTools
/// finds a delta.
pub fn round_trip_real(value: f64) -> f64 {
    format_g8(value).parse().unwrap_or(value)
}

/// Format like C's `"%.8G"`: 8 significant digits, scientific notation when
/// the decimal exponent is < -4 or >= 8, trailing zeros removed.
fn format_g8(value: f64) -> String {
    let sci = format!("{value:.7E}");
    let (mantissa, exponent) = sci.split_once('E').unwrap();
    let exponent: i32 = exponent.parse().unwrap();
    if !(-4..8).contains(&exponent) {
        let mantissa = mantissa.trim_end_matches('0').trim_end_matches('.');
        // C pads the exponent to at least two digits.
        let sign = if exponent < 0 { "-" } else { "+" };
        return format!("{}E{}{:02}", mantissa, sign, exponent.abs());
    }
    let precision = (7 - exponent).max(0) as usize;
    let fixed = format!("{value:.precision$}");
    if fixed.contains('.') {
        fixed
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    } else {
        fixed
    }
}

/// Write `values` as a delta-encoded DICT array operand list.
///
/// The first element is absolute; each subsequent element is the difference
/// from the previous original value.
pub fn encode_delta(out: &mut Vec<u8>, values: &[i32]) {
    let mut last = 0;
    for value in values {
        encode_int(out, value - last);
        last = *value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cormorant's `BlueScale`: ufo2ft computes 3/(4*19), but the default
    /// master's copy has been through the encoder, and the difference is
    /// exactly the delta fontmake writes into the blended Private DICT
    /// (`<blend value="0.039473684 2.1052631e-10"/>`).
    #[test]
    fn a_real_loses_everything_past_eight_digits() {
        let exact = 3.0 / (4.0 * 19.0);
        assert_eq!(exact, 0.039473684210526314);
        assert_eq!(round_trip_real(exact), 0.039473684);
        assert_eq!(exact - round_trip_real(exact), 2.1052631166140756e-10);
        // an integral value, and one that needs the exponent form, survive
        assert_eq!(round_trip_real(0.06), 0.06);
        assert_eq!(round_trip_real(-12.0), -12.0);
        assert_eq!(round_trip_real(1.5e-9), 1.5e-9);
    }

    fn int_bytes(v: i32) -> Vec<u8> {
        let mut out = Vec::new();
        encode_int(&mut out, v);
        out
    }

    fn real_bytes(v: f64) -> Vec<u8> {
        let mut out = Vec::new();
        encode_real(&mut out, v);
        out
    }

    #[test]
    fn int_encoding_matches_fonttools() {
        // expectations produced by fontTools' encodeIntCFF
        assert_eq!(int_bytes(0), [0x8b]);
        assert_eq!(int_bytes(107), [0xf6]);
        assert_eq!(int_bytes(108), [0xf7, 0x00]);
        assert_eq!(int_bytes(1131), [0xfa, 0xff]);
        assert_eq!(int_bytes(1132), [0x1c, 0x04, 0x6c]);
        assert_eq!(int_bytes(-107), [0x20]);
        assert_eq!(int_bytes(-108), [0xfb, 0x00]);
        assert_eq!(int_bytes(-1131), [0xfe, 0xff]);
        assert_eq!(int_bytes(-1132), [0x1c, 0xfb, 0x94]);
        assert_eq!(int_bytes(32767), [0x1c, 0x7f, 0xff]);
        assert_eq!(int_bytes(-32768), [0x1c, 0x80, 0x00]);
        assert_eq!(int_bytes(32768), [0x1d, 0x00, 0x00, 0x80, 0x00]);
        assert_eq!(int_size(32768), 5);
        assert_eq!(int_size(-32768), 3);
    }

    #[test]
    fn real_encoding_matches_fonttools() {
        // expectations produced by fontTools' encodeFloat
        assert_eq!(real_bytes(0.0), [0x1e, 0x0f]);
        assert_eq!(real_bytes(0.0375), [0x1e, 0x37, 0x5c, 0x4f]);
        assert_eq!(real_bytes(-0.0375), [0x1e, 0xe3, 0x75, 0xc4, 0xff]);
        assert_eq!(real_bytes(0.001), [0x1e, 0x1c, 0x3f]);
        assert_eq!(real_bytes(0.039625), [0x1e, 0x39, 0x62, 0x5c, 0x6f]);
        assert_eq!(
            real_bytes(1.0 / 2048.0),
            [0x1e, 0x48, 0x82, 0x81, 0x25, 0xc1, 0x1f]
        );
        assert_eq!(real_bytes(100.5), [0x1e, 0x10, 0x0a, 0x5f]);
        assert_eq!(real_bytes(1e-05), [0x1e, 0x1c, 0x5f]);
        assert_eq!(real_bytes(0.009375), [0x1e, 0x93, 0x75, 0xc6, 0xff]);
        // integral float still encodes as a real, like fontTools
        assert_eq!(real_bytes(-12.0), [0x1e, 0xe1, 0x2f]);
    }

    #[test]
    fn delta_encoding() {
        let mut out = Vec::new();
        encode_delta(&mut out, &[-10, 0, 500, 520]);
        // -10, +10, +500, +20
        let mut expected = Vec::new();
        for v in [-10, 10, 500, 20] {
            encode_int(&mut expected, v);
        }
        assert_eq!(out, expected);
    }

    #[test]
    fn operators() {
        let mut out = Vec::new();
        Operator::VERSION.write(&mut out);
        Operator::COPYRIGHT.write(&mut out);
        Operator::CHARSTRINGS.write(&mut out);
        assert_eq!(out, [0x00, 0x0c, 0x00, 0x11]);
    }
}

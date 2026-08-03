//! Charstring arguments and operand encoding, shared by CFF and CFF2.
//!
//! A CFF2 charstring argument can vary across the font's variation regions:
//! where CFF1 has a number, CFF2 may have the default master's value plus one
//! delta per region, which the charstring later spells out as a `blend`
//! operator. fontTools models this by letting an argument be either a number
//! or a list (`fontTools/cffLib/specializer.py`, and
//! `reorder_blend_args` at `fontTools/varLib/cff.py:559`); [`Arg`] is the
//! typed equivalent, and the specializer works on it for both versions.

use crate::{ps::cff::CffBuildError, OtRound};

/// The maximum argument stack depth of a Type 2 charstring interpreter.
pub const MAX_STACK_CFF1: usize = 48;

/// The CFF2 argument stack limit, `cffLib`'s `maxStackLimit`
/// (`cffLib/__init__.py:44`). varLib passes it to `specializeCommands`
/// (`varLib/cff.py:623-626`) in place of the 48 the CFF1 path uses.
pub const MAX_STACK_CFF2: usize = 513;

/// A charstring argument.
///
/// `Blend` carries the default master's value and one delta per variation
/// region, exactly as fontTools' `[default, d0…dK-1, 1]` list does before
/// the specializer's last pass turns runs of them into `blend` operators.
/// Deltas are not necessarily integers: varLib rounds them only when they
/// land within 0.01 of one (`varLib/cff.py:482`), and whatever survives is
/// written as a 16.16 fixed operand.
#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    /// A value that is the same in every master.
    Scalar(i32),
    /// A value that varies across regions.
    Blend {
        /// The default master's value, which the pen has already rounded.
        default: i32,
        /// One delta per variation region, in region order.
        deltas: Vec<f64>,
    },
}

impl Arg {
    /// Whether this argument is the number zero.
    ///
    /// A blend never is, even when its default value is: fontTools tests
    /// `if not v[0]` and a non-empty list is always truthy
    /// (`specializer.py:403-427`), so a blended x-component of 0 still makes
    /// the command an `h`/`v`/`r` variant rather than a zero one. Measured on
    /// the research doc's fixture: `0 11 1 blend hmoveto`.
    pub fn is_zero(&self) -> bool {
        matches!(self, Arg::Scalar(0))
    }

    /// The value in the default master, which is what the outline at the
    /// default location is drawn from.
    pub fn default_value(&self) -> i32 {
        match self {
            Arg::Scalar(v) => *v,
            Arg::Blend { default, .. } => *default,
        }
    }

    /// The number of variation regions this argument blends over, or 0.
    pub fn region_count(&self) -> usize {
        match self {
            Arg::Scalar(_) => 0,
            Arg::Blend { deltas, .. } => deltas.len(),
        }
    }

    /// Add two arguments, as fontTools' `_addArgs`
    /// (`specializer.py:506-518`) does.
    ///
    /// Two blends add elementwise, and a scalar adds to a blend's default
    /// value only. Returns `None` where fontTools raises `ValueError`: two
    /// blends over different numbers of regions cannot be combined, and the
    /// caller must leave the two commands alone.
    pub fn add(&self, other: &Arg) -> Option<Arg> {
        match (self, other) {
            (Arg::Scalar(a), Arg::Scalar(b)) => Some(Arg::Scalar(a + b)),
            (
                Arg::Blend {
                    default: a,
                    deltas: da,
                },
                Arg::Blend {
                    default: b,
                    deltas: db,
                },
            ) => (da.len() == db.len()).then(|| Arg::Blend {
                default: a + b,
                deltas: da.iter().zip(db).map(|(a, b)| a + b).collect(),
            }),
            (Arg::Blend { default, deltas }, Arg::Scalar(v))
            | (Arg::Scalar(v), Arg::Blend { default, deltas }) => Some(Arg::Blend {
                default: default + v,
                deltas: deltas.clone(),
            }),
        }
    }
}

impl From<i32> for Arg {
    fn from(value: i32) -> Self {
        Arg::Scalar(value)
    }
}

/// The stack depth an argument list occupies, as fontTools' `_argsStackUse`
/// (`specializer.py:521-532`) counts it.
///
/// A blend costs its whole operand list transiently — the default value, its
/// deltas and the count — but only one slot once the `blend` operator has
/// consumed them. With no blends this is just the number of arguments.
pub fn args_stack_use(args: &[Arg]) -> usize {
    let mut stack_len = 0;
    let mut max_len = 0;
    for arg in args {
        match arg {
            Arg::Scalar(_) => stack_len += 1,
            // the operand list is `[default, delta…, numBlends]`, and
            // numBlends is 1 until the blends are batched
            Arg::Blend { deltas, .. } => {
                max_len = max_len.max(stack_len + deltas.len() + 2);
                stack_len += 1;
            }
        }
    }
    stack_len.max(max_len)
}

/// Encode a Type 2 charstring integer operand.
///
/// Unlike DICT integers there is no 5-byte integer form; a value outside the
/// i16 range cannot be represented. (fontTools emits the 16.16 fixed form for
/// these, which silently mangles the value, so we report an error instead.)
pub fn encode_int(out: &mut Vec<u8>, value: i32) -> Result<(), CffBuildError> {
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
        _ => return Err(CffBuildError::OperandOutOfRange { value }),
    }
    Ok(())
}

/// Encode a fractional charstring operand as 16.16 fixed, like fontTools'
/// `encodeFixed` (`psCharStrings.py:255-261`).
///
/// A value that lands exactly on an integer is written as an integer operand
/// instead, which is both shorter and what fontTools does. CFF2 charstrings
/// are where this form appears: varLib leaves a region delta fractional
/// whenever it is more than 0.01 away from an integer.
pub fn encode_fixed(out: &mut Vec<u8>, value: f64) -> Result<(), CffBuildError> {
    let scaled = value * 65536.0;
    if !(i32::MIN as f64..=i32::MAX as f64).contains(&scaled) {
        return Err(CffBuildError::OperandOutOfRange {
            value: value.trunc() as i32,
        });
    }
    let bits: i32 = scaled.ot_round();
    if bits & 0xffff == 0 {
        return encode_int(out, bits >> 16);
    }
    out.push(255);
    out.extend_from_slice(&bits.to_be_bytes());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_bytes(value: f64) -> Vec<u8> {
        let mut out = Vec::new();
        encode_fixed(&mut out, value).unwrap();
        out
    }

    #[test]
    fn fixed_encoding_matches_fonttools() {
        // expectations from fontTools' encodeFixed
        assert_eq!(fixed_bytes(0.5), [0xff, 0x00, 0x00, 0x80, 0x00]);
        assert_eq!(fixed_bytes(-0.5), [0xff, 0xff, 0xff, 0x80, 0x00]);
        assert_eq!(fixed_bytes(213.56), [0xff, 0x00, 0xd5, 0x8f, 0x5c]);
        assert_eq!(fixed_bytes(-103.29), [0xff, 0xff, 0x98, 0xb5, 0xc3]);
        // one 16.16 step, and the smallest representable magnitude
        assert_eq!(fixed_bytes(1.0 / 65536.0), [0xff, 0x00, 0x00, 0x00, 0x01]);
        // integral values fall back to the (shorter) integer form
        assert_eq!(fixed_bytes(0.0), [0x8b]);
        assert_eq!(fixed_bytes(-12.0), [0x7f]);
        assert_eq!(fixed_bytes(1000.0), [0xfa, 0x7c]);
        // and one that rounds onto an integer: otRound(f * 65536)
        assert_eq!(fixed_bytes(3.0 - 1e-9), [0x8e]);
    }

    #[test]
    fn fixed_out_of_range_is_an_error() {
        assert!(encode_fixed(&mut Vec::new(), 32768.0).is_err());
        // 16.16 covers this, but the integer form does not
        assert!(encode_fixed(&mut Vec::new(), 40000.5).is_err());
    }

    #[test]
    fn blends_are_never_zero() {
        assert!(Arg::Scalar(0).is_zero());
        assert!(!Arg::Scalar(1).is_zero());
        assert!(!Arg::Blend {
            default: 0,
            deltas: vec![11.0]
        }
        .is_zero());
    }

    #[test]
    fn adding_args_matches_fonttools() {
        let blend = |default, deltas: &[f64]| Arg::Blend {
            default,
            deltas: deltas.to_vec(),
        };
        assert_eq!(
            Arg::Scalar(3).add(&Arg::Scalar(4)),
            Some(Arg::Scalar(7)),
            "_addArgs(3, 4)"
        );
        assert_eq!(
            blend(3, &[1.0, 2.5]).add(&blend(4, &[10.0, 0.5])),
            Some(blend(7, &[11.0, 3.0])),
            "_addArgs([3,1,2.5,1], [4,10,0.5,1])"
        );
        // a scalar only reaches the default value, either way round
        assert_eq!(
            blend(3, &[1.0]).add(&Arg::Scalar(4)),
            Some(blend(7, &[1.0]))
        );
        assert_eq!(
            Arg::Scalar(4).add(&blend(3, &[1.0])),
            Some(blend(7, &[1.0]))
        );
        // differing region counts are fontTools' ValueError
        assert_eq!(blend(3, &[1.0]).add(&blend(4, &[1.0, 2.0])), None);
    }

    #[test]
    fn stack_use_matches_fonttools() {
        let blend = |default, regions| Arg::Blend {
            default,
            deltas: vec![1.0; regions],
        };
        // _argsStackUse([1, 2, 3]) == 3
        assert_eq!(args_stack_use(&[1.into(), 2.into(), 3.into()]), 3);
        // _argsStackUse([[1,2,3,1]]) == 4: three operands and the count,
        // even though the blend leaves only one value behind
        assert_eq!(args_stack_use(&[blend(1, 2)]), 4);
        // two blends over two regions, with a scalar in front:
        // stackLen 1 -> transient 1 + 4, then 2 -> transient 2 + 4
        assert_eq!(args_stack_use(&[1.into(), blend(1, 2), blend(2, 2)]), 6);
        assert_eq!(args_stack_use(&[]), 0);
    }
}

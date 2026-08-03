//! CFF and CFF2 fonts.

pub mod charstring;
pub mod dict;
pub mod index;
pub mod v1;
pub mod v2;

/// An error produced when building a `CFF ` or `CFF2` table.
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

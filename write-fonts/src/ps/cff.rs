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
    /// A CFF2 charstring was built with no masters at all
    NoMasters,
    /// A master draws a different outline than the default master
    IncompatibleMasters {
        /// Which master, by index in variation-model order
        master: usize,
        /// The first command whose operator or argument count differs
        command: usize,
    },
    /// A variation region has the wrong number of axes
    WrongAxisCount {
        /// The font's axis count
        expected: usize,
        /// What the region has
        got: usize,
    },
    /// A variation store subtable references a region that does not exist
    RegionOutOfRange {
        /// The offending region index
        region: u16,
    },
    /// More variation regions or subtables than a 16-bit count can hold
    VarStoreTooBig,
    /// A variation model returned the wrong number of deltas
    WrongDeltaCount {
        /// One per master, counting the default master
        expected: usize,
        /// What the model returned
        got: usize,
    },
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
            CffBuildError::NoMasters => {
                write!(f, "a CFF2 charstring needs at least one master")
            }
            CffBuildError::IncompatibleMasters { master, command } => write!(
                f,
                "master {master} draws a different outline than the default \
                 master, from command {command}"
            ),
            CffBuildError::WrongAxisCount { expected, got } => write!(
                f,
                "a variation region has {got} axes, but the font has {expected}"
            ),
            CffBuildError::RegionOutOfRange { region } => {
                write!(f, "variation region {region} does not exist")
            }
            CffBuildError::VarStoreTooBig => write!(
                f,
                "a variation store cannot hold more than 65535 regions or subtables"
            ),
            CffBuildError::WrongDeltaCount { expected, got } => write!(
                f,
                "the variation model returned {got} deltas for {expected} masters"
            ),
        }
    }
}

impl std::error::Error for CffBuildError {}

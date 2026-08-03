//! CFF version 1.

include!("../../../generated/generated_cff.rs");

pub mod charstring;
pub mod font;

pub use crate::ps::cff::CffBuildError;
pub use font::{Cff, CffFontBuilder, GlyphData, PrivateDictValues, TopDictValues};

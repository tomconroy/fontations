//! CFF version 1.

include!("../../../generated/generated_cff.rs");

pub mod charstring;
pub mod dict;
pub mod font;

pub use font::{Cff, CffFontBuilder, GlyphData, PrivateDictValues, TopDictValues};

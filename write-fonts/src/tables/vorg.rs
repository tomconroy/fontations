//! The [VORG (Vertical Origin)](https://docs.microsoft.com/en-us/typography/opentype/spec/vorg) table

include!("../../generated/generated_vorg.rs");

#[cfg(test)]
mod tests {
    use super::*;
    use read_fonts::{FontRead, FontRef, TableProvider};

    #[test]
    fn write_hand_computed_bytes() {
        let vorg = Vorg::new(
            880,
            vec![
                VertOriginYMetrics::new(GlyphId16::new(1), 867),
                VertOriginYMetrics::new(GlyphId16::new(3), -824),
            ],
        );
        let bytes = crate::dump_table(&vorg).unwrap();
        assert_eq!(
            bytes,
            vec![
                0x00, 0x01, // majorVersion
                0x00, 0x00, // minorVersion
                0x03, 0x70, // defaultVertOriginY = 880
                0x00, 0x02, // numVertOriginYMetrics
                0x00, 0x01, 0x03, 0x63, // gid 1, 867
                0x00, 0x03, 0xfc, 0xc8, // gid 3, -824
            ]
        );
    }

    #[test]
    fn no_records() {
        let vorg = Vorg::new(1000, vec![]);
        let bytes = crate::dump_table(&vorg).unwrap();
        assert_eq!(bytes, vec![0x00, 0x01, 0x00, 0x00, 0x03, 0xe8, 0x00, 0x00]);
        let read = read_fonts::tables::vorg::Vorg::read(bytes.as_slice().into()).unwrap();
        assert_eq!(read.default_vert_origin_y(), 1000);
        assert_eq!(read.vert_origin_y_metrics().len(), 0);
    }

    #[test]
    fn roundtrip() {
        let font = FontRef::new(font_test_data::VORG).unwrap();
        let orig = font.vorg().unwrap();
        let owned: Vorg = orig.to_owned_table();
        let bytes = crate::dump_table(&owned).unwrap();
        assert_eq!(bytes, font.table_data(Vorg::TAG).unwrap().as_bytes());

        let reloaded = read_fonts::tables::vorg::Vorg::read(bytes.as_slice().into()).unwrap();
        assert_eq!(reloaded.default_vert_origin_y(), 880);
        assert_eq!(reloaded.vertical_origin_y(GlyphId::new(1)), 867);
        assert_eq!(reloaded.vertical_origin_y(GlyphId::new(2)), 880);
        assert_eq!(reloaded.vertical_origin_y(GlyphId::new(3)), 824);
    }
}

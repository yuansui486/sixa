//! Bundled SIL Open Font License font; identical rendering on Windows and macOS.
use ab_glyph::FontArc;
use domain::{Error, Result};

pub const CJK_FONT_BYTES: &[u8] = include_bytes!("../assets/fonts/NotoSansSC.ttf");

pub fn replacement_font() -> Result<FontArc> {
    static FONT: std::sync::OnceLock<std::result::Result<FontArc, String>> =
        std::sync::OnceLock::new();
    FONT.get_or_init(|| {
        FontArc::try_from_slice(CJK_FONT_BYTES)
            .map_err(|_| "内置中文字体无法读取，请重新安装私匣".to_owned())
    })
    .clone()
    .map_err(Error::State)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ab_glyph::Font;
    #[test]
    fn bundled_font_covers_semantic_chinese_replacements() {
        let font = replacement_font().unwrap();
        for character in "某人某公司某学校电话地址***".chars() {
            assert_ne!(font.glyph_id(character).0, 0, "missing {character}");
        }
    }
}

//! Unicode script detection and shaping direction.

use harfrust::{Direction, Script, Tag};
use icu_properties::{
    CodePointMapData,
    props::{NamedEnumeratedProperty, Script as IcuScript},
};
use unicode_bidi::BidiInfo;

use crate::layout::WritingMode;

pub(crate) fn shaping_direction_for_text(
    text: &str,
    writing_mode: WritingMode,
) -> (Direction, Option<Script>) {
    if writing_mode.is_vertical() {
        return (
            Direction::TopToBottom,
            dominant_script(text).and_then(harfrust_script),
        );
    }

    let bidi = BidiInfo::new(text, None);
    let direction = if bidi
        .paragraphs
        .first()
        .is_some_and(|paragraph| paragraph.level.is_rtl())
    {
        Direction::RightToLeft
    } else {
        Direction::LeftToRight
    };
    (direction, dominant_script(text).and_then(harfrust_script))
}

/// Returns the distinct ISO 15924 scripts Fontique should use for fallback.
pub(crate) fn fontique_scripts(text: &str) -> Vec<fontique::Script> {
    let script_map = CodePointMapData::<IcuScript>::new();
    let mut scripts = Vec::new();
    let mut has_emoji = false;
    for character in text.chars() {
        let script = script_map.get(character);
        if script == IcuScript::Common || script == IcuScript::Inherited {
            has_emoji |= character >= '\u{1F000}';
            continue;
        }
        let script = fallback_script(script);
        let Some(tag) = script_tag(script) else {
            continue;
        };
        let script = fontique::Script::from_bytes(tag);
        if !scripts.contains(&script) {
            scripts.push(script);
        }
    }
    if has_emoji {
        let emoji = fontique::Script::from_bytes(*b"Zsye");
        if !scripts.contains(&emoji) {
            scripts.push(emoji);
        }
    }
    if scripts.is_empty() {
        scripts.push(fontique::Script::from_bytes(*b"Latn"));
    }
    scripts
}

pub(crate) fn harfrust_script(script: IcuScript) -> Option<Script> {
    Script::from_iso15924_tag(Tag::new(&script_tag(fallback_script(script))?))
}

fn dominant_script(text: &str) -> Option<IcuScript> {
    let script_map = CodePointMapData::<IcuScript>::new();
    text.chars()
        .map(|character| script_map.get(character))
        .find(|script| *script != IcuScript::Common && *script != IcuScript::Inherited)
        .map(fallback_script)
}

fn fallback_script(script: IcuScript) -> IcuScript {
    match script {
        IcuScript::Hiragana | IcuScript::Katakana | IcuScript::Bopomofo => IcuScript::Han,
        other => other,
    }
}

fn script_tag(script: IcuScript) -> Option<[u8; 4]> {
    let bytes = script.short_name().as_bytes();
    (bytes.len() == 4).then(|| [bytes[0], bytes[1], bytes[2], bytes[3]])
}

pub(crate) fn is_chinese_or_japanese_text(text: &str) -> bool {
    let script_map = CodePointMapData::<IcuScript>::new();
    text.chars().any(|character| {
        matches!(
            script_map.get(character),
            IcuScript::Han | IcuScript::Hiragana | IcuScript::Katakana | IcuScript::Bopomofo
        )
    })
}

/// Maps `text` to a BCP-47 language tag from the Unicode scripts it contains.
///
/// Hangul and kana each identify a single language, so they outrank Han
/// (shared by Japanese and Chinese) and Latin (digits and names routinely
/// mixed into CJK text). Han maps to `ja-JP` on purpose: a pure-kanji line is
/// identical in Japanese and Chinese, and this fork only feeds Japanese source
/// images, so kanji is deliberately never disambiguated to `zh`. Unknown
/// scripts and empty text keep the historical `ja-JP` fallback.
pub fn detect_language_tag(text: &str) -> &'static str {
    let script_map = CodePointMapData::<IcuScript>::new();
    let mut has_hangul = false;
    let mut has_kana = false;
    let mut has_han = false;
    let mut has_latin = false;
    for character in text.chars() {
        match script_map.get(character) {
            IcuScript::Hangul => has_hangul = true,
            IcuScript::Hiragana | IcuScript::Katakana => has_kana = true,
            IcuScript::Han => has_han = true,
            IcuScript::Latin => has_latin = true,
            _ => {}
        }
    }
    if has_hangul {
        "ko-KR"
    } else if has_kana || has_han {
        "ja-JP"
    } else if has_latin {
        "en-US"
    } else {
        "ja-JP"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_scripts_preserve_cjk_locale_selection() {
        assert_eq!(
            fontique_scripts("日本語"),
            [fontique::Script::from_bytes(*b"Hani")]
        );
        assert_eq!(
            fontique_scripts("한국어"),
            [fontique::Script::from_bytes(*b"Hang")]
        );
        assert_eq!(
            fontique_scripts("مرحبا"),
            [fontique::Script::from_bytes(*b"Arab")]
        );
    }

    #[test]
    fn arabic_text_uses_rtl_shaping() {
        let (direction, script) = shaping_direction_for_text("مرحبا", WritingMode::Horizontal);
        assert_eq!(direction, Direction::RightToLeft);
        assert_eq!(script.unwrap().tag(), Tag::new(b"Arab"));
    }

    #[test]
    fn source_writing_mode_is_limited_to_chinese_and_japanese() {
        assert!(is_chinese_or_japanese_text("日本語"));
        assert!(is_chinese_or_japanese_text("繁體中文"));
        assert!(!is_chinese_or_japanese_text("한국어"));
        assert!(!is_chinese_or_japanese_text("English"));
    }

    #[test]
    fn hangul_outranks_latin_for_language_tag() {
        assert_eq!(detect_language_tag("한국어"), "ko-KR");
        assert_eq!(detect_language_tag("B6층 모험가들이"), "ko-KR");
    }

    #[test]
    fn japanese_scripts_stay_japanese() {
        assert_eq!(detect_language_tag("ひらがな"), "ja-JP");
        assert_eq!(detect_language_tag("カタカナ"), "ja-JP");
        assert_eq!(detect_language_tag("日本語"), "ja-JP");
    }

    #[test]
    fn mixed_kana_kanji_and_digits_stay_japanese() {
        assert_eq!(detect_language_tag("第3巻の発売日は2024年"), "ja-JP");
    }

    #[test]
    fn latin_detects_english_and_unknown_falls_back_to_japanese() {
        assert_eq!(detect_language_tag("English"), "en-US");
        assert_eq!(detect_language_tag("مرحبا"), "ja-JP");
        assert_eq!(detect_language_tag("123"), "ja-JP");
        assert_eq!(detect_language_tag(""), "ja-JP");
    }
}

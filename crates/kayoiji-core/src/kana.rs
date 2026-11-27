//! かな文字の補助関数。

const PROLONGED_SOUND_MARK: char = 'ー';

/// ひらがな。辞書の読みに使われる長音記号を含む。
pub fn is_kana(c: char) -> bool {
    ('\u{3041}'..='\u{3096}').contains(&c) || c == PROLONGED_SOUND_MARK
}

pub fn to_katakana(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\u{3041}'..='\u{3096}' => char::from_u32(c as u32 + 0x60).unwrap_or(c),
            _ => c,
        })
        .collect()
}

pub fn to_hiragana(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\u{30A1}'..='\u{30F6}' => char::from_u32(c as u32 - 0x60).unwrap_or(c),
            _ => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hiragana_conversion_keeps_other_characters() {
        assert_eq!(to_hiragana("ネコとコーヒーヴ猫a"), "ねことこーひーゔ猫a");
    }

    #[test]
    fn katakana_conversion_keeps_other_characters() {
        // Arrange
        let text = "こーひーをのむゔ1a";

        // Act
        let katakana = to_katakana(text);

        // Assert
        assert_eq!(katakana, "コーヒーヲノムヴ1a");
    }

    #[test]
    fn kana_detection_excludes_katakana_and_ascii() {
        assert!(is_kana('あ'));
        assert!(is_kana('ー'));
        assert!(is_kana('ゔ'));
        assert!(!is_kana('ア'));
        assert!(!is_kana('a'));
        assert!(!is_kana('、'));
    }
}

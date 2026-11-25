//! 英語か日本語かを決め打ちする判断データベース。
//!
//! 統計による判定 (`detector`) より先に評価する。標準の内容は `data/rules.toml`。

use std::collections::HashMap;

use serde::Deserialize;

const DEFAULT_RULES: &str = include_str!("../data/rules.toml");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    English,
    Japanese,
}

#[derive(Debug, Deserialize)]
struct RuleFile {
    #[serde(default)]
    rules: Vec<SequenceRule>,
    #[serde(default)]
    words: Words,
}

#[derive(Debug, Deserialize)]
struct SequenceRule {
    sequence: String,
    language: Language,
}

#[derive(Debug, Default, Deserialize)]
struct Words {
    #[serde(default)]
    english: Vec<String>,
    #[serde(default)]
    japanese: Vec<String>,
}

#[derive(Debug, Default, Clone)]
pub struct RuleSet {
    /// 単語の途中でも、打鍵列がこの文字列に一致した時点で確定する。
    sequences: HashMap<String, Language>,
    /// 単語の区切りで、単語全体がこの文字列に一致した時に確定する。
    words: HashMap<String, Language>,
}

impl RuleSet {
    pub fn standard() -> Self {
        Self::parse(DEFAULT_RULES).expect("bundled rules are valid")
    }

    /// `sequence` が空白で終わる規則は、単語の区切りで評価する規則として扱う。
    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        let file: RuleFile = toml::from_str(text)?;
        let mut set = Self::default();
        for rule in file.rules {
            match rule.sequence.strip_suffix(' ') {
                Some(word) => set.words.insert(word.to_string(), rule.language),
                None => set.sequences.insert(rule.sequence, rule.language),
            };
        }
        for word in file.words.english {
            set.words.insert(word, Language::English);
        }
        for word in file.words.japanese {
            set.words.insert(word, Language::Japanese);
        }
        Ok(set)
    }

    /// 後から読み込んだ規則 (利用者の規則) で上書きする。
    pub fn merge(&mut self, other: RuleSet) {
        self.sequences.extend(other.sequences);
        self.words.extend(other.words);
    }

    pub fn sequence(&self, buffer: &str) -> Option<Language> {
        self.sequences.get(buffer).copied()
    }

    pub fn word(&self, word: &str) -> Option<Language> {
        self.words.get(word).copied()
    }

    pub fn words(&self) -> impl Iterator<Item = (&str, Language)> {
        self.words.iter().map(|(word, language)| (word.as_str(), *language))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailing_space_makes_a_word_rule() {
        // Arrange
        let text = r#"
            [[rules]]
            sequence = "for "
            language = "english"

            [[rules]]
            sequence = "tatoe"
            language = "japanese"
        "#;

        // Act
        let rules = RuleSet::parse(text).expect("valid rules");

        // Assert
        assert_eq!(rules.word("for"), Some(Language::English));
        assert_eq!(rules.sequence("for"), None);
        assert_eq!(rules.sequence("tatoe"), Some(Language::Japanese));
        assert_eq!(rules.word("tatoe"), None);
    }

    #[test]
    fn word_lists_become_word_rules() {
        let rules = RuleSet::parse(
            r#"
            [words]
            english = ["sudo"]
            japanese = ["sore"]
            "#,
        )
        .expect("valid rules");
        assert_eq!(rules.word("sudo"), Some(Language::English));
        assert_eq!(rules.word("sore"), Some(Language::Japanese));
    }

    #[test]
    fn merged_rules_override_earlier_ones() {
        // Arrange
        let mut rules = RuleSet::parse("[words]\nenglish = [\"nano\"]").expect("valid rules");
        let user = RuleSet::parse("[words]\njapanese = [\"nano\"]").expect("valid rules");

        // Act
        rules.merge(user);

        // Assert
        assert_eq!(rules.word("nano"), Some(Language::Japanese));
    }

    #[test]
    fn bundled_rules_contain_the_documented_examples() {
        let rules = RuleSet::standard();
        assert_eq!(rules.word("for"), Some(Language::English));
        assert_eq!(rules.sequence("tatoe"), Some(Language::Japanese));
        assert_eq!(rules.word("sudo"), Some(Language::English));
    }

    #[test]
    fn invalid_language_is_an_error() {
        assert!(RuleSet::parse("[[rules]]\nsequence = \"a\"\nlanguage = \"french\"").is_err());
    }
}

//! 打鍵列が英語か日本語かを判定する。
//!
//! 判断データベース (`rules`) に一致すればそれに従う。一致しなければ、打鍵列を英語として
//! 解釈した時の確率と、ローマ字として読んだかなを日本語として解釈した時の確率を比べる。

use std::collections::BTreeSet;
use std::ops::Bound;

use serde::{Deserialize, Serialize};

use crate::converter::Converter;
use crate::english::EnglishLexicon;
use crate::romaji::RomajiTable;
use crate::rules::{Language, RuleSet};

/// Mozc のコストは `-500 * ln(確率)`。常用対数に直す時の除数。
const COST_PER_LOG10: f64 = 500.0 * std::f64::consts::LN_10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Japanese,
    English,
    Undetermined,
}

/// 直前の入力。同じ打鍵列でも、日本語の入力が続いている間は日本語である可能性が高い。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Context {
    Neutral,
    /// 日本語を確定した後。単語は日本語の文の途中にあるものとして評価する。
    AfterJapanese,
    AfterEnglish,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DetectorConfig {
    /// 単語の途中で日本語と確定するのに必要な、日本語と英語の確率の比 (常用対数)。
    pub mid_word_margin: f64,
    /// 単語の途中で日本語と確定してよい、英語側の確率の上限 (常用対数)。
    /// その打鍵列で始まる英単語がほとんど無い時だけ、単語の途中で確定する。
    pub mid_word_max_english: f64,
    /// 単語の区切りで日本語と判定するのに必要な比。
    pub boundary_margin: f64,
    /// 出現確率 (常用対数) がこれ以上の英単語は、日本語の直後でない限り英語にする。
    /// you / site / sure のような語は、かなとして読んだ「よう」「して」の方が確率が高くなるが、
    /// 単語単独で打たれた場合は英語であることの方が多い。
    pub common_english: f64,
    /// 単語の途中で日本語と確定するのに必要な、変換済みのかなの数 (拗音は 1 つと数える)。
    pub min_kana_mid_word: usize,
    /// 日本語の入力が続いている間に、日本語側へ加える補正。
    pub japanese_context_bonus: f64,
    /// 日本語の入力が続いている間でも、よく使う英単語 (`common_english` 以上) は英語にする。
    /// ただし、かなとして読んだ時の確率 (常用対数) がこれ以上なら日本語にする。
    /// to / made / site は「と」「まで」「して」と読む方を優先し、home / name は英語のままにする。
    pub flow_japanese_for_common_english: f64,
    /// 英単語を入力した直後に、日本語側から引く補正。
    pub english_context_penalty: f64,
    /// 語彙に無い英単語の確率。`10^-(oov_base + oov_per_char * 文字数)` とみなす。
    pub oov_base: f64,
    pub oov_per_char: f64,
    /// 変換コストが `max_cost_base + max_cost_per_kana * かなの文字数` を超える読みは、
    /// 辞書の単語でうまく区切れていないので日本語とみなさない。
    pub max_cost_base: f64,
    pub max_cost_per_kana: f64,
}

impl Default for DetectorConfig {
    fn default() -> Self {
        Self {
            mid_word_margin: 2.5,
            mid_word_max_english: -6.0,
            boundary_margin: 0.5,
            common_english: -5.0,
            min_kana_mid_word: 2,
            japanese_context_bonus: 1.5,
            flow_japanese_for_common_english: -2.8,
            english_context_penalty: 0.5,
            oov_base: 3.0,
            oov_per_char: 1.2,
            max_cost_base: 2500.0,
            max_cost_per_kana: 2500.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Explanation {
    pub kana: String,
    pub pending: String,
    pub strict: bool,
    /// 英単語としての出現確率の常用対数。
    pub english_word: Option<f64>,
    /// この打鍵列で始まる英単語の出現確率の合計の常用対数。
    pub english_prefix: Option<f64>,
    /// かなを日本語として解釈した時の確率の常用対数。
    pub japanese: Option<f64>,
    pub cost: Option<i32>,
    /// 文の途中 (名詞の直後) にも置かれうるものとして求めた確率の常用対数。
    pub japanese_in_flow: Option<f64>,
}

pub struct Detector {
    converter: Converter,
    english: EnglishLexicon,
    rules: RuleSet,
    /// 日本語の入力が続いていない時に、必ず英語として扱う単語。
    /// 単語の集合は、接頭辞で引けるように順序付きで持つ。
    english_words: BTreeSet<String>,
    /// 日本語の入力が続いている間でも、必ず英語として扱う単語。
    /// "nano" と「なの」のように、文中では日本語として打つ綴りがあるので分けて持つ。
    flow_english_words: BTreeSet<String>,
    japanese_words: BTreeSet<String>,
    config: DetectorConfig,
}

impl Detector {
    pub fn new(converter: Converter, english: EnglishLexicon, rules: RuleSet, config: DetectorConfig) -> Self {
        let mut english_words = BTreeSet::new();
        let mut japanese_words = BTreeSet::new();
        for (word, language) in rules.words() {
            match language {
                Language::English => english_words.insert(word.to_string()),
                Language::Japanese => japanese_words.insert(word.to_string()),
            };
        }
        Self {
            converter,
            english,
            rules,
            english_words,
            flow_english_words: BTreeSet::new(),
            japanese_words,
            config,
        }
    }

    pub fn converter(&self) -> &Converter {
        &self.converter
    }

    pub fn set_config(&mut self, config: DetectorConfig) {
        self.config = config;
    }

    /// 利用者の訂正を覚える。後から覚えた方を優先する。
    /// `in_flow` は、日本語の入力が続いている間に行われた訂正かどうか。
    pub fn learn(&mut self, word: &str, language: Language, in_flow: bool) {
        match language {
            Language::English => {
                self.japanese_words.remove(word);
                self.english_words.insert(word.to_string());
                if in_flow {
                    self.flow_english_words.insert(word.to_string());
                }
            }
            Language::Japanese => {
                self.english_words.remove(word);
                self.flow_english_words.remove(word);
                self.japanese_words.insert(word.to_string());
            }
        }
    }

    /// 単語の途中の打鍵列を評価する。`English` は「この単語は英語で確定」、
    /// `Undetermined` は「まだ決められないので打鍵を待つ」を表す。
    pub fn evaluate_prefix(&self, buffer: &str, context: Context) -> Verdict {
        if let Some(language) = self.sequence_rule(buffer, context, false) {
            return verdict_of(language);
        }
        let parsed = RomajiTable::global().parse(buffer);
        if !parsed.is_strict() {
            // "wi" のような外来語用の綴りでも、日本語として覚えた単語の途中なら単語の区切りまで待つ。
            // ここで英語に決めると、区切りで覚えた単語と照合する前に単語が終わる。
            if has_word_starting_with(&self.japanese_words, buffer) {
                return Verdict::Undetermined;
            }
            return Verdict::English;
        }
        if parsed.converted_count() < self.config.min_kana_mid_word {
            return Verdict::Undetermined;
        }
        if self.is_prefix_of_english_word(buffer) {
            return Verdict::Undetermined;
        }
        let english = self.english.prefix_log_probability(buffer);
        if english.is_some_and(|probability| probability > self.config.mid_word_max_english) {
            return Verdict::Undetermined;
        }
        if self.favors_japanese(buffer, &parsed.text(), english, context, self.config.mid_word_margin) {
            return Verdict::Japanese;
        }
        Verdict::Undetermined
    }

    /// 単語の区切り (空白など) に来た時点で、単語全体を評価する。
    /// `Undetermined` は返さない。
    pub fn evaluate_word(&self, word: &str, context: Context) -> Verdict {
        if let Some(language) = self.sequence_rule(word, context, true) {
            return verdict_of(language);
        }
        let in_flow = context == Context::AfterJapanese;
        if self.is_english_word(word, context) {
            return Verdict::English;
        }
        if self.japanese_words.contains(word) {
            return Verdict::Japanese;
        }
        let parsed = RomajiTable::global().parse(word);
        // 日本語の入力が続いている間は、"wi" や "thi" のような外来語用の綴りも日本語として読む。
        let readable = if in_flow {
            parsed.segments.iter().all(|segment| segment.converted)
        } else {
            parsed.is_strict()
        };
        if !readable || !(parsed.pending.is_empty() || parsed.pending == "n") {
            return Verdict::English;
        }
        let kana: String = parsed.finalize().iter().map(|s| s.out.as_str()).collect();
        let english = self.english.word_log_probability(word);
        let is_common = english.is_some_and(|probability| probability >= self.config.common_english);

        if in_flow {
            // 英単語として知られていない綴りは、日本語のままにする。
            let Some(english) = english else {
                return Verdict::Japanese;
            };
            let Some(japanese) = self.japanese_log_probability(&kana, true) else {
                return Verdict::English;
            };
            if is_common && japanese < self.config.flow_japanese_for_common_english {
                return Verdict::English;
            }
            let favors = japanese - english + self.config.japanese_context_bonus >= self.config.boundary_margin;
            return if favors { Verdict::Japanese } else { Verdict::English };
        }

        if is_common {
            return Verdict::English;
        }
        if self.favors_japanese(word, &kana, english, context, self.config.boundary_margin) {
            return Verdict::Japanese;
        }
        Verdict::English
    }

    /// 判定に使う値を取り出す。しきい値の調整と、誤判定の原因調査に使う。
    pub fn explain(&self, typed: &str) -> Explanation {
        let parsed = RomajiTable::global().parse(typed);
        let strict = parsed.is_strict();
        let kana = parsed.text();
        let cost = self.converter.open_cost(&kana);
        Explanation {
            strict,
            pending: parsed.pending,
            english_word: self.english.word_log_probability(typed),
            english_prefix: self.english.prefix_log_probability(typed),
            japanese: cost.map(|cost| -(cost as f64) / COST_PER_LOG10),
            cost,
            japanese_in_flow: self
                .converter
                .flow_cost(&kana)
                .map(|cost| -(cost as f64) / COST_PER_LOG10),
            kana,
        }
    }

    /// 打鍵列に一致する規則。利用者が逆の言語に訂正した単語と重なる時は、訂正を優先して規則を使わない。
    ///
    /// 単語の途中 (`whole_word` が false) では、訂正した単語の先頭に一致するだけで規則を止める。
    /// "tatoeba" を英語に訂正してあれば、"tatoe" まで打った時点の規則で日本語にしない。
    fn sequence_rule(&self, buffer: &str, context: Context, whole_word: bool) -> Option<Language> {
        let language = self.rules.sequence(buffer)?;
        let corrections = match language {
            Language::Japanese => self.english_words_in(context),
            Language::English => &self.japanese_words,
        };
        let corrected = if whole_word {
            corrections.contains(buffer)
        } else {
            has_word_starting_with(corrections, buffer)
        };
        (!corrected).then_some(language)
    }

    /// 英語として扱う単語。日本語の入力が続いている間は、その間に訂正された単語だけを英語とする。
    fn english_words_in(&self, context: Context) -> &BTreeSet<String> {
        if context == Context::AfterJapanese {
            &self.flow_english_words
        } else {
            &self.english_words
        }
    }

    fn is_english_word(&self, word: &str, context: Context) -> bool {
        self.english_words_in(context).contains(word)
    }

    fn is_prefix_of_english_word(&self, buffer: &str) -> bool {
        has_word_starting_with(&self.english_words, buffer)
    }

    /// かなを日本語として解釈した時の確率の常用対数。辞書の単語でうまく区切れない読みは `None`。
    fn japanese_log_probability(&self, kana: &str, in_flow: bool) -> Option<f64> {
        let cost = if in_flow {
            self.converter.flow_cost(kana)?
        } else {
            self.converter.open_cost(kana)?
        };
        let kana_count = kana.chars().count() as f64;
        if cost as f64 > self.config.max_cost_base + self.config.max_cost_per_kana * kana_count {
            return None;
        }
        Some(-(cost as f64) / COST_PER_LOG10)
    }

    fn favors_japanese(
        &self,
        typed: &str,
        kana: &str,
        english_log_probability: Option<f64>,
        context: Context,
        margin: f64,
    ) -> bool {
        let Some(japanese) = self.japanese_log_probability(kana, false) else {
            return false;
        };
        let out_of_vocabulary = -(self.config.oov_base + self.config.oov_per_char * typed.len() as f64);
        let english = english_log_probability.map_or(out_of_vocabulary, |known| known.max(out_of_vocabulary));
        let prior = match context {
            Context::Neutral | Context::AfterJapanese => 0.0,
            Context::AfterEnglish => -self.config.english_context_penalty,
        };
        japanese - english + prior >= margin
    }
}

fn has_word_starting_with(words: &BTreeSet<String>, prefix: &str) -> bool {
    words
        .range::<str, _>((Bound::Included(prefix), Bound::Unbounded))
        .next()
        .is_some_and(|word| word.starts_with(prefix))
}

fn verdict_of(language: Language) -> Verdict {
    match language {
        Language::English => Verdict::English,
        Language::Japanese => Verdict::Japanese,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::converter::tests::{sample_converter, sample_dictionary};
    use crate::english::tests::sample_lexicon;

    pub(crate) fn sample_detector() -> Detector {
        Detector::new(
            sample_converter(),
            sample_lexicon(),
            RuleSet::standard(),
            DetectorConfig::default(),
        )
    }

    #[test]
    fn spec_example_tatoe_is_japanese_without_the_rule() {
        // Arrange
        let detector = Detector::new(
            Converter::new(sample_dictionary()),
            sample_lexicon(),
            RuleSet::default(),
            DetectorConfig::default(),
        );

        // Act
        let verdicts: Vec<Verdict> = ["t", "ta", "tat", "tato", "tatoe"]
            .iter()
            .map(|buffer| detector.evaluate_prefix(buffer, Context::Neutral))
            .collect();

        // Assert
        // "tat" までは tattoo の可能性がある。"tato" は英単語の接頭辞ではないが、
        // 「たと」は日本語としても弱いので確定しない。
        assert_eq!(
            verdicts,
            [
                Verdict::Undetermined,
                Verdict::Undetermined,
                Verdict::Undetermined,
                Verdict::Undetermined,
                Verdict::Japanese,
            ]
        );
    }

    #[test]
    fn spec_example_for_is_english() {
        let detector = sample_detector();
        // "for" は「ふぉら」に育つ可能性があるので、区切りの空白が来た時に英語で確定する。
        assert_eq!(detector.evaluate_prefix("for", Context::Neutral), Verdict::Undetermined);
        assert_eq!(detector.evaluate_word("for", Context::Neutral), Verdict::English);
        assert_eq!(detector.evaluate_prefix("fort", Context::Neutral), Verdict::English);
    }

    #[test]
    fn invalid_romaji_is_english_immediately() {
        let detector = sample_detector();
        for buffer in ["th", "wh", "st", "x", "cl"] {
            assert_eq!(detector.evaluate_prefix(buffer, Context::Neutral), Verdict::English, "{buffer}");
        }
    }

    #[test]
    fn english_prefix_keeps_the_verdict_open() {
        let detector = sample_detector();
        // "kore" は korea の接頭辞。続きが英語でなくなった時点で日本語になる。
        assert_eq!(detector.evaluate_prefix("kore", Context::Neutral), Verdict::Undetermined);
        assert_eq!(detector.evaluate_prefix("korew", Context::Neutral), Verdict::Japanese);
    }

    #[test]
    fn word_boundary_compares_both_languages() {
        let detector = sample_detector();
        assert_eq!(detector.evaluate_word("kore", Context::Neutral), Verdict::Japanese);
        assert_eq!(detector.evaluate_word("mata", Context::Neutral), Verdict::Japanese);
        assert_eq!(detector.evaluate_word("made", Context::Neutral), Verdict::English);
        assert_eq!(detector.evaluate_word("take", Context::Neutral), Verdict::English);
        assert_eq!(detector.evaluate_word("home", Context::Neutral), Verdict::English);
    }

    #[test]
    fn single_kana_words_are_japanese_unless_they_are_common_english() {
        let detector = sample_detector();
        // "wo" は英単語ではないので、文頭に来にくい助詞でも日本語になる。
        assert_eq!(detector.evaluate_word("wo", Context::Neutral), Verdict::Japanese);
        assert_eq!(detector.evaluate_word("me", Context::Neutral), Verdict::English);
        assert_eq!(detector.evaluate_word("to", Context::Neutral), Verdict::English);
    }

    #[test]
    fn english_context_shifts_a_close_call() {
        // Arrange
        let detector = sample_detector();

        // Act
        let neutral = detector.evaluate_word("kane", Context::Neutral);
        let after_english = detector.evaluate_word("kane", Context::AfterEnglish);

        // Assert
        // 日本語 -4.60 (コスト 5300)、英語 -5.31。差 0.71 は、英語の直後の補正 0.5 を引くとしきい値を割る。
        assert_eq!(neutral, Verdict::Japanese);
        assert_eq!(after_english, Verdict::English);
    }

    #[test]
    fn in_japanese_flow_particles_win_over_common_english_words() {
        let detector = sample_detector();
        // 文の途中なら「と」「まで」は普通に現れる。
        assert_eq!(detector.evaluate_word("to", Context::AfterJapanese), Verdict::Japanese);
        assert_eq!(detector.evaluate_word("made", Context::AfterJapanese), Verdict::Japanese);
        // 「目」は「me」より弱い。「ほめ」「たけ」は辞書に無い。
        assert_eq!(detector.evaluate_word("me", Context::AfterJapanese), Verdict::English);
        assert_eq!(detector.evaluate_word("home", Context::AfterJapanese), Verdict::English);
        assert_eq!(detector.evaluate_word("take", Context::AfterJapanese), Verdict::English);
    }

    #[test]
    fn in_japanese_flow_unknown_spellings_stay_japanese() {
        let detector = sample_detector();
        assert_eq!(detector.evaluate_word("nunu", Context::AfterJapanese), Verdict::Japanese);
        assert_eq!(detector.evaluate_word("nunu", Context::Neutral), Verdict::English);
    }

    #[test]
    fn in_japanese_flow_loanword_spellings_are_readable() {
        let detector = sample_detector();
        // "wi" は英語に多い綴りなので、英語の文脈ではローマ字とみなさない。
        assert_eq!(detector.evaluate_word("windou", Context::Neutral), Verdict::English);
        assert_eq!(detector.evaluate_word("windou", Context::AfterJapanese), Verdict::Japanese);
    }

    #[test]
    fn in_japanese_flow_unreadable_words_are_english() {
        let detector = sample_detector();
        for word in ["for", "the", "string", "window"] {
            assert_eq!(detector.evaluate_word(word, Context::AfterJapanese), Verdict::English, "{word}");
        }
    }

    #[test]
    fn incomplete_romaji_at_a_boundary_is_english() {
        let detector = sample_detector();
        assert_eq!(detector.evaluate_word("korek", Context::Neutral), Verdict::English);
    }

    #[test]
    fn unlikely_kana_is_not_japanese() {
        let detector = sample_detector();
        // 辞書に無いかなの連続は、コストが高すぎるので日本語とみなさない。
        assert_eq!(detector.evaluate_word("nunu", Context::Neutral), Verdict::English);
        assert_eq!(detector.evaluate_prefix("nununu", Context::Neutral), Verdict::Undetermined);
    }

    #[test]
    fn rule_words_block_mid_word_detection() {
        let detector = sample_detector();
        // "sudo" は判断データベースで英語。途中の "sud" でも日本語にしない。
        assert_eq!(detector.evaluate_word("sudo", Context::Neutral), Verdict::English);
        assert_ne!(detector.evaluate_prefix("sudo", Context::Neutral), Verdict::Japanese);
    }

    #[test]
    fn learning_overrides_the_statistics_in_both_directions() {
        // Arrange
        let mut detector = sample_detector();
        assert_eq!(detector.evaluate_word("made", Context::Neutral), Verdict::English);
        assert_eq!(detector.evaluate_word("kore", Context::Neutral), Verdict::Japanese);

        // Act
        detector.learn("made", Language::Japanese, false);
        detector.learn("kore", Language::English, false);

        // Assert
        assert_eq!(detector.evaluate_word("made", Context::Neutral), Verdict::Japanese);
        assert_eq!(detector.evaluate_word("kore", Context::Neutral), Verdict::English);
        assert_eq!(detector.evaluate_prefix("kore", Context::Neutral), Verdict::Undetermined);

        // Act: 逆向きに覚え直す
        detector.learn("kore", Language::Japanese, false);

        // Assert
        assert_eq!(detector.evaluate_word("kore", Context::Neutral), Verdict::Japanese);
    }

    #[test]
    fn english_words_apply_in_japanese_flow_only_when_learned_there() {
        // Arrange
        let mut detector = sample_detector();

        // Act
        detector.learn("kore", Language::English, false);
        detector.learn("sore", Language::English, true);

        // Assert
        // 文の途中で打つ "kore" は「これ」のまま。"sore" は文の途中でも英語にする。
        assert_eq!(detector.evaluate_word("kore", Context::AfterJapanese), Verdict::Japanese);
        assert_eq!(detector.evaluate_word("sore", Context::AfterJapanese), Verdict::English);
        assert_eq!(detector.evaluate_word("sore", Context::Neutral), Verdict::English);
    }

    #[test]
    fn a_learned_correction_overrides_a_sequence_rule() {
        // Arrange
        let mut detector = sample_detector();
        // 判断データベースでは、"tatoe" は単語の途中でも日本語に決まる。
        assert_eq!(detector.evaluate_prefix("tatoe", Context::Neutral), Verdict::Japanese);

        // Act
        detector.learn("tatoe", Language::English, false);

        // Assert
        assert_eq!(detector.evaluate_prefix("tatoe", Context::Neutral), Verdict::Undetermined);
        assert_eq!(detector.evaluate_word("tatoe", Context::Neutral), Verdict::English);
        // 日本語の入力が続いている間は、その間に訂正していないので規則のまま。
        assert_eq!(detector.evaluate_prefix("tatoe", Context::AfterJapanese), Verdict::Japanese);
    }

    #[test]
    fn a_learned_japanese_word_is_kept_open_even_in_a_rare_spelling() {
        // Arrange
        let mut detector = sample_detector();
        // "wi" は「うぃ」と読めるが、一般的なローマ字ではないので英語に決まる。
        assert_eq!(detector.evaluate_prefix("wi", Context::Neutral), Verdict::English);

        // Act
        detector.learn("wi", Language::Japanese, false);

        // Assert
        assert_eq!(detector.evaluate_prefix("wi", Context::Neutral), Verdict::Undetermined);
        assert_eq!(detector.evaluate_word("wi", Context::Neutral), Verdict::Japanese);
        // 覚えた単語から外れた打鍵列は、これまでどおり英語。
        assert_eq!(detector.evaluate_prefix("win", Context::Neutral), Verdict::English);
    }

    #[test]
    fn a_learned_word_stops_a_sequence_rule_inside_it() {
        // Arrange
        let mut detector = sample_detector();

        // Act
        detector.learn("tatoeba", Language::English, false);

        // Assert
        // "tatoe" まで打った時点で日本語にすると、"tatoeba" の訂正に届かない。
        assert_eq!(detector.evaluate_prefix("tatoe", Context::Neutral), Verdict::Undetermined);
        assert_eq!(detector.evaluate_word("tatoeba", Context::Neutral), Verdict::English);
        // 単語が "tatoe" で終わった時は、規則のとおり日本語。
        assert_eq!(detector.evaluate_word("tatoe", Context::Neutral), Verdict::Japanese);
    }

    #[test]
    fn rule_words_do_not_apply_in_japanese_flow() {
        let detector = sample_detector();
        // "nano" は判断データベースで英語だが、文の途中では「なの」として読む。
        assert_eq!(detector.evaluate_word("nano", Context::Neutral), Verdict::English);
        assert_eq!(detector.evaluate_word("nano", Context::AfterJapanese), Verdict::Japanese);
    }
}

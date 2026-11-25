//! 英単語の出現頻度。打鍵列が英語である確からしさを求めるのに使う。

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

use fst::{IntoStreamer, MapBuilder, Streamer};

/// 接頭辞ごとの頻度の合計を、この長さまでは読み込み時に計算しておく。
/// 短い接頭辞ほど該当する単語が多く、打鍵のたびに数えると遅い。
const PRECOMPUTED_PREFIX_LEN: usize = 3;

pub struct EnglishLexicon {
    map: fst::Map<Vec<u8>>,
    total: f64,
    short_prefix_counts: HashMap<Vec<u8>, u64>,
}

impl EnglishLexicon {
    pub fn open(path: &Path) -> io::Result<Self> {
        Self::from_bytes(fs::read(path)?)
    }

    pub fn from_bytes(bytes: Vec<u8>) -> io::Result<Self> {
        let invalid = |error: fst::Error| io::Error::new(io::ErrorKind::InvalidData, error.to_string());
        let map = fst::Map::new(bytes).map_err(invalid)?;
        // 壊れた fst は、たどっている途中で panic することがある。
        map.as_fst().verify().map_err(invalid)?;
        let mut total = 0u64;
        let mut short_prefix_counts: HashMap<Vec<u8>, u64> = HashMap::new();
        let mut stream = map.stream();
        while let Some((word, count)) = stream.next() {
            total += count;
            for length in 1..=word.len().min(PRECOMPUTED_PREFIX_LEN) {
                *short_prefix_counts.entry(word[..length].to_vec()).or_default() += count;
            }
        }
        drop(stream);
        Ok(Self {
            map,
            total: total as f64,
            short_prefix_counts,
        })
    }

    /// 単語と出現回数の組から、`from_bytes` で読めるデータを作る。
    pub fn build(words: impl IntoIterator<Item = (String, u64)>) -> Vec<u8> {
        let mut counts: HashMap<String, u64> = HashMap::new();
        for (word, count) in words {
            *counts.entry(word).or_default() += count;
        }
        let mut sorted: Vec<(String, u64)> = counts.into_iter().collect();
        sorted.sort();
        let mut builder = MapBuilder::memory();
        for (word, count) in sorted {
            builder.insert(word, count).expect("words are inserted in sorted order");
        }
        builder.into_inner().expect("in-memory fst")
    }

    pub fn contains(&self, word: &str) -> bool {
        self.map.contains_key(word)
    }

    /// 単語の出現確率の常用対数。語彙に無ければ `None`。
    pub fn word_log_probability(&self, word: &str) -> Option<f64> {
        let count = self.map.get(word)?;
        Some((count as f64 / self.total).log10())
    }

    /// `prefix` で始まる単語の出現確率の合計の常用対数。該当する単語が無ければ `None`。
    pub fn prefix_log_probability(&self, prefix: &str) -> Option<f64> {
        let count = self.prefix_count(prefix.as_bytes());
        (count > 0).then(|| (count as f64 / self.total).log10())
    }

    fn prefix_count(&self, prefix: &[u8]) -> u64 {
        if prefix.is_empty() {
            return self.total as u64;
        }
        if prefix.len() <= PRECOMPUTED_PREFIX_LEN {
            return self.short_prefix_counts.get(prefix).copied().unwrap_or(0);
        }
        let mut upper = prefix.to_vec();
        // 語彙は ASCII の小文字だけなので、末尾のバイトを 1 進めれば接頭辞の範囲の上端になる。
        *upper.last_mut().expect("prefix is not empty") += 1;
        let mut stream = self.map.range().ge(prefix).lt(&upper).into_stream();
        let mut count = 0;
        while let Some((_, value)) = stream.next() {
            count += value;
        }
        count
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn sample_lexicon() -> EnglishLexicon {
        // 先頭の語は、他の語の出現確率を実際の語彙に近い大きさへ下げるための詰め物。
        let words = [
            ("zzzz", 100_000_000),
            ("kane", 500),
            ("the", 500_000),
            ("for", 100_000),
            ("made", 12_000),
            ("make", 9_000),
            ("take", 8_000),
            ("home", 7_000),
            ("korea", 900),
            ("korean", 600),
            ("sake", 60),
            ("sore", 40),
            ("tattoo", 30),
            ("image", 3_000),
            ("imagine", 800),
            ("more", 30_000),
            ("me", 60_000),
            ("to", 300_000),
            ("no", 40_000),
            ("son", 2_000),
            ("one", 80_000),
            ("area", 5_000),
            ("are", 150_000),
        ];
        let bytes = EnglishLexicon::build(words.iter().map(|(word, count)| (word.to_string(), *count)));
        EnglishLexicon::from_bytes(bytes).expect("valid lexicon")
    }

    #[test]
    fn word_probability_is_the_share_of_all_counts() {
        // Arrange
        let bytes = EnglishLexicon::build([("aa".to_string(), 900), ("ab".to_string(), 100)]);
        let lexicon = EnglishLexicon::from_bytes(bytes).expect("valid lexicon");

        // Act
        let probability = lexicon.word_log_probability("ab").expect("known word");

        // Assert
        assert!((probability - (-1.0)).abs() < 1e-9);
        assert_eq!(lexicon.word_log_probability("ac"), None);
    }

    #[test]
    fn prefix_probability_sums_every_word_with_the_prefix() {
        // Arrange
        let bytes = EnglishLexicon::build([
            ("korea".to_string(), 60),
            ("korean".to_string(), 30),
            ("kore".to_string(), 10),
            ("zzzz".to_string(), 900),
        ]);
        let lexicon = EnglishLexicon::from_bytes(bytes).expect("valid lexicon");

        // Act
        let short = lexicon.prefix_log_probability("ko").expect("prefix exists");
        let long = lexicon.prefix_log_probability("korea").expect("prefix exists");

        // Assert
        assert!((short - (0.1f64).log10()).abs() < 1e-9);
        assert!((long - (0.09f64).log10()).abs() < 1e-9);
        assert_eq!(lexicon.prefix_log_probability("korew"), None);
        assert_eq!(lexicon.prefix_log_probability("x"), None);
    }

    #[test]
    fn duplicate_words_are_summed_when_building() {
        let bytes = EnglishLexicon::build([("a".to_string(), 1), ("a".to_string(), 2), ("b".to_string(), 3)]);
        let lexicon = EnglishLexicon::from_bytes(bytes).expect("valid lexicon");
        assert!((lexicon.word_log_probability("a").expect("known") - (0.5f64).log10()).abs() < 1e-9);
    }
}

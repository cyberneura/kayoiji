//! ローマ字からかなへの変換。変換規則は Mozc のローマ字テーブルを使う。

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

const TABLE_TSV: &str = include_str!("../data/romaji-hiragana.tsv");

/// ローマ字としては有効だが、日本語の入力よりも英語の綴りとして現れる方がはるかに多い
/// 規則 ("the", "what", "twice")。入力中の文字列の変換には使うが、日本語と判定する根拠にはしない。
const RARE_PREFIXES: [&str; 13] = [
    "wh", "th", "dh", "tw", "dw", "sw", "zw", "kw", "gw", "hw", "t'", "d'", "wy",
];
const RARE_RULES: [&str; 9] = ["ye", "wi", "we", "wu", "www", "zh", "zj", "zk", "zl"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// このセグメントの元になった打鍵。
    pub raw: String,
    pub out: String,
    /// どの規則にも当たらず、打鍵のまま残した文字では false。
    pub converted: bool,
    core: bool,
}

impl Segment {
    fn literal(c: char) -> Self {
        Self {
            raw: c.to_string(),
            out: c.to_string(),
            converted: false,
            core: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Parsed {
    pub segments: Vec<Segment>,
    /// 続きの打鍵で規則に育つ可能性がある末尾の打鍵 ("k", "ny", 単独の "n")。
    pub pending: String,
}

impl Parsed {
    /// 未確定の打鍵を除いた変換済みの文字列。
    pub fn text(&self) -> String {
        self.segments.iter().map(|s| s.out.as_str()).collect()
    }

    /// 入力中に表示する文字列。変換済みの文字列に未確定の打鍵を続けたもの。
    pub fn display(&self) -> String {
        let mut text = self.text();
        text.push_str(&self.pending);
        text
    }

    /// 規則で変換できたセグメントの数。
    pub fn converted_count(&self) -> usize {
        self.segments.iter().filter(|s| s.converted).count()
    }

    /// すべての打鍵が、一般的なローマ字入力の日本語として成立する場合に true。
    pub fn is_strict(&self) -> bool {
        let table = RomajiTable::global();
        let segments_are_core = self
            .segments
            .iter()
            .enumerate()
            // 長音記号は直前にかなが必要。先頭の "-" はハイフンとみなす。
            .all(|(index, s)| s.core && !(s.raw == "-" && index == 0));
        segments_are_core && (self.pending.is_empty() || table.core_prefixes.contains(&self.pending))
    }

    /// ローマ字の規則に当てはまらない打鍵があるかどうか ("st"、"pl")。あれば、日本語の入力ではない。
    ///
    /// `is_strict` より狭い。"thi" (てぃ)、"dhi" (でぃ)、"zh" (←) のように、英語の綴りに多くても
    /// 規則にある打鍵と、その途中 ("th") は、日本語の入力とみなす。
    pub fn has_unreadable(&self) -> bool {
        self.segments.iter().any(|segment| !segment.converted)
    }

    /// 先頭から、規則で変換できない英字の手前までの表示と、その後ろの打鍵を返す。
    /// "kyouhast" なら ("きょうは", "st")、"3gatsust" なら ("3がつ", "st")。
    /// 数字や記号は規則に無くても、打ったとおりに表示しているので、かなの側に含める。
    pub fn split_at_unreadable(&self) -> (String, String) {
        let readable = self
            .segments
            .iter()
            .take_while(|segment| segment.converted || !segment.raw.starts_with(|c: char| c.is_ascii_alphabetic()))
            .count();
        let kana = self.segments[..readable].iter().map(|s| s.out.as_str()).collect();
        let mut rest: String = self.segments[readable..].iter().map(|s| s.raw.as_str()).collect();
        rest.push_str(&self.pending);
        (kana, rest)
    }

    /// 確定時と同じ方法で未確定の打鍵を解決する。単独の "n" は「ん」にし、
    /// それ以外は打鍵のまま残す。
    pub fn finalize(mut self) -> Vec<Segment> {
        let pending = std::mem::take(&mut self.pending);
        if pending == "n" {
            self.segments.push(Segment {
                raw: pending,
                out: "ん".to_string(),
                converted: true,
                core: true,
            });
            return self.segments;
        }
        self.segments.extend(pending.chars().map(Segment::literal));
        self.segments
    }
}

struct Rule {
    output: String,
    /// 入力の末尾のうち、次の規則の先頭として読み直す打鍵の数 ("kk" → 「っ」+ "k")。
    carry: usize,
    core: bool,
}

pub struct RomajiTable {
    rules: HashMap<String, Rule>,
    prefixes: HashSet<String>,
    core_prefixes: HashSet<String>,
    max_len: usize,
}

impl RomajiTable {
    pub fn global() -> &'static RomajiTable {
        static TABLE: OnceLock<RomajiTable> = OnceLock::new();
        TABLE.get_or_init(|| RomajiTable::from_tsv(TABLE_TSV))
    }

    fn from_tsv(tsv: &str) -> Self {
        let mut rules = HashMap::new();
        let mut prefixes = HashSet::new();
        let mut core_prefixes = HashSet::new();
        let mut max_len = 0;
        for line in tsv.lines() {
            let mut fields = line.split('\t');
            let (Some(input), Some(output)) = (fields.next(), fields.next()) else {
                continue;
            };
            if input.is_empty() {
                continue;
            }
            let carry = fields.next().map_or(0, |next| next.chars().count());
            let core = is_core_rule(input);
            let length = input.chars().count();
            max_len = max_len.max(length);
            for end in 1..length {
                let prefix: String = input.chars().take(end).collect();
                if core {
                    core_prefixes.insert(prefix.clone());
                }
                prefixes.insert(prefix);
            }
            rules.insert(
                input.to_string(),
                Rule {
                    output: output.to_string(),
                    carry,
                    core,
                },
            );
        }
        Self {
            rules,
            prefixes,
            core_prefixes,
            max_len,
        }
    }

    /// 打鍵を変換する。最初の大文字以降は打鍵のまま残す。
    /// "Tauri" のような名前を入力中の文字列に含められるようにするため。
    pub fn parse(&self, raw: &str) -> Parsed {
        let chars: Vec<char> = raw.chars().collect();
        let mut segments = Vec::new();
        let mut literal_run = false;
        let mut index = 0;
        while index < chars.len() {
            let c = chars[index];
            if literal_run || c.is_ascii_uppercase() {
                literal_run = true;
                segments.push(Segment::literal(c));
                index += 1;
                continue;
            }
            let rest = &chars[index..];
            if rest.len() <= self.max_len {
                let rest_text: String = rest.iter().collect();
                if self.prefixes.contains(&rest_text) {
                    return Parsed {
                        segments,
                        pending: rest_text,
                    };
                }
            }
            match self.longest_rule(rest) {
                Some((length, rule)) => {
                    let consumed = length - rule.carry;
                    segments.push(Segment {
                        raw: rest[..consumed].iter().collect(),
                        out: rule.output.clone(),
                        converted: true,
                        core: rule.core,
                    });
                    index += consumed;
                }
                None => {
                    segments.push(Segment::literal(c));
                    index += 1;
                }
            }
        }
        Parsed {
            segments,
            pending: String::new(),
        }
    }

    fn longest_rule(&self, rest: &[char]) -> Option<(usize, &Rule)> {
        (1..=rest.len().min(self.max_len)).rev().find_map(|length| {
            let key: String = rest[..length].iter().collect();
            self.rules.get(&key).map(|rule| (length, rule))
        })
    }
}

fn is_core_rule(input: &str) -> bool {
    if input == "-" {
        return true;
    }
    let first = input.as_bytes()[0];
    if matches!(first, b'q' | b'v' | b'l' | b'x') {
        return false;
    }
    // "ca" や "ci" は英語の綴り。"ch" と "kocchi" の "cc" はローマ字として扱う。
    if first == b'c' && !(input.starts_with("ch") || input == "cc") {
        return false;
    }
    if RARE_PREFIXES.iter().any(|prefix| input.starts_with(prefix)) {
        return false;
    }
    if RARE_RULES.contains(&input) {
        return false;
    }
    input
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b == b'\'')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(raw: &str) -> Parsed {
        RomajiTable::global().parse(raw)
    }

    #[test]
    fn converts_plain_syllables() {
        let parsed = parse("tatoeba");
        assert_eq!(parsed.text(), "たとえば");
        assert_eq!(parsed.pending, "");
        assert!(parsed.is_strict());
    }

    #[test]
    fn keeps_incomplete_consonants_pending() {
        let parsed = parse("tatoeb");
        assert_eq!(parsed.text(), "たとえ");
        assert_eq!(parsed.pending, "b");
        assert_eq!(parsed.display(), "たとえb");
        assert!(parsed.is_strict());
    }

    #[test]
    fn doubled_consonant_becomes_small_tsu() {
        assert_eq!(parse("kitte").text(), "きって");
        assert_eq!(parse("kocchi").text(), "こっち");
        assert_eq!(parse("matcha").text(), "まっちゃ");
        assert!(parse("kocchi").is_strict());
    }

    #[test]
    fn n_before_consonant_becomes_hatsuon() {
        assert_eq!(parse("kangae").text(), "かんがえ");
        assert_eq!(parse("konnna").text(), "こんな");
        assert_eq!(parse("kon'ya").text(), "こんや");
    }

    #[test]
    fn trailing_n_stays_pending_until_finalized() {
        // Arrange
        let parsed = parse("hon");

        // Act
        let display = parsed.display();
        let finalized: String = parsed.finalize().iter().map(|s| s.out.as_str()).collect();

        // Assert
        assert_eq!(display, "ほn");
        assert_eq!(finalized, "ほん");
    }

    #[test]
    fn contracted_sounds_convert() {
        assert_eq!(parse("kyou").text(), "きょう");
        assert_eq!(parse("shashin").display(), "しゃしn");
        assert_eq!(parse("jisho").text(), "じしょ");
        assert_eq!(parse("fairu").text(), "ふぁいる");
    }

    #[test]
    fn prolonged_sound_mark_needs_a_preceding_kana() {
        assert_eq!(parse("ko-hi-").text(), "こーひー");
        assert!(parse("ko-hi-").is_strict());
        assert!(!parse("-ko").is_strict());
    }

    #[test]
    fn english_spellings_are_not_strict() {
        for word in ["the", "what", "list", "quit", "city", "next", "sk", "fort", "ask"] {
            assert!(!parse(word).is_strict(), "{word} must not look like romaji");
        }
    }

    #[test]
    fn trailing_consonant_that_can_still_grow_is_strict() {
        // "for" は、続きが "a" なら「ふぉら」になる。区切りが来るまで英語とは断定できない。
        let parsed = parse("for");
        assert_eq!(parsed.pending, "r");
        assert!(parsed.is_strict());
    }

    #[test]
    fn unconvertible_keys_stay_as_typed() {
        let parsed = parse("kor");
        assert_eq!(parsed.display(), "こr");

        let parsed = parse("for ");
        assert_eq!(parsed.display(), "ふぉr ");
        assert!(!parsed.is_strict());
    }

    #[test]
    fn uppercase_starts_a_literal_run() {
        let parsed = parse("korehaTauri");
        assert_eq!(parsed.display(), "これはTauri");
        assert!(!parsed.is_strict());
    }

    #[test]
    fn segments_keep_the_keys_they_came_from() {
        // Arrange
        let parsed = parse("kitte");

        // Act
        let raws: Vec<&str> = parsed.segments.iter().map(|s| s.raw.as_str()).collect();

        // Assert
        assert_eq!(raws, ["ki", "t", "te"]);
    }

    #[test]
    fn punctuation_converts_but_is_not_strict() {
        let parsed = parse("sore,");
        assert_eq!(parsed.text(), "それ、");
        assert!(!parsed.is_strict());
    }
}

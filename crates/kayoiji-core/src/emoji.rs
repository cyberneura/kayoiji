//! 絵文字の一覧と、キーワードから絵文字を引く索引。
//!
//! 元のデータは emoji-explorer (https://emoji.ytyng.com/) の一覧 (`emoji.ja.json`) と索引
//! (`search-index.json`)。ビルドで、肌の色の違いを基本の絵文字の下にまとめた一覧 (`CATALOG_FILE`) と、
//! キーワード (読み・表記・英語) から基本の絵文字を引く辞書 (`INDEX_FILE`) を作る。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::ops::RangeInclusive;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::dict::{Dictionary, DictionaryBuilder};
use crate::kana::to_hiragana;

pub const CATALOG_FILE: &str = "emoji.json";
pub const INDEX_FILE: &str = "emoji.dict";

/// 絵文字として表示させる異体字セレクター。索引には、これが付かない形も入っている。
const VARIATION_SELECTOR: char = '\u{FE0F}';
const SKIN_TONES: RangeInclusive<char> = '\u{1F3FB}'..='\u{1F3FF}';

/// 絵文字の一覧。グループ (「スマイリーと感情」など) ごとに、基本の絵文字が並ぶ。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalog {
    pub groups: Vec<Group>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub title: String,
    pub emoji: Vec<EmojiEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmojiEntry {
    pub text: String,
    pub name: String,
    /// 肌の色の違い。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<String>,
}

/// 一覧の中の絵文字の位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EmojiId {
    pub group: usize,
    pub index: usize,
}

pub struct Emoji {
    catalog: Catalog,
    index: Dictionary,
    ids: HashMap<String, EmojiId>,
}

impl Emoji {
    pub fn new(catalog: Catalog, index: Dictionary) -> Self {
        let mut ids = HashMap::new();
        for (group, entries) in catalog.groups.iter().enumerate() {
            for (index, entry) in entries.emoji.iter().enumerate() {
                ids.insert(entry.text.clone(), EmojiId { group, index });
            }
        }
        Self { catalog, index, ids }
    }

    /// `dir` の一覧と索引を開く。
    pub fn open(dir: &Path) -> io::Result<Self> {
        let catalog = std::fs::read_to_string(dir.join(CATALOG_FILE))?;
        let catalog: Catalog =
            serde_json::from_str(&catalog).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let index = Dictionary::open(&dir.join(INDEX_FILE))?;
        Ok(Self::new(catalog, index))
    }

    pub fn groups(&self) -> &[Group] {
        &self.catalog.groups
    }

    pub fn entry(&self, id: EmojiId) -> &EmojiEntry {
        &self.catalog.groups[id.group].emoji[id.index]
    }

    /// キーワードに完全に一致する絵文字を、索引に並んでいる順に返す。
    pub fn lookup(&self, keyword: &str) -> Vec<&str> {
        self.index
            .lookup(keyword)
            .map(|range| self.index.entries(range).map(|entry| entry.surface).collect())
            .unwrap_or_default()
    }

    /// `queries` のどれかで始まるキーワードの絵文字を、最大 `limit` 個返す。
    /// 完全に一致するキーワード、短いキーワード、`queries` で先に渡した方を前に並べる。
    pub fn search(&self, queries: &[&str], limit: usize) -> Vec<EmojiId> {
        let mut keys = Vec::new();
        for (order, query) in queries.iter().enumerate().filter(|(_, query)| !query.is_empty()) {
            for (keyword, range) in self.index.prefix_search(query) {
                let exact = keyword == *query;
                keys.push((!exact, keyword.chars().count(), order, keyword, range));
            }
        }
        keys.sort_by(|a, b| (a.0, a.1, a.2, &a.3).cmp(&(b.0, b.1, b.2, &b.3)));
        let mut seen = HashSet::new();
        let mut found = Vec::new();
        for (.., range) in keys {
            for entry in self.index.entries(range) {
                if let Some(&id) = self.ids.get(entry.surface) {
                    if seen.insert(id) {
                        found.push(id);
                        if found.len() == limit {
                            return found;
                        }
                    }
                }
            }
        }
        found
    }
}

/// 異体字セレクターと肌の色を除いた並び。同じ絵文字の表記の違いと、肌の色の違いが同じになる。
fn base_key(text: &str) -> String {
    text.chars()
        .filter(|&c| c != VARIATION_SELECTOR && !SKIN_TONES.contains(&c))
        .collect()
}

/// 漢字を含むキーワード。読みでは引けないので、ビルドの時に読みを足す。
fn has_kanji(keyword: &str) -> bool {
    keyword.chars().any(|c| ('\u{3400}'..='\u{9FFF}').contains(&c) || c == '々')
}

#[derive(Deserialize)]
struct RawGroup {
    #[serde(rename = "groupTitleJa")]
    title: String,
    subgroups: Vec<RawSubgroup>,
}

#[derive(Deserialize)]
struct RawSubgroup {
    emojis: Vec<RawEmoji>,
}

#[derive(Deserialize)]
struct RawEmoji {
    #[serde(rename = "emojiChar")]
    text: String,
    status: String,
    #[serde(rename = "descriptionJa")]
    name: String,
}

/// emoji-explorer のデータを読んだもの。
pub struct Source {
    catalog: Catalog,
    /// キーワードと、その基本の絵文字。絵文字は索引に並んでいた順。
    keywords: BTreeMap<String, Vec<String>>,
}

impl Source {
    /// `emoji.ja.json` と `search-index.json` の内容を読む。
    ///
    /// 一覧からは fully-qualified の絵文字だけを取り、肌の色の違いを基本の絵文字の下にまとめる。
    /// 索引の値は一覧の基本の絵文字に読み替え、一覧に無い文字 (矢印などの記号) は捨てる。
    pub fn parse(catalog_json: &str, index_json: &str) -> io::Result<Self> {
        let invalid = |error: serde_json::Error| io::Error::new(io::ErrorKind::InvalidData, error);
        let groups: Vec<RawGroup> = serde_json::from_str(catalog_json).map_err(invalid)?;
        let index: BTreeMap<String, Vec<String>> = serde_json::from_str(index_json).map_err(invalid)?;

        let mut catalog = Catalog { groups: Vec::new() };
        let mut bases: HashMap<String, (usize, usize)> = HashMap::new();
        for raw in &groups {
            let mut group = Group {
                title: raw.title.clone(),
                emoji: Vec::new(),
            };
            let at = catalog.groups.len();
            for subgroup in &raw.subgroups {
                let mut previous = None;
                for emoji in subgroup.emojis.iter().filter(|emoji| emoji.status == "fully-qualified") {
                    let key = base_key(&emoji.text);
                    if !emoji.text.chars().any(|c| SKIN_TONES.contains(&c)) {
                        bases.insert(key, (at, group.emoji.len()));
                        previous = Some(group.emoji.len());
                        group.emoji.push(EmojiEntry {
                            text: emoji.text.clone(),
                            name: emoji.name.clone(),
                            variants: Vec::new(),
                        });
                        continue;
                    }
                    // 肌の色を 2 つ持つもの (握手 🫱🏻‍🫲🏼 など) は、肌の色を除いた形が基本の絵文字 (🤝) と違う。
                    // 一覧ではそれぞれの基本の絵文字の後ろに並んでいるので、直前の基本の絵文字の下に置く。
                    let base = match bases.get(&key) {
                        Some(&(group_at, index)) if group_at == at => Some(index),
                        _ => previous,
                    };
                    if let Some(index) = base {
                        bases.entry(key).or_insert((at, index));
                        group.emoji[index].variants.push(emoji.text.clone());
                    }
                }
            }
            // 肌の色や髪型の部品だけのグループ (component) は、絵文字が残らないので並べない。
            if !group.emoji.is_empty() {
                catalog.groups.push(group);
            }
        }

        let mut keywords = BTreeMap::new();
        for (keyword, values) in index {
            let keyword = keyword.trim();
            let mut emoji: Vec<String> = Vec::new();
            for value in values {
                let Some(&(group, index)) = bases.get(&base_key(&value)) else {
                    continue;
                };
                let text = &catalog.groups[group].emoji[index].text;
                if !emoji.contains(text) {
                    emoji.push(text.clone());
                }
            }
            if !keyword.is_empty() && !emoji.is_empty() {
                keywords.insert(keyword.to_string(), emoji);
            }
        }
        Ok(Self { catalog, keywords })
    }

    /// 読みを付けたいキーワード (漢字を含むもの)。
    pub fn kanji_keywords(&self) -> HashSet<String> {
        self.keywords.keys().filter(|keyword| has_kanji(keyword)).cloned().collect()
    }

    /// 一覧 (JSON) と索引 (辞書) を作る。`readings` は、漢字を含むキーワードの読み。
    ///
    /// 索引のキーは空白を詰める (`red heart` は `redheart`)。絵文字モードの Space は選択に使うので、
    /// 検索語に空白を打てない。英語は小文字にし、カタカナはひらがなにしたものも足す。漢字を含むキーワードは、
    /// 普通の変換で候補の表記から引くのでそのまま入れ、絵文字モードでかなから引けるように読みも足す。
    pub fn build(&self, readings: &HashMap<String, Vec<String>>) -> (String, Vec<u8>) {
        let mut keys: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut add = |key: String, emoji: &[String]| {
            let list = keys.entry(key).or_default();
            for text in emoji {
                if !list.contains(text) {
                    list.push(text.clone());
                }
            }
        };
        for (keyword, emoji) in &self.keywords {
            let key: String = keyword.chars().filter(|c| !c.is_whitespace()).collect();
            if key.is_ascii() {
                add(key.to_lowercase(), emoji);
                continue;
            }
            let hiragana = to_hiragana(&key);
            add(key.clone(), emoji);
            if hiragana != key {
                add(hiragana, emoji);
            }
            for reading in readings.get(keyword).into_iter().flatten() {
                add(reading.clone(), emoji);
            }
        }
        let mut builder = DictionaryBuilder::lookup_only();
        for (key, emoji) in &keys {
            for (order, text) in emoji.iter().enumerate() {
                builder.add(key, 0, 0, i16::try_from(order).unwrap_or(i16::MAX), text);
            }
        }
        let catalog = serde_json::to_string(&self.catalog).expect("the catalog serializes");
        (catalog, builder.build())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// テスト用の小さな絵文字のデータ。グループは 2 つ。👍 には肌の色の違いがある。
    pub(crate) const SAMPLE_CATALOG: &str = r#"[
        {"groupTitle": "Smileys", "groupTitleJa": "スマイリー", "subgroups": [
            {"subgroupTitle": "cat-face", "subgroupTitleJa": "猫の顔", "emojis": [
                {"codePoints": [128570], "status": "fully-qualified", "emojiChar": "😺", "descriptionJa": "笑う猫"},
                {"codePoints": [9786, 65039], "status": "fully-qualified", "emojiChar": "☺️", "descriptionJa": "笑顔"},
                {"codePoints": [9786], "status": "unqualified", "emojiChar": "☺", "descriptionJa": "笑顔"}
            ]}
        ]},
        {"groupTitle": "Animals", "groupTitleJa": "動物", "subgroups": [
            {"subgroupTitle": "animal-mammal", "subgroupTitleJa": "哺乳類", "emojis": [
                {"codePoints": [128049], "status": "fully-qualified", "emojiChar": "🐱", "descriptionJa": "猫の顔"},
                {"codePoints": [128008], "status": "fully-qualified", "emojiChar": "🐈", "descriptionJa": "猫"},
                {"codePoints": [128054], "status": "fully-qualified", "emojiChar": "🐶", "descriptionJa": "犬の顔"}
            ]},
            {"subgroupTitle": "hand", "subgroupTitleJa": "手", "emojis": [
                {"codePoints": [128077], "status": "fully-qualified", "emojiChar": "👍", "descriptionJa": "いいね"},
                {"codePoints": [128077, 127995], "status": "fully-qualified", "emojiChar": "👍🏻", "descriptionJa": "いいね: 明るい肌"},
                {"codePoints": [128077, 127999], "status": "fully-qualified", "emojiChar": "👍🏿", "descriptionJa": "いいね: 濃い肌"},
                {"codePoints": [129309], "status": "fully-qualified", "emojiChar": "🤝", "descriptionJa": "握手"},
                {"codePoints": [129309, 127995], "status": "fully-qualified", "emojiChar": "🤝🏻", "descriptionJa": "握手: 明るい肌"},
                {"codePoints": [129777, 127995, 8205, 129778, 127999], "status": "fully-qualified", "emojiChar": "🫱🏻‍🫲🏿", "descriptionJa": "握手: 明るい肌と濃い肌"}
            ]}
        ]}
    ]"#;

    pub(crate) const SAMPLE_INDEX: &str = r#"{
        "ねこ": ["🐱", "🐈"],
        "ネコ": ["🐈", "😺"],
        "猫": ["😺", "🐱"],
        "Cat": ["🐈"],
        "いいね": ["👍", "👍🏻"],
        "笑顔": ["☺", "→"],
        "いぬ": ["🐶"],
        "shake hands": ["🫱🏻‍🫲🏿"]
    }"#;

    pub(crate) fn sample_emoji() -> Emoji {
        let source = Source::parse(SAMPLE_CATALOG, SAMPLE_INDEX).expect("sample data");
        let readings = HashMap::from([("猫".to_string(), vec!["びょう".to_string()])]);
        let (catalog, index) = source.build(&readings);
        Emoji::new(
            serde_json::from_str(&catalog).expect("catalog"),
            Dictionary::from_bytes(index).expect("index"),
        )
    }

    fn texts(emoji: &Emoji, ids: &[EmojiId]) -> Vec<String> {
        ids.iter().map(|&id| emoji.entry(id).text.clone()).collect()
    }

    #[test]
    fn skin_tones_are_grouped_under_the_base_emoji() {
        // Act
        let emoji = sample_emoji();

        // Assert
        let hands = &emoji.groups()[1].emoji;
        let texts: Vec<&str> = hands.iter().map(|entry| entry.text.as_str()).collect();
        assert_eq!(texts, ["🐱", "🐈", "🐶", "👍", "🤝"]);
        assert_eq!(hands[3].variants, ["👍🏻", "👍🏿"]);
        // 肌の色を 2 つ持つ握手は、肌の色を除いても 🤝 にならないが、🤝 の下に並べる。
        assert_eq!(hands[4].variants, ["🤝🏻", "🫱🏻‍🫲🏿"]);
    }

    #[test]
    fn keywords_with_spaces_are_typed_without_them() {
        // Act
        let emoji = sample_emoji();

        // Assert: 索引の値の 🫱🏻‍🫲🏿 は、それを並べた基本の絵文字に読み替える。
        assert_eq!(emoji.lookup("shakehands"), ["🤝"]);
    }

    #[test]
    fn index_values_are_read_as_the_qualified_base_emoji() {
        // Act
        let emoji = sample_emoji();

        // Assert: U+FE0F の無い形は付いた形に、肌の色の違いは基本の絵文字に、一覧に無い記号は捨てる。
        assert_eq!(emoji.lookup("笑顔"), ["☺️"]);
        assert_eq!(emoji.lookup("いいね"), ["👍"]);
    }

    #[test]
    fn keywords_are_found_by_kana_surface_reading_and_lowercase_english() {
        // Act
        let emoji = sample_emoji();

        // Assert: ひらがなのキーワードの後に、カタカナのキーワードの分が続く。
        assert_eq!(emoji.lookup("ねこ"), ["🐱", "🐈", "😺"]);
        assert_eq!(emoji.lookup("猫"), ["😺", "🐱"]);
        assert_eq!(emoji.lookup("びょう"), ["😺", "🐱"]);
        assert_eq!(emoji.lookup("cat"), ["🐈"]);
        assert!(emoji.lookup("Cat").is_empty());
    }

    #[test]
    fn search_puts_exact_and_short_keywords_first() {
        // Arrange
        let emoji = sample_emoji();

        // Act
        let found = emoji.search(&["い"], 10);
        let both = emoji.search(&["いぬ", "cat"], 10);
        let limited = emoji.search(&["ねこ"], 2);

        // Assert
        assert_eq!(texts(&emoji, &found), ["🐶", "👍"]);
        assert_eq!(texts(&emoji, &both), ["🐶", "🐈"]);
        assert_eq!(texts(&emoji, &limited), ["🐱", "🐈"]);
    }

    #[test]
    fn kanji_keywords_are_the_ones_that_need_readings() {
        // Arrange
        let source = Source::parse(SAMPLE_CATALOG, SAMPLE_INDEX).expect("sample data");

        // Act
        let kanji = source.kanji_keywords();

        // Assert
        assert_eq!(kanji, HashSet::from(["猫".to_string(), "笑顔".to_string()]));
    }
}

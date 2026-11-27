//! 利用者の選択の記録。変換で選んだ表記、区切り直して確定した語、英語・日本語の判定の訂正を覚える。

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::rules::Language;

/// 1 つの読みについて覚えておく表記の数。
const SURFACES_PER_READING: usize = 8;
/// 覚えておく語の数。変換のたびに読みの各位置で引くので、際限なく増やさない。
const PHRASES: usize = 1000;

/// 利用者が文節を区切り直して確定した範囲。読みを 1 語の表記として変換する。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Phrase {
    reading: String,
    surface: String,
    /// 語を作っている辞書の語の `(読み, 表記)`。辞書を作り直すと文脈 ID が変わりうるので、
    /// ID ではなく語を覚えておき、起動のたびに辞書から引き直す。
    words: Vec<(String, String)>,
}

/// 記録への 1 回の変更。保存する時に、ほかのプロセスが書いた内容に重ね直す。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Change {
    Conversion { reading: String, surface: String },
    DropConversions { reading: String, surfaces: Vec<String> },
    Phrase(Phrase),
    ForgetPhrase(String),
    Language { word: String, language: Language, in_flow: bool },
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Learning {
    /// 読みごとの、選ばれた表記。新しく選んだものが先頭。
    #[serde(default)]
    conversions: BTreeMap<String, Vec<String>>,
    /// 新しく覚えたものが先頭。読みは重複しない。
    #[serde(default)]
    phrases: Vec<Phrase>,
    #[serde(default)]
    english_words: BTreeSet<String>,
    /// `english_words` のうち、日本語の入力が続いている間に訂正されたもの。
    #[serde(default)]
    flow_english_words: BTreeSet<String>,
    #[serde(default)]
    japanese_words: BTreeSet<String>,
    #[serde(skip)]
    path: Option<PathBuf>,
    /// ファイルに書けていない変更。保存に失敗したら残し、次に記録する時に保存し直す。
    #[serde(skip)]
    pending: Vec<Change>,
    /// 保存の時に読み直した、ほかのプロセスの記録で、覚えた語か英語・日本語の訂正が変わったかどうか。
    #[serde(skip)]
    outside_changes: bool,
}

impl Learning {
    /// ファイルに保存しない記録。
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// ファイルが無ければ空の記録から始める。
    pub fn load(path: &Path) -> io::Result<Self> {
        let mut learning = match fs::read_to_string(path) {
            Ok(text) => serde_json::from_str::<Learning>(&text)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Learning::default(),
            Err(error) => return Err(error),
        };
        learning.path = Some(path.to_path_buf());
        Ok(learning)
    }

    /// `load` と同じだが、読めないファイルがあっても空の記録から始める。その時は理由を返す。
    ///
    /// 記録が壊れていても入力メソッドは起動させたい。読めないファイルは `<名前>.broken` に
    /// 退避して残す。退避できない時は、上書きしないようにファイルへの保存をやめる。
    pub fn load_or_set_aside(path: &Path) -> (Self, Option<io::Error>) {
        let error = match Self::load(path) {
            Ok(learning) => return (learning, None),
            Err(error) => error,
        };
        let mut broken = path.as_os_str().to_owned();
        broken.push(".broken");
        let learning = match fs::rename(path, &broken) {
            Ok(()) => Learning {
                path: Some(path.to_path_buf()),
                ..Learning::default()
            },
            Err(_) => Learning::in_memory(),
        };
        (learning, Some(error))
    }

    pub fn conversions(&self, reading: &str) -> &[String] {
        self.conversions.get(reading).map_or(&[], Vec::as_slice)
    }

    /// 覚えた語を、語を作っている辞書の語 `(読み, 表記)` の並びで返す。
    pub fn phrases(&self) -> impl Iterator<Item = &[(String, String)]> {
        self.phrases.iter().map(|phrase| phrase.words.as_slice())
    }

    /// `reading` を読みとする覚えた語の表記。
    pub fn phrase(&self, reading: &str) -> Option<&str> {
        self.phrases
            .iter()
            .find(|phrase| phrase.reading == reading)
            .map(|phrase| phrase.surface.as_str())
    }

    /// `reading` の語を新しく覚えると数の上限で忘れることになる語の読み。
    pub fn phrase_to_drop(&self, reading: &str) -> Option<&str> {
        if self.phrases.len() < PHRASES || self.phrase(reading).is_some() {
            return None;
        }
        self.phrases.last().map(|phrase| phrase.reading.as_str())
    }

    /// 読みに `part` を含む文節のうち、選んだ表記を覚えているものの読み。`part` そのものは含めない。
    pub fn readings_containing(&self, part: &str) -> Vec<String> {
        self.conversions
            .keys()
            .filter(|reading| reading.as_str() != part && reading.contains(part))
            .cloned()
            .collect()
    }

    /// 覚えた単語を `(単語, 言語, 日本語の入力中に訂正されたか)` の組で返す。
    pub fn languages(&self) -> impl Iterator<Item = (&str, Language, bool)> {
        let english = self
            .english_words
            .iter()
            .map(|word| (word.as_str(), Language::English, self.flow_english_words.contains(word)));
        let japanese = self
            .japanese_words
            .iter()
            .map(|word| (word.as_str(), Language::Japanese, false));
        english.chain(japanese)
    }

    pub fn record_conversion(&mut self, reading: &str, surface: &str) -> io::Result<()> {
        self.record(Change::Conversion {
            reading: reading.to_string(),
            surface: surface.to_string(),
        })
    }

    /// `reading` の文節で選んだ表記のうち、`keep` が false を返すものを忘れる。
    pub fn retain_conversions(&mut self, reading: &str, keep: impl Fn(&str) -> bool) -> io::Result<()> {
        let surfaces = self
            .conversions(reading)
            .iter()
            .filter(|surface| !keep(surface))
            .cloned()
            .collect();
        self.record(Change::DropConversions {
            reading: reading.to_string(),
            surfaces,
        })
    }

    /// 辞書の語の並びを、1 語として覚える。
    pub fn record_phrase(&mut self, words: &[(String, String)]) -> io::Result<()> {
        self.record(Change::Phrase(Phrase {
            reading: words.iter().map(|(reading, _)| reading.as_str()).collect(),
            surface: words.iter().map(|(_, surface)| surface.as_str()).collect(),
            words: words.to_vec(),
        }))
    }

    /// 覚えた語を忘れる。その語を使って確定した文節の表記も忘れる。残すと、助詞を付けて
    /// 確定したことのある読みでは、忘れた語が先頭に出続ける。
    pub fn forget_phrase(&mut self, reading: &str) -> io::Result<()> {
        self.record(Change::ForgetPhrase(reading.to_string()))
    }

    pub fn record_language(&mut self, word: &str, language: Language, in_flow: bool) -> io::Result<()> {
        self.record(Change::Language {
            word: word.to_string(),
            language,
            in_flow,
        })
    }

    /// 保存の時に、ほかのプロセスの記録で、覚えた語 (`phrases`) か英語・日本語の訂正 (`languages`) が
    /// 変わったかどうかを返し、知らせを消す。変わっていたら、変換器と判定器に覚えさせ直す。
    pub fn take_outside_changes(&mut self) -> bool {
        std::mem::take(&mut self.outside_changes)
    }

    /// 変更を記録に適用し、変わったら保存する。
    fn record(&mut self, change: Change) -> io::Result<()> {
        if self.apply(&change) {
            self.pending.push(change);
        }
        self.save()
    }

    /// 変更を適用して、記録が変わったかどうかを返す。同じ変更を 2 度適用しても、結果は 1 度と同じ。
    fn apply(&mut self, change: &Change) -> bool {
        match change {
            Change::Conversion { reading, surface } => {
                let surfaces = self.conversions.entry(reading.clone()).or_default();
                if surfaces.first() == Some(surface) {
                    return false;
                }
                surfaces.retain(|known| known != surface);
                surfaces.insert(0, surface.clone());
                surfaces.truncate(SURFACES_PER_READING);
                true
            }
            Change::DropConversions { reading, surfaces } => {
                let Some(known) = self.conversions.get_mut(reading) else {
                    return false;
                };
                let count = known.len();
                known.retain(|surface| !surfaces.contains(surface));
                let changed = known.len() != count;
                if known.is_empty() {
                    self.conversions.remove(reading);
                }
                changed
            }
            Change::Phrase(phrase) => {
                if self.phrases.first() == Some(phrase) {
                    return false;
                }
                self.phrases.retain(|known| known.reading != phrase.reading);
                self.phrases.insert(0, phrase.clone());
                self.phrases.truncate(PHRASES);
                true
            }
            Change::ForgetPhrase(reading) => {
                let Some(index) = self.phrases.iter().position(|known| &known.reading == reading) else {
                    return false;
                };
                let phrase = self.phrases.remove(index);
                for (known, surfaces) in &mut self.conversions {
                    if known != reading && known.contains(reading.as_str()) {
                        surfaces.retain(|surface| !surface.contains(&phrase.surface));
                    }
                }
                self.conversions.retain(|_, surfaces| !surfaces.is_empty());
                true
            }
            Change::Language { word, language, in_flow } => match language {
                Language::English => {
                    let forgotten = self.japanese_words.remove(word);
                    let learned = self.english_words.insert(word.clone());
                    let learned_in_flow = *in_flow && self.flow_english_words.insert(word.clone());
                    forgotten || learned || learned_in_flow
                }
                Language::Japanese => {
                    let forgotten = self.english_words.remove(word);
                    let forgotten_in_flow = self.flow_english_words.remove(word);
                    let learned = self.japanese_words.insert(word.clone());
                    forgotten || forgotten_in_flow || learned
                }
            },
        }
    }

    /// 書けていない変更があれば保存する。
    ///
    /// 同じファイルに書くプロセスは 1 つとは限らない (Windows の入力メソッドは入力先のプロセスごとに動き、
    /// macOS でも入力メソッドと KayoijiPad が同時に動く)。ファイル全体を書き直すと、ほかのプロセスが
    /// 覚えた分が消えるので、ロックを取ってから読み直し、その上にこのプロセスの変更を重ねて書く。
    fn save(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let Some(path) = self.path.clone() else {
            self.pending.clear();
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let lock = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path.with_extension("json.lock"))?;
        lock.lock()?;
        let mut merged = match fs::read_to_string(&path) {
            // 読めない内容に重ねることはできないので、このプロセスの記録で置き換える。
            Ok(text) => serde_json::from_str::<Learning>(&text).unwrap_or_else(|_| self.clone()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Learning::default(),
            Err(error) => return Err(error),
        };
        for change in &self.pending {
            merged.apply(change);
        }
        // 書き込みの途中で終了しても、元のファイルが壊れないようにする。
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(&merged).expect("learning serializes"))?;
        fs::rename(&temporary, &path)?;
        self.outside_changes |= merged.phrases != self.phrases
            || merged.english_words != self.english_words
            || merged.flow_english_words != self.flow_english_words
            || merged.japanese_words != self.japanese_words;
        self.conversions = merged.conversions;
        self.phrases = merged.phrases;
        self.english_words = merged.english_words;
        self.flow_english_words = merged.flow_english_words;
        self.japanese_words = merged.japanese_words;
        self.pending.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_file(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!("kayoiji-learning-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        directory.join("nested").join("learning.json")
    }

    #[test]
    fn latest_choice_comes_first_without_duplicates() {
        // Arrange
        let mut learning = Learning::in_memory();

        // Act
        learning.record_conversion("あめ", "雨").expect("recorded");
        learning.record_conversion("あめ", "飴").expect("recorded");
        learning.record_conversion("あめ", "雨").expect("recorded");

        // Assert
        assert_eq!(learning.conversions("あめ"), ["雨", "飴"]);
        assert!(learning.conversions("はれ").is_empty());
    }

    #[test]
    fn old_choices_are_dropped_beyond_the_limit() {
        let mut learning = Learning::in_memory();
        for index in 0..SURFACES_PER_READING + 3 {
            learning.record_conversion("よみ", &format!("表記{index}")).expect("recorded");
        }
        let surfaces = learning.conversions("よみ");
        assert_eq!(surfaces.len(), SURFACES_PER_READING);
        assert_eq!(surfaces[0], format!("表記{}", SURFACES_PER_READING + 2));
    }

    fn words(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(reading, surface)| (reading.to_string(), surface.to_string()))
            .collect()
    }

    #[test]
    fn the_latest_phrase_replaces_the_one_with_the_same_reading() {
        // Arrange
        let mut learning = Learning::in_memory();

        // Act
        learning.record_phrase(&words(&[("かよい", "通い"), ("じ", "時")])).expect("recorded");
        learning.record_phrase(&words(&[("に", "二"), ("じ", "時")])).expect("recorded");
        learning.record_phrase(&words(&[("かよい", "通い"), ("じ", "路")])).expect("recorded");

        // Assert
        assert_eq!(learning.phrase("かよいじ"), Some("通い路"));
        let phrases: Vec<_> = learning.phrases().collect();
        assert_eq!(phrases, [words(&[("かよい", "通い"), ("じ", "路")]), words(&[("に", "二"), ("じ", "時")])]);
    }

    #[test]
    fn the_oldest_phrase_is_dropped_beyond_the_limit() {
        // Arrange
        let mut learning = Learning::in_memory();
        for index in 0..PHRASES {
            let reading = format!("よみ{index}");
            learning.record_phrase(&words(&[(reading.as_str(), "語"), ("ご", "後")])).expect("recorded");
        }

        // Act
        let known = learning.phrase_to_drop("よみ3ご").map(str::to_string);
        let dropped = learning.phrase_to_drop("あたらしいご").map(str::to_string);
        learning.record_phrase(&words(&[("あたらしい", "新しい"), ("ご", "語")])).expect("recorded");

        // Assert
        // すでに覚えている読みを覚え直す時は、何も忘れない。
        assert_eq!(known, None);
        assert_eq!(dropped.as_deref(), Some("よみ0ご"));
        assert_eq!(learning.phrases().count(), PHRASES);
        assert_eq!(learning.phrase("よみ0ご"), None);
        assert_eq!(learning.phrase("よみ1ご"), Some("語後"));
    }

    #[test]
    fn forgetting_a_phrase_forgets_the_segments_committed_with_it() {
        // Arrange
        let mut learning = Learning::in_memory();
        learning.record_phrase(&words(&[("かよい", "通い"), ("じ", "路")])).expect("recorded");
        learning.record_conversion("かよいじを", "通い路を").expect("recorded");
        learning.record_conversion("かよいじが", "通地が").expect("recorded");

        // Act
        learning.forget_phrase("かよいじ").expect("forgotten");

        // Assert
        assert_eq!(learning.phrase("かよいじ"), None);
        assert!(learning.conversions("かよいじを").is_empty());
        // 語を使っていない選択は残す。
        assert_eq!(learning.conversions("かよいじが"), ["通地が"]);
    }

    #[test]
    fn conversions_are_found_by_a_part_of_the_reading_and_trimmed() {
        // Arrange
        let mut learning = Learning::in_memory();
        learning.record_conversion("かよいじを", "通地を").expect("recorded");
        learning.record_conversion("かよいじを", "通い路を").expect("recorded");
        learning.record_conversion("かよいじ", "通地").expect("recorded");
        learning.record_conversion("こうきゅう", "恒久").expect("recorded");

        // Act
        let readings = learning.readings_containing("かよいじ");
        learning
            .retain_conversions("かよいじを", |surface| surface.contains("通い路"))
            .expect("saved");

        // Assert
        assert_eq!(readings, ["かよいじを"]);
        assert_eq!(learning.conversions("かよいじを"), ["通い路を"]);
        assert_eq!(learning.conversions("こうきゅう"), ["恒久"]);
    }

    #[test]
    fn a_word_belongs_to_one_language_at_a_time() {
        // Arrange
        let mut learning = Learning::in_memory();

        // Act
        learning.record_language("nano", Language::Japanese, false).expect("recorded");
        learning.record_language("nano", Language::English, false).expect("recorded");

        // Assert
        let languages: Vec<(&str, Language, bool)> = learning.languages().collect();
        assert_eq!(languages, [("nano", Language::English, false)]);
    }

    #[test]
    fn learning_japanese_forgets_the_english_record_made_in_flow() {
        // Arrange
        let mut learning = Learning::in_memory();
        learning.record_language("made", Language::English, true).expect("recorded");
        assert_eq!(
            learning.languages().collect::<Vec<_>>(),
            [("made", Language::English, true)]
        );

        // Act
        learning.record_language("made", Language::Japanese, false).expect("recorded");
        learning.record_language("made", Language::English, false).expect("recorded");

        // Assert
        assert_eq!(
            learning.languages().collect::<Vec<_>>(),
            [("made", Language::English, false)]
        );
    }

    #[test]
    fn records_survive_a_reload() {
        // Arrange
        let path = temporary_file("reload");
        let mut learning = Learning::load(&path).expect("missing file is an empty record");
        learning.record_conversion("あめ", "飴").expect("saved");
        learning.record_phrase(&words(&[("かよい", "通い"), ("じ", "路")])).expect("saved");
        learning.record_language("sore", Language::Japanese, false).expect("saved");
        learning.record_language("made", Language::English, true).expect("saved");

        // Act
        let reloaded = Learning::load(&path).expect("saved file loads");

        // Assert
        assert_eq!(reloaded.conversions("あめ"), ["飴"]);
        assert_eq!(reloaded.phrase("かよいじ"), Some("通い路"));
        assert_eq!(reloaded.phrases().next(), Some(words(&[("かよい", "通い"), ("じ", "路")]).as_slice()));
        assert_eq!(
            reloaded.languages().collect::<Vec<_>>(),
            [("made", Language::English, true), ("sore", Language::Japanese, false)]
        );
    }

    #[test]
    fn a_broken_file_is_reported_instead_of_overwritten() {
        // Arrange
        let path = temporary_file("broken");
        fs::create_dir_all(path.parent().expect("has parent")).expect("directory");
        fs::write(&path, "{ not json").expect("written");

        // Act
        let result = Learning::load(&path);

        // Assert
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&path).expect("still there"), "{ not json");
    }

    #[test]
    fn a_broken_file_is_set_aside_and_recording_starts_over() {
        // Arrange
        let path = temporary_file("set-aside");
        fs::create_dir_all(path.parent().expect("has parent")).expect("directory");
        fs::write(&path, "{ not json").expect("written");

        // Act
        let (mut learning, error) = Learning::load_or_set_aside(&path);
        learning.record_conversion("あめ", "飴").expect("saved");

        // Assert
        assert!(error.is_some());
        assert_eq!(
            fs::read_to_string(path.with_extension("json.broken")).expect("kept"),
            "{ not json"
        );
        assert_eq!(Learning::load(&path).expect("new record").conversions("あめ"), ["飴"]);
    }

    #[test]
    fn records_from_two_processes_are_both_kept() {
        // Arrange: 同じファイルを、2 つのプロセスが別々に読み込んでいる
        let path = temporary_file("two-processes");
        let mut first = Learning::load(&path).expect("missing file is an empty record");
        let mut second = Learning::load(&path).expect("missing file is an empty record");

        // Act
        first.record_conversion("あめ", "飴").expect("saved");
        second.record_phrase(&words(&[("かよい", "通い"), ("じ", "路")])).expect("saved");
        second.record_language("made", Language::English, false).expect("saved");

        // Assert: 後から保存した側も、先に保存された分を消さない。読み直した分は、記録にも入る。
        let saved = Learning::load(&path).expect("saved file loads");
        assert_eq!(saved.conversions("あめ"), ["飴"]);
        assert_eq!(saved.phrase("かよいじ"), Some("通い路"));
        assert_eq!(second.conversions("あめ"), ["飴"]);
        assert!(first.phrase("かよいじ").is_none());
        // 変換の選択は記録から直接引くが、語と訂正は変換器と判定器に覚えさせ直す必要がある。
        assert!(!first.take_outside_changes());
        first.record_conversion("はれ", "晴れ").expect("saved");
        assert!(first.take_outside_changes());
        assert!(!first.take_outside_changes());
    }

    #[test]
    fn a_failed_save_is_retried_by_the_next_record() {
        // Arrange
        let path = temporary_file("retry");
        let directory = path.parent().expect("has parent").to_path_buf();
        let mut learning = Learning::load(&path).expect("missing file is an empty record");
        // 保存先のディレクトリと同じ名前のファイルがあると、ディレクトリを作れずに保存が失敗する。
        fs::create_dir_all(directory.parent().expect("has parent")).expect("directory");
        fs::write(&directory, "in the way").expect("written");
        assert!(learning.record_conversion("あめ", "飴").is_err());
        fs::remove_file(&directory).expect("removed");

        // Act
        // 同じ表記をもう一度選ぶ。記録の内容は変わらないが、書けていない分を保存する。
        learning.record_conversion("あめ", "飴").expect("saved");

        // Assert
        assert_eq!(Learning::load(&path).expect("saved file loads").conversions("あめ"), ["飴"]);
    }
}

//! 利用者の選択の記録。変換で選んだ表記と、英語・日本語の判定の訂正を覚える。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::rules::Language;

/// 1 つの読みについて覚えておく表記の数。
const SURFACES_PER_READING: usize = 8;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Learning {
    /// 読みごとの、選ばれた表記。新しく選んだものが先頭。
    #[serde(default)]
    conversions: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    english_words: BTreeSet<String>,
    /// `english_words` のうち、日本語の入力が続いている間に訂正されたもの。
    #[serde(default)]
    flow_english_words: BTreeSet<String>,
    #[serde(default)]
    japanese_words: BTreeSet<String>,
    #[serde(skip)]
    path: Option<PathBuf>,
    /// 保存に失敗して、ファイルに書けていない変更があるかどうか。次に記録する時に保存し直す。
    #[serde(skip)]
    unsaved: bool,
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
        let surfaces = self.conversions.entry(reading.to_string()).or_default();
        if surfaces.first().is_none_or(|first| first != surface) {
            surfaces.retain(|known| known != surface);
            surfaces.insert(0, surface.to_string());
            surfaces.truncate(SURFACES_PER_READING);
            self.unsaved = true;
        }
        self.save()
    }

    pub fn record_language(&mut self, word: &str, language: Language, in_flow: bool) -> io::Result<()> {
        let changed = match language {
            Language::English => {
                let forgotten = self.japanese_words.remove(word);
                let learned = self.english_words.insert(word.to_string());
                let learned_in_flow = in_flow && self.flow_english_words.insert(word.to_string());
                forgotten || learned || learned_in_flow
            }
            Language::Japanese => {
                let forgotten = self.english_words.remove(word);
                let forgotten_in_flow = self.flow_english_words.remove(word);
                let learned = self.japanese_words.insert(word.to_string());
                forgotten || forgotten_in_flow || learned
            }
        };
        self.unsaved |= changed;
        self.save()
    }

    /// 書けていない変更があれば保存する。
    fn save(&mut self) -> io::Result<()> {
        if !self.unsaved {
            return Ok(());
        }
        let Some(path) = &self.path else {
            self.unsaved = false;
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        // 書き込みの途中で終了しても、元のファイルが壊れないようにする。
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(self).expect("learning serializes"))?;
        fs::rename(&temporary, path)?;
        self.unsaved = false;
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
        learning.record_language("sore", Language::Japanese, false).expect("saved");
        learning.record_language("made", Language::English, true).expect("saved");

        // Act
        let reloaded = Learning::load(&path).expect("saved file loads");

        // Assert
        assert_eq!(reloaded.conversions("あめ"), ["飴"]);
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

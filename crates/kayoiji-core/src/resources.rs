//! ビルド済みの辞書からエンジンを組み立てる。

use std::fs;
use std::io;
use std::path::Path;
use std::sync::Arc;

use crate::converter::Converter;
use crate::detector::Detector;
use crate::dict::Dictionary;
use crate::engine::{Engine, EngineConfig};
use crate::english::EnglishLexicon;
use crate::learning::Learning;
use crate::rules::RuleSet;

pub const DICTIONARY_FILE: &str = "kayoiji.dict";
pub const ENGLISH_FILE: &str = "english.fst";
pub const SYMBOLS_FILE: &str = "symbols.dict";

fn with_path(path: &Path, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{}: {error}", path.display()))
}

/// 標準の判断データベースに、`user_rules` のファイルの規則を重ねる。ファイルが無ければ標準の規則だけを返す。
///
/// ファイルを読めない時は、標準の規則とその理由を返す。利用者が書き足すファイルなので、
/// 書き間違いで入力メソッドごと起動しなくなるのを避ける。
pub fn load_rules(user_rules: &Path) -> (RuleSet, Option<io::Error>) {
    let mut rules = RuleSet::standard();
    let text = match fs::read_to_string(user_rules) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return (rules, None),
        Err(error) => return (rules, Some(with_path(user_rules, error))),
    };
    match RuleSet::parse(&text) {
        Ok(user) => {
            rules.merge(user);
            (rules, None)
        }
        Err(error) => {
            let error = io::Error::new(io::ErrorKind::InvalidData, error);
            (rules, Some(with_path(user_rules, error)))
        }
    }
}

/// `data_dir` の辞書を開いてエンジンを作る。
pub fn open_engine(data_dir: &Path, rules: RuleSet, learning: Learning, config: EngineConfig) -> io::Result<Engine> {

    let dictionary_path = data_dir.join(DICTIONARY_FILE);
    let dictionary = Dictionary::open(&dictionary_path).map_err(|error| with_path(&dictionary_path, error))?;
    let english_path = data_dir.join(ENGLISH_FILE);
    let english = EnglishLexicon::open(&english_path).map_err(|error| with_path(&english_path, error))?;
    let symbols_path = data_dir.join(SYMBOLS_FILE);
    let symbols = Dictionary::open(&symbols_path).map_err(|error| with_path(&symbols_path, error))?;

    let detector = Detector::new(
        Converter::new(Arc::new(dictionary)).with_symbols(Arc::new(symbols)),
        english,
        rules,
        config.detector.clone(),
    );
    Ok(Engine::new(detector, learning, config))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict::DictionaryBuilder;
    use crate::engine::Key;
    use std::path::PathBuf;

    fn data_dir(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!("kayoiji-resources-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("directory");
        let mut builder = DictionaryBuilder::new(2, vec![0; 4], 1);
        builder.add("これ", 1, 1, 2500, "これ");
        fs::write(directory.join(DICTIONARY_FILE), builder.build()).expect("dictionary");
        let english = EnglishLexicon::build([("the".to_string(), 1_000_000)]);
        fs::write(directory.join(ENGLISH_FILE), english).expect("lexicon");
        let mut symbols = DictionaryBuilder::lookup_only();
        symbols.add("これ", 0, 0, 0, "☞");
        fs::write(directory.join(SYMBOLS_FILE), symbols.build()).expect("symbols");
        directory
    }

    fn typed_kore(engine: &mut Engine) -> String {
        for (index, c) in "kore".chars().enumerate() {
            engine.handle(Key::Char(c), index as u64 * 50);
        }
        engine.handle(Key::Space, 200).preedit
    }

    #[test]
    fn engine_opens_from_built_files() {
        // Arrange
        let directory = data_dir("open");

        // Act
        let mut engine = open_engine(&directory, RuleSet::standard(), Learning::in_memory(), EngineConfig::default())
            .expect("engine opens");

        // Assert
        assert_eq!(typed_kore(&mut engine), "これ");
        // 次の候補は、記号の辞書から来る。
        assert_eq!(engine.handle(Key::Space, 300).preedit, "☞");
    }

    #[test]
    fn user_rules_override_the_standard_ones() {
        // Arrange
        let directory = data_dir("rules");
        let rules = directory.join("rules.toml");
        fs::write(&rules, "[words]\nenglish = [\"kore\"]\n").expect("rules");

        // Act
        let (rules, error) = load_rules(&rules);
        let mut engine =
            open_engine(&directory, rules, Learning::in_memory(), EngineConfig::default()).expect("engine opens");

        // Assert
        assert!(error.is_none());
        // 利用者の規則で "kore" が英語になったので、空白は変換にならない。
        assert_eq!(typed_kore(&mut engine), "");
    }

    #[test]
    fn missing_user_rules_are_not_an_error() {
        let directory = data_dir("no-rules");
        let (_, error) = load_rules(&directory.join("rules.toml"));
        assert!(error.is_none());
    }

    #[test]
    fn broken_user_rules_fall_back_to_the_standard_rules() {
        // Arrange
        let directory = data_dir("broken-rules");
        let rules = directory.join("rules.toml");
        fs::write(&rules, "[words]\nenglish = [\"kore\"]\n[[rules]]\nsequence = 1").expect("rules");

        // Act
        let (rules, error) = load_rules(&rules);
        let mut engine =
            open_engine(&directory, rules, Learning::in_memory(), EngineConfig::default()).expect("engine opens");

        // Assert
        assert!(error.expect("reported").to_string().contains("rules.toml"));
        // 壊れたファイルの規則は一部だけ使うことはせず、標準の規則で動く。
        assert_eq!(typed_kore(&mut engine), "これ");
    }

    #[test]
    fn a_missing_dictionary_names_the_file_that_failed() {
        // Arrange
        let missing = data_dir("missing").join("missing");

        // Act
        let result = open_engine(&missing, RuleSet::standard(), Learning::in_memory(), EngineConfig::default());

        // Assert
        assert!(result.err().expect("fails").to_string().contains(DICTIONARY_FILE));
    }
}

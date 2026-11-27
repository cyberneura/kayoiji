//! 利用者の設定。設定ディレクトリの `settings.toml` に保存する。

use std::fs;
use std::io;
use std::path::Path;

use crate::engine::EngineConfig;
use serde::{Deserialize, Serialize};

/// 設定できる打鍵の間隔の範囲 (ミリ秒)。
const SEQUENCE_TIMEOUT_RANGE: std::ops::RangeInclusive<u64> = 50..=2000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub sequence_timeout_ms: u64,
    /// 英語か日本語かを判定しながら入力する。false なら、「かな」「英数」キーで切り替える。
    pub auto_switch: bool,
    /// 日本語入力で英語を打ち始めたら (大文字、ローマ字として読めない並び)、直接入力に切り替える。
    pub leave_japanese_for_english: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            sequence_timeout_ms: EngineConfig::default().sequence_timeout_ms,
            auto_switch: false,
            leave_japanese_for_english: false,
        }
    }
}

impl Settings {
    /// ファイルが無ければ既定値を返す。手で書き換えたファイルも読むので、値の範囲もここで確かめる。
    pub fn load(path: &Path) -> io::Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        let settings: Self =
            toml::from_str(&text).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        settings
            .validate()
            .map_err(|message| io::Error::new(io::ErrorKind::InvalidData, message))?;
        Ok(settings)
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        // 書き込みの途中で終了しても、元のファイルが壊れないようにする。
        let temporary = path.with_extension("toml.tmp");
        fs::write(&temporary, toml::to_string_pretty(self).expect("settings serialize"))?;
        fs::rename(&temporary, path)
    }

    pub fn validate(&self) -> Result<(), String> {
        if !SEQUENCE_TIMEOUT_RANGE.contains(&self.sequence_timeout_ms) {
            return Err(format!(
                "打鍵の間隔は {}〜{} ミリ秒で指定してください",
                SEQUENCE_TIMEOUT_RANGE.start(),
                SEQUENCE_TIMEOUT_RANGE.end()
            ));
        }
        Ok(())
    }

    pub fn engine_config(&self) -> EngineConfig {
        EngineConfig {
            sequence_timeout_ms: self.sequence_timeout_ms,
            auto_switch: self.auto_switch,
            leave_japanese_for_english: self.leave_japanese_for_english,
            ..EngineConfig::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_gives_defaults() {
        let path = std::env::temp_dir().join("kayoiji-settings-missing/settings.toml");
        let settings = Settings::load(&path).expect("defaults");
        assert_eq!(settings, Settings::default());
        assert_eq!(settings.sequence_timeout_ms, 200);
    }

    #[test]
    fn saved_settings_load_back() {
        // Arrange
        let path = std::env::temp_dir()
            .join(format!("kayoiji-settings-{}", std::process::id()))
            .join("settings.toml");
        let settings = Settings {
            sequence_timeout_ms: 350,
            auto_switch: true,
            leave_japanese_for_english: true,
        };

        // Act
        settings.save(&path).expect("saved");
        let loaded = Settings::load(&path).expect("loaded");

        // Assert
        assert_eq!(loaded, settings);
        assert_eq!(loaded.engine_config().sequence_timeout_ms, 350);
        assert!(loaded.engine_config().auto_switch);
        assert!(loaded.engine_config().leave_japanese_for_english);
    }

    #[test]
    fn a_file_with_a_value_outside_the_range_is_rejected() {
        // Arrange
        let path = std::env::temp_dir()
            .join(format!("kayoiji-settings-range-{}", std::process::id()))
            .join("settings.toml");
        fs::create_dir_all(path.parent().expect("has parent")).expect("directory");
        fs::write(&path, "sequence_timeout_ms = 600000\n").expect("written");

        // Act
        let result = Settings::load(&path);

        // Assert
        assert_eq!(result.expect_err("rejected").kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn empty_file_keeps_the_defaults() {
        let settings: Settings = toml::from_str("").expect("valid");
        assert_eq!(settings.sequence_timeout_ms, 200);
        assert!(!settings.auto_switch);
    }

    #[test]
    fn a_file_written_before_auto_switch_existed_turns_it_off() {
        let settings: Settings = toml::from_str("sequence_timeout_ms = 350\n").expect("valid");
        assert_eq!(settings.sequence_timeout_ms, 350);
        assert!(!settings.engine_config().auto_switch);
        assert!(!settings.engine_config().leave_japanese_for_english);
    }

    #[test]
    fn timeout_outside_the_range_is_rejected() {
        let too_short = Settings {
            sequence_timeout_ms: 10,
            ..Settings::default()
        };
        let too_long = Settings {
            sequence_timeout_ms: 5000,
            ..Settings::default()
        };
        assert!(too_short.validate().is_err());
        assert!(too_long.validate().is_err());
        assert!(Settings::default().validate().is_ok());
    }
}

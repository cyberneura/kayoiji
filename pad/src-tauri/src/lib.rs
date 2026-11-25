//! KayoijiPad: Kayoiji のエンジンを、入力メソッドとして入れずに試すためのテキストエリア。
//!
//! テキストエリアの打鍵をエンジンへ渡し、確定する文字列と未確定文字列を返す。
//! OS の入力メソッドは経由しない。

mod input_source;

use std::error::Error;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use kayoiji_core::engine::{Engine, EngineConfig, Key, Output, View};
use kayoiji_core::learning::Learning;
use kayoiji_core::resources::{load_rules, open_engine};
use serde::{Deserialize, Serialize};
use tauri::path::BaseDirectory;
use tauri::{App, Manager, State};

/// 辞書があるディレクトリを差し替える環境変数。開発時に使う。
const DATA_DIR_VARIABLE: &str = "KAYOIJI_DATA_DIR";
/// 入力メソッド本体の設定ディレクトリ。利用者が足した判断データベースを、ここから読む。
const INPUT_METHOD_IDENTIFIER: &str = "com.cyberneura.inputmethod.Kayoiji";

struct PadState {
    engine: Mutex<Engine>,
    /// 打鍵の時刻の基準。
    started: Instant,
}

impl PadState {
    fn now(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }
}

/// テキストエリアから届くキー。
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
enum PadKey {
    Char(char),
    Space,
    Enter,
    Backspace,
    Escape,
    Tab,
    Up,
    Down,
    Left,
    Right,
    ShiftLeft,
    ShiftRight,
    Henkan,
    Muhenkan,
    ForceJapanese,
    ForceEnglish,
    Katakana,
}

impl From<PadKey> for Key {
    fn from(key: PadKey) -> Self {
        match key {
            PadKey::Char(c) => Key::Char(c),
            PadKey::Space => Key::Space,
            PadKey::Enter => Key::Enter,
            PadKey::Backspace => Key::Backspace,
            PadKey::Escape => Key::Escape,
            PadKey::Tab => Key::Tab,
            PadKey::Up => Key::Up,
            PadKey::Down => Key::Down,
            PadKey::Left => Key::Left,
            PadKey::Right => Key::Right,
            PadKey::ShiftLeft => Key::ShiftLeft,
            PadKey::ShiftRight => Key::ShiftRight,
            PadKey::Henkan => Key::Henkan,
            PadKey::Muhenkan => Key::Muhenkan,
            PadKey::ForceJapanese => Key::ForceJapanese,
            PadKey::ForceEnglish => Key::ForceEnglish,
            PadKey::Katakana => Key::Katakana,
        }
    }
}

#[derive(Serialize)]
struct PadResponse {
    consumed: bool,
    delete_before: usize,
    commit: String,
    preedit: String,
    /// 未確定文字列の中で、候補を選んでいる文節の範囲 (先頭からの文字数と長さ)。
    focus: Option<(usize, usize)>,
    view: View,
    /// 判定中の単語を英語として確定するまでの時間。この時間だけ打鍵が無ければ `pad_timeout` を呼ぶ。
    timeout_ms: Option<u64>,
}

fn response(engine: &Engine, output: Output, now: u64) -> PadResponse {
    PadResponse {
        consumed: output.consumed,
        delete_before: output.reread.chars().count(),
        commit: output.commit,
        preedit: output.preedit,
        focus: output.focus,
        view: engine.view(),
        timeout_ms: engine.timeout_deadline().map(|deadline| deadline.saturating_sub(now)),
    }
}

#[tauri::command]
fn pad_key(state: State<'_, PadState>, key: PadKey) -> PadResponse {
    let now = state.now();
    let mut engine = state.engine.lock().expect("engine lock");
    let output = engine.handle(key.into(), now);
    response(&engine, output, now)
}

#[tauri::command]
fn pad_timeout(state: State<'_, PadState>) -> PadResponse {
    let now = state.now();
    let mut engine = state.engine.lock().expect("engine lock");
    let output = engine.handle_timeout(now);
    response(&engine, output, now)
}

/// 未確定文字列を、表示されているとおりに確定したことにする。カーソルを動かした時に呼ぶ。
/// `reason` は、画面側で確定のきっかけになった操作。意図しない確定の原因を調べるために記録する。
#[tauri::command]
fn pad_reset(state: State<'_, PadState>, reason: String) -> View {
    let mut engine = state.engine.lock().expect("engine lock");
    let committed = engine.commit_composition();
    if !committed.is_empty() {
        eprintln!("kayoiji-pad: committed {} characters ({reason})", committed.chars().count());
    }
    engine.view()
}

/// OS の入力メソッドが有効かどうか。有効な間は、テキストエリアの入力を OS に任せる。
#[tauri::command]
fn pad_system_input_method() -> bool {
    input_source::input_method_active()
}

fn data_dir(app: &App) -> tauri::Result<PathBuf> {
    match std::env::var_os(DATA_DIR_VARIABLE) {
        Some(directory) => Ok(PathBuf::from(directory)),
        None => app.path().resolve("data", BaseDirectory::Resource),
    }
}

fn setup(app: &mut App) -> Result<(), Box<dyn Error>> {
    let user_rules = app.path().config_dir()?.join(INPUT_METHOD_IDENTIFIER).join("rules.toml");
    let (rules, rules_error) = load_rules(&user_rules);
    if let Some(error) = rules_error {
        eprintln!("kayoiji-pad: using the standard rules: {error}");
    }
    // 試し打ちの結果を、入力メソッド本体の学習内容に混ぜない。
    let engine = open_engine(&data_dir(app)?, rules, Learning::in_memory(), EngineConfig::default())?;
    app.manage(PadState {
        engine: Mutex::new(engine),
        started: Instant::now(),
    });
    Ok(())
}

pub fn run() {
    tauri::Builder::default()
        .setup(setup)
        .invoke_handler(tauri::generate_handler![
            pad_key,
            pad_timeout,
            pad_reset,
            pad_system_input_method
        ])
        .run(tauri::generate_context!())
        .expect("error while running KayoijiPad");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_arrive_as_tagged_json() {
        // Arrange
        let json = r#"[{"type":"char","value":"a"},{"type":"space"},{"type":"shift_left"},{"type":"henkan"}]"#;

        // Act
        let keys: Vec<PadKey> = serde_json::from_str(json).expect("valid keys");
        let keys: Vec<Key> = keys.into_iter().map(Key::from).collect();

        // Assert
        assert_eq!(keys, [Key::Char('a'), Key::Space, Key::ShiftLeft, Key::Henkan]);
    }

    #[test]
    fn unknown_key_names_are_rejected() {
        assert!(serde_json::from_str::<PadKey>(r#"{"type":"other"}"#).is_err());
        assert!(serde_json::from_str::<PadKey>(r#"{"type":"char","value":"ab"}"#).is_err());
    }
}

//! エンジンを、入力先のテキストを模した文字列に対して動かす。テストと CLI から使う。

use crate::engine::{Engine, Key, Output};

/// `{wait}` で空ける時間。打鍵の間隔のしきい値より十分に長くする。
const WAIT_MS: u64 = 1000;

pub struct Simulator {
    pub engine: Engine,
    /// 入力先に見えているテキスト。確定済みのテキストに未確定文字列を続けたもの。
    pub text: String,
    /// 確定済みのテキスト。
    pub committed: String,
    /// 打鍵の間隔。
    pub interval_ms: u64,
    time_ms: u64,
}

impl Simulator {
    pub fn new(engine: Engine) -> Self {
        Self {
            engine,
            text: String::new(),
            committed: String::new(),
            interval_ms: 50,
            time_ms: 0,
        }
    }

    pub fn clear(&mut self) {
        self.engine.reset();
        self.text.clear();
        self.committed.clear();
    }

    pub fn wait(&mut self, duration_ms: u64) {
        self.advance(duration_ms);
    }

    pub fn press(&mut self, key: Key) {
        self.advance(self.interval_ms);
        let output = self.engine.handle(key, self.time_ms);
        self.apply(output, Some(key));
    }

    /// 時刻を進める。判定中の単語を確定する時刻を過ぎたら、タイマーが発火したものとして扱う。
    fn advance(&mut self, duration_ms: u64) {
        self.time_ms += duration_ms;
        if self.engine.timeout_deadline().is_some_and(|deadline| self.time_ms > deadline) {
            let output = self.engine.handle_timeout(self.time_ms);
            self.apply(output, None);
        }
    }

    fn apply(&mut self, output: Output, key: Option<Key>) {
        // 読み直す文字列は、いつも確定済みの文字列の末尾にある。
        let kept = self.committed.strip_suffix(output.reread.as_str()).expect("reread text precedes the caret");
        self.committed.truncate(kept.len());
        self.committed.push_str(&output.commit);
        if !output.consumed {
            match key {
                Some(Key::Char(c)) => self.committed.push(c),
                Some(Key::Space) => self.committed.push(' '),
                Some(Key::Enter) => self.committed.push('\n'),
                Some(Key::Tab) => self.committed.push('\t'),
                Some(Key::Backspace) => {
                    self.committed.pop();
                }
                _ => {}
            }
        }
        self.text = format!("{}{}", self.committed, output.preedit);
    }

    /// 打鍵の列を順に入力する。空白は変換・区切りのキー、改行は Enter として扱う。
    /// `{enter}` `{esc}` `{bs}` `{tab}` `{up}` `{down}` `{left}` `{right}` `{sleft}` `{sright}` (Shift + 矢印)
    /// `{henkan}` `{muhenkan}` `{ja}` `{en}` `{kata}` `{other}` で特殊キーを、`{wait}` で打鍵の間隔を空けることを表す。
    pub fn run(&mut self, script: &str) {
        let mut rest = script;
        while let Some(c) = rest.chars().next() {
            if c == '{' {
                if let Some(end) = rest.find('}') {
                    let name = &rest[1..end];
                    rest = &rest[end + 1..];
                    match special_key(name) {
                        Some(key) => self.press(key),
                        None if name == "wait" => self.wait(WAIT_MS),
                        None => panic!("unknown key name: {{{name}}}"),
                    }
                    continue;
                }
            }
            rest = &rest[c.len_utf8()..];
            self.press(match c {
                ' ' => Key::Space,
                '\n' => Key::Enter,
                other => Key::Char(other),
            });
        }
    }
}

fn special_key(name: &str) -> Option<Key> {
    Some(match name {
        "enter" => Key::Enter,
        "esc" => Key::Escape,
        "bs" => Key::Backspace,
        "tab" => Key::Tab,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "sleft" => Key::ShiftLeft,
        "sright" => Key::ShiftRight,
        "henkan" => Key::Henkan,
        "muhenkan" => Key::Muhenkan,
        "ja" => Key::ForceJapanese,
        "en" => Key::ForceEnglish,
        "kata" => Key::Katakana,
        "other" => Key::Other,
        _ => return None,
    })
}

//! 入力の状態機械。
//!
//! 打鍵を 1 つずつ受け取り、入力先へ確定する文字列と、未確定文字列 (preedit) を返す。
//!
//! 状態は 3 つ。
//! - `Direct`: 単語が英語か日本語かを判定している。判定がつくまでの英字は未確定文字列として持ち、
//!   英語と決まった時点で確定する。英語と決まった単語の残りの打鍵は、そのまま入力先へ通す。
//!   打鍵の間隔が空いたら、そこまでを確定し、次の打鍵から新しい入力として判定し直す。
//!   日本語の文には空白が無いので、空白を待たずに判定をやり直せる必要がある。
//! - `Composing`: 日本語と判定した後。打鍵をかなに変換して、未確定文字列として表示する。
//! - `Converting`: 変換候補を選んでいる。
//!
//! 日本語を確定した後は「日本語の入力が続いている」とみなし、次の単語も最初からかなで表示する
//! (仮の日本語入力)。その単語がローマ字として読めないか、英単語だと分かった時点で英字に戻す。
//!
//! `Direct` での判定と仮の日本語入力は、自動切り替え (`InputMode::Auto`) の時の動作。
//! 日本語入力 (`InputMode::Japanese`) では判定をせず、文字のキーはすべて `Composing` から始める。
//! 直接入力 (`InputMode::Direct`) の間は、入力モードを切り替えるキー以外の打鍵をそのまま入力先へ通す。

use std::io;

use serde::{Deserialize, Serialize};

use crate::converter::{self, Candidate};
use crate::detector::{Context, Detector, DetectorConfig, Verdict};
use crate::kana::to_katakana;
use crate::learning::Learning;
use crate::romaji::RomajiTable;
use crate::rules::Language;

/// 訂正として覚える英単語の長さの上限。長い文を取り消した時に、文全体を単語として覚えない。
const LEARNABLE_WORD_LEN: usize = 12;
/// 区切り直した範囲を 1 語として覚える時の、読みの長さの上限 (文字数)。前の文節を区切り直すと後ろの
/// 文節も変換し直されるので、文の大半が 1 つの範囲になることがある。それは語ではないので覚えない。
const PHRASE_MAX_CHARS: usize = 12;
/// 「かな」をこの時間 (ミリ秒) の内に 2 度押したら、英文チェックにする。
/// 「英数」の 2 度押しは使えない。「英数」では OS の英字のキーボード配列に切り替わるので、
/// 2 度目が入力メソッドに届かない。
pub const DOUBLE_PRESS_MS: u64 = 400;
/// 覚える語を作る、辞書の語の数の下限。辞書の 1 語を分けて選んだだけなら、辞書がすでに変換できる。
const PHRASE_MIN_WORDS: usize = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EngineConfig {
    /// 打鍵の間隔がこれ以内なら、ひと続きの入力とみなして判定を続ける。
    /// これを超えたら、その単語は英語として確定する。
    pub sequence_timeout_ms: u64,
    pub candidates_per_page: usize,
    /// 日本語の入力が続いている間に英単語をこの数だけ続けて入力したら、英語の入力に戻ったとみなす。
    /// 1 語で戻すと、文中に英単語を 1 つ挟んだだけで後続の助詞を日本語にできなくなる。
    pub english_words_to_leave_japanese: u32,
    /// 「かな」キーで入る入力モードを、自動切り替え (`InputMode::Auto`) にする。
    /// false なら日本語入力 (`InputMode::Japanese`)。
    pub auto_switch: bool,
    /// 日本語入力 (`InputMode::Japanese`) で英語を打ち始めたら、直接入力に切り替える。
    /// 英語を打ち始めたとみなすのは、大文字の英字と、ローマ字の規則に無い並び ("st", "pl")。
    /// "thi" (てぃ) のように規則にある並びと、その途中 ("th") は、日本語のままにする。
    pub leave_japanese_for_english: bool,
    pub detector: DetectorConfig,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            sequence_timeout_ms: 200,
            candidates_per_page: 9,
            english_words_to_leave_japanese: 2,
            auto_switch: false,
            leave_japanese_for_english: false,
            detector: DetectorConfig::default(),
        }
    }
}

impl EngineConfig {
    fn is_timed_out(&self, last_key_ms: Option<u64>, time_ms: u64) -> bool {
        last_key_ms.is_some_and(|last| time_ms.saturating_sub(last) > self.sequence_timeout_ms)
    }

    fn kana_key_mode(&self) -> InputMode {
        if self.auto_switch {
            InputMode::Auto
        } else {
            InputMode::Japanese
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// 文字を入力するキー。空白は `Space` で渡す。
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
    /// Shift + 左矢印。変換中の文節を縮める。
    ShiftLeft,
    /// Shift + 右矢印。変換中の文節を伸ばす。
    ShiftRight,
    /// 変換キー (JIS 配列の「かな」)。日本語入力か自動切り替えのうち、設定の側のモード
    /// (`EngineConfig::auto_switch`) に入る。すでにそのモードなら、日本語入力の時は何もしない。
    /// 自動切り替えの時は、かなを変換する。入力中でなければ、次の入力を日本語として始める。
    Henkan,
    /// 無変換キー (JIS 配列の「英数」)。直接入力にする。直接入力の時は何もしない。
    Muhenkan,
    /// 直前の単語、または入力中の単語を日本語として読み直す。
    ForceJapanese,
    /// 入力中の日本語を、打鍵したままの英字に戻す。
    ForceEnglish,
    Katakana,
    /// カーソル移動やショートカットなど、入力位置が変わりうる操作。
    Other,
}

/// 打鍵の結果として、入力先に対して行うこと。`reread`、`commit`、`preedit` の順に適用する。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Output {
    /// true なら、打鍵そのものは入力先へ渡さない。false なら、`commit` を書き込んだ後に渡す。
    pub consumed: bool,
    /// キャレットの手前から削除する、確定済みの文字列。直前の単語を日本語として読み直す時に使う。
    /// 利用者がキャレットを動かしていると、手前にあるのは別の文字列なので、入力先は一致を確かめてから消す。
    pub reread: String,
    /// 入力先に確定する文字列。
    pub commit: String,
    /// 適用後の未確定文字列。空なら未確定文字列は無い。
    pub preedit: String,
    /// 未確定文字列の中で、候補を選んでいる文節の範囲 (先頭からの文字数と長さ)。
    pub focus: Option<(usize, usize)>,
}

/// 状態遷移 1 回分の結果。未確定文字列は、遷移後の状態から求める。
#[derive(Debug, Default)]
struct Step {
    consumed: bool,
    reread: String,
    commit: String,
}

impl Step {
    fn pass() -> Self {
        Self::default()
    }

    fn consume() -> Self {
        Self {
            consumed: true,
            ..Self::default()
        }
    }

    fn commit(text: String) -> Self {
        Self {
            consumed: true,
            reread: String::new(),
            commit: text,
        }
    }

    /// `text` を確定してから、打鍵を入力先へ渡す。
    fn commit_and_pass(text: String) -> Self {
        Self {
            consumed: false,
            reread: String::new(),
            commit: text,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum InputMode {
    /// すべての打鍵を日本語として入力する。
    Japanese,
    /// 英文チェック。英文を未確定文字列として受け取り、確定するまで入力先に出さない。
    /// 受け取っている間の綴りの訂正や訳は、エンジンの外で作って表示する。
    English,
    /// 英語か日本語かを判定しながら入力する。
    Auto,
    /// 入力モードを切り替えるキー以外の打鍵を、そのまま入力先へ通す。
    Direct,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Direct,
    Composing,
    Converting,
    /// 英文チェックで、英文を受け取っている。`View::reading` がその英文。
    Checking,
}

/// 候補ウインドウに表示する内容。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct View {
    pub input_mode: InputMode,
    /// 次の入力を、判定せずに日本語として始めるかどうか。
    pub forced: bool,
    pub mode: Mode,
    /// 入力中のかな。変換中は、候補を選んでいる文節の読み。
    pub reading: String,
    /// 表示中のページの候補。
    pub candidates: Vec<String>,
    /// `candidates` の中で選択されている位置。
    pub selected: Option<usize>,
    pub page: usize,
    pub page_count: usize,
}

#[derive(Debug)]
struct Direct {
    /// 直前の区切りから続いている英字。
    word: String,
    /// `word` がまだ確定されておらず、未確定文字列として表示されている。
    pending: bool,
    /// この単語の判定がまだ決まっていなければ true。
    open: bool,
    /// 直前の区切りの時点での `open`。単語を全部消した時に戻す。
    open_at_boundary: bool,
    last_key_ms: Option<u64>,
    /// 英語として確定した直前の単語。`ForceJapanese` で読み直す対象。
    recent: Option<Recent>,
    /// 日本語を確定した直後で、まだ何も打鍵していない。
    after_commit: bool,
}

impl Direct {
    fn at_boundary() -> Self {
        Self {
            word: String::new(),
            pending: false,
            open: true,
            open_at_boundary: true,
            last_key_ms: None,
            recent: None,
            after_commit: false,
        }
    }

    fn after_commit() -> Self {
        Self {
            after_commit: true,
            ..Self::at_boundary()
        }
    }

    /// 単語を英語と決める。未確定だった場合は、確定する文字列を返す。
    fn close_word(&mut self) -> String {
        self.open = false;
        if !std::mem::take(&mut self.pending) {
            return String::new();
        }
        self.word.clone()
    }

    /// 打鍵の間隔が空いた。ここまでの単語を英語として終え、次の打鍵から新しく判定する。
    /// 未確定だった場合は、確定する文字列を返す。
    fn break_sequence(&mut self) -> String {
        let commit = self.close_word();
        self.open = true;
        self.open_at_boundary = true;
        if !self.word.is_empty() {
            self.recent = Some(Recent {
                raw: std::mem::take(&mut self.word),
                trailing: 0,
            });
        }
        commit
    }

    /// 日本語として読み直す単語のうち、入力先に確定済みの文字列。判定中の単語は未確定なので含めない。
    fn committed_reread(&self) -> String {
        match (&self.recent, self.word.is_empty()) {
            (_, false) if self.pending => String::new(),
            (_, false) => self.word.clone(),
            (Some(recent), true) => format!("{}{}", recent.raw, " ".repeat(recent.trailing)),
            (None, true) => String::new(),
        }
    }

    fn end_word(&mut self, open: bool) {
        self.word.clear();
        self.pending = false;
        self.open = open;
        self.open_at_boundary = open;
        self.recent = None;
        self.after_commit = false;
    }
}

#[derive(Debug)]
struct Recent {
    raw: String,
    /// 単語の後ろに入力された空白の数。
    trailing: usize,
}

#[derive(Debug)]
struct ConvertingSegment {
    reading: String,
    candidates: Vec<Candidate>,
    selected: usize,
}

impl ConvertingSegment {
    fn surface(&self) -> &str {
        &self.candidates[self.selected].surface
    }
}

#[derive(Debug)]
struct Converting {
    /// かなの元になった打鍵。
    raw: String,
    /// 仮の日本語入力から変換を始めたかどうか。
    tentative: bool,
    /// 文節。それぞれが 1 つ以上の候補を持つ。
    segments: Vec<ConvertingSegment>,
    /// 候補を選んでいる文節。
    focus: usize,
    /// 変換を始めた時の、文節の終わりの位置 (読みの先頭からの文字数)。
    /// 利用者が区切り直した範囲を知るために覚えておく。カタカナにする変換は、文節に区切らずに
    /// 始めるので空にする。そこから区切り直しても、変換器の区切りを直したことにはならない。
    converted_ends: Vec<usize>,
}

impl Converting {
    fn display(&self) -> String {
        self.segments.iter().map(ConvertingSegment::surface).collect()
    }

    fn focused(&mut self) -> &mut ConvertingSegment {
        &mut self.segments[self.focus]
    }

    /// 利用者が区切り直した範囲を返す。範囲は、文節ごとの読みと表記の並びと、変換を始めた時の
    /// 文節をまたいでまとめたかどうか。
    ///
    /// 変換を始めた時の区切りと今の区切りが一致する位置で全体を分け、その間の区切りが変わっている
    /// 部分を 1 つの範囲にする。「かよいじ」を「かよい」「じ」に分けて確定したなら、「かよいじ」が範囲になる。
    fn regrouped(&self) -> Vec<(Vec<(String, String)>, bool)> {
        let mut groups = Vec::new();
        let mut group_start = 0;
        let mut begin = 0;
        let mut end = 0;
        for (index, segment) in self.segments.iter().enumerate() {
            end += segment.reading.chars().count();
            if !self.converted_ends.contains(&end) {
                continue;
            }
            let group = &self.segments[group_start..=index];
            let merged = self.converted_ends.iter().any(|&converted| begin < converted && converted < end);
            if group.len() > 1 || merged {
                groups.push((group, merged));
            }
            group_start = index + 1;
            begin = end;
        }
        groups
            .into_iter()
            .map(|(group, merged)| {
                let segments = group
                    .iter()
                    .map(|segment| (segment.reading.clone(), segment.surface().to_string()))
                    .collect();
                (segments, merged)
            })
            .collect()
    }

    /// 候補を選んでいる文節の、表示の中での範囲 (先頭からの文字数と長さ)。
    fn focus_range(&self) -> (usize, usize) {
        let start = self.segments[..self.focus]
            .iter()
            .map(|segment| segment.surface().chars().count())
            .sum();
        (start, self.segments[self.focus].surface().chars().count())
    }
}

#[derive(Debug)]
enum State {
    Direct(Direct),
    Composing {
        raw: String,
        /// 判定を経ずに始めた、仮の日本語入力。英語だと分かったら英字に戻す。
        tentative: bool,
    },
    Converting(Converting),
}

pub struct Engine {
    detector: Detector,
    learning: Learning,
    config: EngineConfig,
    state: State,
    /// 日本語を確定した後で、まだ英語の入力に戻っていない。
    japanese_flow: bool,
    /// 直前から続けて入力された英単語の数。
    english_streak: u32,
    input_mode: InputMode,
    /// 次の入力を、判定せずに日本語として始める。
    force_next: bool,
    last_error: Option<io::Error>,
    /// 英文チェックで受け取っている英文。
    english: String,
    /// 最後に「かな」が押された時刻。2 度押しを見分ける。
    last_henkan_ms: Option<u64>,
}

impl Engine {
    pub fn new(mut detector: Detector, learning: Learning, config: EngineConfig) -> Self {
        detector.set_config(config.detector.clone());
        for (word, language, in_flow) in learning.languages() {
            detector.learn(word, language, in_flow);
        }
        for words in learning.phrases() {
            detector.converter_mut().learn_phrase(words);
        }
        Self {
            detector,
            learning,
            state: State::Direct(Direct::at_boundary()),
            japanese_flow: false,
            english_streak: 0,
            input_mode: config.kana_key_mode(),
            force_next: false,
            last_error: None,
            english: String::new(),
            last_henkan_ms: None,
            config,
        }
    }

    /// 設定を差し替える。「かな」キーで入る入力モードが変わり、今がその側のモードなら、
    /// 新しいモードへ切り替える。戻り値は、切り替えに伴って入力先に対して行うこと。
    pub fn set_config(&mut self, config: EngineConfig) -> Output {
        self.detector.set_config(config.detector.clone());
        self.config = config;
        // 直接入力と英文チェックは、「かな」キーで入るモードが変わっても、そのまま続ける。
        if matches!(self.input_mode, InputMode::Direct | InputMode::English) {
            return self.output(Step::consume());
        }
        self.set_input_mode(self.config.kana_key_mode())
    }

    pub fn detector(&self) -> &Detector {
        &self.detector
    }

    /// 学習内容の保存に失敗していれば、その原因を取り出す。
    pub fn take_error(&mut self) -> Option<io::Error> {
        self.last_error.take()
    }

    /// 未確定文字列を捨てて、最初の状態に戻す。入力モードは変えない。
    pub fn reset(&mut self) {
        self.state = State::Direct(Direct::at_boundary());
        self.japanese_flow = false;
        self.english_streak = 0;
        self.force_next = false;
        self.english.clear();
    }

    pub fn input_mode(&self) -> InputMode {
        self.input_mode
    }

    /// 「かな」キーで入る入力モード。
    pub fn kana_key_mode(&self) -> InputMode {
        self.config.kana_key_mode()
    }

    /// `Key::ForceJapanese` を渡すと入力先から消すことになる、確定済みの文字列 (`Output::reread`)。
    ///
    /// 読み直しは単語を日本語として覚えるので、入力先は打鍵を渡す前にこの文字列がキャレットの
    /// 手前にあることを確かめる。無ければ、打鍵を渡さない。
    pub fn reread_target(&self) -> String {
        match &self.state {
            State::Direct(direct) if self.input_mode == InputMode::Auto => direct.committed_reread(),
            _ => String::new(),
        }
    }

    /// 入力モードを切り替える。入力中の文字列は確定する。
    pub fn set_input_mode(&mut self, mode: InputMode) -> Output {
        let mut step = Step::consume();
        if mode == self.input_mode {
            return self.output(step);
        }
        step.commit = match &self.state {
            // 自動切り替えから直接入力にするのは、日本語と判定されたのを取り消す操作でもある。
            // 打鍵したままの英字で確定する。
            State::Composing { raw, .. } if self.leaves_auto_for_direct(mode) => raw.clone(),
            State::Converting(converting) if self.leaves_auto_for_direct(mode) => converting.raw.clone(),
            _ => self.preedit(),
        };
        self.reset();
        self.input_mode = mode;
        self.output(step)
    }

    fn leaves_auto_for_direct(&self, mode: InputMode) -> bool {
        self.input_mode == InputMode::Auto && mode == InputMode::Direct
    }

    /// 未確定文字列を、表示されているとおりに確定する。入力先が切り替わる時に呼ぶ。
    /// 戻り値は、入力先に確定する文字列。
    pub fn commit_composition(&mut self) -> String {
        let text = self.preedit();
        self.reset();
        text
    }

    /// 現在の未確定文字列。
    pub fn preedit(&self) -> String {
        if self.input_mode == InputMode::English {
            return self.english.clone();
        }
        match &self.state {
            State::Direct(direct) if direct.pending => direct.word.clone(),
            State::Direct(_) => String::new(),
            State::Composing { raw, .. } => RomajiTable::global().parse(raw).display(),
            State::Converting(converting) => converting.display(),
        }
    }

    pub fn view(&self) -> View {
        let idle = View {
            input_mode: self.input_mode,
            forced: self.force_next,
            mode: Mode::Direct,
            reading: String::new(),
            candidates: Vec::new(),
            selected: None,
            page: 0,
            page_count: 0,
        };
        if self.input_mode == InputMode::English {
            return View {
                mode: Mode::Checking,
                reading: self.english.clone(),
                ..idle
            };
        }
        match &self.state {
            State::Direct(_) => idle,
            State::Composing { raw, .. } => View {
                mode: Mode::Composing,
                reading: RomajiTable::global().parse(raw).display(),
                ..idle
            },
            State::Converting(converting) => {
                let segment = &converting.segments[converting.focus];
                let per_page = self.config.candidates_per_page.max(1);
                let page = segment.selected / per_page;
                View {
                    mode: Mode::Converting,
                    reading: segment.reading.clone(),
                    candidates: segment
                        .candidates
                        .iter()
                        .skip(page * per_page)
                        .take(per_page)
                        .map(|candidate| candidate.surface.clone())
                        .collect(),
                    selected: Some(segment.selected % per_page),
                    page,
                    page_count: segment.candidates.len().div_ceil(per_page),
                    ..idle
                }
            }
        }
    }

    /// 判定中の単語を英語として確定する時刻。判定中の単語が無ければ `None`。
    /// この時刻を過ぎても打鍵が無ければ、`handle_timeout` を呼ぶ。
    pub fn timeout_deadline(&self) -> Option<u64> {
        match &self.state {
            State::Direct(direct) if direct.pending => {
                direct.last_key_ms.map(|last| last + self.config.sequence_timeout_ms)
            }
            _ => None,
        }
    }

    /// 打鍵の間隔が空いた時に呼ぶ。判定中の単語があれば、英語として確定する。
    pub fn handle_timeout(&mut self, time_ms: u64) -> Output {
        let mut step = Step::consume();
        let state = std::mem::replace(&mut self.state, State::Direct(Direct::at_boundary()));
        self.state = match state {
            State::Direct(mut direct) => {
                step.commit = self.break_if_timed_out(&mut direct, time_ms);
                State::Direct(direct)
            }
            other => other,
        };
        self.output(step)
    }

    /// 打鍵の間隔が空いていれば、ここまでの単語を終える。確定する文字列を返す。
    fn break_if_timed_out(&mut self, direct: &mut Direct, time_ms: u64) -> String {
        if !self.config.is_timed_out(direct.last_key_ms, time_ms) {
            return String::new();
        }
        direct.last_key_ms = None;
        if !direct.word.is_empty() {
            self.count_english_word();
        }
        direct.break_sequence()
    }

    /// 打鍵を 1 つ処理する。`time_ms` は単調増加する時刻。
    pub fn handle(&mut self, key: Key, time_ms: u64) -> Output {
        // 2 度押しを数えるのは、何も入力していない時の「かな」だけ。自動切り替えでは、入力中の「かな」は
        // 変換と次の候補に使うので、続けて押すのが普通の操作になる。
        let counts = key == Key::Henkan && self.input_mode != InputMode::English && self.preedit().is_empty();
        if !counts {
            // 間にほかのキーを挟んだ「かな」は、続けて 2 度押したことにしない。
            self.last_henkan_ms = None;
        } else if self.pressed_henkan_twice(time_ms) {
            return self.set_input_mode(InputMode::English);
        }
        if key == Key::Muhenkan {
            // 直接入力の時は、何も変えない。入力先へ通すと、このキーで空白を入力する入力先がある。
            return self.set_input_mode(InputMode::Direct);
        }
        if self.input_mode == InputMode::Direct {
            return match key {
                Key::Henkan => self.set_input_mode(self.config.kana_key_mode()),
                _ => Output::default(),
            };
        }
        if self.input_mode == InputMode::English {
            return self.handle_english(key);
        }
        if key == Key::Henkan && self.input_mode != self.config.kana_key_mode() {
            // 入力ソースの一覧で、設定と違う側のモードが選ばれている。「かな」は設定の側のモードに入る。
            return self.set_input_mode(self.config.kana_key_mode());
        }
        if self.input_mode == InputMode::Japanese && key == Key::Henkan {
            // すでに日本語入力なので、何も変えない。
            return self.output(Step::consume());
        }
        if let Some(output) = self.leave_japanese_for_english(key) {
            return output;
        }
        let state = std::mem::replace(&mut self.state, State::Direct(Direct::at_boundary()));
        let step = match state {
            State::Direct(_) if self.input_mode == InputMode::Japanese => self.japanese_idle_key(key),
            State::Direct(direct) => self.handle_direct(direct, key, time_ms),
            State::Composing { raw, tentative } => self.handle_composing(raw, tentative, key, time_ms),
            State::Converting(converting) => self.handle_converting(converting, key, time_ms),
        };
        self.output(step)
    }

    /// 「かな」が押されたことを記録して、続けて 2 度押されたかどうかを返す。
    fn pressed_henkan_twice(&mut self, time_ms: u64) -> bool {
        let twice = self
            .last_henkan_ms
            .is_some_and(|last| time_ms.saturating_sub(last) <= DOUBLE_PRESS_MS);
        // 3 度目を、次の 2 度押しの 2 度目として数えない。
        self.last_henkan_ms = (!twice).then_some(time_ms);
        twice
    }

    /// エンジンに渡さなかった「かな」を、2 度押しの 1 度目として数える。OS が「かな」で入力メソッドを
    /// 選び直した直後の打鍵は、切り替えの一部としてエンジンに渡さない。
    pub fn mark_henkan(&mut self, time_ms: u64) {
        self.last_henkan_ms = Some(time_ms);
    }

    /// 英文チェックの打鍵。文字は英文に足し、Enter で確定する。英文が空の間は、文字以外の
    /// キーを入力先に通す。
    fn handle_english(&mut self, key: Key) -> Output {
        let step = match key {
            Key::Henkan => return self.set_input_mode(self.config.kana_key_mode()),
            Key::Char(c) => {
                self.english.push(c);
                Step::consume()
            }
            // 英文が空の間に Esc を押したら、英文チェックをやめる。
            Key::Escape if self.english.is_empty() => return self.set_input_mode(InputMode::Direct),
            _ if self.english.is_empty() => Step::pass(),
            Key::Space => {
                self.english.push(' ');
                Step::consume()
            }
            Key::Backspace => {
                self.english.pop();
                Step::consume()
            }
            Key::Enter => Step::commit(std::mem::take(&mut self.english)),
            Key::Escape => {
                self.english.clear();
                Step::consume()
            }
            // Option や Command を付けた打鍵は、英文を確定してから入力先に通す。
            Key::Other => Step::commit_and_pass(std::mem::take(&mut self.english)),
            // 英文を受け取っている間は、矢印などを入力先に通さない。通すと、未確定文字列を
            // 表示したままキャレットが動く。
            _ => Step::consume(),
        };
        self.output(step)
    }

    /// 英文チェックで受け取っている英文を、訂正した文に置き換える。
    /// 英文を受け取っていなければ何もせず、打鍵を消費しなかったことにして返す。
    pub fn replace_english(&mut self, text: &str) -> Output {
        if self.input_mode != InputMode::English || self.english.is_empty() {
            return self.output(Step::pass());
        }
        self.english = text.to_string();
        self.output(Step::consume())
    }

    fn output(&self, step: Step) -> Output {
        Output {
            consumed: step.consumed,
            reread: step.reread,
            commit: step.commit,
            preedit: self.preedit(),
            focus: match &self.state {
                State::Converting(converting) => Some(converting.focus_range()),
                _ => None,
            },
        }
    }

    fn context(&self) -> Context {
        if self.japanese_flow {
            Context::AfterJapanese
        } else if self.english_streak > 0 {
            Context::AfterEnglish
        } else {
            Context::Neutral
        }
    }

    fn count_english_word(&mut self) {
        self.english_streak += 1;
        if self.english_streak >= self.config.english_words_to_leave_japanese {
            self.japanese_flow = false;
        }
    }

    /// 日本語入力で英語を打ち始めたら、入力中の文字列を確定して直接入力にする (設定で有効な時)。
    /// 英語を打ち始めていなければ `None`。
    ///
    /// 「英数」を押した時と同じく、かなは表示のとおりに確定する。違うのは、打った文字の扱い。
    /// 大文字と記号は、直接入力にした後の打鍵として入力先に通す。ローマ字として読めない並びは、
    /// 読める所までをかなで、残りを打ったままの英字で確定する。
    /// 何も入力していない時のスペースも、直接入力にしてから入力先に通す。
    /// 日本語の文の中でスペースを打つのは、たいてい英語の前だから。
    fn leave_japanese_for_english(&mut self, key: Key) -> Option<Output> {
        if self.input_mode != InputMode::Japanese || !self.config.leave_japanese_for_english {
            return None;
        }
        let c = match key {
            Key::Char(c) => c,
            // 入力中のスペースは変換に使う。
            Key::Space if self.preedit().is_empty() => ' ',
            _ => return None,
        };
        let completes_rule = match &self.state {
            State::Composing { raw, .. } => completes_rule(raw, c),
            _ => false,
        };
        let step = if c == ' ' || c.is_ascii_uppercase() || (leads_english(c) && !completes_rule) {
            let commit = match std::mem::replace(&mut self.state, State::Direct(Direct::at_boundary())) {
                State::Converting(converting) => self.commit(converting).commit,
                state => {
                    self.state = state;
                    self.preedit()
                }
            };
            Step::commit_and_pass(commit)
        } else {
            let State::Composing { raw, .. } = &self.state else {
                return None;
            };
            let mut typed = raw.clone();
            typed.push(c);
            // 1 文字だけでは決めない。"q" や "x" のように、単独では読めない打鍵で始まるかながある。
            let word = typed.rsplit(|c: char| !c.is_ascii_alphabetic()).next().unwrap_or_default();
            if !c.is_ascii_lowercase() || word.len() < 2 || !RomajiTable::global().parse(word).has_unreadable() {
                return None;
            }
            let (kana, letters) = RomajiTable::global().parse(&typed).split_at_unreadable();
            Step::commit(kana + &letters)
        };
        self.reset();
        self.input_mode = InputMode::Direct;
        Some(self.output(step))
    }

    /// 日本語入力で、入力中の文字列が無い時の打鍵。文字のキーは、記号も含めて日本語の入力を始める。
    /// それ以外のキーは入力先のもの。
    fn japanese_idle_key(&mut self, key: Key) -> Step {
        let Key::Char(c) = key else {
            return Step::pass();
        };
        self.state = State::Composing {
            raw: c.to_string(),
            tentative: false,
        };
        Step::consume()
    }

    fn handle_direct(&mut self, mut direct: Direct, key: Key, time_ms: u64) -> Step {
        // タイマーより先に次の打鍵が届いた場合も、間隔が空いていれば単語を終える。
        let timed_out_word = self.break_if_timed_out(&mut direct, time_ms);
        let mut step = self.direct_key(direct, key, time_ms);
        if !timed_out_word.is_empty() {
            if let Some(rest) = step.reread.strip_suffix(timed_out_word.as_str()) {
                // たった今確定するはずだった単語を読み直す。確定も削除もしなかったことにする。
                step.reread = rest.to_string();
                return step;
            }
        }
        step.commit.insert_str(0, &timed_out_word);
        step
    }

    fn direct_key(&mut self, mut direct: Direct, key: Key, time_ms: u64) -> Step {
        match key {
            // 変換キーで日本語を指定された後の入力は、記号も含めて判定せずに日本語として始める。
            Key::Char(c) if self.force_next && !direct.pending => {
                self.force_next = false;
                self.state = State::Composing {
                    raw: c.to_string(),
                    tentative: false,
                };
                Step::consume()
            }
            Key::Char(c)
                if c.is_ascii_lowercase() && self.japanese_flow && direct.open && direct.word.is_empty() =>
            {
                self.state = State::Composing {
                    raw: c.to_string(),
                    tentative: true,
                };
                Step::consume()
            }
            Key::Char(c) if extends_word(&direct, c) => {
                direct.recent = None;
                direct.after_commit = false;
                direct.word.push(c);
                direct.last_key_ms = Some(time_ms);
                if !direct.open {
                    self.state = State::Direct(direct);
                    return Step::pass();
                }
                match self.detector.evaluate_prefix(&direct.word, self.context()) {
                    Verdict::Japanese => {
                        self.state = State::Composing {
                            raw: direct.word,
                            tentative: false,
                        };
                        Step::consume()
                    }
                    Verdict::English => {
                        direct.pending = true;
                        let word = direct.close_word();
                        self.state = State::Direct(direct);
                        Step::commit(word)
                    }
                    Verdict::Undetermined => {
                        direct.pending = true;
                        self.state = State::Direct(direct);
                        Step::consume()
                    }
                }
            }
            Key::Char(c) if c.is_ascii_alphabetic() => {
                // 大文字を含む単語は英語。
                direct.recent = None;
                direct.after_commit = false;
                direct.word.push(c);
                direct.last_key_ms = Some(time_ms);
                let was_pending = direct.pending;
                let word = direct.close_word();
                self.state = State::Direct(direct);
                if was_pending {
                    Step::commit(word)
                } else {
                    Step::pass()
                }
            }
            Key::Space => {
                if self.word_is_japanese(&direct) {
                    // 空白は変換の指示として受け取る。
                    return self.begin_conversion(direct.word, false, false);
                }
                let commit = direct.close_word();
                if direct.word.is_empty() {
                    if let Some(recent) = &mut direct.recent {
                        recent.trailing += 1;
                    }
                } else {
                    self.count_english_word();
                    direct.recent = Some(Recent {
                        raw: std::mem::take(&mut direct.word),
                        trailing: 1,
                    });
                }
                direct.open = true;
                direct.open_at_boundary = true;
                direct.after_commit = false;
                direct.last_key_ms = Some(time_ms);
                self.state = State::Direct(direct);
                Step::commit_and_pass(commit)
            }
            // 日本語を確定した直後の句読点は「、」「。」にする。
            Key::Char(c @ (',' | '.')) if direct.after_commit => {
                self.state = State::Direct(direct);
                Step::commit(japanese_punctuation(c).to_string())
            }
            Key::Char(c @ (',' | '.')) if self.word_is_japanese(&direct) => {
                let mut raw = direct.word;
                raw.push(c);
                self.state = State::Composing { raw, tentative: false };
                Step::consume()
            }
            Key::Char(c) => {
                // 記号の後ろは、パス・識別子・URL の続きであることが多い。空白か開き括弧が来るまで
                // 判定しない。単語に続かない数字は、「3つ」のように日本語が続くことがあるので区切らない。
                let open = if c.is_ascii_digit() && direct.word.is_empty() {
                    direct.open
                } else {
                    matches!(c, '(' | '[' | '{' | '"' | '\'' | '<')
                };
                let commit = direct.close_word();
                if !direct.word.is_empty() {
                    self.count_english_word();
                }
                direct.end_word(open);
                direct.last_key_ms = Some(time_ms);
                self.state = State::Direct(direct);
                Step::commit_and_pass(commit)
            }
            Key::Enter | Key::Tab => {
                let commit = direct.close_word();
                if !direct.word.is_empty() {
                    self.count_english_word();
                }
                direct.end_word(true);
                self.state = State::Direct(direct);
                Step::commit_and_pass(commit)
            }
            Key::Backspace => {
                direct.recent = None;
                direct.after_commit = false;
                if direct.pending {
                    direct.word.pop();
                    direct.pending = !direct.word.is_empty();
                    self.state = State::Direct(direct);
                    return Step::consume();
                }
                direct.word.pop();
                if direct.word.is_empty() {
                    direct.open = direct.open_at_boundary;
                }
                direct.last_key_ms = Some(time_ms);
                self.state = State::Direct(direct);
                Step::pass()
            }
            Key::ForceJapanese => self.force_japanese(direct),
            Key::Henkan if direct.pending => {
                self.state = State::Composing {
                    raw: direct.word,
                    tentative: false,
                };
                Step::consume()
            }
            Key::Henkan => {
                self.force_next = true;
                self.state = State::Direct(direct);
                Step::consume()
            }
            Key::Katakana if direct.pending => self.begin_conversion(direct.word, false, true),
            Key::Escape | Key::ForceEnglish if direct.pending => {
                let word = direct.close_word();
                self.state = State::Direct(direct);
                Step::commit(word)
            }
            Key::ForceEnglish | Key::Katakana | Key::Muhenkan => {
                self.state = State::Direct(direct);
                Step::pass()
            }
            Key::Escape
            | Key::Up
            | Key::Down
            | Key::Left
            | Key::Right
            | Key::ShiftLeft
            | Key::ShiftRight
            | Key::Other => {
                let commit = direct.close_word();
                self.reset();
                Step::commit_and_pass(commit)
            }
        }
    }

    /// 英字に戻す操作を、単語の言語の訂正として覚えるかどうか。日本語入力では、利用者は言語を
    /// 自分で選んでいる。判定を直しているのではないので、覚えない。
    fn learns_language(&self) -> bool {
        self.input_mode == InputMode::Auto
    }

    fn word_is_japanese(&self, direct: &Direct) -> bool {
        direct.pending && self.detector.evaluate_word(&direct.word, self.context()) == Verdict::Japanese
    }

    fn force_japanese(&mut self, direct: Direct) -> Step {
        // 確定済みの単語は、入力先から消してから読み直す。
        let reread = direct.committed_reread();
        let raw = match (&direct.recent, direct.word.is_empty()) {
            (_, false) if direct.pending => direct.word.clone(),
            (_, false) => direct.word.to_lowercase(),
            (Some(recent), true) => recent.raw.to_lowercase(),
            (None, true) => {
                self.state = State::Direct(direct);
                return Step::pass();
            }
        };
        if RomajiTable::global().parse(&raw).converted_count() == 0 {
            self.state = State::Direct(direct);
            return Step::pass();
        }
        self.learn_language(&raw, Language::Japanese, false);
        self.state = State::Composing { raw, tentative: false };
        Step {
            consumed: true,
            reread,
            commit: String::new(),
        }
    }

    fn handle_composing(&mut self, mut raw: String, tentative: bool, key: Key, time_ms: u64) -> Step {
        // 仮の日本語入力で、単語の区切りになる文字が来た。単語が英語なら英字に戻す。
        let ends_word = match key {
            Key::Space => true,
            Key::Char(c) => !c.is_ascii_alphabetic() && !continues_romaji(&raw, c),
            _ => false,
        };
        if tentative
            && ends_word
            && is_word(&raw)
            && self.detector.evaluate_word(&raw, Context::AfterJapanese) == Verdict::English
        {
            return self.revert_and_pass(raw, key, time_ms);
        }
        match key {
            Key::Char(c) => {
                raw.push(c);
                // ローマ字として読めない英字が混ざった時点で、英単語だと分かる。
                let is_english = tentative
                    && raw.bytes().all(|b| b.is_ascii_alphabetic())
                    && RomajiTable::global()
                        .parse(&raw)
                        .segments
                        .iter()
                        .any(|segment| !segment.converted);
                if is_english {
                    return self.revert_to_english(raw, None, time_ms);
                }
                self.state = State::Composing { raw, tentative };
                Step::consume()
            }
            Key::Space | Key::Henkan => self.begin_conversion(raw, tentative, false),
            Key::Katakana => self.begin_conversion(raw, tentative, true),
            Key::Enter => {
                let text: String = RomajiTable::global()
                    .parse(&raw)
                    .finalize()
                    .iter()
                    .map(|segment| segment.out.as_str())
                    .collect();
                self.finish_japanese();
                Step::commit(text)
            }
            Key::Backspace => {
                raw.truncate(raw.len() - backspace_len(&raw));
                self.state = if raw.is_empty() {
                    State::Direct(Direct::at_boundary())
                } else {
                    State::Composing { raw, tentative }
                };
                Step::consume()
            }
            // 日本語入力での Esc は、入力の取り消し。
            Key::Escape if self.input_mode == InputMode::Japanese => Step::consume(),
            Key::Escape | Key::ForceEnglish => {
                let learn_in_flow = self.learns_language().then_some(tentative);
                self.revert_to_english(raw, learn_in_flow, time_ms)
            }
            Key::ForceJapanese => {
                self.state = State::Composing { raw, tentative: false };
                Step::consume()
            }
            // 日本語の入力は、確定の操作があるまで続ける。カーソル移動のキーでは確定しない。
            Key::Tab | Key::Up | Key::Down | Key::Left | Key::Right | Key::ShiftLeft | Key::ShiftRight => {
                self.state = State::Composing { raw, tentative };
                Step::consume()
            }
            Key::Muhenkan => unreachable!("handled before the state"),
            Key::Other => {
                // ショートカットは入力先のもの。表示されているとおりに確定してから渡す。
                let text = RomajiTable::global().parse(&raw).display();
                self.reset();
                Step::commit_and_pass(text)
            }
        }
    }

    /// かなを打鍵したままの英字に戻して確定する。`learn_in_flow` が `Some` なら利用者の訂正として覚える。
    /// 中身は、日本語の入力が続いている間の訂正かどうか。
    fn revert_to_english(&mut self, raw: String, learn_in_flow: Option<bool>, time_ms: u64) -> Step {
        if let Some(in_flow) = learn_in_flow {
            if raw.len() <= LEARNABLE_WORD_LEN && raw.bytes().all(|b| b.is_ascii_lowercase()) {
                self.learn_language(&raw, Language::English, in_flow);
            }
        }
        self.state = State::Direct(Direct {
            word: raw.clone(),
            open: false,
            last_key_ms: Some(time_ms),
            ..Direct::at_boundary()
        });
        Step::commit(raw)
    }

    /// かなを英字に戻して確定してから、区切りの打鍵を処理する。
    fn revert_and_pass(&mut self, raw: String, key: Key, time_ms: u64) -> Step {
        let reverted = self.revert_to_english(raw, None, time_ms);
        let mut step = self.handle_as_direct(key, time_ms);
        step.commit.insert_str(0, &reverted.commit);
        step
    }

    /// 確定して `Direct` に戻った直後の打鍵を、`Direct` の打鍵として処理する。
    fn handle_as_direct(&mut self, key: Key, time_ms: u64) -> Step {
        let State::Direct(direct) = std::mem::replace(&mut self.state, State::Direct(Direct::at_boundary()))
        else {
            unreachable!("the engine returns to Direct after a commit");
        };
        if self.input_mode == InputMode::Japanese {
            return self.japanese_idle_key(key);
        }
        self.direct_key(direct, key, time_ms)
    }

    fn begin_conversion(&mut self, raw: String, tentative: bool, katakana: bool) -> Step {
        let reading: String = RomajiTable::global()
            .parse(&raw)
            .finalize()
            .iter()
            .map(|segment| segment.out.as_str())
            .collect();
        let segments = if katakana {
            // カタカナにする時は、文節に区切らずに全体を 1 つとして扱う。
            let mut segment = self.learned(self.detector.converter().segment(&reading, None));
            select_katakana(&mut segment);
            vec![segment]
        } else {
            let converted = self.detector.converter().segments(&reading);
            converted.into_iter().map(|segment| self.learned(segment)).collect()
        };
        let converted_ends = if katakana {
            Vec::new()
        } else {
            segments
                .iter()
                .scan(0, |end, segment: &ConvertingSegment| {
                    *end += segment.reading.chars().count();
                    Some(*end)
                })
                .collect()
        };
        self.state = State::Converting(Converting {
            raw,
            tentative,
            segments,
            focus: 0,
            converted_ends,
        });
        Step::consume()
    }

    /// 利用者が以前に選んだ表記を、候補の先頭に置く。
    fn learned(&self, segment: converter::Segment) -> ConvertingSegment {
        let mut candidates: Vec<Candidate> = self
            .learning
            .conversions(&segment.reading)
            .iter()
            .map(|surface| Candidate {
                surface: surface.clone(),
                learnable: true,
            })
            .collect();
        for candidate in segment.candidates {
            if !candidates.iter().any(|known| known.surface == candidate.surface) {
                candidates.push(candidate);
            }
        }
        ConvertingSegment {
            reading: segment.reading,
            candidates,
            selected: 0,
        }
    }

    /// 候補を選んでいる文節を 1 文字伸ばす、または縮める。後ろの文節は変換し直す。
    fn resize_segment(&self, converting: &mut Converting, grow: bool) {
        let focus = converting.focus;
        let mut head: Vec<char> = converting.segments[focus].reading.chars().collect();
        let mut tail: Vec<char> = converting.segments[focus + 1..]
            .iter()
            .flat_map(|segment| segment.reading.chars())
            .collect();
        if grow {
            if tail.is_empty() {
                return;
            }
            head.push(tail.remove(0));
        } else {
            if head.len() <= 1 {
                return;
            }
            tail.insert(0, head.pop().expect("more than one character"));
        }
        let head: String = head.into_iter().collect();
        let tail: String = tail.into_iter().collect();
        let converter = self.detector.converter();
        converting.segments.truncate(focus);
        converting.segments.push(self.learned(converter.segment(&head, None)));
        for segment in converter.segments(&tail) {
            converting.segments.push(self.learned(segment));
        }
    }

    fn handle_converting(&mut self, mut converting: Converting, key: Key, time_ms: u64) -> Step {
        match key {
            Key::Space | Key::Down | Key::Henkan => {
                let segment = converting.focused();
                segment.selected = (segment.selected + 1) % segment.candidates.len();
                self.state = State::Converting(converting);
                Step::consume()
            }
            Key::Up => {
                let segment = converting.focused();
                segment.selected = (segment.selected + segment.candidates.len() - 1) % segment.candidates.len();
                self.state = State::Converting(converting);
                Step::consume()
            }
            Key::Left | Key::Right => {
                converting.focus = if key == Key::Left {
                    converting.focus.saturating_sub(1)
                } else {
                    (converting.focus + 1).min(converting.segments.len() - 1)
                };
                self.state = State::Converting(converting);
                Step::consume()
            }
            Key::ShiftLeft | Key::ShiftRight => {
                self.resize_segment(&mut converting, key == Key::ShiftRight);
                self.state = State::Converting(converting);
                Step::consume()
            }
            Key::Char(digit @ '1'..='9') => {
                let per_page = self.config.candidates_per_page.max(1);
                let offset = digit as usize - '1' as usize;
                let segment = converting.focused();
                let index = segment.selected / per_page * per_page + offset;
                if offset >= per_page || index >= segment.candidates.len() {
                    self.state = State::Converting(converting);
                    return Step::consume();
                }
                segment.selected = index;
                // 最後の文節で選んだら確定する。それ以外は、次の文節へ進む。
                if converting.focus + 1 == converting.segments.len() {
                    return self.commit(converting);
                }
                converting.focus += 1;
                self.state = State::Converting(converting);
                Step::consume()
            }
            Key::Enter => self.commit(converting),
            Key::Backspace | Key::Escape => {
                self.state = State::Composing {
                    raw: converting.raw,
                    tentative: false,
                };
                Step::consume()
            }
            Key::ForceEnglish => {
                let learn_in_flow = self.learns_language().then_some(converting.tentative);
                self.revert_to_english(converting.raw, learn_in_flow, time_ms)
            }
            Key::Katakana => {
                select_katakana(converting.focused());
                self.state = State::Converting(converting);
                Step::consume()
            }
            Key::ForceJapanese => {
                self.state = State::Converting(converting);
                Step::consume()
            }
            Key::Muhenkan => unreachable!("handled before the state"),
            Key::Char(_) => {
                // 候補を選んだまま次の文字を打ったら、その候補で確定して入力を続ける。
                let committed = self.commit(converting);
                let mut step = self.handle_as_direct(key, time_ms);
                step.commit.insert_str(0, &committed.commit);
                step
            }
            Key::Tab | Key::Other => {
                let committed = self.commit(converting);
                self.reset();
                Step::commit_and_pass(committed.commit)
            }
        }
    }

    /// すべての文節を、選択中の候補で確定する。
    fn commit(&mut self, converting: Converting) -> Step {
        for segment in &converting.segments {
            let candidate = &segment.candidates[segment.selected];
            if candidate.learnable {
                self.learn_conversion(&segment.reading, &candidate.surface);
            }
        }
        for (segments, merged) in converting.regrouped() {
            self.learn_phrase(&segments, merged);
        }
        let text = converting.display();
        self.finish_japanese();
        Step::commit(text)
    }

    fn finish_japanese(&mut self) {
        self.japanese_flow = true;
        self.english_streak = 0;
        self.state = State::Direct(Direct::after_commit());
    }

    fn learn_language(&mut self, word: &str, language: Language, in_flow: bool) {
        self.detector.learn(word, language, in_flow);
        if let Err(error) = self.learning.record_language(word, language, in_flow) {
            self.last_error = Some(error);
        }
    }

    fn learn_conversion(&mut self, reading: &str, surface: &str) {
        // 区切り直して覚えた語と違う表記を選んだら、その語は忘れる。覚えたままにすると、
        // 文の途中ではその語が出続ける。
        if self.learning.phrase(reading).is_some_and(|known| known != surface) {
            self.forget_phrase(reading);
        }
        if let Err(error) = self.learning.record_conversion(reading, surface) {
            self.last_error = Some(error);
        }
    }

    fn forget_phrase(&mut self, reading: &str) {
        self.detector.converter_mut().forget_phrase(reading);
        if let Err(error) = self.learning.forget_phrase(reading) {
            self.last_error = Some(error);
        }
    }

    /// 利用者が区切り直して確定した範囲 (文節ごとの読みと表記) を、1 語として覚える。
    /// 同じ読みを次に変換した時に先頭に出し、助詞が続く時や文の途中でも、変換に使う。
    ///
    /// 覚えるのは、範囲から末尾の助詞や助動詞を除いた部分。「かよい」「じを」を「通い」「路を」で
    /// 確定したら、「かよいじ」を「通い路」として覚える。助詞ごと覚えると、ほかの助詞が続く時に
    /// 効かない上に、後ろに続く語とのつながりを誤る。
    fn learn_phrase(&mut self, segments: &[(String, String)], merged: bool) {
        let converter = self.detector.converter();
        let mut words = Vec::new();
        for (reading, surface) in segments {
            // 辞書の語だけで作れない表記 (ひらがなのまま、カタカナ) を含む範囲は覚えない。
            // 語の前後のつながりを辞書から決められない。
            let Some(found) = converter.words(reading, surface) else {
                return;
            };
            words.extend(found);
        }
        // 辞書の付属語には、助詞や助動詞のほかに接尾語 (「路」「時」) も入っている。かなのままの
        // 付属語だけを助詞や助動詞とみなして除く。利用者が漢字で選んだ接尾語は、語の一部として残す。
        while words.last().is_some_and(|word| word.function && word.reading == word.surface) {
            words.pop();
        }
        let words: Vec<(String, String)> = words.into_iter().map(|word| (word.reading, word.surface)).collect();
        let reading: String = words.iter().map(|(reading, _)| reading.as_str()).collect();
        let surface: String = words.iter().map(|(_, surface)| surface.as_str()).collect();
        if reading.chars().count() > PHRASE_MAX_CHARS {
            return;
        }
        if words.len() < PHRASE_MIN_WORDS {
            // 文節をまとめて辞書の 1 語を選んだ時は、変換器が 2 つに分けてしまう語として覚える。
            // 覚えないと、次の変換でもまた分かれて、文節ごとに覚えた候補が当たらない。
            let split_by_converter = merged
                && words.len() == 1
                && converter
                    .convert(&reading, 1)
                    .first()
                    .is_none_or(|best| best.surface != surface);
            if !split_by_converter {
                return;
            }
        }
        if let Some(dropped) = self.learning.phrase_to_drop(&reading).map(str::to_string) {
            self.detector.converter_mut().forget_phrase(&dropped);
        }
        if !self.detector.converter_mut().learn_phrase(&words) {
            return;
        }
        if let Err(error) = self.learning.record_phrase(&words) {
            self.last_error = Some(error);
        }
        // 同じ読みの文節で以前に選んだ表記は忘れる。残すと、語より先に出る。
        // ここで語の表記を文節の選択として記録することはしない。「ゆきが」を「雪賀」と覚えた時に
        // 記録すると、「雪が積もる」の文節「ゆきが」まで「雪賀」になる。
        if let Err(error) = self.learning.retain_conversions(&reading, |chosen| chosen == surface) {
            self.last_error = Some(error);
        }
        // この語を含む読みで以前に選んだ表記が残っていると、助詞が付いた読みでは、語を覚える前の
        // 選択が先頭に出続ける。変換にこの語が使われる読みに限って、語を使っていない選択を忘れる。
        for known in self.learning.readings_containing(&reading) {
            let uses_phrase = self
                .detector
                .converter()
                .convert(&known, 1)
                .first()
                .is_some_and(|best| best.surface.contains(&surface));
            if !uses_phrase {
                continue;
            }
            if let Err(error) = self.learning.retain_conversions(&known, |chosen| chosen.contains(&surface)) {
                self.last_error = Some(error);
            }
        }
    }
}

/// かなの入力中の Backspace で、打鍵列の末尾から消すバイト数。かな 1 文字分の打鍵を消す。
fn backspace_len(raw: &str) -> usize {
    let table = RomajiTable::global();
    let parsed = table.parse(raw);
    if !parsed.pending.is_empty() {
        return 1;
    }
    let Some((last, kept)) = parsed.segments.split_last() else {
        return raw.len();
    };
    if !last.raw.is_ascii() {
        return last.raw.len();
    }
    // 促音の規則 ("kk" → っ + k) は、次のかなと打鍵を共有する。「がっか」の "ka" をすべて消すと
    // "gak" になり、手前の「っ」まで消える。手前のかなが変わらない範囲で、できるだけ多く消す。
    let kept: String = kept.iter().map(|segment| segment.out.as_str()).collect();
    (1..=last.raw.len())
        .rev()
        .find(|length| table.parse(&raw[..raw.len() - length]).text() == kept)
        .unwrap_or(last.raw.len())
}

/// 入力中の単語の続きとして、日本語の判定に含める文字かどうか。
fn extends_word(direct: &Direct, c: char) -> bool {
    if c.is_ascii_lowercase() {
        return true;
    }
    if !direct.pending {
        return false;
    }
    match c {
        // 「こーひー」の長音。かなの直後でなければハイフンとして扱う。
        '-' => {
            let parsed = RomajiTable::global().parse(&direct.word);
            parsed.pending.is_empty() && parsed.is_strict()
        }
        // 「こんや」を "kon'ya" と打つ時の区切り。
        '\'' => direct.word.ends_with('n'),
        _ => false,
    }
}

/// 英小文字と、ローマ字の一部として打つ記号だけでできた打鍵列。
fn is_word(raw: &str) -> bool {
    !raw.is_empty() && raw.bytes().all(|b| b.is_ascii_lowercase() || b == b'-' || b == b'\'')
}

/// `c` が、入力中のローマ字の続きとして打たれる記号かどうか。
fn continues_romaji(raw: &str, c: char) -> bool {
    match c {
        '-' => !raw.is_empty(),
        '\'' => raw.ends_with('n'),
        _ => false,
    }
}

/// 文節の候補から、読みをカタカナにしたものを選ぶ。
fn select_katakana(segment: &mut ConvertingSegment) {
    let katakana = to_katakana(&segment.reading);
    segment.selected = match segment.candidates.iter().position(|candidate| candidate.surface == katakana) {
        Some(index) => index,
        None => {
            segment.candidates.push(Candidate {
                surface: katakana,
                learnable: true,
            });
            segment.candidates.len() - 1
        }
    };
}

/// 日本語入力で打つと、続けて英語を打つと分かる記号 ("/"、"@"、"#")。
/// ローマ字の表で日本語の記号になるもの (句読点、かぎ括弧、長音記号、波ダッシュ)、
/// ローマ字の一部として打つ "'"、日本語の文末に付ける "!" と "?" は含めない。
fn leads_english(c: char) -> bool {
    c.is_ascii_punctuation() && !matches!(c, ',' | '.' | '[' | ']' | '-' | '~' | '\'' | '!' | '?')
}

/// 入力中の打鍵 `raw` に `c` を足すと、ローマ字の規則の一部になるかどうか ("z" + "/" で「・」)。
fn completes_rule(raw: &str, c: char) -> bool {
    let mut typed = raw.to_string();
    typed.push(c);
    let parsed = RomajiTable::global().parse(&typed);
    parsed.pending.ends_with(c)
        || parsed
            .segments
            .last()
            .is_some_and(|segment| segment.converted && segment.raw.ends_with(c))
}

fn japanese_punctuation(c: char) -> char {
    match c {
        ',' => '、',
        '.' => '。',
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detector::tests::sample_detector;
    use crate::simulate::Simulator;

    fn simulator() -> Simulator {
        simulator_with(true)
    }

    fn simulator_with(auto_switch: bool) -> Simulator {
        Simulator::new(Engine::new(
            sample_detector(),
            Learning::in_memory(),
            EngineConfig {
                auto_switch,
                ..EngineConfig::default()
            },
        ))
    }

    fn typed(script: &str) -> String {
        let mut simulator = simulator();
        simulator.run(script);
        simulator.text
    }

    #[test]
    fn english_text_passes_through_unchanged() {
        assert_eq!(typed("for the home made take "), "for the home made take ");
    }

    #[test]
    fn spec_example_switches_to_japanese_at_tatoe() {
        // Arrange
        let mut simulator = simulator();

        // Act / Assert
        simulator.run("tato");
        assert_eq!(simulator.text, "tato");
        assert_eq!(simulator.engine.view().mode, Mode::Direct);

        simulator.run("e");
        assert_eq!(simulator.text, "たとえ");
        assert_eq!(simulator.engine.view().mode, Mode::Composing);

        simulator.run("ba");
        assert_eq!(simulator.text, "たとえば");

        simulator.run(" ");
        assert_eq!(simulator.text, "例えば");
        assert_eq!(simulator.engine.view().mode, Mode::Converting);

        simulator.run("{enter}");
        assert_eq!(simulator.text, "例えば");
        assert_eq!(simulator.engine.view().mode, Mode::Direct);
    }

    #[test]
    fn a_pause_inside_a_word_commits_it_as_english() {
        assert_eq!(typed("ta{wait}toeba "), "tatoeba ");
    }

    #[test]
    fn slow_typing_never_switches_to_japanese() {
        // Arrange
        let mut simulator = simulator();
        simulator.interval_ms = 300;

        // Act
        simulator.run("tatoeba kore ");

        // Assert
        assert_eq!(simulator.text, "tatoeba kore ");
    }

    #[test]
    fn typing_within_the_timeout_is_one_sequence() {
        // Arrange
        let mut simulator = simulator();
        simulator.interval_ms = 200;

        // Act
        simulator.run("tatoe");

        // Assert
        assert_eq!(simulator.text, "たとえ");
    }

    #[test]
    fn space_converts_a_word_that_is_japanese_at_the_boundary() {
        // Arrange
        let mut simulator = simulator();

        // Act
        simulator.run("kore ");

        // Assert
        assert_eq!(simulator.text, "これ");
        assert_eq!(simulator.engine.view().mode, Mode::Converting);
    }

    #[test]
    fn a_pause_before_space_keeps_the_word_english() {
        assert_eq!(typed("kore{wait} "), "kore ");
    }

    #[test]
    fn force_japanese_rereads_the_previous_word() {
        // Arrange
        let mut simulator = simulator();
        simulator.run("made ");
        assert_eq!(simulator.text, "made ");

        // Act
        simulator.run("{ja}");

        // Assert
        assert_eq!(simulator.text, "まで");
        assert_eq!(simulator.engine.view().mode, Mode::Composing);
    }

    #[test]
    fn force_japanese_rereads_the_word_being_typed() {
        assert_eq!(typed("hon{ja}{enter}"), "ほん");
    }

    #[test]
    fn force_japanese_is_remembered_for_the_next_time() {
        // Arrange
        let mut simulator = simulator();
        simulator.run("made {ja}{enter} ");
        assert_eq!(simulator.text, "まで ");

        // Act
        simulator.run("made ");

        // Assert
        assert_eq!(simulator.text, "まで まで");
    }

    #[test]
    fn force_japanese_is_remembered_for_a_rare_spelling() {
        // Arrange
        let mut simulator = simulator();
        // "wi" は一般的なローマ字ではないので、打った時点で英語に決まる。
        simulator.run("wi {ja}{enter} ");
        let learned = simulator.text.clone();

        // Act
        simulator.run("wi ");

        // Assert
        assert_eq!(learned, "うぃ ");
        assert_eq!(simulator.text, "うぃ うぃ");
    }

    #[test]
    fn force_japanese_without_a_word_passes_the_key_through() {
        // Arrange
        let mut simulator = simulator();

        // Act
        let output = simulator.engine.handle(Key::ForceJapanese, 0);

        // Assert
        assert!(!output.consumed);
    }

    #[test]
    fn escape_restores_the_typed_letters() {
        // Arrange
        let mut simulator = simulator();
        simulator.run("watashi");
        assert_eq!(simulator.text, "わたし");

        // Act
        simulator.run("{esc}");

        // Assert
        assert_eq!(simulator.text, "watashi");
        assert_eq!(simulator.engine.view().mode, Mode::Direct);
    }

    #[test]
    fn letters_after_escape_continue_the_english_word() {
        assert_eq!(typed("watashi{esc}wa "), "watashiwa ");
    }

    #[test]
    fn escape_is_remembered_for_the_next_time() {
        // Arrange
        let mut simulator = simulator();
        simulator.run("watashi{esc} ");

        // Act
        simulator.run("watashi ");

        // Assert
        assert_eq!(simulator.text, "watashi watashi ");
    }

    #[test]
    fn typing_after_a_conversion_commits_and_stays_japanese() {
        // Arrange
        let mut simulator = simulator();

        // Act / Assert
        simulator.run("kyouha ");
        assert_eq!(simulator.text, "今日は");

        simulator.run("hare");
        assert_eq!(simulator.text, "今日ははれ");
        assert_eq!(simulator.engine.view().mode, Mode::Composing);

        simulator.run(" {enter}");
        assert_eq!(simulator.text, "今日は晴れ");
    }

    #[test]
    fn english_inside_japanese_flow_is_restored_with_escape() {
        assert_eq!(typed("kore for{esc} "), "これfor ");
    }

    #[test]
    fn english_word_in_japanese_flow_returns_to_letters_at_the_space() {
        // "for" は「ふぉr」と表示されるが、空白の時点で子音が余るので英語に戻る。
        assert_eq!(typed("kore for "), "これfor ");
        assert_eq!(typed("kore the "), "これthe ");
    }

    #[test]
    fn unreadable_letters_in_japanese_flow_return_to_letters_immediately() {
        // Arrange
        let mut simulator = simulator();
        simulator.run("kore s");
        assert_eq!(simulator.text, "これs");
        assert_eq!(simulator.engine.view().mode, Mode::Composing);

        // Act
        simulator.run("t");

        // Assert
        assert_eq!(simulator.text, "これst");
        assert_eq!(simulator.engine.view().mode, Mode::Direct);

        simulator.run("ring ");
        assert_eq!(simulator.text, "これstring ");
    }

    #[test]
    fn particle_in_japanese_flow_stays_japanese() {
        // Arrange
        let mut simulator = simulator();

        // Act
        simulator.run("kore to ");

        // Assert
        assert_eq!(simulator.text, "これと");
        assert_eq!(simulator.engine.view().mode, Mode::Converting);
    }

    #[test]
    fn one_english_word_does_not_end_japanese_flow() {
        // "made" は単独なら英語。日本語の入力が続いている間は「まで」になる。
        assert_eq!(typed("made "), "made ");
        assert_eq!(typed("kore for made "), "これfor まで");
        assert_eq!(typed("kore Tauri no "), "これTauri の");
    }

    #[test]
    fn two_english_words_end_japanese_flow() {
        assert_eq!(typed("kore for the made "), "これfor the made ");
    }

    #[test]
    fn japanese_flow_continues_on_the_next_line() {
        assert_eq!(typed("kore {enter}\nmade "), "これ\nまで");
    }

    #[test]
    fn moving_the_cursor_ends_japanese_flow() {
        assert_eq!(typed("kore {enter}{other}made "), "これmade ");
    }

    #[test]
    fn digits_do_not_break_japanese_flow() {
        assert_eq!(typed("kore {enter}3wo "), "これ3を");
    }

    #[test]
    fn symbols_in_japanese_flow_start_an_identifier() {
        assert_eq!(typed("kore {enter}_kore "), "これ_kore ");
    }

    #[test]
    fn punctuation_ends_a_tentative_word() {
        // 「ほめ」は辞書に無いので home は英語。「それ」は日本語なので句読点も日本語にする。
        assert_eq!(typed("kore {enter}home."), "これhome.");
        assert_eq!(typed("kore {enter}sore."), "これそれ。");
    }

    #[test]
    fn deleting_a_tentative_word_keeps_japanese_flow() {
        // Arrange
        let mut simulator = simulator();
        simulator.run("kore k");
        assert_eq!(simulator.text, "これk");

        // Act
        simulator.run("{bs}");

        // Assert
        assert_eq!(simulator.text, "これ");
        assert_eq!(simulator.engine.view().mode, Mode::Direct);

        simulator.run("to");
        assert_eq!(simulator.text, "これと");
    }

    #[test]
    fn deleting_committed_japanese_keeps_japanese_flow() {
        assert_eq!(typed("kore {enter}{bs}to"), "こと");
    }

    #[test]
    fn escape_is_remembered_even_for_a_sequence_in_the_rules() {
        // Arrange
        let mut simulator = simulator();
        // "tatoe" は判断データベースの規則で、打った時点で日本語になる。
        simulator.run("tatoe{esc} ");
        assert_eq!(simulator.committed, "tatoe ");

        // Act
        simulator.run("tatoe ");

        // Assert
        assert_eq!(simulator.committed, "tatoe tatoe ");
    }

    #[test]
    fn escape_in_japanese_flow_is_remembered() {
        // Arrange
        let mut simulator = simulator();
        simulator.run("kore made{esc} ");
        assert_eq!(simulator.text, "これmade ");

        // Act
        simulator.run("sore made ");

        // Assert
        assert_eq!(simulator.text, "これmade それmade ");
    }

    #[test]
    fn escape_outside_japanese_flow_does_not_apply_inside_it() {
        // Arrange
        let mut simulator = simulator();
        simulator.run("watashi{esc} ");
        assert_eq!(simulator.text, "watashi ");

        // Act
        simulator.run("kore watashi ");

        // Assert
        // 単独の "watashi" は英語と覚えたが、日本語の続きとして打った場合は「私」のまま。
        assert_eq!(simulator.text, "watashi これ私");
    }

    #[test]
    fn candidates_cycle_in_both_directions() {
        // Arrange
        let mut simulator = simulator();

        // Act / Assert
        simulator.run("kyou ");
        assert_eq!(simulator.text, "今日");
        simulator.run(" ");
        assert_eq!(simulator.text, "京");
        simulator.run("{up}");
        assert_eq!(simulator.text, "今日");
        simulator.run("{up}");
        assert_eq!(simulator.text, "キョウ");
        simulator.run("{down}");
        assert_eq!(simulator.text, "今日");
    }

    #[test]
    fn a_digit_selects_and_commits_a_candidate() {
        // Arrange
        let mut simulator = simulator();
        simulator.run("kyou ");

        // Act
        simulator.run("3");

        // Assert
        assert_eq!(simulator.text, "教");
        assert_eq!(simulator.engine.view().mode, Mode::Direct);
    }

    #[test]
    fn a_digit_beyond_the_list_is_ignored() {
        // Arrange
        let mut simulator = simulator();
        simulator.run("kore ");

        // Act
        simulator.run("9");

        // Assert
        assert_eq!(simulator.text, "これ");
        assert_eq!(simulator.engine.view().mode, Mode::Converting);
    }

    #[test]
    fn view_lists_the_candidates_on_the_current_page() {
        // Arrange
        let mut simulator = simulator();

        // Act
        simulator.run("kyou  ");
        let view = simulator.engine.view();

        // Assert
        assert_eq!(view.reading, "きょう");
        assert_eq!(
            view.candidates,
            ["今日", "京", "教", "2026-11-25", "20261125", "2026/11/25", "2026年11月25日", "11月25日", "きょう"]
        );
        assert_eq!(view.selected, Some(1));
        assert_eq!((view.page, view.page_count), (0, 2));
    }

    #[test]
    fn each_segment_has_its_own_candidates() {
        // Arrange
        let mut simulator = simulator();
        simulator.run("kyouhahare ");
        assert_eq!(simulator.text, "今日は晴れ");
        assert_eq!(simulator.engine.view().reading, "きょうは");

        // Act / Assert
        simulator.run("{right}");
        assert_eq!(simulator.engine.view().reading, "はれ");
        simulator.run(" ");
        assert_eq!(simulator.text, "今日は腫れ");
        simulator.run("{left} ");
        assert_eq!(simulator.text, "京は腫れ");
        simulator.run("{enter}");
        assert_eq!(simulator.text, "京は腫れ");
        assert_eq!(simulator.engine.view().mode, Mode::Direct);
    }

    #[test]
    fn focus_stops_at_the_first_and_last_segment() {
        // Arrange
        let mut simulator = simulator();
        simulator.run("kyouhahare ");

        // Act / Assert
        simulator.run("{left}");
        assert_eq!(simulator.engine.view().reading, "きょうは");
        simulator.run("{right}{right}{right}");
        assert_eq!(simulator.engine.view().reading, "はれ");
    }

    #[test]
    fn shift_arrows_resize_the_focused_segment() {
        // Arrange
        let mut simulator = simulator();
        simulator.run("kyouhahare ");

        // Act: 「きょうは」を 1 文字縮める
        simulator.run("{sleft}");

        // Assert
        // 「きょう」「は」「はれ」に区切り直される。
        assert_eq!(simulator.engine.view().reading, "きょう");
        assert_eq!(simulator.text, "今日は晴れ");
        simulator.run(" ");
        assert_eq!(simulator.text, "京は晴れ");

        // Act: 伸ばして元に戻す
        simulator.run("{sright}");

        // Assert
        assert_eq!(simulator.engine.view().reading, "きょうは");
        assert_eq!(simulator.text, "今日は晴れ");
    }

    fn words(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(reading, surface)| (reading.to_string(), surface.to_string()))
            .collect()
    }

    #[test]
    fn a_resegmented_span_is_offered_first_next_time() {
        // Arrange: 「羽目」で確定したことがある「はめ」を、「は」「め」に区切り直して確定する
        let mut simulator = simulator_with(false);
        simulator.run("hame {enter}hame ");
        assert_eq!(simulator.text, "羽目羽目");
        simulator.run("{sleft}{enter}");
        assert_eq!(simulator.text, "羽目歯目");

        // Act
        simulator.run("hame ");

        // Assert
        assert_eq!(simulator.text, "羽目歯目歯目");
        // 区切り直した範囲が、1 つの文節になる。
        assert_eq!(simulator.engine.view().reading, "はめ");
    }

    #[test]
    fn a_learned_span_is_used_before_a_particle() {
        // Arrange
        let mut simulator = simulator_with(false);
        simulator.run("hamega ");
        assert_eq!(simulator.text, "羽目が");
        simulator.run("{enter}hame {sleft}{enter}");

        // Act
        simulator.run("hamega ");

        // Assert
        // 語を覚える前に「羽目が」で確定したことより、覚えた語を優先する。
        assert_eq!(simulator.text, "羽目が歯目歯目が");
    }

    #[test]
    fn a_span_resegmented_with_its_particle_is_learned_without_the_particle() {
        // Arrange: 「はめが」を「は」「めが」に区切り直して確定する
        let mut simulator = simulator_with(false);
        simulator.run("hamega {sleft}{sleft}");
        assert_eq!(simulator.text, "歯目が");
        simulator.run("{enter}");

        // Act
        simulator.run("hame {enter}hamewo ");

        // Assert
        assert_eq!(simulator.text, "歯目が歯目歯目を");
        assert_eq!(simulator.engine.learning.phrase("はめが"), None);
    }

    #[test]
    fn a_suffix_chosen_in_kanji_stays_in_the_learned_span() {
        // Arrange: 「側」は辞書では付属語。「かねがわ」を「かね」「がわ」に区切り直して確定する
        let mut simulator = simulator_with(false);
        simulator.run("kanegawa ");
        assert_eq!(simulator.engine.view().reading, "かねがわ");

        // Act
        simulator.run("{sleft}{sleft}{enter}");

        // Assert
        assert_eq!(simulator.text, "鐘側");
        assert_eq!(simulator.engine.learning.phrase("かねがわ"), Some("鐘側"));
    }

    #[test]
    fn splitting_a_particle_off_a_word_learns_nothing() {
        // Arrange
        let mut simulator = simulator_with(false);
        simulator.run("kyouha ");

        // Act: 「きょうは」を「きょう」「は」に分けて、「京」と助詞の「は」を選んで確定する
        simulator.run("{sleft} {right} ");
        assert_eq!(simulator.text, "京は");
        simulator.run("{enter}");

        // Assert
        // 「京」は辞書の 1 語なので、語としては覚えない。覚えると、「きょう」を含む変換がどれも変わる。
        assert_eq!(simulator.engine.learning.phrases().count(), 0);
    }

    #[test]
    fn learned_spans_are_used_after_a_restart() {
        // Arrange
        let mut learning = Learning::in_memory();
        learning.record_phrase(&words(&[("は", "歯"), ("め", "目")])).expect("recorded");
        let config = EngineConfig::default();
        let mut simulator = Simulator::new(Engine::new(sample_detector(), learning, config));

        // Act
        simulator.run("hamega ");

        // Assert
        assert_eq!(simulator.text, "歯目が");
    }

    #[test]
    fn choosing_another_candidate_forgets_the_learned_span() {
        // Arrange: 語を覚えた後に、助詞を付けて確定したことがある
        let mut simulator = simulator_with(false);
        simulator.run("hame {sleft}{enter}hamega {enter}hame ");
        assert_eq!(simulator.text, "歯目歯目が歯目");

        // Act: 同じ読みで「羽目」を選び直す
        simulator.run(" ");
        assert_eq!(simulator.text, "歯目歯目が羽目");
        simulator.run("{enter}hamega ");

        // Assert
        assert_eq!(simulator.text, "歯目歯目が羽目羽目が");
    }

    #[test]
    fn segments_merged_into_one_are_learned_as_a_span() {
        // Arrange: 「これ」「あめ」を 1 つの文節に伸ばして、「これ飴」を選ぶ
        let mut simulator = simulator_with(false);
        simulator.run("koreame ");
        assert_eq!(simulator.text, "これ雨");
        assert_eq!(simulator.engine.view().reading, "これ");
        simulator.run("{sright}{sright}");
        assert_eq!(simulator.engine.view().reading, "これあめ");
        simulator.run(" ");
        assert_eq!(simulator.text, "これ飴");
        simulator.run("{enter}");

        // Act
        simulator.run("koreame ");

        // Assert
        assert_eq!(simulator.text, "これ飴これ飴");
        assert_eq!(simulator.engine.view().reading, "これあめ");
    }

    #[test]
    fn a_span_read_like_a_word_and_a_particle_leaves_the_particle_alone() {
        // Arrange: 「これは」を「これ」「は」に分けて、「歯」を選んで確定する
        let mut simulator = simulator_with(false);
        simulator.run("koreha {sleft}{enter}");
        assert_eq!(simulator.text, "これ歯");
        assert_eq!(simulator.engine.learning.phrase("これは"), Some("これ歯"));

        // Act
        simulator.run("korehaame ");

        // Assert
        // 文の中の「これは」は、助詞の「は」のまま。
        assert_eq!(simulator.text, "これ歯これは雨");
    }

    #[test]
    fn a_dictionary_word_chosen_by_merging_segments_is_learned() {
        // Arrange: 「此雨」は辞書の 1 語だが、変換器は「これ」「あめ」に分ける
        let mut simulator = simulator_with(false);
        simulator.run("koreame {sright}{sright}");
        for _ in 0..20 {
            if simulator.text == "此雨" {
                break;
            }
            simulator.run(" ");
        }
        assert_eq!(simulator.text, "此雨");
        simulator.run("{enter}");

        // Act
        simulator.run("koreame ");

        // Assert
        assert_eq!(simulator.text, "此雨此雨");
        assert_eq!(simulator.engine.view().reading, "これあめ");
    }

    #[test]
    fn spans_are_learned_separately_around_an_untouched_segment() {
        // Arrange: 「はめ」「これ」「はめ」の、最初と最後だけを区切り直す
        let mut simulator = simulator_with(false);
        simulator.run("hamekorehame ");
        assert_eq!(simulator.text, "羽目これ羽目");

        // Act
        simulator.run("{sleft}{right}{right}{right}{sleft}{enter}");

        // Assert
        assert_eq!(simulator.text, "歯目これ歯目");
        assert_eq!(simulator.engine.learning.phrases().count(), 1);
        assert_eq!(simulator.engine.learning.phrase("はめ"), Some("歯目"));
    }

    #[test]
    fn a_span_as_long_as_a_sentence_is_not_learned() {
        // Arrange
        let mut simulator = simulator_with(false);
        simulator.run("watasinonamaehanihongonyuuryokudesu ");
        let converted = simulator.text.clone();

        // Act: 最初の文節を、文の終わりまで伸ばして確定する
        for _ in 0..16 {
            simulator.run("{sright}");
        }
        assert_eq!(simulator.engine.view().reading, "わたしのなまえはにほんごにゅうりょくです");
        simulator.run("{enter}");

        // Assert
        assert_eq!(simulator.text, converted);
        assert_eq!(simulator.engine.learning.phrases().count(), 0);
    }

    #[test]
    fn a_span_with_a_date_is_not_learned() {
        // Arrange
        let mut simulator = simulator_with(false);
        simulator.run("kyouha {sleft}");
        let today = simulator
            .engine
            .view()
            .candidates
            .iter()
            .position(|candidate| candidate.chars().next().is_some_and(|c| c.is_ascii_digit()))
            .expect("a date is offered for きょう");

        // Act
        for _ in 0..today {
            simulator.run(" ");
        }
        simulator.run("{enter}");

        // Assert
        // 日付は辞書の語ではないので、前後のつながりを決められない。
        assert_eq!(simulator.engine.learning.phrases().count(), 0);
    }

    #[test]
    fn resegmenting_after_a_katakana_conversion_learns_nothing() {
        // Arrange: カタカナにする変換は、文節に区切らずに始まる
        let mut simulator = simulator_with(false);
        simulator.run("hamega{kata}");
        assert_eq!(simulator.text, "ハメガ");

        // Act
        simulator.run("{sleft}{sleft}{enter}");

        // Assert
        assert_eq!(simulator.text, "歯目が");
        assert_eq!(simulator.engine.learning.phrases().count(), 0);
    }

    #[test]
    fn a_span_dropped_beyond_the_limit_is_no_longer_converted() {
        // Arrange: 覚えた語が上限に達していて、いちばん古い語が「はめ」
        let mut learning = Learning::in_memory();
        learning.record_phrase(&words(&[("は", "歯"), ("め", "目")])).expect("recorded");
        while learning.phrase_to_drop("これあめ").is_none() {
            let reading = format!("よみ{}", learning.phrases().count());
            learning.record_phrase(&words(&[(reading.as_str(), "語"), ("ご", "後")])).expect("recorded");
        }
        let config = EngineConfig::default();
        let mut simulator = Simulator::new(Engine::new(sample_detector(), learning, config));
        simulator.run("hame ");
        assert_eq!(simulator.text, "歯目");
        simulator.run("{enter}");

        // Act: 新しい語を覚える
        simulator.run("koreame {sright}{sright}{enter}");

        // Assert
        assert_eq!(simulator.engine.learning.phrase("はめ"), None);
        simulator.run("hamega ");
        assert!(simulator.text.ends_with("羽目が"), "{}", simulator.text);
    }

    #[test]
    fn learning_a_span_keeps_choices_for_readings_it_does_not_change() {
        // Arrange: 「あめ」「かね」を 1 つの文節にして「雨鐘」で確定してある
        let mut simulator = simulator_with(false);
        simulator.run("amekane {sright}{sright}{enter}");
        assert_eq!(simulator.text, "雨鐘");
        assert_eq!(simulator.engine.learning.conversions("あめかね"), ["雨鐘"]);

        // Act: 「め」「かね」を 1 つの文節にして、「めかね」を「目鐘」として覚える
        simulator.run("mekane {sright}{sright}{enter}");

        // Assert
        assert_eq!(simulator.engine.learning.phrase("めかね"), Some("目鐘"));
        // 「あめかね」は読みに「めかね」を含むが、変換に「目鐘」は使われない。選んだ表記は残す。
        assert_eq!(simulator.engine.learning.conversions("あめかね"), ["雨鐘"]);
    }

    #[test]
    fn a_segment_cannot_shrink_below_one_character_or_grow_past_the_end() {
        // Arrange
        let mut simulator = simulator();
        simulator.run("me ");
        assert_eq!(simulator.text, "me ");
        simulator.run("{ja} ");
        assert_eq!(simulator.engine.view().reading, "め");

        // Act
        simulator.run("{sleft}{sright}");

        // Assert
        assert_eq!(simulator.engine.view().reading, "め");
        assert_eq!(simulator.text, "目");
    }

    #[test]
    fn output_reports_the_focused_segment_inside_the_preedit() {
        // Arrange
        let mut simulator = simulator();
        simulator.run("kyouhahare ");

        // Act
        let output = simulator.engine.handle(Key::Right, 10_000);

        // Assert
        // 「今日は晴れ」のうち、「晴れ」は 3 文字目から 2 文字。
        assert_eq!(output.preedit, "今日は晴れ");
        assert_eq!(output.focus, Some((3, 2)));
    }

    #[test]
    fn a_digit_moves_to_the_next_segment_before_the_last_one() {
        // Arrange
        let mut simulator = simulator();
        simulator.run("kyouhahare ");

        // Act / Assert
        simulator.run("2");
        assert_eq!(simulator.text, "京は晴れ");
        assert_eq!(simulator.engine.view().mode, Mode::Converting);
        assert_eq!(simulator.engine.view().reading, "はれ");
        simulator.run("2");
        assert_eq!(simulator.text, "京は腫れ");
        assert_eq!(simulator.engine.view().mode, Mode::Direct);
    }

    #[test]
    fn dates_are_offered_but_not_remembered() {
        // Arrange
        let mut simulator = simulator();

        // Act
        simulator.run("kyou 4");

        // Assert
        assert_eq!(simulator.text, "2026-11-25");

        // 日付は日ごとに変わるので、次回の先頭にはしない。
        simulator.run(" kyou ");
        assert_eq!(simulator.text, "2026-11-25 今日");
    }

    #[test]
    fn symbols_are_offered_for_their_readings() {
        assert_eq!(typed("migi "), "右");
        assert_eq!(typed("migi  "), "→");
    }

    #[test]
    fn muhenkan_switches_to_direct_input() {
        // Arrange
        let mut simulator = simulator();

        // Act
        simulator.run("{muhenkan}tatoeba kore ");

        // Assert
        assert_eq!(simulator.text, "tatoeba kore ");
        assert_eq!(simulator.engine.view().input_mode, InputMode::Direct);
        assert_eq!(simulator.engine.handle(Key::Char('a'), 10_000), Output::default());
    }

    #[test]
    fn henkan_returns_to_automatic_switching() {
        // Arrange
        let mut simulator = simulator();
        simulator.run("{muhenkan}tatoe ");

        // Act
        simulator.run("{henkan}tatoe");

        // Assert
        assert_eq!(simulator.text, "tatoe たとえ");
        assert_eq!(simulator.engine.view().input_mode, InputMode::Auto);
        assert!(!simulator.engine.view().forced);
    }

    #[test]
    fn muhenkan_commits_what_was_typed_as_letters() {
        assert_eq!(typed("tatoeba{muhenkan}"), "tatoeba");
        assert_eq!(typed("kyou {muhenkan}x"), "kyoux");
        assert_eq!(typed("kor{muhenkan}"), "kor");
    }

    #[test]
    fn henkan_forces_the_next_input_to_be_japanese() {
        // Arrange
        let mut simulator = simulator();

        // Act / Assert
        simulator.run("{henkan}");
        assert!(simulator.engine.view().forced);

        // 「.」は、判定を経ずに「。」として入力が始まる。
        simulator.run(".");
        assert_eq!(simulator.text, "。");
        assert_eq!(simulator.engine.view().mode, Mode::Composing);
        assert!(!simulator.engine.view().forced);

        simulator.run("{henkan}");
        assert_eq!(simulator.text, "。");
        assert_eq!(simulator.engine.view().mode, Mode::Converting);
        simulator.run("{henkan}");
        assert_eq!(simulator.text, "…");
    }

    #[test]
    fn henkan_forces_only_one_input() {
        assert_eq!(typed("{henkan}me{enter} me "), "め me ");
    }

    #[test]
    fn henkan_reads_the_undetermined_word_as_japanese() {
        // Arrange
        let mut simulator = simulator();
        simulator.run("me");
        assert_eq!(simulator.engine.view().mode, Mode::Direct);

        // Act
        simulator.run("{henkan}");

        // Assert
        assert_eq!(simulator.text, "め");
        assert_eq!(simulator.engine.view().mode, Mode::Composing);
    }

    #[test]
    fn a_composition_stays_until_it_is_committed() {
        // Arrange
        let mut simulator = simulator();
        simulator.run("tatoeba");

        // Act: 放置しても、カーソル移動のキーを押しても確定しない
        simulator.wait(60_000);
        simulator.run("{left}{right}{up}{down}{tab}{sleft}");

        // Assert
        assert_eq!(simulator.committed, "");
        assert_eq!(simulator.engine.preedit(), "たとえば");
        assert_eq!(simulator.engine.view().mode, Mode::Composing);
        assert_eq!(simulator.engine.timeout_deadline(), None);
    }

    #[test]
    fn a_chosen_candidate_comes_first_the_next_time() {
        // Arrange
        let mut simulator = simulator();
        simulator.run("kyou  {enter} ");
        assert_eq!(simulator.text, "京 ");

        // Act
        simulator.run("kyou ");

        // Assert
        assert_eq!(simulator.text, "京 京");
    }

    #[test]
    fn punctuation_right_after_a_commit_is_japanese() {
        assert_eq!(typed("kore {enter},"), "これ、");
        assert_eq!(typed("kore {enter}."), "これ。");
    }

    #[test]
    fn punctuation_after_english_is_left_alone() {
        assert_eq!(typed("home, made. "), "home, made. ");
        assert_eq!(typed("kore {enter} home."), "これ home.");
    }

    #[test]
    fn punctuation_after_a_japanese_word_joins_the_composition() {
        // Arrange
        let mut simulator = simulator();

        // Act
        simulator.run("kore,");

        // Assert
        assert_eq!(simulator.text, "これ、");
        assert_eq!(simulator.engine.view().mode, Mode::Composing);
    }

    #[test]
    fn symbols_typed_while_converting_stay_half_width() {
        assert_eq!(typed("kore ?"), "これ?");
        assert_eq!(typed("kore !"), "これ!");
    }

    #[test]
    fn backspace_removes_one_kana_at_a_time() {
        // Arrange
        let mut simulator = simulator();
        simulator.run("tatoeb");
        assert_eq!(simulator.text, "たとえb");

        // Act / Assert
        simulator.run("{bs}");
        assert_eq!(simulator.text, "たとえ");
        simulator.run("{bs}");
        assert_eq!(simulator.text, "たと");
        simulator.run("{bs}{bs}");
        assert_eq!(simulator.text, "");
        assert_eq!(simulator.engine.view().mode, Mode::Direct);
    }

    #[test]
    fn backspace_after_a_doubled_consonant_keeps_the_small_tsu() {
        // Arrange
        let mut simulator = simulator();
        simulator.run("{henkan}gakka");
        assert_eq!(simulator.text, "がっか");

        // Act
        simulator.run("{bs}");
        let after_backspace = simulator.text.clone();
        simulator.run("i");

        // Assert
        // 「っ」は次のかなと "k" を共有しているので、"k" は残る。
        assert_eq!(after_backspace, "がっk");
        assert_eq!(simulator.text, "がっき");
    }

    #[test]
    fn backspace_while_converting_returns_to_kana() {
        // Arrange
        let mut simulator = simulator();
        simulator.run("kyou ");

        // Act
        simulator.run("{bs}");

        // Assert
        assert_eq!(simulator.text, "きょう");
        assert_eq!(simulator.engine.view().mode, Mode::Composing);
    }

    #[test]
    fn enter_commits_kana_and_resolves_a_trailing_n() {
        assert_eq!(typed("hon{ja}{enter}"), "ほん");
        assert_eq!(typed("tatoeba{enter}"), "たとえば");
    }

    #[test]
    fn prolonged_sound_mark_is_part_of_the_word() {
        // Arrange
        let mut simulator = simulator();

        // Act
        simulator.run("ko-hi-");

        // Assert
        assert_eq!(simulator.text, "こーひー");
    }

    #[test]
    fn katakana_key_selects_the_katakana_candidate() {
        assert_eq!(typed("tatoeba{kata}"), "タトエバ");
        assert_eq!(typed("tatoeba{kata}{enter}"), "タトエバ");
    }

    #[test]
    fn uppercase_words_are_english() {
        assert_eq!(typed("Kore Tatoeba "), "Kore Tatoeba ");
        assert_eq!(typed("tatoE"), "tatoE");
    }

    #[test]
    fn words_after_a_symbol_are_not_evaluated_until_a_space() {
        assert_eq!(typed("src/kore "), "src/kore ");
        assert_eq!(typed("a_tatoeba "), "a_tatoeba ");
        assert_eq!(typed("home.tatoeba kore "), "home.tatoeba これ");
    }

    #[test]
    fn an_opening_bracket_starts_a_new_word() {
        assert_eq!(typed("(tatoe"), "(たとえ");
    }

    #[test]
    fn moving_the_cursor_commits_the_composition_as_shown() {
        // Arrange
        let mut simulator = simulator();
        simulator.run("tatoe");

        // Act
        simulator.run("{other}");
        let after_move = simulator.engine.view().mode;
        simulator.run("x");

        // Assert
        assert_eq!(after_move, Mode::Direct);
        assert_eq!(simulator.text, "たとえx");
    }

    #[test]
    fn detection_continues_after_deleting_into_the_previous_word() {
        // 日本語の文には単語の区切りが無い。消した位置から、新しい入力として判定する。
        assert_eq!(typed("xy {bs}{bs}tatoe"), "xたとえ");
    }

    #[test]
    fn a_pause_starts_a_new_sequence() {
        // 英語と決まった単語の続きでも、間隔を空ければ判定し直す。
        assert_eq!(typed("xy{wait}tatoe"), "xyたとえ");
        assert_eq!(typed("src/{wait}kore "), "src/これ");
        // 間隔を空けなければ、同じ単語の続きとして通す。
        assert_eq!(typed("xytatoe "), "xytatoe ");
    }

    #[test]
    fn a_pause_after_an_english_word_resumes_japanese_flow() {
        // 英単語の後ろに空白を入れなくても、間隔を空ければ日本語の入力に戻る。
        assert_eq!(typed("kore Tauri{wait}de "), "これTauriで");
        assert_eq!(typed("kore test{wait}wo "), "これtestを");
    }

    #[test]
    fn a_word_that_timed_out_can_still_be_reread() {
        assert_eq!(typed("made{wait}{ja}"), "まで");
    }

    #[test]
    fn retyping_a_word_from_its_start_is_evaluated_again() {
        assert_eq!(typed("kox{bs}{bs}{bs}tatoe"), "たとえ");
    }

    #[test]
    fn undetermined_letters_are_kept_as_preedit() {
        // Arrange
        let mut simulator = simulator();

        // Act
        simulator.run("tato");

        // Assert
        assert_eq!(simulator.committed, "");
        assert_eq!(simulator.engine.preedit(), "tato");
    }

    #[test]
    fn letters_are_committed_as_soon_as_the_word_is_english() {
        // Arrange
        let mut simulator = simulator();

        // Act / Assert
        simulator.run("he");
        assert_eq!(simulator.committed, "");
        simulator.run("l");
        assert_eq!(simulator.committed, "hel");
        assert_eq!(simulator.engine.preedit(), "");

        // 英語と決まった単語の残りは、間隔を空けずに打てば、そのまま入力先へ通る。
        let output = simulator.engine.handle(Key::Char('l'), 200);
        assert_eq!(output, Output::default());
    }

    #[test]
    fn timeout_commits_the_undetermined_word() {
        // Arrange
        let mut engine = simulator().engine;
        for (index, c) in "kore".chars().enumerate() {
            engine.handle(Key::Char(c), index as u64 * 50);
        }
        assert_eq!(engine.timeout_deadline(), Some(350));

        // Act
        let early = engine.handle_timeout(300);
        let late = engine.handle_timeout(351);

        // Assert
        assert_eq!(early.commit, "");
        assert_eq!(early.preedit, "kore");
        assert_eq!(late.commit, "kore");
        assert_eq!(late.preedit, "");
        assert_eq!(engine.timeout_deadline(), None);
    }

    #[test]
    fn a_key_after_the_timeout_commits_the_word_first() {
        // Arrange
        let mut engine = simulator().engine;
        engine.handle(Key::Char('k'), 0);
        engine.handle(Key::Char('o'), 50);

        // Act: タイマーが発火する前に次の打鍵が届いた場合
        let output = engine.handle(Key::Char('r'), 1000);

        // Assert
        // "ko" を確定し、"r" から新しい入力として判定を始める。
        assert_eq!(
            output,
            Output {
                consumed: true,
                reread: String::new(),
                commit: "ko".to_string(),
                preedit: "r".to_string(),
                focus: None,
            }
        );
    }

    #[test]
    fn backspace_edits_the_undetermined_word() {
        // Arrange
        let mut simulator = simulator();
        simulator.run("kor");

        // Act
        simulator.run("{bs}");

        // Assert
        assert_eq!(simulator.committed, "");
        assert_eq!(simulator.engine.preedit(), "ko");

        simulator.run("{bs}{bs}tatoe");
        assert_eq!(simulator.text, "たとえ");
    }

    #[test]
    fn enter_commits_the_undetermined_word_and_passes_through() {
        assert_eq!(typed("kore\n"), "kore\n");
    }

    #[test]
    fn escape_commits_the_undetermined_word_as_english() {
        // Arrange
        let mut simulator = simulator();
        simulator.run("kor");

        // Act
        let output = simulator.engine.handle(Key::Escape, 200);

        // Assert
        assert_eq!(
            output,
            Output {
                consumed: true,
                reread: String::new(),
                commit: "kor".to_string(),
                preedit: String::new(),
                focus: None,
            }
        );
    }

    #[test]
    fn switching_to_japanese_replaces_the_preedit_without_committing() {
        // Arrange
        let mut simulator = simulator();
        simulator.run("tato");

        // Act
        let output = simulator.engine.handle(Key::Char('e'), 250);

        // Assert
        assert_eq!(
            output,
            Output {
                consumed: true,
                reread: String::new(),
                commit: String::new(),
                preedit: "たとえ".to_string(),
                focus: None,
            }
        );
    }

    #[test]
    fn committing_a_candidate_reports_the_text_and_clears_the_preedit() {
        // Arrange
        let mut simulator = simulator();
        simulator.run("kyou ");
        assert_eq!(simulator.engine.preedit(), "今日");

        // Act
        let output = simulator.engine.handle(Key::Enter, 10_000);

        // Assert
        assert_eq!(
            output,
            Output {
                consumed: true,
                reread: String::new(),
                commit: "今日".to_string(),
                preedit: String::new(),
                focus: None,
            }
        );
    }

    #[test]
    fn rereading_a_committed_word_deletes_it_from_the_document() {
        // Arrange
        let mut simulator = simulator();
        simulator.run("made ");
        assert_eq!(simulator.committed, "made ");
        let target = simulator.engine.reread_target();

        // Act
        let output = simulator.engine.handle(Key::ForceJapanese, 10_000);

        // Assert
        // "made" と空白の 5 文字を消して、「まで」を未確定文字列にする。
        assert_eq!(
            output,
            Output {
                consumed: true,
                reread: "made ".to_string(),
                commit: String::new(),
                preedit: "まで".to_string(),
                focus: None,
            }
        );
        // 入力先は、打鍵を渡す前にこの文字列で照合する。
        assert_eq!(target, "made ");
    }

    #[test]
    fn an_undetermined_word_has_nothing_to_reread_from_the_document() {
        // Arrange
        let mut engine = simulator().engine;
        for (index, c) in "made".chars().enumerate() {
            engine.handle(Key::Char(c), index as u64 * 50);
        }

        // Act
        let while_typing = engine.reread_target();
        // 間隔が空いた後も、単語はまだ入力先に確定されていない。
        let output = engine.handle(Key::ForceJapanese, 5_000);

        // Assert
        assert_eq!(while_typing, "");
        assert_eq!(output.reread, "");
    }

    #[test]
    fn rereading_a_word_that_just_timed_out_neither_commits_nor_deletes() {
        // Arrange
        let mut engine = simulator().engine;
        for (index, c) in "made".chars().enumerate() {
            engine.handle(Key::Char(c), index as u64 * 50);
        }

        // Act: タイマーが発火する前に読み直しのキーが届いた場合
        let output = engine.handle(Key::ForceJapanese, 5_000);

        // Assert
        assert_eq!(
            output,
            Output {
                consumed: true,
                reread: String::new(),
                commit: String::new(),
                preedit: "まで".to_string(),
                focus: None,
            }
        );
    }

    #[test]
    fn commit_composition_returns_what_is_shown() {
        // Arrange
        let mut simulator = simulator();
        simulator.run("kyouha");
        assert_eq!(simulator.engine.preedit(), "きょうは");

        // Act
        let text = simulator.engine.commit_composition();

        // Assert
        assert_eq!(text, "きょうは");
        assert_eq!(simulator.engine.preedit(), "");
        assert_eq!(simulator.engine.view().mode, Mode::Direct);
        assert_eq!(simulator.engine.commit_composition(), "");
    }

    fn typed_in_japanese_mode(script: &str) -> String {
        let mut simulator = simulator_with(false);
        simulator.run(script);
        simulator.text
    }

    #[test]
    fn the_engine_starts_in_japanese_mode_unless_auto_switch_is_on() {
        // Arrange / Act
        let default = Engine::new(sample_detector(), Learning::in_memory(), EngineConfig::default());
        let auto = simulator_with(true).engine;

        // Assert
        assert_eq!(default.input_mode(), InputMode::Japanese);
        assert_eq!(auto.input_mode(), InputMode::Auto);
    }

    #[test]
    fn japanese_mode_reads_an_english_word_as_kana() {
        // 自動切り替えでは、「home」は英語と判定される。
        assert_eq!(typed("home "), "home ");
        assert_eq!(typed_in_japanese_mode("home{enter}"), "ほめ");
    }

    #[test]
    fn japanese_mode_keeps_a_slowly_typed_word_as_kana() {
        // Arrange
        let mut simulator = simulator_with(false);
        simulator.interval_ms = 1000;

        // Act
        simulator.run("kyou");

        // Assert
        assert_eq!(simulator.text, "きょう");
        assert_eq!(simulator.committed, "");
        assert_eq!(simulator.engine.timeout_deadline(), None);
    }

    #[test]
    fn japanese_mode_converts_and_continues_with_the_next_word() {
        // Arrange
        let mut simulator = simulator_with(false);

        // Act
        simulator.run("kyou ha");

        // Assert
        assert_eq!(simulator.committed, "今日");
        assert_eq!(simulator.text, "今日は");
        assert_eq!(simulator.engine.view().mode, Mode::Composing);
    }

    #[test]
    fn japanese_mode_composes_symbols_and_digits() {
        assert_eq!(typed_in_japanese_mode("kyou{enter}."), "きょう。");
        assert_eq!(typed_in_japanese_mode("123"), "123");
        assert_eq!(typed_in_japanese_mode("-"), "ー");
    }

    #[test]
    fn japanese_mode_passes_keys_through_when_nothing_is_typed() {
        // Arrange
        let mut simulator = simulator_with(false);

        // Act
        let space = simulator.engine.handle(Key::Space, 0);
        let enter = simulator.engine.handle(Key::Enter, 50);
        let backspace = simulator.engine.handle(Key::Backspace, 100);
        let reread = simulator.engine.handle(Key::ForceJapanese, 150);

        // Assert
        assert_eq!(space, Output::default());
        assert_eq!(enter, Output::default());
        assert_eq!(backspace, Output::default());
        assert_eq!(reread, Output::default());
        assert_eq!(simulator.engine.reread_target(), "");
    }

    #[test]
    fn escape_in_japanese_mode_cancels_the_composition() {
        // Arrange
        let mut simulator = simulator_with(false);
        simulator.run("kyou");

        // Act
        let output = simulator.engine.handle(Key::Escape, 1_000);

        // Assert
        assert_eq!(
            output,
            Output {
                consumed: true,
                ..Output::default()
            }
        );
        assert_eq!(simulator.engine.view().mode, Mode::Direct);
    }

    #[test]
    fn escape_while_converting_in_japanese_mode_returns_to_kana() {
        assert_eq!(typed_in_japanese_mode("kyou {esc}"), "きょう");
    }

    #[test]
    fn restoring_letters_in_japanese_mode_is_not_remembered() {
        // Arrange: 自動切り替えでは、英字に戻すとその単語を英語として覚える
        let mut auto = simulator();
        auto.run("kyou{en} kyou");
        assert_eq!(auto.text, "kyou kyou");
        let mut simulator = simulator_with(false);

        // Act: 日本語入力で英字に戻してから、自動切り替えにする
        simulator.run("kyou{en}");
        assert_eq!(simulator.text, "kyou");
        simulator.clear();
        simulator.engine.set_config(EngineConfig {
            auto_switch: true,
            ..EngineConfig::default()
        });
        simulator.run("kyou");

        // Assert
        assert_eq!(simulator.text, "きょう");
    }

    #[test]
    fn henkan_in_japanese_mode_changes_nothing() {
        // Arrange
        let mut simulator = simulator_with(false);

        // Act / Assert
        simulator.run("{henkan}");
        assert!(!simulator.engine.view().forced);
        assert_eq!(simulator.engine.input_mode(), InputMode::Japanese);

        simulator.run("kyou{henkan}");
        assert_eq!(simulator.text, "きょう");
        assert_eq!(simulator.engine.view().mode, Mode::Composing);

        simulator.run(" {henkan}");
        assert_eq!(simulator.text, "今日");
        assert_eq!(simulator.engine.view().mode, Mode::Converting);
    }

    #[test]
    fn muhenkan_in_japanese_mode_commits_what_is_shown() {
        // Arrange
        let mut simulator = simulator_with(false);

        // Act
        simulator.run("kyou{muhenkan}kyou ");

        // Assert
        assert_eq!(simulator.text, "きょうkyou ");
        assert_eq!(simulator.engine.input_mode(), InputMode::Direct);
    }

    #[test]
    fn henkan_enters_the_configured_mode_from_the_other_japanese_side_mode() {
        // Arrange: 自動切り替えは無効。入力ソースの一覧で、自動切り替えのモードが選ばれた
        let mut simulator = simulator_with(false);
        simulator.engine.set_input_mode(InputMode::Auto);
        simulator.run("kyou");

        // Act
        simulator.run("{henkan}");

        // Assert
        // 入力中の文字列は、表示されているとおりに確定する。
        assert_eq!(simulator.text, "きょう");
        assert_eq!(simulator.engine.input_mode(), InputMode::Japanese);
    }

    fn leaving_simulator() -> Simulator {
        Simulator::new(Engine::new(
            sample_detector(),
            Learning::in_memory(),
            EngineConfig {
                leave_japanese_for_english: true,
                ..EngineConfig::default()
            },
        ))
    }

    #[test]
    fn an_uppercase_letter_leaves_japanese_input() {
        // Arrange
        let mut simulator = leaving_simulator();
        simulator.run("kyouha");

        // Act
        let output = simulator.engine.handle(Key::Char('I'), 10_000);

        // Assert
        // かなは表示のとおりに確定し、大文字は直接入力の打鍵として入力先に通す。
        assert_eq!(output.commit, "きょうは");
        assert!(!output.consumed);
        assert_eq!(output.preedit, "");
        assert_eq!(simulator.engine.input_mode(), InputMode::Direct);
    }

    #[test]
    fn an_uppercase_first_letter_leaves_japanese_input() {
        // Arrange: 何も入力していない
        let mut simulator = leaving_simulator();

        // Act: "I " と打つ
        let letter = simulator.engine.handle(Key::Char('I'), 10_000);
        let space = simulator.engine.handle(Key::Space, 10_050);

        // Assert
        // 1 文字目の大文字で抜ける。大文字も続く空白も、直接入力の打鍵として入力先に通る。
        assert_eq!(simulator.engine.input_mode(), InputMode::Direct);
        assert!(!letter.consumed && letter.commit.is_empty() && letter.preedit.is_empty());
        assert!(!space.consumed);
    }

    #[test]
    fn an_uppercase_letter_while_converting_commits_the_conversion_and_leaves() {
        // Arrange
        let mut simulator = leaving_simulator();
        simulator.run("kyouha ");

        // Act
        let output = simulator.engine.handle(Key::Char('T'), 10_000);

        // Assert
        assert_eq!(output.commit, "今日は");
        assert!(!output.consumed);
        assert_eq!(simulator.engine.input_mode(), InputMode::Direct);
    }

    #[test]
    fn letters_that_are_not_romaji_leave_japanese_input() {
        // Arrange
        let mut simulator = leaving_simulator();

        // Act: "pl" で始まるかなは無い
        simulator.run("kyouhapl");

        // Assert
        // 読める所まではかな、残りは打ったままの英字で確定する。
        assert_eq!(simulator.text, "きょうはpl");
        assert_eq!(simulator.engine.preedit(), "");
        assert_eq!(simulator.engine.input_mode(), InputMode::Direct);
    }

    #[test]
    fn romaji_does_not_leave_japanese_input() {
        // Arrange
        let mut simulator = leaving_simulator();

        // Act: 促音、撥音、拗音、長音を含む入力
        simulator.run("kocchihasinkansennnonyu-su");

        // Assert
        assert_eq!(simulator.engine.input_mode(), InputMode::Japanese);
        assert_eq!(simulator.text, "こっちはしんかんせんのにゅーす");
    }

    #[test]
    fn rare_but_finished_romaji_does_not_leave_japanese_input() {
        // Arrange
        let mut simulator = leaving_simulator();

        // Act: 英語の綴りに多い "th" "dh" "tw" "kw" で始まるかなと、"zh" (←)、"wi" (うぃ)
        simulator.run("thidhitwukwazhwi");

        // Assert
        assert_eq!(simulator.engine.input_mode(), InputMode::Japanese);
        assert_eq!(simulator.engine.preedit(), "てぃでぃとぅくぁ←うぃ");
    }

    #[test]
    fn letters_outside_the_romaji_rules_leave_japanese_input() {
        // Arrange
        let mut simulator = leaving_simulator();

        // Act: "st" で始まるかなは無い
        simulator.run("korehast");

        // Assert
        assert_eq!(simulator.text, "これはst");
        assert_eq!(simulator.engine.input_mode(), InputMode::Direct);
    }

    #[test]
    fn a_symbol_before_english_leaves_japanese_input() {
        // Arrange
        let mut simulator = leaving_simulator();
        simulator.run("kyouha");

        // Act
        let output = simulator.engine.handle(Key::Char('@'), 10_000);

        // Assert
        // かなは表示のとおりに確定し、記号は直接入力の打鍵として入力先に通す。
        assert_eq!(output.commit, "きょうは");
        assert!(!output.consumed);
        assert_eq!(simulator.engine.input_mode(), InputMode::Direct);
    }

    #[test]
    fn a_symbol_with_nothing_typed_leaves_japanese_input() {
        // Arrange
        let mut simulator = leaving_simulator();

        // Act
        simulator.run("#");

        // Assert
        assert_eq!(simulator.text, "#");
        assert_eq!(simulator.engine.input_mode(), InputMode::Direct);
    }

    #[test]
    fn japanese_symbols_do_not_leave_japanese_input() {
        // Arrange
        let mut simulator = leaving_simulator();

        // Act
        simulator.run("[ra-menn~],.kan'i!?");

        // Assert
        assert_eq!(simulator.text, "「らーめん〜」、。かんい!?");
        assert_eq!(simulator.engine.input_mode(), InputMode::Japanese);
    }

    #[test]
    fn a_symbol_in_a_romaji_rule_does_not_leave_japanese_input() {
        // Arrange
        let mut simulator = leaving_simulator();

        // Act: "z/" は「・」
        simulator.run("z/");

        // Assert
        assert_eq!(simulator.text, "・");
        assert_eq!(simulator.engine.input_mode(), InputMode::Japanese);
    }

    #[test]
    fn kana_after_digits_and_symbols_stays_kana_when_leaving() {
        // Arrange
        let mut simulator = leaving_simulator();

        // Act
        simulator.run("3gatsu?korehast");

        // Assert
        assert_eq!(simulator.text, "3がつ?これはst");
        assert_eq!(simulator.engine.input_mode(), InputMode::Direct);
    }

    #[test]
    fn letters_after_a_wide_character_are_read_without_breaking() {
        // Arrange: JIS キーボードの ¥ キーは、2 バイトの文字として届く
        let mut simulator = leaving_simulator();

        // Act
        simulator.run("¥st");

        // Assert
        assert_eq!(simulator.text, "¥st");
        assert_eq!(simulator.engine.input_mode(), InputMode::Direct);
    }

    #[test]
    fn a_space_with_nothing_typed_leaves_japanese_input() {
        // Arrange
        let mut simulator = leaving_simulator();
        simulator.run("kyouha\n");

        // Act
        simulator.run(" ");

        // Assert
        assert_eq!(simulator.text, "きょうは ");
        assert_eq!(simulator.engine.input_mode(), InputMode::Direct);
    }

    #[test]
    fn a_space_while_typing_converts_instead_of_leaving() {
        // Arrange
        let mut simulator = leaving_simulator();

        // Act
        simulator.run("kyouha ");

        // Assert
        assert_eq!(simulator.text, "今日は");
        assert_eq!(simulator.engine.input_mode(), InputMode::Japanese);
    }

    #[test]
    fn japanese_input_stays_when_leaving_is_off() {
        // Arrange: 既定では無効
        let mut simulator = simulator_with(false);

        // Act
        simulator.run("kyouhathI");

        // Assert
        assert_eq!(simulator.engine.input_mode(), InputMode::Japanese);
    }

    #[test]
    fn pressing_henkan_twice_starts_the_english_check() {
        // Arrange
        let mut simulator = simulator_with(false);

        // Act: 1 度目は日本語入力のまま何も変えず、続けて押した 2 度目で英文チェックになる
        simulator.run("{henkan}{henkan}");

        // Assert
        assert_eq!(simulator.engine.input_mode(), InputMode::English);
        assert_eq!(simulator.text, "");
    }

    #[test]
    fn henkan_pressed_twice_while_composing_is_not_a_double_press() {
        // Arrange: 自動切り替えでは、入力中の「かな」は変換と次の候補
        let mut simulator = simulator();
        simulator.run("{henkan}.");

        // Act
        simulator.run("{henkan}{henkan}");

        // Assert
        assert_eq!(simulator.engine.input_mode(), InputMode::Auto);
        assert_eq!(simulator.text, "…");
    }

    #[test]
    fn a_henkan_marked_outside_the_engine_counts_as_the_first_press() {
        // Arrange: OS が「かな」で入力メソッドを選び直した。その打鍵はエンジンに渡らない
        let mut simulator = simulator_with(false);
        simulator.engine.set_input_mode(InputMode::Direct);
        simulator.engine.set_input_mode(InputMode::Japanese);
        simulator.engine.mark_henkan(1_000);

        // Act
        simulator.engine.handle(Key::Henkan, 1_200);

        // Assert
        assert_eq!(simulator.engine.input_mode(), InputMode::English);
    }

    #[test]
    fn a_slow_second_henkan_does_not_start_the_english_check() {
        // Arrange
        let mut simulator = simulator_with(false);
        simulator.engine.handle(Key::Henkan, 1_000);

        // Act
        simulator.engine.handle(Key::Henkan, 1_000 + DOUBLE_PRESS_MS + 1);

        // Assert
        assert_eq!(simulator.engine.input_mode(), InputMode::Japanese);
        // 間が空いた 2 度目は、次の 2 度押しの 1 度目になる。
        simulator.engine.handle(Key::Henkan, 1_000 + DOUBLE_PRESS_MS + 100);
        assert_eq!(simulator.engine.input_mode(), InputMode::English);
    }

    #[test]
    fn a_key_between_two_henkan_presses_is_not_a_double_press() {
        // Arrange
        let mut simulator = simulator_with(false);

        // Act: 400 ミリ秒の内に「かな」「a」「かな」
        simulator.run("{henkan}a{henkan}");

        // Assert
        assert_eq!(simulator.engine.input_mode(), InputMode::Japanese);
        assert_eq!(simulator.engine.preedit(), "あ");
    }

    #[test]
    fn muhenkan_twice_stays_in_direct_input() {
        // Arrange
        let mut simulator = simulator_with(false);

        // Act
        simulator.run("{muhenkan}{muhenkan}");

        // Assert
        assert_eq!(simulator.engine.input_mode(), InputMode::Direct);
    }

    #[test]
    fn a_modified_key_commits_the_text_of_the_english_check_and_passes() {
        // Arrange
        let mut simulator = simulator_with(false);
        simulator.run("{henkan}{henkan}wait");

        // Act: Option を付けた打鍵 (ダッシュの入力など)
        let output = simulator.engine.handle(Key::Other, 10_000);

        // Assert
        assert_eq!(output.commit, "wait");
        assert!(!output.consumed);
        assert_eq!(output.preedit, "");
        assert_eq!(simulator.engine.input_mode(), InputMode::English);
    }

    #[test]
    fn a_config_change_keeps_the_english_check() {
        // Arrange
        let mut simulator = simulator_with(false);
        simulator.run("{henkan}{henkan}pen");

        // Act
        let output = simulator.engine.set_config(EngineConfig {
            auto_switch: true,
            ..EngineConfig::default()
        });

        // Assert
        assert_eq!(output.commit, "");
        assert_eq!(simulator.engine.input_mode(), InputMode::English);
        assert_eq!(simulator.engine.preedit(), "pen");
    }

    #[test]
    fn replacing_without_a_text_leaves_the_key_to_the_client() {
        // Arrange: 英文チェックで、英文をまだ受け取っていない
        let mut simulator = simulator_with(false);
        simulator.run("{henkan}{henkan}");

        // Act
        let output = simulator.engine.replace_english("I have a pen");

        // Assert
        assert!(!output.consumed);
        assert_eq!(output.preedit, "");
    }

    #[test]
    fn the_english_check_holds_the_text_until_enter() {
        // Arrange
        let mut simulator = simulator_with(false);
        simulator.run("{henkan}{henkan}");

        // Act
        simulator.run("I has a pen");

        // Assert: 入力先にはまだ出ていない
        let view = simulator.engine.view();
        assert_eq!(view.mode, Mode::Checking);
        assert_eq!(view.reading, "I has a pen");
        assert_eq!(simulator.engine.preedit(), "I has a pen");

        // Act
        simulator.run("{enter}");

        // Assert
        assert_eq!(simulator.text, "I has a pen");
        assert_eq!(simulator.engine.preedit(), "");
        assert_eq!(simulator.engine.input_mode(), InputMode::English);
    }

    #[test]
    fn the_english_check_edits_and_discards_the_text() {
        // Arrange
        let mut simulator = simulator_with(false);
        simulator.run("{henkan}{henkan}pens");

        // Act
        simulator.run("{bs}");
        assert_eq!(simulator.engine.preedit(), "pen");
        simulator.run("{esc}");

        // Assert: Esc は英文を捨てるだけで、英文チェックは続く
        assert_eq!(simulator.engine.preedit(), "");
        assert_eq!(simulator.engine.input_mode(), InputMode::English);
        assert_eq!(simulator.text, "");

        // Act: 英文が空の時の Esc は、英文チェックをやめる
        simulator.run("{esc}");

        // Assert
        assert_eq!(simulator.engine.input_mode(), InputMode::Direct);
    }

    #[test]
    fn keys_pass_through_the_english_check_while_the_text_is_empty() {
        // Arrange
        let mut simulator = simulator_with(false);
        simulator.run("{henkan}{henkan}");

        // Act
        let space = simulator.engine.handle(Key::Space, 10_000);
        let enter = simulator.engine.handle(Key::Enter, 10_050);

        // Assert
        assert!(!space.consumed);
        assert!(!enter.consumed);
    }

    #[test]
    fn henkan_commits_the_text_of_the_english_check_and_returns_to_japanese() {
        // Arrange
        let mut simulator = simulator_with(false);
        simulator.run("{henkan}{henkan}pen");

        // Act
        simulator.run("{henkan}");

        // Assert
        assert_eq!(simulator.text, "pen");
        assert_eq!(simulator.engine.input_mode(), InputMode::Japanese);
        // 抜けるのに押した「かな」は、次の 2 度押しの 1 度目に数えない。
        simulator.run("{henkan}");
        assert_eq!(simulator.engine.input_mode(), InputMode::Japanese);
    }

    #[test]
    fn muhenkan_commits_the_text_of_the_english_check_and_leaves_for_direct_input() {
        // Arrange
        let mut simulator = simulator_with(false);
        simulator.run("{henkan}{henkan}ink");

        // Act
        let output = simulator.engine.handle(Key::Muhenkan, 100_000);

        // Assert
        assert_eq!(output.commit, "ink");
        assert_eq!(simulator.engine.input_mode(), InputMode::Direct);
    }

    #[test]
    fn the_text_of_the_english_check_is_replaced_by_a_correction() {
        // Arrange
        let mut simulator = simulator_with(false);
        simulator.run("{henkan}{henkan}I has a pen");

        // Act
        let output = simulator.engine.replace_english("I have a pen");

        // Assert
        assert_eq!(output.preedit, "I have a pen");
        assert_eq!(output.commit, "");
        simulator.run("{enter}");
        assert_eq!(simulator.text, "I have a pen");
    }

    #[test]
    fn muhenkan_in_direct_input_changes_nothing() {
        // Arrange
        let mut simulator = simulator_with(false);
        simulator.run("{muhenkan}home");

        // Act
        let output = simulator.engine.handle(Key::Muhenkan, 10_000);

        // Assert
        // 入力先へ通すと、空白を入力する入力先がある。
        assert!(output.consumed);
        assert_eq!(output.commit, "");
        assert_eq!(output.preedit, "");
        assert_eq!(simulator.engine.input_mode(), InputMode::Direct);
    }

    #[test]
    fn henkan_returns_from_direct_input_to_japanese_mode() {
        // Arrange
        let mut simulator = simulator_with(false);
        simulator.run("{muhenkan}home ");

        // Act
        simulator.run("{henkan}home");

        // Assert
        assert_eq!(simulator.text, "home ほめ");
        assert_eq!(simulator.engine.input_mode(), InputMode::Japanese);
    }

    #[test]
    fn turning_auto_switch_on_commits_the_composition_and_changes_the_mode() {
        // Arrange
        let mut simulator = simulator_with(false);
        simulator.run("kyou");

        // Act
        let output = simulator.engine.set_config(EngineConfig {
            auto_switch: true,
            ..EngineConfig::default()
        });

        // Assert
        assert_eq!(
            output,
            Output {
                consumed: true,
                commit: "きょう".to_string(),
                ..Output::default()
            }
        );
        assert_eq!(simulator.engine.input_mode(), InputMode::Auto);
    }

    #[test]
    fn turning_auto_switch_off_commits_the_undetermined_word() {
        // Arrange
        let mut simulator = simulator();
        simulator.run("me");
        assert_eq!(simulator.committed, "");

        // Act
        let output = simulator.engine.set_config(EngineConfig::default());

        // Assert
        assert_eq!(output.commit, "me");
        assert_eq!(output.preedit, "");
        assert_eq!(simulator.engine.input_mode(), InputMode::Japanese);
        assert_eq!(simulator.engine.timeout_deadline(), None);
    }

    #[test]
    fn changing_the_config_in_direct_input_only_changes_where_henkan_goes() {
        // Arrange
        let mut simulator = simulator_with(false);
        simulator.run("{muhenkan}");

        // Act
        let output = simulator.engine.set_config(EngineConfig {
            auto_switch: true,
            ..EngineConfig::default()
        });
        let mode_after_config = simulator.engine.input_mode();
        simulator.run("{henkan}");

        // Assert
        assert_eq!(output.commit, "");
        assert_eq!(mode_after_config, InputMode::Direct);
        assert_eq!(simulator.engine.input_mode(), InputMode::Auto);
    }

    #[test]
    fn a_config_change_that_keeps_the_mode_keeps_the_composition() {
        // Arrange
        let mut simulator = simulator_with(false);
        simulator.run("kyou");

        // Act
        let output = simulator.engine.set_config(EngineConfig {
            sequence_timeout_ms: 500,
            ..EngineConfig::default()
        });

        // Assert
        assert_eq!(output.commit, "");
        assert_eq!(output.preedit, "きょう");
    }
}

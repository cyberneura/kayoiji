//! 絵文字モード。検索語をローマ字で打って絵文字を探すか、グループごとの一覧から選ぶ。

use std::sync::Arc;

use serde::Serialize;

use crate::emoji::{Emoji, EmojiId};
use crate::romaji::RomajiTable;

/// 一覧の 1 行に並べる数。上下の矢印は、この数だけ進む。
pub const EMOJI_COLUMNS: usize = 10;
/// 検索で出す数の上限。1 文字目では、多くのキーワードが一致する。
const SEARCH_LIMIT: usize = 300;

/// 候補ウインドウに出す、絵文字モードの内容。絵文字そのものは `View::candidates` に入る。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EmojiView {
    /// 打っている検索語 (かな)。
    pub query: String,
    pub groups: Vec<String>,
    /// 一覧を見ているグループ。検索している間は `None`。
    pub group: Option<usize>,
    pub columns: usize,
    /// 選んでいる絵文字の名前。
    pub name: String,
    /// 肌の色の違いを出している時の、基本の絵文字と色違い。
    pub variants: Vec<String>,
    /// `variants` の中で選んでいる位置。
    pub variant: Option<usize>,
}

pub(crate) struct EmojiSearch {
    emoji: Arc<Emoji>,
    /// 検索語として打ったままの打鍵。
    raw: String,
    group: usize,
    items: Vec<EmojiId>,
    selected: usize,
    /// 肌の色の違いを出している時の、その中で選んでいる位置。0 が基本の絵文字。
    variant: Option<usize>,
}

impl EmojiSearch {
    pub(crate) fn new(emoji: Arc<Emoji>) -> Self {
        let mut search = Self {
            emoji,
            raw: String::new(),
            group: 0,
            items: Vec::new(),
            selected: 0,
            variant: None,
        };
        search.refresh();
        search
    }

    fn query(&self) -> String {
        RomajiTable::global().parse(&self.raw).display()
    }

    /// 打鍵に合わせて一覧を作り直す。選択は先頭に戻す。
    fn refresh(&mut self) {
        self.selected = 0;
        self.variant = None;
        if self.raw.is_empty() {
            let count = self.emoji.groups().get(self.group).map_or(0, |group| group.emoji.len());
            self.items = (0..count).map(|index| EmojiId { group: self.group, index }).collect();
            return;
        }
        // かなにした検索語に加えて、打ったままの英字で英語のキーワードも引く。
        // かなの側は、規則に育つ途中の末尾 ("k" など) を除く。
        let kana = RomajiTable::global().parse(&self.raw).text();
        self.items = self.emoji.search(&[&kana, &self.raw], SEARCH_LIMIT);
    }

    pub(crate) fn type_char(&mut self, c: char) {
        self.raw.extend(c.to_lowercase());
        self.refresh();
    }

    /// 検索語の最後の打鍵を消す。消すものが無ければ false。
    pub(crate) fn backspace(&mut self) -> bool {
        if self.raw.pop().is_none() {
            return false;
        }
        self.refresh();
        true
    }

    /// 選択を動かす。肌の色の違いを出している間は、左右でその中を動き、上下で閉じる。
    pub(crate) fn move_selection(&mut self, columns: isize, rows: isize) {
        if let Some(variant) = self.variant {
            if rows != 0 {
                self.variant = None;
                return;
            }
            let count = self.variants().len() as isize;
            self.variant = Some((variant as isize + columns).clamp(0, count - 1) as usize);
            return;
        }
        if self.items.is_empty() {
            return;
        }
        let step = columns + rows * EMOJI_COLUMNS as isize;
        let last = self.items.len() as isize - 1;
        self.selected = (self.selected as isize + step).clamp(0, last) as usize;
    }

    /// 前後のグループの一覧にする。検索語は消す。検索していた時は、検索の前に見ていたグループの前後にする。
    pub(crate) fn switch_group(&mut self, forward: bool) {
        let count = self.emoji.groups().len();
        if count == 0 {
            return;
        }
        self.group = if forward { (self.group + 1) % count } else { (self.group + count - 1) % count };
        self.raw.clear();
        self.refresh();
    }

    /// 選んでいる絵文字の、肌の色の違いを出す。出していれば閉じる。
    pub(crate) fn toggle_variants(&mut self) {
        self.variant = match self.variant {
            Some(_) => None,
            None if self.variants().len() > 1 => Some(0),
            None => None,
        };
    }

    /// 閉じるものがあれば閉じて true を返す。
    pub(crate) fn close_variants(&mut self) -> bool {
        self.variant.take().is_some()
    }

    /// 選んでいる絵文字の、基本の形と肌の色の違い。
    fn variants(&self) -> Vec<String> {
        let Some(&id) = self.items.get(self.selected) else {
            return Vec::new();
        };
        let entry = self.emoji.entry(id);
        std::iter::once(&entry.text).chain(&entry.variants).cloned().collect()
    }

    /// Enter で確定する絵文字。
    pub(crate) fn chosen(&self) -> Option<String> {
        let id = *self.items.get(self.selected)?;
        match self.variant {
            Some(variant) => self.variants().get(variant).cloned(),
            None => Some(self.emoji.entry(id).text.clone()),
        }
    }

    pub(crate) fn items(&self) -> Vec<String> {
        self.items.iter().map(|&id| self.emoji.entry(id).text.clone()).collect()
    }

    pub(crate) fn selected(&self) -> Option<usize> {
        (!self.items.is_empty()).then_some(self.selected)
    }

    pub(crate) fn view(&self) -> EmojiView {
        let name = self.items.get(self.selected).map(|&id| self.emoji.entry(id).name.clone()).unwrap_or_default();
        EmojiView {
            query: self.query(),
            groups: self.emoji.groups().iter().map(|group| group.title.clone()).collect(),
            group: self.raw.is_empty().then_some(self.group),
            columns: EMOJI_COLUMNS,
            name,
            variants: if self.variant.is_some() { self.variants() } else { Vec::new() },
            variant: self.variant,
        }
    }
}

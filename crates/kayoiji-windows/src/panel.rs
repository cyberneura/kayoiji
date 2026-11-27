//! 候補ウインドウに描く内容。エンジンの表示内容 (`View`) から、行とその中の欄を作る。
//! 描くのは Win32 の側 (`tsf::window`) で、ここは OS に依存しないのでテストできる。

use kayoiji_core::engine::{InputMode, Mode, View};

/// 絵文字モードで一度に見せる行の数。macOS の候補ウインドウ (`src/lib/emoji.ts`) と同じ。
const EMOJI_VISIBLE_ROWS: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub text: String,
    pub selected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Panel {
    /// 上から順の行。行の中の欄は左から並べる。
    pub rows: Vec<Vec<Cell>>,
    /// 最後の行の下に、小さく出す説明。
    pub footer: String,
    /// 欄を同じ幅で並べるか (絵文字の格子)。false なら、欄の幅は文字に合わせる。
    pub grid: bool,
}

impl Panel {
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.footer.is_empty()
    }
}

fn cell(text: impl Into<String>, selected: bool) -> Cell {
    Cell {
        text: text.into(),
        selected,
    }
}

/// `view` の時に候補ウインドウに出す内容。出すものが無ければ空。
pub fn panel(view: &View) -> Panel {
    match view.mode {
        Mode::Direct | Mode::Checking => Panel::default(),
        Mode::Composing => Panel {
            rows: Vec::new(),
            footer: if view.input_mode == InputMode::Japanese {
                "Space 変換  Enter 確定  Esc 取消".to_string()
            } else {
                "Space 変換  Enter 確定  Esc 英字".to_string()
            },
            grid: false,
        },
        Mode::Converting => Panel {
            rows: view
                .candidates
                .iter()
                .enumerate()
                .map(|(index, candidate)| vec![cell(format!("{} {candidate}", index + 1), view.selected == Some(index))])
                .collect(),
            footer: format!("{}  {}/{}", view.reading, view.page + 1, view.page_count.max(1)),
            grid: false,
        },
        Mode::Emoji => emoji_panel(view),
    }
}

fn emoji_panel(view: &View) -> Panel {
    let Some(emoji) = &view.emoji else {
        return Panel::default();
    };
    let columns = emoji.columns.max(1);
    let selected_row = view.selected.map_or(0, |selected| selected / columns);
    let rows = view.candidates.len().div_ceil(columns);
    // 選んでいる行を、なるべく上から 3 行目に置く。
    let start = selected_row.saturating_sub(2).min(rows.saturating_sub(EMOJI_VISIBLE_ROWS));
    let title = match emoji.group {
        Some(group) => format!("{} {}/{}", emoji.groups.get(group).map_or("", String::as_str), group + 1, emoji.groups.len()),
        None => format!("検索 {}", emoji.query),
    };
    let mut lines = vec![vec![cell(title, false)]];
    for row in start..(start + EMOJI_VISIBLE_ROWS).min(rows) {
        let cells = (row * columns..((row + 1) * columns).min(view.candidates.len()))
            .map(|index| cell(view.candidates[index].clone(), view.selected == Some(index)))
            .collect();
        lines.push(cells);
    }
    if view.candidates.is_empty() {
        lines.push(vec![cell("見つかりません", false)]);
    }
    if !emoji.variants.is_empty() {
        lines.push(
            emoji
                .variants
                .iter()
                .enumerate()
                .map(|(index, text)| cell(text.clone(), emoji.variant == Some(index)))
                .collect(),
        );
    }
    Panel {
        rows: lines,
        footer: format!("{}  Shift+←→ 分類  Tab 肌の色  Enter 確定  Esc やめる", emoji.name),
        grid: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kayoiji_core::emoji_search::EmojiView;

    fn view(mode: Mode) -> View {
        View {
            input_mode: InputMode::Japanese,
            forced: false,
            mode,
            reading: String::new(),
            candidates: Vec::new(),
            selected: None,
            page: 0,
            page_count: 0,
            emoji: None,
        }
    }

    fn texts(panel: &Panel) -> Vec<Vec<String>> {
        panel
            .rows
            .iter()
            .map(|row| row.iter().map(|cell| cell.text.clone()).collect())
            .collect()
    }

    #[test]
    fn nothing_is_shown_in_direct_input() {
        assert!(panel(&view(Mode::Direct)).is_empty());
    }

    #[test]
    fn candidates_are_numbered_with_the_selected_one_marked() {
        // Arrange
        let view = View {
            reading: "きょう".to_string(),
            candidates: vec!["今日".to_string(), "京".to_string()],
            selected: Some(1),
            page: 0,
            page_count: 2,
            ..view(Mode::Converting)
        };

        // Act
        let panel = panel(&view);

        // Assert
        assert_eq!(texts(&panel), [["1 今日"], ["2 京"]]);
        assert!(!panel.rows[0][0].selected);
        assert!(panel.rows[1][0].selected);
        assert_eq!(panel.footer, "きょう  1/2");
    }

    #[test]
    fn the_emoji_grid_shows_six_rows_around_the_selection() {
        // Arrange: 2 列で 20 個 (10 行)。15 番目 (8 行目) を選んでいる
        let view = View {
            candidates: (0..20).map(|index| index.to_string()).collect(),
            selected: Some(15),
            emoji: Some(EmojiView {
                query: String::new(),
                groups: vec!["動物".to_string(), "旗".to_string()],
                group: Some(0),
                columns: 2,
                name: "猫".to_string(),
                variants: Vec::new(),
                variant: None,
            }),
            ..view(Mode::Emoji)
        };

        // Act
        let panel = panel(&view);

        // Assert: 見出しの後に、5 行目 (8 から) から 6 行
        let rows = texts(&panel);
        assert_eq!(rows[0], ["動物 1/2"]);
        assert_eq!(rows[1], ["8", "9"]);
        assert_eq!(rows.len(), 1 + 6);
        assert!(panel.rows[4][1].selected);
        assert!(panel.grid);
    }
}

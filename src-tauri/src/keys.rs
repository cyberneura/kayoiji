//! macOS のキーイベントを、エンジンのキーに読み替える。

use kayoiji_core::engine::{Key, Mode};

const KEY_RETURN: u16 = 36;
const KEY_TAB: u16 = 48;
const KEY_SPACE: u16 = 49;
const KEY_DELETE: u16 = 51;
const KEY_ESCAPE: u16 = 53;
const KEY_KEYPAD_ENTER: u16 = 76;
const KEY_F7: u16 = 98;
const KEY_JIS_EISU: u16 = 102;
const KEY_JIS_KANA: u16 = 104;
const KEY_F10: u16 = 109;
const KEY_LEFT: u16 = 123;
const KEY_RIGHT: u16 = 124;
const KEY_DOWN: u16 = 125;
const KEY_UP: u16 = 126;

#[derive(Debug, Clone, Copy, Default)]
pub struct KeyPress {
    pub key_code: u16,
    /// Shift だけを反映した、キーが表す文字。
    pub character: Option<char>,
    pub shift: bool,
    pub control: bool,
    pub option: bool,
    pub command: bool,
    pub repeat: bool,
}

/// `mode` によって意味が変わるキーがある。
pub fn translate(press: KeyPress, mode: Mode) -> Key {
    if press.option || press.command {
        return Key::Other;
    }
    if press.control {
        return match press.character {
            Some('j') => Key::ForceJapanese,
            Some(';') => Key::ForceEnglish,
            Some('k') => Key::Katakana,
            _ => Key::Other,
        };
    }
    match press.key_code {
        KEY_RETURN | KEY_KEYPAD_ENTER => Key::Enter,
        KEY_TAB => Key::Tab,
        KEY_SPACE if press.shift && mode == Mode::Converting => Key::Up,
        KEY_SPACE => Key::Space,
        KEY_DELETE => Key::Backspace,
        KEY_ESCAPE => Key::Escape,
        KEY_UP => Key::Up,
        KEY_DOWN => Key::Down,
        KEY_LEFT if press.shift => Key::ShiftLeft,
        KEY_RIGHT if press.shift => Key::ShiftRight,
        KEY_LEFT => Key::Left,
        KEY_RIGHT => Key::Right,
        // JIS 配列の「かな」「英数」。PC 用のキーボードの「変換」「無変換」も、macOS では同じキーとして届く。
        KEY_JIS_KANA => Key::Henkan,
        KEY_JIS_EISU => Key::Muhenkan,
        KEY_F10 => Key::ForceEnglish,
        KEY_F7 => Key::Katakana,
        _ => match press.character {
            // キーを押し続けて同じ文字を並べるのは、日本語の入力ではない。
            Some(_) if press.repeat && mode == Mode::Direct => Key::Other,
            Some(c) if !c.is_control() && !is_function_key(c) => Key::Char(c),
            _ => Key::Other,
        },
    }
}

/// 矢印キーやファンクションキーは、私用領域の文字として届く。
fn is_function_key(c: char) -> bool {
    ('\u{F700}'..='\u{F8FF}').contains(&c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn character(c: char) -> KeyPress {
        KeyPress {
            key_code: 0,
            character: Some(c),
            ..KeyPress::default()
        }
    }

    fn code(key_code: u16) -> KeyPress {
        KeyPress {
            key_code,
            ..KeyPress::default()
        }
    }

    #[test]
    fn plain_keys_map_to_engine_keys() {
        assert_eq!(translate(character('a'), Mode::Direct), Key::Char('a'));
        assert_eq!(translate(character('A'), Mode::Direct), Key::Char('A'));
        assert_eq!(translate(character('?'), Mode::Composing), Key::Char('?'));
        assert_eq!(translate(code(KEY_SPACE), Mode::Direct), Key::Space);
        assert_eq!(translate(code(KEY_RETURN), Mode::Composing), Key::Enter);
        assert_eq!(translate(code(KEY_DELETE), Mode::Composing), Key::Backspace);
        assert_eq!(translate(code(KEY_ESCAPE), Mode::Composing), Key::Escape);
    }

    #[test]
    fn control_shortcuts_map_to_language_keys() {
        let control = |c| KeyPress {
            control: true,
            ..character(c)
        };
        assert_eq!(translate(control('j'), Mode::Direct), Key::ForceJapanese);
        assert_eq!(translate(control(';'), Mode::Composing), Key::ForceEnglish);
        assert_eq!(translate(control('k'), Mode::Composing), Key::Katakana);
        assert_eq!(translate(control('c'), Mode::Composing), Key::Other);
    }

    #[test]
    fn japanese_keyboard_keys_switch_the_input_mode() {
        assert_eq!(translate(code(KEY_JIS_KANA), Mode::Direct), Key::Henkan);
        assert_eq!(translate(code(KEY_JIS_EISU), Mode::Composing), Key::Muhenkan);
        assert_eq!(translate(code(KEY_F10), Mode::Composing), Key::ForceEnglish);
        assert_eq!(translate(code(KEY_F7), Mode::Composing), Key::Katakana);
    }

    #[test]
    fn shortcuts_with_command_or_option_end_the_input() {
        let command = KeyPress {
            command: true,
            ..character('v')
        };
        let option = KeyPress {
            option: true,
            ..character('a')
        };
        assert_eq!(translate(command, Mode::Composing), Key::Other);
        assert_eq!(translate(option, Mode::Direct), Key::Other);
    }

    #[test]
    fn shift_space_goes_back_only_while_converting() {
        let shift_space = KeyPress {
            shift: true,
            ..code(KEY_SPACE)
        };
        assert_eq!(translate(shift_space, Mode::Converting), Key::Up);
        assert_eq!(translate(shift_space, Mode::Direct), Key::Space);
    }

    #[test]
    fn held_letter_is_not_part_of_a_word_outside_a_composition() {
        let repeated = KeyPress {
            repeat: true,
            ..character('a')
        };
        assert_eq!(translate(repeated, Mode::Direct), Key::Other);
        assert_eq!(translate(repeated, Mode::Composing), Key::Char('a'));
    }

    #[test]
    fn arrow_and_function_keys_are_not_characters() {
        // ファンクションキーは、私用領域の文字として届く。F5 は keyCode 96、文字は U+F708。
        let f5 = KeyPress {
            key_code: 96,
            character: Some('\u{F708}'),
            ..KeyPress::default()
        };
        assert_eq!(translate(f5, Mode::Composing), Key::Other);
        assert_eq!(translate(code(KEY_LEFT), Mode::Converting), Key::Left);
        assert_eq!(translate(code(KEY_RIGHT), Mode::Converting), Key::Right);
        let shift = |key_code| KeyPress {
            shift: true,
            ..code(key_code)
        };
        assert_eq!(translate(shift(KEY_LEFT), Mode::Converting), Key::ShiftLeft);
        assert_eq!(translate(shift(KEY_RIGHT), Mode::Converting), Key::ShiftRight);
        assert_eq!(translate(code(KEY_UP), Mode::Converting), Key::Up);
        assert_eq!(translate(code(KEY_DOWN), Mode::Converting), Key::Down);
    }
}

//! Windows のキー (仮想キーコード) をエンジンのキーに読み替える。OS に依存しないので、どの OS でもテストできる。

use kayoiji_core::engine::{Key, Mode};

pub const VK_BACK: u16 = 0x08;
pub const VK_TAB: u16 = 0x09;
pub const VK_RETURN: u16 = 0x0D;
pub const VK_ESCAPE: u16 = 0x1B;
pub const VK_SPACE: u16 = 0x20;
pub const VK_LEFT: u16 = 0x25;
pub const VK_UP: u16 = 0x26;
pub const VK_RIGHT: u16 = 0x27;
pub const VK_DOWN: u16 = 0x28;
pub const VK_F7: u16 = 0x76;
pub const VK_F10: u16 = 0x79;
/// 日本語キーボードの「変換」。
pub const VK_CONVERT: u16 = 0x1C;
/// 日本語キーボードの「無変換」。
pub const VK_NONCONVERT: u16 = 0x1D;
/// 日本語キーボードの「カタカナ ひらがな」。
pub const VK_DBE_HIRAGANA: u16 = 0xF2;
/// 日本語キーボードの「半角/全角」。押すたびに OEM_AUTO と OEM_ENLW が交互に届く。KANJI で届く環境もある。
pub const VK_KANJI: u16 = 0x19;
pub const VK_OEM_AUTO: u16 = 0xF3;
pub const VK_OEM_ENLW: u16 = 0xF4;
/// IME をオン・オフにするキー (Windows 10 以降の日本語キーボードの設定で、変換・無変換に割り当てられる)。
pub const VK_IME_ON: u16 = 0x16;
pub const VK_IME_OFF: u16 = 0x1A;
pub const VK_SHIFT: u16 = 0x10;
pub const VK_CONTROL: u16 = 0x11;
pub const VK_MENU: u16 = 0x12;

/// 押されたキー。`character` は、Ctrl を除いた修飾キーを反映して、今のキーボード配列で打たれる文字。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyStroke {
    pub vk: u16,
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub repeat: bool,
    pub character: Option<char>,
}

impl KeyStroke {
    pub fn new(vk: u16) -> Self {
        Self {
            vk,
            shift: false,
            control: false,
            alt: false,
            repeat: false,
            character: None,
        }
    }
}

/// キーの扱い。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mapped {
    Engine(Key),
    /// 「半角/全角」。日本語入力と直接入力を入れ替える。
    Toggle,
    /// 修飾キーだけの打鍵。エンジンにも渡さず、入力も区切らない。
    Modifier,
}

pub fn translate(stroke: KeyStroke, mode: Mode) -> Mapped {
    match stroke.vk {
        VK_SHIFT | VK_CONTROL | VK_MENU | 0xA0..=0xA5 | 0x5B | 0x5C => return Mapped::Modifier,
        VK_KANJI | VK_OEM_AUTO | VK_OEM_ENLW => return Mapped::Toggle,
        VK_CONVERT | VK_DBE_HIRAGANA | VK_IME_ON => return Mapped::Engine(Key::Henkan),
        VK_NONCONVERT | VK_IME_OFF => return Mapped::Engine(Key::Muhenkan),
        _ => {}
    }
    if stroke.alt {
        return Mapped::Engine(Key::Other);
    }
    if stroke.control {
        return Mapped::Engine(match stroke.character.map(|c| c.to_ascii_lowercase()) {
            Some('j') => Key::ForceJapanese,
            Some(';') => Key::ForceEnglish,
            Some('k') => Key::Katakana,
            _ => Key::Other,
        });
    }
    Mapped::Engine(match stroke.vk {
        VK_RETURN => Key::Enter,
        VK_TAB => Key::Tab,
        VK_SPACE if stroke.shift && mode == Mode::Converting => Key::Up,
        VK_SPACE => Key::Space,
        VK_BACK => Key::Backspace,
        VK_ESCAPE => Key::Escape,
        VK_UP => Key::Up,
        VK_DOWN => Key::Down,
        VK_LEFT if stroke.shift => Key::ShiftLeft,
        VK_RIGHT if stroke.shift => Key::ShiftRight,
        VK_LEFT => Key::Left,
        VK_RIGHT => Key::Right,
        VK_F7 => Key::Katakana,
        VK_F10 => Key::ForceEnglish,
        _ => match stroke.character {
            // キーを押し続けて同じ文字を並べるのは、日本語の入力ではない。
            Some(_) if stroke.repeat && mode == Mode::Direct => Key::Other,
            Some(c) if !c.is_control() => Key::Char(c),
            _ => Key::Other,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn char_key(vk: u16, c: char) -> KeyStroke {
        KeyStroke {
            character: Some(c),
            ..KeyStroke::new(vk)
        }
    }

    #[test]
    fn letters_and_symbols_are_typed_as_the_layout_gives_them() {
        assert_eq!(translate(char_key(0x41, 'a'), Mode::Direct), Mapped::Engine(Key::Char('a')));
        let shifted = KeyStroke {
            shift: true,
            ..char_key(0x41, 'A')
        };
        assert_eq!(translate(shifted, Mode::Direct), Mapped::Engine(Key::Char('A')));
        assert_eq!(translate(char_key(0xBD, '-'), Mode::Composing), Mapped::Engine(Key::Char('-')));
    }

    #[test]
    fn japanese_keyboard_keys_switch_the_input_mode() {
        assert_eq!(translate(KeyStroke::new(VK_CONVERT), Mode::Direct), Mapped::Engine(Key::Henkan));
        assert_eq!(translate(KeyStroke::new(VK_DBE_HIRAGANA), Mode::Direct), Mapped::Engine(Key::Henkan));
        assert_eq!(translate(KeyStroke::new(VK_IME_ON), Mode::Direct), Mapped::Engine(Key::Henkan));
        assert_eq!(translate(KeyStroke::new(VK_NONCONVERT), Mode::Composing), Mapped::Engine(Key::Muhenkan));
        assert_eq!(translate(KeyStroke::new(VK_IME_OFF), Mode::Composing), Mapped::Engine(Key::Muhenkan));
        assert_eq!(translate(KeyStroke::new(VK_OEM_AUTO), Mode::Direct), Mapped::Toggle);
        assert_eq!(translate(KeyStroke::new(VK_OEM_ENLW), Mode::Direct), Mapped::Toggle);
        assert_eq!(translate(KeyStroke::new(VK_KANJI), Mode::Direct), Mapped::Toggle);
    }

    #[test]
    fn shift_space_is_the_previous_candidate_only_while_converting() {
        let shift_space = KeyStroke {
            shift: true,
            ..char_key(VK_SPACE, ' ')
        };
        assert_eq!(translate(shift_space, Mode::Converting), Mapped::Engine(Key::Up));
        assert_eq!(translate(shift_space, Mode::Composing), Mapped::Engine(Key::Space));
    }

    #[test]
    fn shift_arrows_resize_segments() {
        let shift_left = KeyStroke {
            shift: true,
            ..KeyStroke::new(VK_LEFT)
        };
        assert_eq!(translate(shift_left, Mode::Converting), Mapped::Engine(Key::ShiftLeft));
        assert_eq!(translate(KeyStroke::new(VK_RIGHT), Mode::Converting), Mapped::Engine(Key::Right));
    }

    #[test]
    fn control_shortcuts_are_read_from_the_character() {
        let control = |c| KeyStroke {
            control: true,
            ..char_key(0, c)
        };
        assert_eq!(translate(control('j'), Mode::Direct), Mapped::Engine(Key::ForceJapanese));
        assert_eq!(translate(control(';'), Mode::Composing), Mapped::Engine(Key::ForceEnglish));
        assert_eq!(translate(control('k'), Mode::Composing), Mapped::Engine(Key::Katakana));
        assert_eq!(translate(control('c'), Mode::Composing), Mapped::Engine(Key::Other));
    }

    #[test]
    fn alt_combinations_and_repeated_letters_are_passed_through() {
        let alt = KeyStroke {
            alt: true,
            ..char_key(0x46, 'f')
        };
        assert_eq!(translate(alt, Mode::Composing), Mapped::Engine(Key::Other));
        let repeated = KeyStroke {
            repeat: true,
            ..char_key(0x41, 'a')
        };
        assert_eq!(translate(repeated, Mode::Direct), Mapped::Engine(Key::Other));
        assert_eq!(translate(repeated, Mode::Composing), Mapped::Engine(Key::Char('a')));
    }

    #[test]
    fn modifier_keys_alone_are_not_keys() {
        assert_eq!(translate(KeyStroke::new(VK_SHIFT), Mode::Composing), Mapped::Modifier);
        assert_eq!(translate(KeyStroke::new(0xA2), Mode::Composing), Mapped::Modifier);
    }
}

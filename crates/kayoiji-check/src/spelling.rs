//! OS のスペルチェッカーで、英文の綴りを調べる。
//!
//! 綴りが誤っているかどうかは 1 ミリ秒未満で分かるが、訂正候補を作るのには 1 語あたり数十ミリ秒かかる。
//! 打鍵のたびに調べる時は、すでに調べた単語の候補だけを付け (`Guesses::Known`)、打鍵が止まってから
//! 残りの候補を作る (`Guesses::Lookup`)。1 回に作るのは数語までにして、メインスレッドを長く止めない。
//! 残りがあれば `Spelling::pending` が真なので、呼び出し側が間を置いて呼び直す。

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use serde::Serialize;

/// 訂正候補の付け方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Guesses {
    /// すでに調べた単語の候補だけを付ける。
    Known,
    /// まだ調べていない単語の候補も、先頭から `LOOKUPS_PER_CALL` 語まで作る。
    Lookup,
}

/// 調べた単語の訂正候補。打っている途中の単語も入るので、この数を超えたら空にする。
const KNOWN_GUESSES: usize = 2000;
static KNOWN: LazyLock<Mutex<HashMap<String, Vec<String>>>> = LazyLock::new(Mutex::default);

/// 綴りが誤っている単語と、その訂正候補。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Misspelling {
    pub word: String,
    /// 英文の先頭からの位置 (UTF-16 の単位)。
    pub start: usize,
    pub guesses: Vec<String>,
}

/// 英文の綴りを調べた結果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Spelling {
    pub misspellings: Vec<Misspelling>,
    /// 綴りが誤っている単語を、最初の訂正候補に置き換えた文。置き換える所が無ければ `None`。
    pub corrected: Option<String>,
    /// 訂正候補をまだ作っていない単語があるかどうか。
    pub pending: bool,
}

/// `text` の綴りを調べる。AppKit を呼ぶので、メインスレッドで呼ぶ。
pub fn check(text: &str, guesses: Guesses) -> Spelling {
    let (misspellings, pending) = misspellings(text, guesses);
    let fixed = corrected(text, &misspellings);
    Spelling {
        corrected: (fixed != text).then_some(fixed),
        misspellings,
        pending,
    }
}

/// 1 つの単語に付ける訂正候補の数。
const GUESSES: usize = 3;

/// `Guesses::Lookup` の 1 回で訂正候補を作る単語の数。1 語あたり数十ミリ秒かかる。
pub const LOOKUPS_PER_CALL: usize = 2;

/// `text` の中の、綴りが誤っている単語を先頭から順に返す。2 つ目の値は、訂正候補をまだ作っていない
/// 単語があるかどうか。AppKit を呼ぶので、メインスレッドで呼ぶ。
#[cfg(target_os = "macos")]
pub fn misspellings(text: &str, guesses: Guesses) -> (Vec<Misspelling>, bool) {
    use objc2_app_kit::NSSpellChecker;
    use objc2_foundation::{ns_string, NSNotFound, NSString};

    let checker = NSSpellChecker::sharedSpellChecker();
    let string = NSString::from_str(text);
    let length = string.length();
    let language = ns_string!("en");
    let mut known = KNOWN.lock().expect("known guesses lock");
    if known.len() > KNOWN_GUESSES {
        known.clear();
    }
    let mut found = Vec::new();
    let mut pending = false;
    let mut lookups = 0;
    let mut position = 0;
    while position < length {
        // SAFETY: 位置は文字列の長さより小さい。文書のタグ 0 は、どの文書にも属さない検査を表す。
        let range = unsafe {
            checker.checkSpellingOfString_startingAt_language_wrap_inSpellDocumentWithTag_wordCount(
                &string,
                position as isize,
                Some(language),
                false,
                0,
                std::ptr::null_mut(),
            )
        };
        if range.location == NSNotFound as usize || range.length == 0 {
            break;
        }
        let word = string.substringWithRange(range).to_string();
        if guesses == Guesses::Lookup && lookups < LOOKUPS_PER_CALL && !known.contains_key(&word) {
            lookups += 1;
            let made =
                checker.guessesForWordRange_inString_language_inSpellDocumentWithTag(range, &string, Some(language), 0);
            let made = made
                .map(|made| made.iter().take(GUESSES).map(|guess| guess.to_string()).collect())
                .unwrap_or_default();
            known.insert(word.clone(), made);
        }
        pending |= !known.contains_key(&word);
        found.push(Misspelling {
            guesses: known.get(&word).cloned().unwrap_or_default(),
            word,
            start: range.location,
        });
        position = range.location + range.length;
    }
    (found, pending)
}

#[cfg(not(target_os = "macos"))]
pub fn misspellings(_text: &str, _guesses: Guesses) -> (Vec<Misspelling>, bool) {
    (Vec::new(), false)
}

/// 綴りが誤っている単語を、それぞれの最初の訂正候補に置き換えた文を返す。
/// 訂正候補が無い単語は、そのまま残す。
pub fn corrected(text: &str, misspellings: &[Misspelling]) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut result: Vec<u16> = Vec::with_capacity(units.len());
    let mut position = 0;
    for misspelling in misspellings {
        let Some(guess) = misspelling.guesses.first() else {
            continue;
        };
        let end = misspelling.start + misspelling.word.encode_utf16().count();
        if misspelling.start < position || end > units.len() {
            continue;
        }
        result.extend_from_slice(&units[position..misspelling.start]);
        result.extend(guess.encode_utf16());
        position = end;
    }
    result.extend_from_slice(&units[position..]);
    String::from_utf16_lossy(&result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn misspelling(word: &str, start: usize, guesses: &[&str]) -> Misspelling {
        Misspelling {
            word: word.to_string(),
            start,
            guesses: guesses.iter().map(|guess| guess.to_string()).collect(),
        }
    }

    #[test]
    fn misspelled_words_are_replaced_by_their_first_guess() {
        // Arrange
        let text = "pleas send me the fiel";
        let found = [misspelling("pleas", 0, &["please", "pleas"]), misspelling("fiel", 18, &["file"])];

        // Act
        let fixed = corrected(text, &found);

        // Assert
        assert_eq!(fixed, "please send me the file");
    }

    #[test]
    fn a_word_without_a_guess_is_kept() {
        // Arrange
        let text = "kayoiji is nise";
        let found = [misspelling("kayoiji", 0, &[]), misspelling("nise", 11, &["nice"])];

        // Act
        let fixed = corrected(text, &found);

        // Assert
        assert_eq!(fixed, "kayoiji is nice");
    }

    #[test]
    fn positions_count_utf16_units() {
        // Arrange: 絵文字は UTF-16 で 2 単位。その後ろの単語の位置は 3 から始まる
        let text = "😀 helo";
        let found = [misspelling("helo", 3, &["hello"])];

        // Act
        let fixed = corrected(text, &found);

        // Assert
        assert_eq!(fixed, "😀 hello");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn checking_offers_a_corrected_text_only_when_something_changes() {
        let fixed = check("Please send me the fiel", Guesses::Lookup);
        // 最初の訂正候補は、ほかのテストが同じ単語を先に調べたかどうかで変わりうる (候補は単語ごとに覚える)。
        // 誤った単語が置き換わっていることだけを確かめる。
        let corrected = fixed.corrected.as_deref().expect("a corrected text");
        assert!(corrected.starts_with("Please send me the ") && !corrected.contains("fiel"), "{corrected}");
        assert!(!fixed.pending);
        assert_eq!(check("Please send me the file", Guesses::Lookup).corrected, None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn guesses_are_made_only_on_lookup_and_then_remembered() {
        // Arrange: ほかのテストが調べない単語
        let text = "an unusal sentense";

        // Act
        let quick = check(text, Guesses::Known);
        let full = check(text, Guesses::Lookup);
        let again = check(text, Guesses::Known);

        // Assert
        // 打鍵のたびの検査では、誤りの場所だけが分かる。
        assert_eq!(quick.misspellings.len(), 2, "{quick:?}");
        assert!(quick.pending);
        assert!(quick.misspellings.iter().all(|misspelling| misspelling.guesses.is_empty()));
        assert_eq!(quick.corrected, None);
        assert!(!full.pending);
        assert_eq!(full.corrected.as_deref(), Some("an unusual sentence"));
        assert_eq!(again, full);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_lookup_makes_guesses_for_a_few_words_at_a_time() {
        // Arrange: ほかのテストが調べない単語を、1 回で作る数より 1 つ多く並べる
        let text = "wierd freind beleive";
        assert_eq!(text.split(' ').count(), LOOKUPS_PER_CALL + 1);

        // Act
        let first = check(text, Guesses::Lookup);
        let second = check(text, Guesses::Lookup);

        // Assert
        assert!(first.pending, "{first:?}");
        assert!(first.misspellings[LOOKUPS_PER_CALL].guesses.is_empty(), "{first:?}");
        assert!(!second.pending, "{second:?}");
        assert!(second.misspellings.iter().all(|misspelling| !misspelling.guesses.is_empty()), "{second:?}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_system_spell_checker_finds_a_misspelled_word() {
        // Act
        let (found, _) = misspellings("Please send me the fiel tomorrow", Guesses::Lookup);

        // Assert
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].word, "fiel");
        assert_eq!(found[0].start, 19);
        assert!(found[0].guesses.iter().any(|guess| guess == "file"), "{found:?}");
    }
}

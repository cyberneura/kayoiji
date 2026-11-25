//! 判定と変換の精度を実データで測る。
//!
//! - 英語: 頻度の高い英単語を 1 語ずつ打ち、日本語に切り替わってしまう割合を数える。
//! - 日本語: Mozc 辞書でコストの低い語の読みをローマ字で打ち、日本語に切り替わる割合を数える。
//! - 変換: Mozc の評価データの読みを変換し、Mozc 自身の出力と一致する割合を数える。

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;
use std::sync::Arc;

use kayoiji_core::converter::Converter;
use kayoiji_core::dict::Dictionary;
use kayoiji_core::engine::{Key, Mode};
use kayoiji_core::kana::is_kana;
use kayoiji_core::simulate::Simulator;

use kayoiji_core::resources::DICTIONARY_FILE;

use crate::{load_engine, read_english_counts, ENGLISH_SOURCE};

/// 日本語を 1 語確定して、日本語の入力が続いている状態にする打鍵。
const FLOW_PREFIX: &str = "kore {enter}";

/// 変換の評価で、正解が上位何件に入っていれば「候補にある」と数えるか。
const CONVERSION_TOP: usize = 10;

pub fn run(data: &Path, raw: &Path, words: usize, examples: usize) -> io::Result<()> {
    let mut simulator = Simulator::new(load_engine(data)?);

    let mut english = read_english_counts(&raw.join(ENGLISH_SOURCE))?;
    english.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    let english: Vec<String> = english
        .into_iter()
        .map(|(word, _)| word)
        .filter(|word| word.len() >= 2)
        .take(words)
        .collect();
    for (label, prefix) in [("after a space", ""), ("after an English word", "the ")] {
        let outcomes: Vec<(&String, Outcome)> = english
            .iter()
            .map(|word| (word, type_word(&mut simulator, prefix, word)))
            .collect();
        for (moment, outcome) in [
            ("before the space", Outcome::JapaneseMidWord),
            ("at the space", Outcome::JapaneseAtSpace),
        ] {
            let mistakes: Vec<&String> = outcomes
                .iter()
                .filter(|(_, actual)| *actual == outcome)
                .map(|(word, _)| *word)
                .collect();
            report(
                &format!("English words typed {label}, switched to Japanese {moment}"),
                mistakes.len(),
                english.len(),
                &mistakes,
                examples,
            );
        }
    }

    // 日本語を確定した直後に、空白を挟まずに英単語を打つ。
    let kept_japanese: Vec<&String> = english
        .iter()
        .filter(|word| type_word_in_flow(&mut simulator, word) != Outcome::English)
        .collect();
    report(
        "English words typed in Japanese flow, left as Japanese",
        kept_japanese.len(),
        english.len(),
        &kept_japanese,
        examples,
    );

    let japanese = japanese_words(&raw.join("mozc"), words)?;
    for (label, style) in [("kunrei", Style::Kunrei), ("hepburn", Style::Hepburn)] {
        let typed: Vec<(String, &String)> = japanese
            .iter()
            .map(|reading| (kana_to_romaji(reading, style), reading))
            // 小書きのかなを "x" で打つ読みは、活用語尾の断片など単独では打たないものが多い。
            .filter(|(romaji, _)| !romaji.contains('x'))
            .collect();
        let outcomes: Vec<Outcome> = typed
            .iter()
            .map(|(romaji, _)| type_word(&mut simulator, "", romaji))
            .collect();
        let missed: Vec<String> = typed
            .iter()
            .zip(&outcomes)
            .filter(|(_, outcome)| **outcome == Outcome::English)
            .map(|((romaji, reading), _)| format!("{romaji}({reading})"))
            .collect();
        let mid_word = outcomes.iter().filter(|o| **o == Outcome::JapaneseMidWord).count();
        report(
            &format!("Japanese words typed in {label} romaji, left as English"),
            missed.len(),
            typed.len(),
            &missed.iter().collect::<Vec<_>>(),
            examples,
        );
        println!(
            "  switched before the space: {mid_word} of {} ({:.1}%)",
            typed.len(),
            100.0 * mid_word as f64 / typed.len() as f64
        );
        let reverted: Vec<String> = typed
            .iter()
            .filter(|(romaji, _)| type_word_in_flow(&mut simulator, romaji) == Outcome::English)
            .map(|(romaji, reading)| format!("{romaji}({reading})"))
            .collect();
        report(
            &format!("Japanese words typed in {label} romaji in Japanese flow, turned into English"),
            reverted.len(),
            typed.len(),
            &reverted.iter().collect::<Vec<_>>(),
            examples,
        );
    }

    conversion(data, &raw.join("mozc/evaluation.tsv"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    English,
    /// 空白を打つ前に日本語へ切り替わった。
    JapaneseMidWord,
    /// 空白を打った時に日本語と判定された。
    JapaneseAtSpace,
}

fn type_word(simulator: &mut Simulator, prefix: &str, word: &str) -> Outcome {
    simulator.clear();
    simulator.run(prefix);
    simulator.run(word);
    if simulator.engine.view().mode != Mode::Direct {
        return Outcome::JapaneseMidWord;
    }
    simulator.press(Key::Space);
    if simulator.engine.view().mode != Mode::Direct {
        return Outcome::JapaneseAtSpace;
    }
    Outcome::English
}

/// 日本語を確定した直後に単語を打つ。かなで表示しながら入力するので、空白を打った後の状態だけを見る。
fn type_word_in_flow(simulator: &mut Simulator, word: &str) -> Outcome {
    simulator.clear();
    simulator.run(FLOW_PREFIX);
    simulator.run(word);
    simulator.press(Key::Space);
    if simulator.engine.view().mode != Mode::Direct {
        return Outcome::JapaneseAtSpace;
    }
    Outcome::English
}

fn report<T: std::fmt::Display>(title: &str, count: usize, total: usize, items: &[T], examples: usize) {
    println!("{title}: {count} of {total} ({:.2}%)", 100.0 * count as f64 / total as f64);
    if !items.is_empty() {
        let shown: Vec<String> = items.iter().take(examples).map(ToString::to_string).collect();
        println!("  e.g. {}", shown.join(" "));
    }
}

/// Mozc 辞書から、コストの低い順に読みを取り出す。
fn japanese_words(mozc: &Path, limit: usize) -> io::Result<Vec<String>> {
    let mut entries: Vec<(i32, String)> = Vec::new();
    for number in 0..10 {
        for line in BufReader::new(File::open(mozc.join(format!("dictionary{number:02}.txt")))?).lines() {
            let line = line?;
            let fields: Vec<&str> = line.split('\t').collect();
            let [reading, _, _, cost, ..] = fields[..] else {
                continue;
            };
            let length = reading.chars().count();
            if length < 2 || !reading.chars().all(is_kana) {
                continue;
            }
            if let Ok(cost) = cost.parse::<i32>() {
                entries.push((cost, reading.to_string()));
            }
        }
    }
    entries.sort();
    let mut seen = HashSet::new();
    Ok(entries
        .into_iter()
        .map(|(_, reading)| reading)
        .filter(|reading| seen.insert(reading.clone()))
        .take(limit)
        .collect())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Style {
    /// si / ti / tu / hu / zi のように、打鍵が少ない綴り。
    Kunrei,
    /// shi / chi / tsu / fu / ji。
    Hepburn,
}

/// かなを、利用者が打つであろうローマ字に直す。
fn kana_to_romaji(reading: &str, style: Style) -> String {
    let chars: Vec<char> = reading.chars().collect();
    let mut romaji = String::new();
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        if c == 'っ' {
            // 促音は次の子音を重ねる。
            let next = syllable(&chars[index + 1..], style).0;
            romaji.push_str(match (next.chars().next(), next.starts_with("ch")) {
                (_, true) => "t",
                (Some(consonant), _) if !"aiueo".contains(consonant) => &next[..1],
                _ => "xtu",
            });
            index += 1;
            continue;
        }
        if c == 'ん' {
            // 母音・や行・な行の前の「ん」は "nn" と打たないと別の音になる。語末も "nn" と打つ人が多い。
            let next = syllable(&chars[index + 1..], style).0;
            let ambiguous = next.is_empty() || next.starts_with(['a', 'i', 'u', 'e', 'o', 'y', 'n']);
            romaji.push_str(if ambiguous { "nn" } else { "n" });
            index += 1;
            continue;
        }
        let (text, used) = syllable(&chars[index..], style);
        romaji.push_str(&text);
        index += used.max(1);
    }
    romaji
}

/// 先頭の 1 音節のローマ字と、消費したかなの文字数を返す。
fn syllable(chars: &[char], style: Style) -> (String, usize) {
    let Some(&first) = chars.first() else {
        return (String::new(), 0);
    };
    let base = match (first, style) {
        ('し', Style::Hepburn) => "shi",
        ('ち', Style::Hepburn) => "chi",
        ('つ', Style::Hepburn) => "tsu",
        ('ふ', Style::Hepburn) => "fu",
        ('じ', Style::Hepburn) => "ji",
        (other, _) => single(other),
    };
    let small = chars.get(1).copied();
    if let Some(vowel) = small.and_then(|s| match s {
        'ゃ' => Some('a'),
        'ゅ' => Some('u'),
        'ょ' => Some('o'),
        _ => None,
    }) {
        if base.ends_with('i') && base.len() >= 2 {
            let stem = &base[..base.len() - 1];
            let text = match (stem, style) {
                ("sh" | "ch" | "j", _) => format!("{stem}{vowel}"),
                (_, _) => format!("{stem}y{vowel}"),
            };
            return (text, 2);
        }
    }
    (base.to_string(), 1)
}

fn single(c: char) -> &'static str {
    match c {
        'あ' => "a", 'い' => "i", 'う' => "u", 'え' => "e", 'お' => "o",
        'か' => "ka", 'き' => "ki", 'く' => "ku", 'け' => "ke", 'こ' => "ko",
        'さ' => "sa", 'し' => "si", 'す' => "su", 'せ' => "se", 'そ' => "so",
        'た' => "ta", 'ち' => "ti", 'つ' => "tu", 'て' => "te", 'と' => "to",
        'な' => "na", 'に' => "ni", 'ぬ' => "nu", 'ね' => "ne", 'の' => "no",
        'は' => "ha", 'ひ' => "hi", 'ふ' => "hu", 'へ' => "he", 'ほ' => "ho",
        'ま' => "ma", 'み' => "mi", 'む' => "mu", 'め' => "me", 'も' => "mo",
        'や' => "ya", 'ゆ' => "yu", 'よ' => "yo",
        'ら' => "ra", 'り' => "ri", 'る' => "ru", 'れ' => "re", 'ろ' => "ro",
        'わ' => "wa", 'を' => "wo",
        'が' => "ga", 'ぎ' => "gi", 'ぐ' => "gu", 'げ' => "ge", 'ご' => "go",
        'ざ' => "za", 'じ' => "zi", 'ず' => "zu", 'ぜ' => "ze", 'ぞ' => "zo",
        'だ' => "da", 'ぢ' => "di", 'づ' => "du", 'で' => "de", 'ど' => "do",
        'ば' => "ba", 'び' => "bi", 'ぶ' => "bu", 'べ' => "be", 'ぼ' => "bo",
        'ぱ' => "pa", 'ぴ' => "pi", 'ぷ' => "pu", 'ぺ' => "pe", 'ぽ' => "po",
        'ぁ' => "xa", 'ぃ' => "xi", 'ぅ' => "xu", 'ぇ' => "xe", 'ぉ' => "xo",
        'ゃ' => "xya", 'ゅ' => "xyu", 'ょ' => "xyo", 'ゎ' => "xwa",
        'ゔ' => "vu", 'ー' => "-",
        _ => "",
    }
}

fn conversion(data: &Path, evaluation: &Path) -> io::Result<()> {
    let converter = Converter::new(Arc::new(Dictionary::open(&data.join(DICTIONARY_FILE))?));
    let mut total = 0;
    let mut first = 0;
    let mut listed = 0;
    for line in BufReader::new(File::open(evaluation)?).lines() {
        let line = line?;
        let fields: Vec<&str> = line.split('\t').collect();
        let [status, input, output, command, ..] = fields[..] else {
            continue;
        };
        if !status.starts_with("OK") || !command.starts_with("Conversion") {
            continue;
        }
        total += 1;
        let conversions = converter.convert(input, CONVERSION_TOP);
        if conversions.first().is_some_and(|best| best.surface == output) {
            first += 1;
        }
        if conversions.iter().any(|conversion| conversion.surface == output) {
            listed += 1;
        }
    }
    println!(
        "Conversion of Mozc's evaluation sentences: first candidate equals Mozc's output in {first} of {total} ({:.1}%), within the top {CONVERSION_TOP} in {listed} ({:.1}%)",
        100.0 * first as f64 / total as f64,
        100.0 * listed as f64 / total as f64
    );
    Ok(())
}

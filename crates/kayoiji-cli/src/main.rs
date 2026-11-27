//! 辞書のビルドと、ターミナルからのエンジンの動作確認。

mod evaluate;

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Parser, Subcommand};
use kayoiji_core::converter::Converter;
use kayoiji_core::detector::Context;
use kayoiji_core::dict::{write_replacing, Dictionary, DictionaryBuilder};
use kayoiji_core::emoji;
use kayoiji_core::engine::{Engine, EngineConfig};
use kayoiji_core::english::EnglishLexicon;
use kayoiji_core::learning::Learning;
use kayoiji_core::resources::{open_engine, DICTIONARY_FILE, ENGLISH_FILE, SYMBOLS_FILE};
use kayoiji_core::rules::RuleSet;
use kayoiji_core::simulate::Simulator;

pub const ENGLISH_SOURCE: &str = "english/enwiki-2023-04-13.txt";

/// 英単語の頻度表から採用する最小の出現回数。これより少ない語は、綴りの誤りや
/// 英語以外の語がほとんどを占める。
const ENGLISH_MIN_COUNT: u64 = 20;

#[derive(Parser)]
#[command(about = "Kayoiji の辞書ビルドと動作確認")]
struct Cli {
    /// ビルド済みの辞書があるディレクトリ。
    #[arg(long, global = true, default_value = "data/build")]
    data: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// `scripts/fetch-data.sh` で取得したデータから辞書を作る。
    BuildData {
        #[arg(long, default_value = "data/raw")]
        raw: PathBuf,
    },
    /// かなを漢字に変換して候補を表示する。
    Convert {
        reading: String,
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// 打鍵の列を入力した結果を表示する。`{enter}` `{esc}` `{ja}` `{wait}` などが使える。
    Type {
        script: String,
        /// 打鍵の間隔 (ミリ秒)。
        #[arg(long, default_value_t = 50)]
        interval: u64,
    },
    /// 打鍵列を 1 文字ずつ伸ばしながら、判定に使う値を表示する。
    Explain { words: Vec<String> },
    /// 英語・日本語の判定と変換の精度を、実データで測る。
    Evaluate {
        #[arg(long, default_value = "data/raw")]
        raw: PathBuf,
        /// 評価に使う単語の数 (英語・日本語それぞれ、頻度の高い順)。
        #[arg(long, default_value_t = 20000)]
        words: usize,
        /// 誤判定の例を表示する数。
        #[arg(long, default_value_t = 40)]
        examples: usize,
    },
}

fn main() -> io::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::BuildData { raw } => build_data(&raw, &cli.data),
        Command::Convert { reading, limit } => {
            let dictionary = Arc::new(Dictionary::open(&cli.data.join(DICTIONARY_FILE))?);
            for conversion in Converter::new(dictionary).convert(&reading, limit) {
                println!("{:>6}  {}", conversion.cost, conversion.surface);
            }
            Ok(())
        }
        Command::Type { script, interval } => {
            let mut simulator = Simulator::new(load_engine(&cli.data)?);
            simulator.interval_ms = interval;
            simulator.run(&script);
            println!("{}", simulator.text);
            let view = simulator.engine.view();
            if !view.candidates.is_empty() {
                println!("candidates: {}", view.candidates.join(" / "));
            }
            Ok(())
        }
        Command::Explain { words } => {
            let engine = load_engine(&cli.data)?;
            for word in words {
                explain(&engine, &word);
            }
            Ok(())
        }
        Command::Evaluate { raw, words, examples } => evaluate::run(&cli.data, &raw, words, examples),
    }
}

/// このコマンドは判定の動作を調べるためのものなので、エンジンは自動切り替えで開く。
pub fn load_engine(data: &Path) -> io::Result<Engine> {
    let config = EngineConfig {
        auto_switch: true,
        ..EngineConfig::default()
    };
    open_engine(data, RuleSet::standard(), Learning::in_memory(), config)
}

fn explain(engine: &Engine, word: &str) {
    let detector = engine.detector();
    let number = |value: Option<f64>| value.map_or("     -".to_string(), |v| format!("{v:6.2}"));
    println!("{word}");
    println!("  typed          kana         strict  en-prefix  en-word  japanese  in-flow  mid-word      at-space  in-flow");
    for end in 1..=word.len() {
        let typed = &word[..end];
        let e = detector.explain(typed);
        println!(
            "  {typed:<14} {:<12} {:<6}  {}     {}   {}   {}  {:<12}  {:<8}  {:?}",
            format!("{}{}", e.kana, e.pending),
            e.strict,
            number(e.english_prefix),
            number(e.english_word),
            number(e.japanese),
            number(e.japanese_in_flow),
            format!("{:?}", detector.evaluate_prefix(typed, Context::Neutral)),
            format!("{:?}", detector.evaluate_word(typed, Context::Neutral)),
            detector.evaluate_word(typed, Context::AfterJapanese),
        );
    }
}

/// 英単語の頻度表 (`単語 出現回数` の行) から、英小文字だけの単語を読む。
pub fn read_english_counts(path: &Path) -> io::Result<Vec<(String, u64)>> {
    let mut words = Vec::new();
    for line in BufReader::new(File::open(path)?).lines() {
        let line = line?;
        let Some((word, count)) = line.split_once(' ') else {
            continue;
        };
        let Ok(count) = count.trim().parse::<u64>() else {
            continue;
        };
        if count >= ENGLISH_MIN_COUNT && word.bytes().all(|b| b.is_ascii_lowercase()) {
            words.push((word.to_string(), count));
        }
    }
    Ok(words)
}

fn build_data(raw: &Path, output: &Path) -> io::Result<()> {
    fs::create_dir_all(output)?;

    let mozc = DictionaryBuilder::from_mozc_dir(&raw.join("mozc"))?;
    build_emoji(raw, output, &mozc)?;
    let dictionary = mozc.build();
    let dictionary_path = output.join(DICTIONARY_FILE);
    write_replacing(&dictionary_path, &dictionary)?;
    println!("{}: {} bytes", dictionary_path.display(), dictionary.len());

    let symbols = DictionaryBuilder::from_mozc_symbols(&raw.join("mozc"))?.build();
    let symbols_path = output.join(SYMBOLS_FILE);
    write_replacing(&symbols_path, &symbols)?;
    println!("{}: {} bytes", symbols_path.display(), symbols.len());

    let english = EnglishLexicon::build(read_english_counts(&raw.join(ENGLISH_SOURCE))?);
    let english_path = output.join(ENGLISH_FILE);
    write_replacing(&english_path, &english)?;
    println!("{}: {} bytes", english_path.display(), english.len());
    Ok(())
}

/// 絵文字の一覧と索引を作る。漢字を含むキーワードには、Mozc の辞書でその表記の読みを足す。
fn build_emoji(raw: &Path, output: &Path, mozc: &DictionaryBuilder) -> io::Result<()> {
    /// 1 つの表記に足す読みの数。コストの小さい方から取る。
    const READINGS_PER_KEYWORD: usize = 2;

    let source = emoji::Source::parse(
        &fs::read_to_string(raw.join("emoji/emoji.ja.json"))?,
        &fs::read_to_string(raw.join("emoji/search-index.json"))?,
    )?;
    let readings = mozc.readings_of(&source.kanji_keywords(), READINGS_PER_KEYWORD);
    let (catalog, index) = source.build(&readings);
    let catalog_path = output.join(emoji::CATALOG_FILE);
    write_replacing(&catalog_path, catalog.as_bytes())?;
    println!("{}: {} bytes", catalog_path.display(), catalog.len());
    let index_path = output.join(emoji::INDEX_FILE);
    write_replacing(&index_path, &index)?;
    println!("{}: {} bytes", index_path.display(), index.len());
    Ok(())
}

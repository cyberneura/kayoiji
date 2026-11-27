//! かな漢字変換。
//!
//! 読みの各位置から辞書を引いてラティスを作り、単語コストと連接コストの合計が最小の経路を
//! Viterbi で求める。2 位以降の候補は、文末から後ろ向きに A* 探索して取り出す。
//! 変換結果は文節に区切り、文節ごとに候補を持つ。

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::Arc;

use chrono::{Local, NaiveDate};

use crate::dates::date_candidates;
use crate::dict::Dictionary;
use crate::emoji::Emoji;
use crate::kana::{is_kana, to_katakana};

/// 文頭・文末の文脈 ID (Mozc の `BOS/EOS`)。
const BOUNDARY_ID: u16 = 0;
/// 辞書に無いかな 1 文字をそのまま通すコスト。辞書の単語が必ず優先される大きさにする。
const UNKNOWN_KANA_COST: i32 = 12000;
/// かな以外 (英数字・記号) の連続をそのまま通すコスト。
const LITERAL_RUN_COST: i32 = 3000;
/// 最良経路からこのコスト以上離れた経路は候補にしない。
const NBEST_COST_WINDOW: i32 = 9000;
/// N-best 探索で取り出す経路の上限。長い読みで探索が終わらなくなるのを防ぐ。
const NBEST_MAX_POPS: usize = 4000;

/// 文節の候補のうち、読み全体を変換した結果から取る数。
const CONVERSION_CANDIDATES: usize = 8;
/// そのうち、記号や日付の候補より前に置く数。
const LEADING_CONVERSIONS: usize = 3;
/// 記号や日付の候補のうち、上位に置く数。残りは一覧の末尾に置く。
const LEADING_SUPPLEMENTS: usize = 6;
const WORD_CANDIDATES: usize = 60;
/// 絵文字を引くのに使う、候補の先頭からの数。読みに加えて、これらの表記 (「猫」など) でも引く。
const EMOJI_KEYWORD_CANDIDATES: usize = 5;
/// 利用者が覚えさせた語 (`Converter::learn_phrase`) のコストの上限。辞書のよく使われる語と同じ程度。
/// 語のコストは、同じ読みの辞書だけの変換に勝つ値にする (`PHRASE_MARGIN`)。その値が大きい時は、
/// 文の途中でも語が使われるように、この上限まで下げる。
///
/// ただし、辞書だけの変換が「雪が」のようにかなの付属語で終わる読みでは、上限まで下げない。
/// その読みは多くの文に現れるので、下げると「雪賀」を覚えた後の「雪が積もる」まで語に置き換わる。
const PHRASE_COST: i32 = 3000;
/// 覚えた語を、同じ読みの辞書だけの変換よりどれだけ優先するか。
const PHRASE_MARGIN: i32 = 500;
/// 表記から辞書の語の並びを求める時に調べる経路の数。
const WORD_PATHS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub surface: String,
    /// 選ばれた時に、次回も先頭に出すよう覚えてよいかどうか。日付は日ごとに変わるので覚えない。
    pub learnable: bool,
}

/// 文節。`candidates` の先頭が、文全体として最も確からしい変換。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub reading: String,
    pub candidates: Vec<Candidate>,
}

/// 変換の経路を作っている、辞書の語。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    pub reading: String,
    pub surface: String,
    /// 助詞や助動詞のように、前の語と同じ文節に入る語かどうか。
    pub function: bool,
}

/// 利用者が覚えさせた語。左右の文脈 ID は、語を作っている最初と最後の辞書の語から取る。
struct Phrase {
    surface: String,
    lid: u16,
    rid: u16,
    cost: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversion {
    pub surface: String,
    pub cost: i32,
}

#[derive(Clone, Copy)]
enum Surface {
    Entry(u32),
    /// 読みの該当範囲をそのまま表記にする。
    Literal,
    /// 利用者が覚えさせた語。表記は読みから引く。
    Phrase,
}

struct Node {
    begin: usize,
    end: usize,
    lid: u16,
    rid: u16,
    cost: i32,
    surface: Surface,
    /// 文頭からこのノードまでの最小コスト (このノードのコストを含む)。
    best: i32,
}

struct Lattice<'a> {
    dictionary: &'a Dictionary,
    /// 利用者が覚えさせた語。変換に使わない時は `None`。
    phrases: Option<&'a Phrases>,
    reading: &'a str,
    /// 各文字のバイト位置。末尾に読み全体の長さを持つ。
    offsets: Vec<usize>,
    nodes: Vec<Node>,
    ends_at: Vec<Vec<usize>>,
}

impl<'a> Lattice<'a> {
    /// `mid_sentence` が true なら、読みが文頭だけでなく名詞の直後にも置かれうるものとして評価する。
    fn build(
        dictionary: &'a Dictionary,
        phrases: Option<&'a Phrases>,
        reading: &'a str,
        mid_sentence: bool,
    ) -> Self {
        let chars: Vec<char> = reading.chars().collect();
        let mut offsets: Vec<usize> = reading.char_indices().map(|(offset, _)| offset).collect();
        offsets.push(reading.len());
        let length = chars.len();
        let byte_to_char: HashMap<usize, usize> = offsets
            .iter()
            .enumerate()
            .map(|(index, &offset)| (offset, index))
            .collect();

        let mut begins_at: Vec<Vec<Node>> = (0..length).map(|_| Vec::new()).collect();
        let unknown_id = dictionary.unknown_id();
        let mut position = 0;
        while position < length {
            if !is_kana(chars[position]) {
                let run_end = (position..length)
                    .find(|&index| is_kana(chars[index]))
                    .unwrap_or(length);
                begins_at[position].push(Node {
                    begin: position,
                    end: run_end,
                    lid: unknown_id,
                    rid: unknown_id,
                    cost: LITERAL_RUN_COST,
                    surface: Surface::Literal,
                    best: i32::MAX,
                });
                position = run_end;
                continue;
            }
            let start_offset = offsets[position];
            dictionary.common_prefix_search(&reading[start_offset..], |matched, range| {
                let end = byte_to_char[&(start_offset + matched)];
                for index in range.indices() {
                    let entry = dictionary.entry(index);
                    begins_at[position].push(Node {
                        begin: position,
                        end,
                        lid: entry.lid,
                        rid: entry.rid,
                        cost: entry.cost as i32,
                        surface: Surface::Entry(index),
                        best: i32::MAX,
                    });
                }
            });
            begins_at[position].push(Node {
                begin: position,
                end: position + 1,
                lid: unknown_id,
                rid: unknown_id,
                cost: UNKNOWN_KANA_COST,
                surface: Surface::Literal,
                best: i32::MAX,
            });
            if let Some(phrases) = phrases {
                // 覚えた語の読みより長い範囲は引かない。語を覚えていなければ、何も調べない。
                let limit = (position + phrases.longest).min(length);
                let kana_end = (position..limit)
                    .find(|&index| !is_kana(chars[index]))
                    .unwrap_or(limit);
                for end in position + 1..=kana_end {
                    let Some(phrase) = phrases.words.get(&reading[start_offset..offsets[end]]) else {
                        continue;
                    };
                    begins_at[position].push(Node {
                        begin: position,
                        end,
                        lid: phrase.lid,
                        rid: phrase.rid,
                        cost: phrase.cost,
                        surface: Surface::Phrase,
                        best: i32::MAX,
                    });
                }
            }
            position += 1;
        }

        let mut lattice = Self {
            dictionary,
            phrases,
            reading,
            offsets,
            nodes: Vec::new(),
            ends_at: (0..=length).map(|_| Vec::new()).collect(),
        };
        lattice.forward(begins_at, mid_sentence);
        lattice
    }

    /// 前向きに最小コストを求める。ある位置で終わるノードは右文脈 ID ごとに最良のものだけを、
    /// 始まるノードは左文脈 ID ごとに 1 回だけ評価する。同音異義語が多い位置でも、
    /// 連接コストを引く回数を ID の種類数の積に抑えられる。
    fn forward(&mut self, begins_at: Vec<Vec<Node>>, mid_sentence: bool) {
        for (position, starting) in begins_at.into_iter().enumerate() {
            let mut best_by_rid: HashMap<u16, i32> = HashMap::new();
            if position == 0 {
                best_by_rid.insert(BOUNDARY_ID, 0);
                if mid_sentence {
                    // 助詞や助動詞は文頭への連接コストが大きい。直前に名詞がある場合も候補に入れる。
                    best_by_rid.insert(self.dictionary.unknown_id(), 0);
                }
            }
            for &index in &self.ends_at[position] {
                let node = &self.nodes[index];
                let best = best_by_rid.entry(node.rid).or_insert(i32::MAX);
                *best = (*best).min(node.best);
            }
            if best_by_rid.is_empty() {
                continue;
            }
            let mut best_by_lid: HashMap<u16, i32> = HashMap::new();
            for mut node in starting {
                let reach = *best_by_lid.entry(node.lid).or_insert_with(|| {
                    best_by_rid
                        .iter()
                        .map(|(&rid, &cost)| cost + self.dictionary.connection_cost(rid, node.lid))
                        .min()
                        .expect("at least one node ends here")
                });
                node.best = reach + node.cost;
                self.ends_at[node.end].push(self.nodes.len());
                self.nodes.push(node);
            }
        }
    }

    fn length(&self) -> usize {
        self.offsets.len() - 1
    }

    fn node_reading(&self, node: &Node) -> &str {
        &self.reading[self.offsets[node.begin]..self.offsets[node.end]]
    }

    fn surface(&self, node: &Node) -> &str {
        match node.surface {
            Surface::Entry(index) => self.dictionary.entry(index).surface,
            Surface::Literal => self.node_reading(node),
            Surface::Phrase => {
                let phrases = self.phrases.expect("a phrase node exists only with phrases");
                &phrases.words[self.node_reading(node)].surface
            }
        }
    }

    /// 直前の語と同じ文節に入るノードかどうか。
    fn is_function(&self, node: &Node) -> bool {
        match node.surface {
            Surface::Entry(_) => self.dictionary.is_function(node.lid),
            // 記号や英数字は、前の文節に付ける。辞書に無いかなは、独立した文節にする。
            Surface::Literal => !self.node_reading(node).chars().all(is_kana),
            Surface::Phrase => false,
        }
    }

    /// 文末への連接を含めない最小コスト。文の途中までしか入力されていない場合の評価に使う。
    fn open_cost(&self) -> Option<i32> {
        self.ends_at[self.length()]
            .iter()
            .map(|&index| self.nodes[index].best)
            .min()
    }

    /// コストの小さい順に、表記が異なる経路を最大 `limit` 件返す。経路は文頭からのノードの並び。
    fn nbest(&self, limit: usize) -> Vec<(i32, Vec<usize>)> {
        struct Step {
            node: usize,
            /// このノードより後ろ (文末側) のコスト。
            tail_cost: i32,
            next: Option<usize>,
        }

        let mut steps: Vec<Step> = Vec::new();
        let mut queue: BinaryHeap<Reverse<(i32, usize)>> = BinaryHeap::new();
        for &index in &self.ends_at[self.length()] {
            let node = &self.nodes[index];
            let tail_cost = self.dictionary.connection_cost(node.rid, BOUNDARY_ID);
            queue.push(Reverse((node.best + tail_cost, steps.len())));
            steps.push(Step {
                node: index,
                tail_cost,
                next: None,
            });
        }

        let mut results: Vec<(i32, Vec<usize>)> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut best_total = None;
        let mut pops = 0;
        while let Some(Reverse((total, step_index))) = queue.pop() {
            pops += 1;
            let limit_total = *best_total.get_or_insert(total) + NBEST_COST_WINDOW;
            if results.len() >= limit || pops > NBEST_MAX_POPS || total > limit_total {
                break;
            }
            let node = &self.nodes[steps[step_index].node];
            if node.begin == 0 {
                let mut path = Vec::new();
                let mut cursor = Some(step_index);
                while let Some(index) = cursor {
                    path.push(steps[index].node);
                    cursor = steps[index].next;
                }
                if seen.insert(self.path_surface(&path)) {
                    results.push((total, path));
                }
                continue;
            }
            let tail_base = steps[step_index].tail_cost + node.cost;
            for &previous_index in &self.ends_at[node.begin] {
                let previous = &self.nodes[previous_index];
                let tail_cost = tail_base + self.dictionary.connection_cost(previous.rid, node.lid);
                let estimate = previous.best + tail_cost;
                if estimate > limit_total {
                    continue;
                }
                queue.push(Reverse((estimate, steps.len())));
                steps.push(Step {
                    node: previous_index,
                    tail_cost,
                    next: Some(step_index),
                });
            }
        }
        results
    }

    fn path_surface(&self, path: &[usize]) -> String {
        path.iter().map(|&index| self.surface(&self.nodes[index])).collect()
    }

    /// 経路を文節に区切り、文節ごとの読みと表記を返す。
    fn bunsetsu(&self, path: &[usize]) -> Vec<(String, String)> {
        let mut segments: Vec<(String, String)> = Vec::new();
        for &index in path {
            let node = &self.nodes[index];
            match segments.last_mut() {
                Some((reading, surface)) if self.is_function(node) => {
                    reading.push_str(self.node_reading(node));
                    surface.push_str(self.surface(node));
                }
                _ => segments.push((self.node_reading(node).to_string(), self.surface(node).to_string())),
            }
        }
        segments
    }
}

/// 利用者が覚えさせた語。読みから引く。
#[derive(Default)]
struct Phrases {
    words: HashMap<String, Phrase>,
    /// 覚えた語の読みの長さの上限 (文字数)。ラティスの各位置で、この長さまでの読みを引く。
    /// 語を忘れても縮めない。
    longest: usize,
}

pub struct Converter {
    dictionary: Arc<Dictionary>,
    /// 読みから記号や顔文字を引く辞書。
    symbols: Option<Arc<Dictionary>>,
    emoji: Option<Arc<Emoji>>,
    phrases: Phrases,
    today: Box<dyn Fn() -> NaiveDate + Send + Sync>,
}

impl Converter {
    pub fn new(dictionary: Arc<Dictionary>) -> Self {
        Self {
            dictionary,
            symbols: None,
            emoji: None,
            phrases: Phrases::default(),
            today: Box::new(|| Local::now().date_naive()),
        }
    }

    /// 辞書の語の並びを、1 語として変換するように覚える。文の途中に現れた時にも使う。
    /// 並びの最初か最後の語が辞書に無ければ、覚えずに `false` を返す。
    pub fn learn_phrase(&mut self, words: &[(String, String)]) -> bool {
        let (Some((first_reading, first_surface)), Some((last_reading, last_surface))) = (words.first(), words.last())
        else {
            return false;
        };
        let (Some(first), Some(last)) = (
            self.entry(first_reading, first_surface),
            self.entry(last_reading, last_surface),
        ) else {
            return false;
        };
        let (lid, rid) = (first.lid, last.rid);
        let reading: String = words.iter().map(|(reading, _)| reading.as_str()).collect();
        let surface: String = words.iter().map(|(_, surface)| surface.as_str()).collect();
        // 辞書だけで変換した時の最良の経路に、文頭・文末への連接を含めて勝つコストにする。
        let lattice = Lattice::build(&self.dictionary, None, &reading, false);
        let rival = lattice.nbest(1).into_iter().next();
        let rival_ends_with_particle = rival.as_ref().is_some_and(|(_, path)| {
            path.last().is_some_and(|&index| {
                let node = &lattice.nodes[index];
                lattice.is_function(node) && lattice.surface(node) == lattice.node_reading(node)
            })
        });
        let connections = self.dictionary.connection_cost(BOUNDARY_ID, lid)
            + self.dictionary.connection_cost(rid, BOUNDARY_ID);
        let winning = rival
            .map_or(i32::MAX, |(cost, _)| cost)
            .saturating_sub(connections)
            .saturating_sub(PHRASE_MARGIN);
        let cost = if rival_ends_with_particle {
            winning
        } else {
            winning.min(PHRASE_COST)
        };
        self.phrases.longest = self.phrases.longest.max(reading.chars().count());
        self.phrases.words.insert(
            reading,
            Phrase {
                surface,
                lid,
                rid,
                cost,
            },
        );
        true
    }

    pub fn forget_phrase(&mut self, reading: &str) {
        self.phrases.words.remove(reading);
    }

    /// `reading` を `surface` に変換する経路を、辞書の語の並びで返す。
    /// 辞書の語だけで作れない表記 (ひらがなのまま、カタカナ、記号、日付) には `None` を返す。
    pub fn words(&self, reading: &str, surface: &str) -> Option<Vec<Word>> {
        if let Some(entry) = self.entry(reading, surface) {
            return Some(vec![Word {
                reading: reading.to_string(),
                surface: surface.to_string(),
                function: self.dictionary.is_function(entry.lid),
            }]);
        }
        let lattice = Lattice::build(&self.dictionary, None, reading, false);
        let (_, path) = lattice
            .nbest(WORD_PATHS)
            .into_iter()
            .find(|(_, path)| lattice.path_surface(path) == surface)?;
        path.iter()
            .map(|&index| {
                let node = &lattice.nodes[index];
                matches!(node.surface, Surface::Entry(_)).then(|| Word {
                    reading: lattice.node_reading(node).to_string(),
                    surface: lattice.surface(node).to_string(),
                    function: lattice.is_function(node),
                })
            })
            .collect()
    }

    /// 読みと表記が一致する辞書の語のうち、コストが最小のもの。
    fn entry(&self, reading: &str, surface: &str) -> Option<crate::dict::Entry<'_>> {
        let range = self.dictionary.lookup(reading)?;
        self.dictionary
            .entries(range)
            .filter(|entry| entry.surface == surface)
            .min_by_key(|entry| entry.cost)
    }

    pub fn with_symbols(mut self, symbols: Arc<Dictionary>) -> Self {
        self.symbols = Some(symbols);
        self
    }

    pub fn with_emoji(mut self, emoji: Arc<Emoji>) -> Self {
        self.emoji = Some(emoji);
        self
    }

    pub fn emoji(&self) -> Option<&Arc<Emoji>> {
        self.emoji.as_ref()
    }

    /// 日付の候補の基準になる日を差し替える。
    pub fn with_today(mut self, today: impl Fn() -> NaiveDate + Send + Sync + 'static) -> Self {
        self.today = Box::new(today);
        self
    }

    /// 読み全体を変換した結果を、コストの小さい順に返す。
    pub fn convert(&self, reading: &str, limit: usize) -> Vec<Conversion> {
        if reading.is_empty() {
            return Vec::new();
        }
        let lattice = Lattice::build(&self.dictionary, Some(&self.phrases), reading, false);
        lattice
            .nbest(limit)
            .into_iter()
            .map(|(cost, path)| Conversion {
                surface: lattice.path_surface(&path),
                cost,
            })
            .collect()
    }

    /// 読みを日本語として解釈した時の最小コスト。値が小さいほど日本語らしい。
    /// 文末への連接は含めない。
    pub fn open_cost(&self, reading: &str) -> Option<i32> {
        if reading.is_empty() {
            return None;
        }
        Lattice::build(&self.dictionary, None, reading, false).open_cost()
    }

    /// 読みが文の途中 (名詞の直後) にも置かれうるものとして求めた `open_cost`。
    /// 文頭に来にくい助詞も、文の途中であれば普通に現れる。
    pub fn flow_cost(&self, reading: &str) -> Option<i32> {
        if reading.is_empty() {
            return None;
        }
        Lattice::build(&self.dictionary, None, reading, true).open_cost()
    }

    /// 読みを変換して、文節に区切る。
    pub fn segments(&self, reading: &str) -> Vec<Segment> {
        if reading.is_empty() {
            return Vec::new();
        }
        let lattice = Lattice::build(&self.dictionary, Some(&self.phrases), reading, false);
        let Some((_, path)) = lattice.nbest(1).into_iter().next() else {
            return vec![self.segment(reading, None)];
        };
        lattice
            .bunsetsu(&path)
            .into_iter()
            .map(|(reading, surface)| self.segment(&reading, Some(&surface)))
            .collect()
    }

    /// 1 つの文節の候補を作る。`preferred` は、文全体の変換でこの文節に当たった表記。
    ///
    /// 並びは、`preferred`、読みを変換した上位の結果、記号や日付の上位、変換の残り、
    /// 読みに一致する単語、ひらがな、カタカナ、絵文字、記号の残りの順。
    pub fn segment(&self, reading: &str, preferred: Option<&str>) -> Segment {
        let mut candidates: Vec<Candidate> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut push = |candidates: &mut Vec<Candidate>, surface: String, learnable: bool| {
            if seen.insert(surface.clone()) {
                candidates.push(Candidate { surface, learnable });
            }
        };

        if let Some(surface) = preferred {
            push(&mut candidates, surface.to_string(), true);
        }
        let conversions = self.convert(reading, CONVERSION_CANDIDATES);
        let supplements = self.supplements(reading);
        let leading_supplements = supplements.len().min(LEADING_SUPPLEMENTS);

        for conversion in conversions.iter().take(LEADING_CONVERSIONS) {
            push(&mut candidates, conversion.surface.clone(), true);
        }
        for candidate in &supplements[..leading_supplements] {
            push(&mut candidates, candidate.surface.clone(), candidate.learnable);
        }
        for conversion in conversions.iter().skip(LEADING_CONVERSIONS) {
            push(&mut candidates, conversion.surface.clone(), true);
        }
        if let Some(range) = self.dictionary.lookup(reading) {
            for entry in self.dictionary.entries(range).take(WORD_CANDIDATES) {
                push(&mut candidates, entry.surface.to_string(), true);
            }
        }
        push(&mut candidates, reading.to_string(), true);
        push(&mut candidates, to_katakana(reading), true);
        if let Some(emoji) = &self.emoji {
            let keywords: Vec<String> = std::iter::once(reading.to_string())
                .chain(candidates.iter().take(EMOJI_KEYWORD_CANDIDATES).map(|candidate| candidate.surface.clone()))
                .collect();
            for keyword in keywords {
                for text in emoji.lookup(&keyword) {
                    push(&mut candidates, text.to_string(), true);
                }
            }
        }
        for candidate in &supplements[leading_supplements..] {
            push(&mut candidates, candidate.surface.clone(), candidate.learnable);
        }
        Segment {
            reading: reading.to_string(),
            candidates,
        }
    }

    /// かな漢字変換では出ない候補。日付と、読みに一致する記号・顔文字。
    fn supplements(&self, reading: &str) -> Vec<Candidate> {
        let mut candidates: Vec<Candidate> = date_candidates(reading, (self.today)())
            .into_iter()
            .map(|surface| Candidate {
                surface,
                learnable: false,
            })
            .collect();
        if let Some(symbols) = &self.symbols {
            if let Some(range) = symbols.lookup(reading) {
                candidates.extend(symbols.entries(range).map(|entry| Candidate {
                    surface: entry.surface.to_string(),
                    learnable: true,
                }));
            }
        }
        candidates
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::dict::DictionaryBuilder;

    /// テスト用の小さな辞書。文脈 ID は 0: 文頭・文末、1: 名詞、2: 助詞、3: 動詞、4: 助動詞。
    pub(crate) fn sample_dictionary() -> Arc<Dictionary> {
        const IDS: usize = 5;
        let mut connections = vec![0i16; IDS * IDS];
        let mut set = |rid: usize, lid: usize, cost: i16| connections[rid * IDS + lid] = cost;
        // 助詞は文頭に来にくく、名詞の後に来やすい。
        set(0, 2, 4000);
        set(1, 2, -500);
        set(1, 1, 1500);
        set(2, 2, 3000);
        set(2, 3, -300);
        set(0, 4, 4000);
        let mut builder = DictionaryBuilder::new(IDS, connections, 1);
        builder.mark_function(2);
        builder.mark_function(4);
        for (reading, lid, rid, cost, surface) in [
            ("きょう", 1, 1, 3000, "今日"),
            ("きょう", 1, 1, 5200, "京"),
            ("きょう", 1, 1, 5500, "教"),
            ("は", 2, 2, 1000, "は"),
            ("は", 1, 1, 4500, "歯"),
            ("はれ", 1, 1, 4000, "晴れ"),
            ("はれ", 1, 1, 6000, "腫れ"),
            ("あめ", 1, 1, 3800, "雨"),
            ("あめ", 1, 1, 5000, "飴"),
            ("たとえ", 1, 1, 3500, "例え"),
            ("たとえば", 1, 1, 3200, "例えば"),
            ("これ", 1, 1, 2500, "これ"),
            ("それ", 1, 1, 2600, "それ"),
            ("を", 2, 2, 900, "を"),
            ("の", 2, 2, 800, "の"),
            ("が", 2, 2, 950, "が"),
            ("たべる", 3, 3, 3600, "食べる"),
            ("わたし", 1, 1, 2800, "私"),
            ("なまえ", 1, 1, 3900, "名前"),
            ("こーひー", 1, 1, 4200, "コーヒー"),
            ("にほんご", 1, 1, 3300, "日本語"),
            ("にゅうりょく", 1, 1, 3700, "入力"),
            ("です", 4, 4, 1200, "です"),
            ("ます", 4, 4, 1300, "ます"),
            ("また", 1, 1, 2700, "また"),
            ("さけ", 1, 1, 4300, "酒"),
            ("まで", 2, 2, 1800, "まで"),
            ("と", 2, 2, 850, "と"),
            ("め", 1, 1, 4000, "目"),
            ("かね", 1, 1, 5300, "鐘"),
            ("みぎ", 1, 1, 3400, "右"),
            ("はめ", 1, 1, 3000, "羽目"),
            ("がわ", 2, 2, 3000, "側"),
            ("これあめ", 1, 1, 9500, "此雨"),
            ("ねこ", 1, 1, 4100, "猫"),
            ("えがお", 1, 1, 4400, "笑顔"),
            ("えもじ", 1, 1, 4600, "絵文字"),
        ] {
            builder.add(reading, lid, rid, cost, surface);
        }
        Arc::new(Dictionary::from_bytes(builder.build()).expect("valid dictionary"))
    }

    pub(crate) fn sample_symbols() -> Arc<Dictionary> {
        let mut builder = DictionaryBuilder::lookup_only();
        for (order, (reading, surface)) in [
            ("みぎ", "→"),
            ("みぎ", "⇒"),
            ("。", "。"),
            ("。", "…"),
            ("。", "．"),
            ("かお", "(^o^)"),
            ("かお", "(^^)"),
        ]
        .into_iter()
        .enumerate()
        {
            builder.add(reading, 0, 0, order as i16, surface);
        }
        Arc::new(Dictionary::from_bytes(builder.build()).expect("valid dictionary"))
    }

    pub(crate) fn sample_converter() -> Converter {
        Converter::new(sample_dictionary())
            .with_symbols(sample_symbols())
            .with_emoji(Arc::new(crate::emoji::tests::sample_emoji()))
            .with_today(|| NaiveDate::from_ymd_opt(2026, 11, 25).expect("valid date"))
    }

    fn surfaces(segment: &Segment) -> Vec<&str> {
        segment.candidates.iter().map(|c| c.surface.as_str()).collect()
    }

    fn words(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(reading, surface)| (reading.to_string(), surface.to_string()))
            .collect()
    }

    #[test]
    fn a_learned_phrase_is_converted_as_one_word() {
        // Arrange
        let mut converter = sample_converter();
        assert_eq!(converter.convert("はめ", 1)[0].surface, "羽目");

        // Act
        let learned = converter.learn_phrase(&words(&[("は", "歯"), ("め", "目")]));

        // Assert
        assert!(learned);
        assert_eq!(converter.convert("はめ", 1)[0].surface, "歯目");
        // 文の途中でも使い、後ろの助詞と同じ文節になる。
        let segments = converter.segments("きょうははめが");
        let readings: Vec<&str> = segments.iter().map(|segment| segment.reading.as_str()).collect();
        assert_eq!(readings, ["きょうは", "はめが"]);
        assert_eq!(segments[1].candidates[0].surface, "歯目が");
    }

    #[test]
    fn a_learned_phrase_costs_no_more_than_a_common_word() {
        // Arrange: 「これあめ」の辞書だけの変換は「これ雨」で、コストが大きい
        let mut converter = sample_converter();

        // Act
        converter.learn_phrase(&words(&[("これ", "これ"), ("あめ", "飴")]));

        // Assert
        // 文の途中でも使われるように、上限まで下げる。
        let conversions = converter.convert("これあめ", 1);
        assert_eq!(conversions[0].surface, "これ飴");
        assert_eq!(conversions[0].cost, PHRASE_COST);
    }

    #[test]
    fn a_phrase_read_like_a_word_and_a_particle_only_just_wins() {
        // Arrange: 「にほんごは」の辞書だけの変換は「日本語は」
        let mut converter = sample_converter();
        let rival = converter.convert("にほんごは", 1)[0].clone();
        assert_eq!(rival.surface, "日本語は");

        // Act
        converter.learn_phrase(&words(&[("にほんご", "日本語"), ("は", "歯")]));

        // Assert
        // 上限まで下げると、「日本語は」で始まる文がどれも「日本語歯」になる。
        let conversions = converter.convert("にほんごは", 1);
        assert_eq!(conversions[0].surface, "日本語歯");
        assert_eq!(conversions[0].cost, rival.cost - PHRASE_MARGIN);
    }

    #[test]
    fn a_learned_phrase_does_not_change_the_cost_used_for_judging() {
        // Arrange
        let mut converter = sample_converter();
        let open_cost = converter.open_cost("はめ");
        let flow_cost = converter.flow_cost("はめ");

        // Act
        converter.learn_phrase(&words(&[("は", "歯"), ("め", "目")]));

        // Assert
        // 表記の学習で、英語か日本語かの判定が変わらないようにする。
        assert_eq!(converter.open_cost("はめ"), open_cost);
        assert_eq!(converter.flow_cost("はめ"), flow_cost);
    }

    #[test]
    fn a_phrase_of_words_missing_from_the_dictionary_is_not_learned() {
        // Arrange
        let mut converter = sample_converter();

        // Act
        let learned = converter.learn_phrase(&words(&[("は", "葉"), ("め", "目")]));

        // Assert
        assert!(!learned);
        assert_eq!(converter.convert("はめ", 1)[0].surface, "羽目");
    }

    #[test]
    fn a_forgotten_phrase_is_not_used() {
        // Arrange
        let mut converter = sample_converter();
        converter.learn_phrase(&words(&[("は", "歯"), ("め", "目")]));

        // Act
        converter.forget_phrase("はめ");

        // Assert
        assert_eq!(converter.convert("はめ", 1)[0].surface, "羽目");
    }

    #[test]
    fn a_surface_is_split_into_dictionary_words() {
        // Arrange
        let converter = sample_converter();

        // Act
        let found = converter.words("はめが", "歯目が").expect("made of dictionary words");

        // Assert
        let pairs: Vec<(&str, &str, bool)> = found
            .iter()
            .map(|word| (word.reading.as_str(), word.surface.as_str(), word.function))
            .collect();
        assert_eq!(pairs, [("は", "歯", false), ("め", "目", false), ("が", "が", true)]);
        // 辞書に無いかなをそのまま通した表記は、語の並びにならない。
        assert_eq!(converter.words("はれれ", "晴れれ"), None);
    }

    #[test]
    fn best_conversion_uses_connection_costs() {
        // Arrange
        let converter = sample_converter();

        // Act
        let conversions = converter.convert("きょうははれ", 5);

        // Assert
        // 「は」は名詞の「歯」より助詞の方が、名詞の後に来るコストが小さい。
        assert_eq!(conversions[0].surface, "今日は晴れ");
    }

    #[test]
    fn nbest_is_ordered_by_cost_and_has_distinct_surfaces() {
        // Arrange
        let converter = sample_converter();

        // Act
        let conversions = converter.convert("きょうははれ", 10);

        // Assert
        let surfaces: Vec<&str> = conversions.iter().map(|c| c.surface.as_str()).collect();
        assert!(surfaces.contains(&"今日は腫れ"));
        assert!(surfaces.contains(&"京は晴れ"));
        let unique: HashSet<&str> = surfaces.iter().copied().collect();
        assert_eq!(unique.len(), surfaces.len());
        assert!(conversions.windows(2).all(|pair| pair[0].cost <= pair[1].cost));
    }

    #[test]
    fn best_cost_matches_a_hand_computed_path() {
        // Arrange
        let converter = sample_converter();

        // Act
        let conversions = converter.convert("これを", 1);

        // Assert
        // 文頭→これ (0) + これ (2500) + 名詞→助詞 (-500) + を (900) + 助詞→文末 (0)
        assert_eq!(conversions[0].surface, "これを");
        assert_eq!(conversions[0].cost, 2900);
        assert_eq!(converter.open_cost("これを"), Some(2900));
    }

    #[test]
    fn flow_cost_allows_a_noun_before_the_reading() {
        // Arrange
        let converter = sample_converter();

        // Act
        let at_start = converter.open_cost("を");
        let in_flow = converter.flow_cost("を");

        // Assert
        // 文頭→助詞の連接コストは 4000、名詞→助詞は -500。
        assert_eq!(at_start, Some(4900));
        assert_eq!(in_flow, Some(400));
        assert_eq!(converter.flow_cost(""), None);
    }

    #[test]
    fn unknown_kana_and_literal_runs_pass_through() {
        // Arrange
        let converter = sample_converter();

        // Act
        let unknown = converter.convert("ぬ", 3);
        let mixed = converter.convert("きょう3じ", 3);

        // Assert
        assert_eq!(unknown[0].surface, "ぬ");
        assert_eq!(mixed[0].surface, "今日3じ");
    }

    #[test]
    fn unknown_text_costs_more_than_dictionary_words() {
        let converter = sample_converter();
        let known = converter.open_cost("これ").expect("cost");
        let unknown = converter.open_cost("ぬぬ").expect("cost");
        assert!(unknown > known + 10000);
        assert_eq!(converter.open_cost(""), None);
    }

    #[test]
    fn sentence_is_split_where_an_independent_word_starts() {
        // Arrange
        let converter = sample_converter();

        // Act
        let segments = converter.segments("きょうははれです");

        // Assert
        // 助詞と助動詞は、前の語と同じ文節に入る。
        let readings: Vec<&str> = segments.iter().map(|s| s.reading.as_str()).collect();
        assert_eq!(readings, ["きょうは", "はれです"]);
        assert_eq!(segments[0].candidates[0].surface, "今日は");
        assert_eq!(segments[1].candidates[0].surface, "晴れです");
    }

    #[test]
    fn punctuation_and_letters_stay_with_the_previous_segment() {
        let converter = sample_converter();
        let readings: Vec<String> = converter
            .segments("それ、これ")
            .into_iter()
            .map(|segment| segment.reading)
            .collect();
        assert_eq!(readings, ["それ、", "これ"]);
    }

    #[test]
    fn segment_candidates_offer_other_conversions_and_kana() {
        // Arrange
        let converter = sample_converter();

        // Act
        let segment = converter.segment("きょうは", Some("今日は"));

        // Assert
        let surfaces = surfaces(&segment);
        assert_eq!(surfaces[0], "今日は");
        assert!(surfaces.contains(&"京は"));
        assert!(surfaces.contains(&"きょうは"));
        assert!(surfaces.contains(&"キョウハ"));
        let unique: HashSet<&&str> = surfaces.iter().collect();
        assert_eq!(unique.len(), surfaces.len());
    }

    #[test]
    fn dates_follow_the_first_conversions_and_are_not_learnable() {
        // Arrange
        let converter = sample_converter();

        // Act
        let segment = converter.segment("きょう", None);

        // Assert
        assert_eq!(
            &surfaces(&segment)[..6],
            ["今日", "京", "教", "2026-11-25", "20261125", "2026/11/25"]
        );
        let date = segment.candidates.iter().find(|c| c.surface == "2026-11-25").expect("date");
        assert!(!date.learnable);
        assert!(segment.candidates[0].learnable);
    }

    #[test]
    fn symbols_are_offered_for_their_readings() {
        let converter = sample_converter();
        assert_eq!(&surfaces(&converter.segment("みぎ", None))[..3], ["右", "→", "⇒"]);
        // かなではない読みも、記号の読みとして引ける。
        assert_eq!(&surfaces(&converter.segment("。", None))[..3], ["。", "…", "．"]);
        assert!(surfaces(&converter.segment("かお", None)).contains(&"(^o^)"));
    }

    #[test]
    fn emoji_follow_the_words_and_kana() {
        // Arrange
        let converter = sample_converter();

        // Act
        let neko = converter.segment("ねこ", None);
        let egao = converter.segment("えがお", None);

        // Assert: 読みで引いた絵文字の後に、表記 (「猫」「笑顔」) で引いた絵文字が続く。
        assert_eq!(surfaces(&neko), ["猫", "ねこ", "ネコ", "🐱", "🐈", "😺"]);
        assert_eq!(surfaces(&egao), ["笑顔", "えがお", "エガオ", "☺️"]);
    }

    #[test]
    fn converter_without_symbols_still_converts() {
        let converter = Converter::new(sample_dictionary());
        assert_eq!(surfaces(&converter.segment("みぎ", None))[0], "右");
        assert!(converter.segments("").is_empty());
    }
}

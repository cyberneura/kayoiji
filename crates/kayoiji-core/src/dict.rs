//! かな漢字変換用のバイナリ辞書。
//!
//! Mozc の OSS 辞書 (読み・左文脈 ID・右文脈 ID・コスト・表記) と連接コスト行列を
//! 1 ファイルにまとめる。ファイルは mmap して使うので、起動時に全体を読み込まない。

use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use fst::raw::Output;
use fst::MapBuilder;
use memmap2::Mmap;

const MAGIC: &[u8; 8] = b"KYJDICT2";
const HEADER_LEN: usize = 56;
const ENTRY_LEN: usize = 12;

/// Mozc の `id.def` で、未知語に割り当てる品詞。
const UNKNOWN_POS: &str = "名詞,一般,*,*,*,*,*";

/// 品詞が付属語 (直前の語と同じ文節に入る語) かどうか。`pos` は `id.def` の品詞の列。
fn is_function_pos(pos: &str) -> bool {
    let mut fields = pos.split(',');
    let (major, minor) = (fields.next().unwrap_or(""), fields.next().unwrap_or(""));
    matches!(major, "助詞" | "助動詞")
        || matches!(minor, "非自立" | "接尾")
        || (major == "記号" && matches!(minor, "句点" | "読点" | "括弧閉"))
}

enum Data {
    Mapped(Mmap),
    Owned(Vec<u8>),
}

impl Data {
    fn bytes(&self) -> &[u8] {
        match self {
            Data::Mapped(map) => map,
            Data::Owned(bytes) => bytes,
        }
    }
}

/// fst がファイルの一部分だけを参照するためのビュー。
struct Section {
    data: Arc<Data>,
    range: Range<usize>,
}

impl AsRef<[u8]> for Section {
    fn as_ref(&self) -> &[u8] {
        &self.data.bytes()[self.range.clone()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry<'a> {
    pub lid: u16,
    pub rid: u16,
    pub cost: i16,
    pub surface: &'a str,
}

/// 同じ読みを持つエントリーの範囲。コストの昇順に並んでいる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryRange {
    pub start: u32,
    pub count: u32,
}

impl EntryRange {
    fn from_value(value: u64) -> Self {
        Self {
            start: (value >> 16) as u32,
            count: (value & 0xffff) as u32,
        }
    }

    pub fn indices(self) -> Range<u32> {
        self.start..self.start + self.count
    }
}

pub struct Dictionary {
    data: Arc<Data>,
    map: fst::Map<Section>,
    entries: Range<usize>,
    strings: Range<usize>,
    connections: Range<usize>,
    /// 文脈 ID ごとに、付属語なら 1。
    functions: Range<usize>,
    id_count: usize,
    entry_count: usize,
    unknown_id: u16,
}

impl Dictionary {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        // SAFETY: マップしている間にファイルの中身が変わると未定義動作になる。辞書を作り直す側は
        // 同じファイルに上書きせず、別のファイルに書いてから置き換える (`write_replacing`)。
        // 置き換えた後も、マップ済みの古いファイルは変わらない。
        let map = unsafe { Mmap::map(&file)? };
        Self::from_data(Data::Mapped(map))
    }

    pub fn from_bytes(bytes: Vec<u8>) -> io::Result<Self> {
        Self::from_data(Data::Owned(bytes))
    }

    /// 読み込む時に全体を検証する。ファイルが途中で切れた・一部が化けたといった破損は、
    /// 打鍵中の panic ではなく、ここでのエラーになる。読みの索引 (fst) はチェックサムで確かめるので、
    /// チェックサムごと作り変えた悪意のあるファイルまでは防がない。
    fn from_data(data: Data) -> io::Result<Self> {
        let invalid = |message: &str| io::Error::new(io::ErrorKind::InvalidData, message.to_string());
        let bytes = data.bytes();
        if bytes.len() < HEADER_LEN || &bytes[..8] != MAGIC {
            return Err(invalid("not a Kayoiji dictionary"));
        }
        let id_count = u32::from_le_bytes(bytes[8..12].try_into().expect("4 bytes")) as usize;
        let unknown_id = u16::from_le_bytes(bytes[12..14].try_into().expect("2 bytes"));

        // セクションは、ヘッダーに書いてある長さの順に隙間なく並ぶ。
        let mut end = HEADER_LEN;
        let mut section = |header_offset: usize| -> io::Result<Range<usize>> {
            let field = bytes[header_offset..header_offset + 8].try_into().expect("8 bytes");
            let start = end;
            end = usize::try_from(u64::from_le_bytes(field))
                .ok()
                .and_then(|length| start.checked_add(length))
                .filter(|end| *end <= bytes.len())
                .ok_or_else(|| invalid("a dictionary section is larger than the file"))?;
            Ok(start..end)
        };
        let fst_range = section(16)?;
        let entries = section(24)?;
        let strings = section(32)?;
        let connections = section(40)?;
        let functions = section(48)?;
        let connections_len = id_count.checked_mul(id_count).and_then(|cells| cells.checked_mul(2));
        if functions.end != bytes.len()
            || Some(connections.len()) != connections_len
            || functions.len() != id_count
            || entries.len() % ENTRY_LEN != 0
            || usize::from(unknown_id) >= id_count
        {
            return Err(invalid("dictionary sections do not match the header"));
        }
        for record in bytes[entries.clone()].as_chunks::<ENTRY_LEN>().0 {
            let lid = usize::from(u16::from_le_bytes([record[0], record[1]]));
            let rid = usize::from(u16::from_le_bytes([record[2], record[3]]));
            let surface_len = usize::from(u16::from_le_bytes([record[6], record[7]]));
            let surface_offset = u32::from_le_bytes(record[8..12].try_into().expect("4 bytes")) as usize;
            let surface = bytes[strings.clone()].get(surface_offset..surface_offset + surface_len);
            if lid >= id_count || rid >= id_count || surface.is_none_or(|surface| std::str::from_utf8(surface).is_err())
            {
                return Err(invalid("a dictionary entry is broken"));
            }
        }

        let entry_count = entries.len() / ENTRY_LEN;
        let data = Arc::new(data);
        let map = fst::Map::new(Section {
            data: Arc::clone(&data),
            range: fst_range,
        })
        .map_err(|error| invalid(&error.to_string()))?;
        // 壊れた fst は、たどっている途中で panic することがある。
        map.as_fst().verify().map_err(|error| invalid(&error.to_string()))?;
        Ok(Self {
            data,
            map,
            entries,
            strings,
            connections,
            functions,
            id_count,
            entry_count,
            unknown_id,
        })
    }

    /// fst の値をエントリーの範囲に読み替える。エントリーの数を超える範囲は、無いものとして扱う。
    fn entry_range(&self, value: u64) -> Option<EntryRange> {
        let range = EntryRange::from_value(value);
        (range.start as usize + range.count as usize <= self.entry_count).then_some(range)
    }

    /// 文脈 ID が付属語 (助詞・助動詞・接尾語など) のものかどうか。
    pub fn is_function(&self, id: u16) -> bool {
        self.data.bytes()[self.functions.start + id as usize] != 0
    }

    /// 未知語のノードに使う文脈 ID。
    pub fn unknown_id(&self) -> u16 {
        self.unknown_id
    }

    /// 左側の単語の右文脈 ID と、右側の単語の左文脈 ID から連接コストを引く。
    pub fn connection_cost(&self, rid: u16, lid: u16) -> i32 {
        let index = self.connections.start + (rid as usize * self.id_count + lid as usize) * 2;
        let bytes = self.data.bytes();
        i16::from_le_bytes([bytes[index], bytes[index + 1]]) as i32
    }

    pub fn entry(&self, index: u32) -> Entry<'_> {
        let bytes = self.data.bytes();
        let offset = self.entries.start + index as usize * ENTRY_LEN;
        let record = &bytes[offset..offset + ENTRY_LEN];
        let surface_len = u16::from_le_bytes([record[6], record[7]]) as usize;
        let surface_offset =
            self.strings.start + u32::from_le_bytes(record[8..12].try_into().expect("4 bytes")) as usize;
        let surface = std::str::from_utf8(&bytes[surface_offset..surface_offset + surface_len])
            .expect("surfaces are validated when the dictionary is opened");
        Entry {
            lid: u16::from_le_bytes([record[0], record[1]]),
            rid: u16::from_le_bytes([record[2], record[3]]),
            cost: i16::from_le_bytes([record[4], record[5]]),
            surface,
        }
    }

    pub fn entries(&self, range: EntryRange) -> impl Iterator<Item = Entry<'_>> {
        range.indices().map(|index| self.entry(index))
    }

    /// 読みが完全に一致するエントリー。
    pub fn lookup(&self, reading: &str) -> Option<EntryRange> {
        self.map.get(reading).and_then(|value| self.entry_range(value))
    }

    /// `text` の先頭に一致する読みをすべて列挙する。コールバックには、一致した長さ
    /// (バイト数) とエントリーの範囲を渡す。
    pub fn common_prefix_search(&self, text: &str, mut visit: impl FnMut(usize, EntryRange)) {
        let fst = self.map.as_fst();
        let mut node = fst.root();
        let mut output = Output::zero();
        for (index, &byte) in text.as_bytes().iter().enumerate() {
            let Some(transition_index) = node.find_input(byte) else {
                return;
            };
            let transition = node.transition(transition_index);
            output = output.cat(transition.out);
            node = fst.node(transition.addr);
            if node.is_final() {
                let value = output.cat(node.final_output()).value();
                if let Some(range) = self.entry_range(value) {
                    visit(index + 1, range);
                }
            }
        }
    }
}

/// `path` のファイルを `bytes` の内容に置き換える。
///
/// 同じファイルに上書きすると、そのファイルを mmap しているプロセスが読んでいる最中の内容が変わる。
/// 別のファイルに書いてから名前を付け替えれば、マップ済みの古いファイルはそのまま残る。
pub fn write_replacing(path: &Path, bytes: &[u8]) -> io::Result<()> {
    // 同時に 2 つのプロセスが作り直しても、同じ一時ファイルに書かないようにする。
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".{}.tmp", std::process::id()));
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, path)
}

struct SourceEntry {
    reading: String,
    lid: u16,
    rid: u16,
    cost: i16,
    surface: String,
}

pub struct DictionaryBuilder {
    entries: Vec<SourceEntry>,
    connections: Vec<i16>,
    functions: Vec<u8>,
    id_count: usize,
    unknown_id: u16,
}

impl DictionaryBuilder {
    /// `connections` は `右文脈 ID * id_count + 左文脈 ID` の順に並んだ連接コスト。
    pub fn new(id_count: usize, connections: Vec<i16>, unknown_id: u16) -> Self {
        assert_eq!(connections.len(), id_count * id_count);
        Self {
            entries: Vec::new(),
            connections,
            functions: vec![0; id_count],
            id_count,
            unknown_id,
        }
    }

    /// 文脈 ID を付属語として登録する。
    pub fn mark_function(&mut self, id: u16) {
        self.functions[id as usize] = 1;
    }

    /// 読みから表記を引くだけの辞書を作る。連接コストを持たず、かな漢字変換には使わない。
    pub fn lookup_only() -> Self {
        Self::new(1, vec![0], 0)
    }

    pub fn add(&mut self, reading: &str, lid: u16, rid: u16, cost: i16, surface: &str) {
        self.entries.push(SourceEntry {
            reading: reading.to_string(),
            lid,
            rid,
            cost,
            surface: surface.to_string(),
        });
    }

    /// Mozc の `src/data/dictionary_oss` と同じ構成のディレクトリから読み込む。
    pub fn from_mozc_dir(dir: &Path) -> io::Result<Self> {
        let invalid = |message: String| io::Error::new(io::ErrorKind::InvalidData, message);

        let mut unknown_id = None;
        let mut function_ids = Vec::new();
        for line in BufReader::new(File::open(dir.join("id.def"))?).lines() {
            let line = line?;
            let Some((id, pos)) = line.split_once(' ') else {
                continue;
            };
            let id: u16 = id
                .parse()
                .map_err(|_| invalid(format!("id.def: unexpected line: {line}")))?;
            if pos == UNKNOWN_POS {
                unknown_id = Some(id);
            }
            if is_function_pos(pos) {
                function_ids.push(id);
            }
        }
        let unknown_id = unknown_id.ok_or_else(|| invalid(format!("id.def has no entry for {UNKNOWN_POS}")))?;

        let mut lines = BufReader::new(File::open(dir.join("connection_single_column.txt"))?).lines();
        let id_count: usize = lines
            .next()
            .transpose()?
            .and_then(|line| line.trim().parse().ok())
            .ok_or_else(|| invalid("connection file has no size line".to_string()))?;
        let mut connections = Vec::with_capacity(id_count * id_count);
        for line in lines {
            let cost: i16 = line?
                .trim()
                .parse()
                .map_err(|_| invalid("connection cost is not a 16-bit integer".to_string()))?;
            connections.push(cost);
        }
        if connections.len() != id_count * id_count {
            return Err(invalid(format!(
                "connection file has {} costs, expected {}",
                connections.len(),
                id_count * id_count
            )));
        }

        let mut builder = Self::new(id_count, connections, unknown_id);
        for id in function_ids {
            builder.mark_function(id);
        }
        for number in 0..10 {
            let path = dir.join(format!("dictionary{number:02}.txt"));
            for line in BufReader::new(File::open(&path)?).lines() {
                let line = line?;
                let fields: Vec<&str> = line.split('\t').collect();
                let [reading, lid, rid, cost, surface, ..] = fields[..] else {
                    return Err(invalid(format!("{}: unexpected line: {line}", path.display())));
                };
                let parse_error = || invalid(format!("{}: unexpected number: {line}", path.display()));
                builder.add(
                    reading,
                    lid.parse().map_err(|_| parse_error())?,
                    rid.parse().map_err(|_| parse_error())?,
                    cost.parse().map_err(|_| parse_error())?,
                    surface,
                );
            }
        }
        Ok(builder)
    }

    /// Mozc の記号 (`symbol/symbol.tsv`) と顔文字 (`emoticon/emoticon.tsv`) から、
    /// 読みで記号を引く辞書を作る。候補は元のファイルに並んでいる順に出る。
    pub fn from_mozc_symbols(dir: &Path) -> io::Result<Self> {
        /// すべての顔文字を出す読み。
        const EMOTICON_READINGS: [&str; 2] = ["かお", "かおもじ"];

        let mut builder = Self::lookup_only();
        let mut order: i16 = 0;
        let mut add = |builder: &mut Self, reading: &str, surface: &str| {
            builder.add(reading, 0, 0, order, surface);
            order = order.saturating_add(1);
        };
        for line in BufReader::new(File::open(dir.join("symbol/symbol.tsv"))?).lines().skip(1) {
            let line = line?;
            let mut fields = line.split('\t');
            let (Some(_pos), Some(surface), Some(readings)) = (fields.next(), fields.next(), fields.next()) else {
                continue;
            };
            for reading in readings.split(' ').filter(|reading| !reading.is_empty()) {
                add(&mut builder, reading, surface);
            }
        }
        for line in BufReader::new(File::open(dir.join("emoticon/emoticon.tsv"))?).lines().skip(1) {
            let line = line?;
            let mut fields = line.split('\t');
            let (Some(surface), Some(readings)) = (fields.next(), fields.next()) else {
                continue;
            };
            let readings = readings.split(' ').filter(|reading| !reading.is_empty());
            for reading in readings.chain(EMOTICON_READINGS) {
                add(&mut builder, reading, surface);
            }
        }
        Ok(builder)
    }

    pub fn build(mut self) -> Vec<u8> {
        self.entries
            .sort_by(|a, b| a.reading.cmp(&b.reading).then(a.cost.cmp(&b.cost)));

        let mut map = MapBuilder::memory();
        let mut records = Vec::with_capacity(self.entries.len() * ENTRY_LEN);
        let mut strings: Vec<u8> = Vec::new();
        let mut group_start = 0;
        while group_start < self.entries.len() {
            let reading = &self.entries[group_start].reading;
            // 範囲の件数は 16 bit で持つ。同じ読みのエントリーがそれを超える分は、
            // コストが高い側なので捨てる。
            let group_end = self.entries[group_start..]
                .iter()
                .position(|entry| &entry.reading != reading)
                .map_or(self.entries.len(), |offset| group_start + offset);
            let kept = (group_end - group_start).min(u16::MAX as usize);
            let first_index = (records.len() / ENTRY_LEN) as u64;
            for entry in &self.entries[group_start..group_start + kept] {
                records.extend_from_slice(&entry.lid.to_le_bytes());
                records.extend_from_slice(&entry.rid.to_le_bytes());
                records.extend_from_slice(&entry.cost.to_le_bytes());
                records.extend_from_slice(&(entry.surface.len() as u16).to_le_bytes());
                records.extend_from_slice(&(strings.len() as u32).to_le_bytes());
                strings.extend_from_slice(entry.surface.as_bytes());
            }
            map.insert(reading, first_index << 16 | kept as u64)
                .expect("readings are inserted in sorted order");
            group_start = group_end;
        }
        let fst_bytes = map.into_inner().expect("in-memory fst");

        let mut output = Vec::with_capacity(
            HEADER_LEN
                + fst_bytes.len()
                + records.len()
                + strings.len()
                + self.connections.len() * 2
                + self.functions.len(),
        );
        output.extend_from_slice(MAGIC);
        output.extend_from_slice(&(self.id_count as u32).to_le_bytes());
        output.extend_from_slice(&self.unknown_id.to_le_bytes());
        output.extend_from_slice(&[0, 0]);
        output.extend_from_slice(&(fst_bytes.len() as u64).to_le_bytes());
        output.extend_from_slice(&(records.len() as u64).to_le_bytes());
        output.extend_from_slice(&(strings.len() as u64).to_le_bytes());
        output.extend_from_slice(&((self.connections.len() * 2) as u64).to_le_bytes());
        output.extend_from_slice(&(self.functions.len() as u64).to_le_bytes());
        output.extend_from_slice(&fst_bytes);
        output.extend_from_slice(&records);
        output.extend_from_slice(&strings);
        for cost in &self.connections {
            output.extend_from_slice(&cost.to_le_bytes());
        }
        output.extend_from_slice(&self.functions);
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_bytes() -> Vec<u8> {
        // ID は 0 (文頭・文末)、1、2 の 3 つ。
        let mut connections = vec![0i16; 9];
        connections[3 + 2] = 700;
        let mut builder = DictionaryBuilder::new(3, connections, 1);
        builder.mark_function(2);
        builder.add("きょう", 1, 1, 3000, "今日");
        builder.add("きょう", 1, 1, 5000, "京");
        builder.add("きょうは", 1, 2, 4000, "今日は");
        builder.add("き", 2, 2, 3500, "木");
        builder.build()
    }

    fn sample() -> Dictionary {
        Dictionary::from_bytes(sample_bytes()).expect("valid dictionary")
    }

    /// ヘッダーの `offset` にある、セクションの長さ。
    fn section_len(bytes: &[u8], offset: usize) -> usize {
        u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("8 bytes")) as usize
    }

    /// エントリーのセクションが始まる位置。
    fn entries_start(bytes: &[u8]) -> usize {
        HEADER_LEN + section_len(bytes, 16)
    }

    fn is_invalid_data(result: io::Result<Dictionary>) -> bool {
        result.is_err_and(|error| error.kind() == io::ErrorKind::InvalidData)
    }

    #[test]
    fn a_section_length_that_overflows_is_rejected() {
        // Arrange
        let mut bytes = sample_bytes();
        bytes[24..32].copy_from_slice(&u64::MAX.to_le_bytes());

        // Act
        let result = Dictionary::from_bytes(bytes);

        // Assert
        assert!(is_invalid_data(result));
    }

    #[test]
    fn an_entry_with_an_unknown_context_id_is_rejected() {
        // Arrange
        let mut bytes = sample_bytes();
        let first_entry = entries_start(&bytes);
        // 左文脈 ID を、ID の数 (3) 以上にする。連接コストを引くと範囲外になる値。
        bytes[first_entry..first_entry + 2].copy_from_slice(&3u16.to_le_bytes());

        // Act
        let result = Dictionary::from_bytes(bytes);

        // Assert
        assert!(is_invalid_data(result));
    }

    #[test]
    fn an_entry_whose_surface_is_outside_the_strings_is_rejected() {
        // Arrange
        let mut bytes = sample_bytes();
        let first_entry = entries_start(&bytes);
        bytes[first_entry + 8..first_entry + 12].copy_from_slice(&u32::MAX.to_le_bytes());

        // Act
        let result = Dictionary::from_bytes(bytes);

        // Assert
        assert!(is_invalid_data(result));
    }

    #[test]
    fn a_surface_that_is_not_utf8_is_rejected() {
        // Arrange
        let mut bytes = sample_bytes();
        let strings_start = entries_start(&bytes) + section_len(&bytes, 24);
        bytes[strings_start] = 0xff;

        // Act
        let result = Dictionary::from_bytes(bytes);

        // Assert
        assert!(is_invalid_data(result));
    }

    #[test]
    fn a_damaged_reading_index_is_rejected() {
        // Arrange
        let mut bytes = sample_bytes();
        // fst の本体の 1 バイトを変える。長さは変わらないので、セクションの検査は通る。
        let middle = HEADER_LEN + section_len(&bytes, 16) / 2;
        bytes[middle] ^= 0x55;

        // Act
        let result = Dictionary::from_bytes(bytes);

        // Assert
        assert!(is_invalid_data(result));
    }

    #[test]
    fn a_reading_that_points_past_the_entries_is_not_found() {
        // Arrange
        let mut bytes = sample_bytes();
        // 最後のエントリー (読みの順で最後の「きょうは」) を取り除く。読みの索引には残る。
        let entries_len = section_len(&bytes, 24);
        let last_entry = entries_start(&bytes) + entries_len - ENTRY_LEN;
        bytes.drain(last_entry..last_entry + ENTRY_LEN);
        bytes[24..32].copy_from_slice(&((entries_len - ENTRY_LEN) as u64).to_le_bytes());
        let dictionary = Dictionary::from_bytes(bytes).expect("the sections still match");
        let mut found = Vec::new();

        // Act
        let exact = dictionary.lookup("きょうは");
        dictionary.common_prefix_search("きょうは", |length, _| found.push(length));

        // Assert
        assert_eq!(exact, None);
        assert_eq!(found, ["き".len(), "きょう".len()]);
    }

    #[test]
    fn lookup_returns_entries_in_cost_order() {
        // Arrange
        let dictionary = sample();

        // Act
        let range = dictionary.lookup("きょう").expect("reading exists");
        let surfaces: Vec<&str> = dictionary.entries(range).map(|entry| entry.surface).collect();

        // Assert
        assert_eq!(surfaces, ["今日", "京"]);
        assert_eq!(dictionary.lookup("きょ"), None);
    }

    #[test]
    fn common_prefix_search_visits_every_matching_reading() {
        // Arrange
        let dictionary = sample();
        let mut found = Vec::new();

        // Act
        dictionary.common_prefix_search("きょうはあめ", |length, range| {
            let first = dictionary.entry(range.start);
            found.push((length, first.surface.to_string()));
        });

        // Assert
        assert_eq!(
            found,
            [
                ("き".len(), "木".to_string()),
                ("きょう".len(), "今日".to_string()),
                ("きょうは".len(), "今日は".to_string()),
            ]
        );
    }

    #[test]
    fn connection_cost_is_indexed_by_right_then_left_id() {
        let dictionary = sample();
        assert_eq!(dictionary.connection_cost(1, 2), 700);
        assert_eq!(dictionary.connection_cost(2, 1), 0);
        assert_eq!(dictionary.unknown_id(), 1);
    }

    #[test]
    fn function_ids_are_stored_per_context_id() {
        let dictionary = sample();
        assert!(!dictionary.is_function(1));
        assert!(dictionary.is_function(2));
    }

    #[test]
    fn function_parts_of_speech_follow_the_previous_word() {
        for pos in [
            "助詞,格助詞,一般,*,*,*,を",
            "助動詞,*,*,*,特殊・デス,基本形,です",
            "動詞,非自立,*,*,一段,連用形,*",
            "名詞,接尾,人名,*,*,*,さん",
            "記号,句点,*,*,*,*,*",
        ] {
            assert!(is_function_pos(pos), "{pos}");
        }
        for pos in ["名詞,一般,*,*,*,*,*", "動詞,自立,*,*,一段,基本形,*", "記号,括弧開,*,*,*,*,*", "BOS/EOS,*,*,*,*,*,*"] {
            assert!(!is_function_pos(pos), "{pos}");
        }
    }

    #[test]
    fn lookup_only_dictionary_keeps_the_insertion_order() {
        // Arrange
        let mut builder = DictionaryBuilder::lookup_only();
        for (order, surface) in ["→", "⇒", "▶"].iter().enumerate() {
            builder.add("みぎ", 0, 0, order as i16, surface);
        }
        let dictionary = Dictionary::from_bytes(builder.build()).expect("valid dictionary");

        // Act
        let range = dictionary.lookup("みぎ").expect("reading exists");
        let surfaces: Vec<&str> = dictionary.entries(range).map(|entry| entry.surface).collect();

        // Assert
        assert_eq!(surfaces, ["→", "⇒", "▶"]);
    }

    #[test]
    fn rejects_data_that_is_not_a_dictionary() {
        assert!(Dictionary::from_bytes(b"not a dictionary".to_vec()).is_err());

        let mut truncated = {
            let builder = DictionaryBuilder::new(1, vec![0], 0);
            builder.build()
        };
        truncated.pop();
        assert!(Dictionary::from_bytes(truncated).is_err());
    }
}

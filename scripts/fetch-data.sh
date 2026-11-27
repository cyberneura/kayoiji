#!/usr/bin/env bash
# 辞書の元になる第三者のデータを取得する。
# データはコミットしない。大きい上に、それぞれのライセンスがある (THIRD-PARTY-NOTICES.md)。
set -euo pipefail

cd "$(dirname "$0")/.."

# 作り直しても同じ辞書になるように、コミットを固定する。
MOZC_SHA=ebe10d6c8bad69637e13ba26c4ef22989cd88767
WIKIFREQ_SHA=d11c4fab211156e96ab5943850dd6f98604707fb

MOZC_BASE="https://raw.githubusercontent.com/google/mozc/${MOZC_SHA}/src/data"
WIKIFREQ_BASE="https://raw.githubusercontent.com/IlyaSemenov/wikipedia-word-frequency/${WIKIFREQ_SHA}"

fetch() {
  local url="$1" dest="$2"
  if [[ -s "$dest" ]]; then
    return
  fi
  mkdir -p "$(dirname "$dest")"
  echo "fetch $url"
  # 転送が止まったまま待ち続けないように、30 秒間 1KB/秒を下回ったら打ち切る。
  curl -fsSL --retry 3 --connect-timeout 20 --speed-limit 1024 --speed-time 30 -o "$dest.part" "$url"
  mv "$dest.part" "$dest"
}

for i in 00 01 02 03 04 05 06 07 08 09; do
  fetch "$MOZC_BASE/dictionary_oss/dictionary$i.txt" "data/raw/mozc/dictionary$i.txt"
done
fetch "$MOZC_BASE/dictionary_oss/connection_single_column.txt" "data/raw/mozc/connection_single_column.txt"
fetch "$MOZC_BASE/dictionary_oss/id.def" "data/raw/mozc/id.def"
fetch "$MOZC_BASE/dictionary_oss/evaluation.tsv" "data/raw/mozc/evaluation.tsv"
fetch "$MOZC_BASE/dictionary_oss/README.txt" "data/raw/mozc/README.txt"
fetch "$MOZC_BASE/symbol/symbol.tsv" "data/raw/mozc/symbol/symbol.tsv"
fetch "$MOZC_BASE/emoticon/emoticon.tsv" "data/raw/mozc/emoticon/emoticon.tsv"
fetch "https://raw.githubusercontent.com/google/mozc/${MOZC_SHA}/LICENSE" "data/raw/mozc/LICENSE"

fetch "$WIKIFREQ_BASE/results/enwiki-2023-04-13.txt" "data/raw/english/enwiki-2023-04-13.txt"
fetch "$WIKIFREQ_BASE/LICENSE" "data/raw/english/LICENSE"

# 絵文字の一覧と、キーワードから絵文字を引く索引 (emoji-explorer)。版を固定する手段が無いので、
# 取得し直すと内容が変わることがある。
fetch "https://emoji.ytyng.com/emoji.ja.json" "data/raw/emoji/emoji.ja.json"
fetch "https://emoji.ytyng.com/search-index.json" "data/raw/emoji/search-index.json"

echo "done: data/raw"

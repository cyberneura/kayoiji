# Third-Party Notices

Kayoiji は、以下の第三者のデータを使う。

## Mozc

- 用途: かな漢字変換の辞書 (`dictionary_oss` の辞書と連接コスト行列)、記号 (`symbol/symbol.tsv`)、
  顔文字 (`emoticon/emoticon.tsv`)、ローマ字テーブル (`crates/kayoiji-core/data/romaji-hiragana.tsv`)
- 入手元: https://github.com/google/mozc (commit `ebe10d6c8bad69637e13ba26c4ef22989cd88767`)
- 辞書はリポジトリには含めず、`scripts/fetch-data.sh` で取得して、ビルドしたものをアプリに同梱する

```
Copyright 2010-2018, Google Inc.
All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are
met:

    * Redistributions of source code must retain the above copyright
notice, this list of conditions and the following disclaimer.
    * Redistributions in binary form must reproduce the above
copyright notice, this list of conditions and the following disclaimer
in the documentation and/or other materials provided with the
distribution.
    * Neither the name of Google Inc. nor the names of its
contributors may be used to endorse or promote products derived from
this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
"AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT
OWNER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```

Mozc の OSS 辞書は IPAdic (mecab-ipadic-2.7.0-20070801) と沖縄辞書を含む。それぞれのライセンスは、
Mozc の `src/data/dictionary_oss/README.txt` にある (取得後は `data/raw/mozc/README.txt`)。
アプリを配布する時は、その全文を同梱すること。

## wikipedia-word-frequency

- 用途: 英単語の出現頻度 (`enwiki-2023-04-13.txt`)
- 入手元: https://github.com/IlyaSemenov/wikipedia-word-frequency
  (commit `d11c4fab211156e96ab5943850dd6f98604707fb`)
- リポジトリには含めず、`scripts/fetch-data.sh` で取得して、ビルドしたものをアプリに同梱する
- ライセンス: MIT (Copyright (c) 2015 Ilya Semenov)。全文は取得後の `data/raw/english/LICENSE` にある

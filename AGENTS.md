# Kayoiji

macOS の日本語入力メソッド。「かな」「英数」キーで切り替える日本語入力が既定で、オプションの自動切り替えを
有効にすると、日本語と英語をキーで切り替えずに入力できる。全体像は `README.md` を読むこと。

## 構成

- `crates/kayoiji-core`: OS に依存しないエンジン。判定 (`detector.rs`)、状態機械 (`engine.rs`)、
  かな漢字変換 (`converter.rs`)、辞書 (`dict.rs`)、判断データベース (`rules.rs`、`data/rules.toml`)
- `crates/kayoiji-cli`: 辞書のビルド、動作確認 (`type`、`explain`)、精度の評価 (`evaluate`)
- `src-tauri`: Tauri アプリ。`imk.rs` が InputMethodKit との接続、`overlay.rs` が候補ウインドウ
- `pad/src-tauri`: KayoijiPad。エンジンを入力メソッドとして入れずに試すための、テキストエリアだけのアプリ
- `src`: SvelteKit のフロントエンド (`/overlay`、`/settings`、KayoijiPad の `/pad`)。
  KayoijiPad のテキストの編集は `src/lib/pad.ts` にあり、`pad.test.ts` でテストする

## コマンド

```sh
pnpm data                    # 辞書の元データを取得して data/build に辞書を作る
cargo test --workspace
cargo clippy --workspace --all-targets
pnpm test                    # フロントエンドのユニットテスト (vitest)
pnpm check
pnpm pad                     # KayoijiPad を開発モードで起動する
./scripts/install-macos.sh   # ビルドして入力メソッドとしてインストールする。インストール済みなら置き換える (j-menu の「Install locally」)
```

## 開発の決まり

- エンジンの挙動を変えたら、`engine.rs` のテストに打鍵の列 (`Simulator::run`) で場面を足す。
  テストは小さな辞書 (`converter::tests::sample_dictionary`) で動くので、実辞書は要らない
- エンジンの既定は日本語入力 (`EngineConfig::auto_switch` が false)。判定を通る場面のテストは `simulator()`
  (自動切り替え)、日本語入力のテストは `simulator_with(false)` で書く。`kayoiji-cli` はいつも自動切り替えで動く
- OS の入力モード (`Info.plist` の `ComponentInputModeDict`) は増やさない。登録済みのバンドルに足した
  入力モードを、OS がログインし直さずに認識するかは確かめられていない。日本語入力と自動切り替えは、
  同じ入力モード ID のまま設定で切り替える
- 判定のしきい値や規則を変えたら、`kayoiji-cli evaluate` を実行して、変更前後の数値を比べる。
  個別の単語は `kayoiji-cli explain <単語>` で値を見る
- エンジンの挙動は、まず KayoijiPad (`pnpm pad`) で確認する。OS の入力ソースが英字の時だけ内蔵のエンジンが動く
- 入力メソッドは OS が起動する。`pnpm tauri dev` では入力を試せないので、入力メソッドとしての確認は
  `./scripts/install-macos.sh` で入れ直して行う。入れ直すと利用者の入力環境が変わるので、
  利用者に断ってから行う
- InputMethodKit のコールバックはメインスレッドで呼ばれる。入力先 (`client`) のメソッドを呼ぶ間は、
  `imk.rs` の `RUNTIME` を借用しない (入力先からの呼び出しが戻ってくることがある)
- KayoijiPad を開発モードで起動している間は、Rust のソースを保存するたびにアプリが再起動する。
  利用者が試している間は、Rust のソースを変えない
- 入力メソッドが起動しないと、選んでも何も打てなくなる。起動を止めてよいのは辞書が開けない時だけで、
  利用者のファイル (`rules.toml`、`settings.toml`、`learning.json`) が読めない時は既定の内容で起動する
- 入力先の確定済みの文字列を書き換える処理 (Ctrl+J の読み直し) は、書き換える前に入力先の内容と照合する。
  クリックでのキャレットの移動は、入力メソッドに通知されない
- `scripts/install-macos.sh` の入れ替えの部分を変えたら、インストール先を変数で差し替えて
  スクラッチのディレクトリで実行し、失敗や中断の後に使える版が残ることを確かめる
- 辞書の形式 (`dict.rs` の `MAGIC`) を変えたら、`pnpm data` で `data/build` を作り直す
- 辞書の元データ (`data/raw`) とビルドした辞書 (`data/build`) はコミットしない
- コードコメントは日本語で、コードから読み取れない理由だけを書く

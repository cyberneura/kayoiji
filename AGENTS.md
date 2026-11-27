# Kayoiji

macOS の日本語入力メソッド。「かな」「英数」キーで切り替える日本語入力が既定で、オプションの自動切り替えを
有効にすると、日本語と英語をキーで切り替えずに入力できる。全体像は `README.md` を読むこと。

## 構成

- `crates/kayoiji-core`: OS に依存しないエンジン。判定 (`detector.rs`)、状態機械 (`engine.rs`)、
  かな漢字変換 (`converter.rs`)、辞書 (`dict.rs`)、判断データベース (`rules.rs`、`data/rules.toml`)
- `crates/kayoiji-check`: 英文チェックの部品。綴りの検査 (`spelling.rs`) と、言語モデルの補助プロセス
  (`helper/english-check.swift`) の呼び出し (`review.rs`)
- `crates/kayoiji-cli`: 辞書のビルド、動作確認 (`type`、`explain`)、精度の評価 (`evaluate`)
- `crates/kayoiji-windows`: Windows 版。TSF のテキストサービスの DLL。入力先のプロセスに読み込まれ、
  スレッドごとにエンジンを持つ。候補ウインドウは Win32 と GDI で描く (`tsf/window.rs`)。OS に依存しない
  キーの読み替え (`keys.rs`) と候補ウインドウの内容 (`panel.rs`) はどの OS でもテストする
- `src-tauri`: Tauri アプリ。`imk.rs` が InputMethodKit との接続、`overlay.rs` が候補ウインドウ
- `pad/src-tauri`: KayoijiPad。エンジンを入力メソッドとして入れずに試すための、テキストエリアだけのアプリ
- `src`: SvelteKit のフロントエンド (`/overlay`、`/settings`、KayoijiPad の `/pad`)。
  KayoijiPad のテキストの編集は `src/lib/pad.ts` にあり、`pad.test.ts` でテストする

## コマンド

```sh
pnpm data                    # 辞書の元データを取得して data/build に辞書を作る
pnpm helper                  # 英文チェックの補助プロセスを data/build にビルドする。無いとアプリのクレートをビルドできない
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
- 入力ソースの一覧に並ぶのも、メニューバーのアイコンが決まるのも、OS の入力モード (`Info.plist` の
  `ComponentInputModeDict`) の単位。一覧に Kayoiji を 1 つだけ並べたままアイコンを切り替えるために、
  Google 日本語入力と同じ形にしてある
  - 直接入力は Kayoiji の中のモードにしない。エンジンが直接入力になったら、`imk.rs` が OS の英字の
    キーボード配列を選ぶ (`input_source::select_ascii_layout`)。待ち時間は入れない。「かな」で OS が
    Kayoiji を選び直すと、`mode_selected` がエンジンを「かな」で入るモードにする
  - 入力モードは日本語入力 (`あ`) と自動切り替え (`通`) の 2 つで、設定 (`auto_switch`) に合う側だけを
    有効にする (`imk.rs` の `sync_input_mode`、`input_source::keep_one_mode_enabled`)
  - 英文チェックに OS の入力モードは無い
- 「英数」を押すと OS の英字のキーボード配列に切り替わり、以後の打鍵は Kayoiji に届かない。「英数」を
  続けて押す操作は作れない。続けて押す操作は「かな」で作る (`DOUBLE_PRESS_MS`)
- 入力モードやアイコンを変えた版は、ログインし直すまで、システム設定の一覧やメニューバーに前の内容が残る
  ことがある。OS が持っている内容は、`TISCreateInputSourceList` を読むだけの Swift で確かめられる
  (入力ソースを登録・有効化・選択するコマンドは、利用者に断らずに実行しない)
- 打鍵の文字は、Shift を反映した `characters` から取る (`imk.rs` の `key_press`)。
  `charactersIgnoringModifiers` は、入力メソッドに届く時には Shift も無視した小文字になっている
- メニューバーのアイコンは `scripts/make-menu-icon.swift` で作る
- 変換の学習は 2 種類ある。文節ごとに選んだ候補 (`Learning::conversions`) は、エンジンが候補の先頭に
  置くだけで、読みが文節と一致する時にしか効かない。区切り直して覚えた語 (`Learning::phrases`) は、
  変換器のラティスに入る (`Converter::learn_phrase`) ので、文の途中や助詞の前でも効く。
  変換の結果を変えるのは後者だけなので、覚える条件 (`Engine::learn_phrase`) やコスト (`PHRASE_COST`) を
  変えたら、実辞書で確かめる (`kayoiji-cli type` で区切り直して確定し、続けて別の読みを変換する)。
  テスト用の辞書と実辞書では、付属語の範囲が違う (実辞書は「路」のような接尾語も付属語)
- キーの動作を変えたら、その動作を説明している 3 か所を合わせる。`README.md` のキー操作の表、
  設定画面の表 (`src/routes/settings`)、候補ウインドウの案内 (`src/routes/overlay`)
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
- 学習 (`learning.json`) は、同じファイルに書くプロセスが複数ありうる (Windows では入力先のプロセスごと、macOS でも
  入力メソッドと KayoijiPad)。保存はロック (`learning.json.lock`) を取ってから読み直し、そのプロセスの変更
  (`learning.rs` の `Change`) を重ねて書く。変更の種類を足す時は、2 度適用しても結果が変わらないようにする
- 入力メソッドが起動しないと、選んでも何も打てなくなる。起動を止めてよいのは辞書が開けない時だけで、
  利用者のファイル (`rules.toml`、`settings.toml`、`learning.json`) が読めない時は既定の内容で起動する
- 入力先の確定済みの文字列を書き換える処理 (Ctrl+J の読み直し) は、書き換える前に入力先の内容と照合する。
  クリックでのキャレットの移動は、入力メソッドに通知されない
- `scripts/install-macos.sh` の入れ替えの部分を変えたら、インストール先を変数で差し替えて
  スクラッチのディレクトリで実行し、失敗や中断の後に使える版が残ることを確かめる
- 英文チェック (`InputMode::English`) は、エンジンが英文を受け取るだけで、綴りの検査 (`kayoiji-check` の
  `spelling`、OS のスペルチェッカー) と訂正文・訳 (`review`、補助プロセス `helper/english-check.swift`) は
  アプリ側 (`imk.rs`、KayoijiPad) がエンジンの外で作る。補助プロセスを変えたら、`pnpm helper` の後に
  `KAYOIJI_ENGLISH_CHECK="$PWD/data/build/english-check-<ターゲット名>" cargo test -p kayoiji-check -- --ignored` で
  実際の言語モデルを通して確かめる (テストの作業ディレクトリはクレートの場所なので、絶対パスで渡す)
- 言語モデル (Foundation Models) には、訂正と訳を別々の 1 行の問いで頼み、答えの長さを抑える。
  構造化出力 (`@Generable`) で 1 度に受け取ると、生成が止まらずに 10 秒以上かかることがあった
- メインスレッドは補助プロセスを待たない。起動・依頼の書き込み・答えの読み取りは `Reviewer` の中のスレッドが行う。
  綴りの訂正候補を作る処理 (1 語あたり数十ミリ秒) も打鍵のたびには行わず、打鍵が止まってから行う (`Guesses::Lookup`)
- 辞書の形式 (`dict.rs` の `MAGIC`) を変えたら、`pnpm data` で `data/build` を作り直す
- 辞書の元データ (`data/raw`) とビルドした辞書 (`data/build`) はコミットしない
- Windows 版 (`crates/kayoiji-windows/src/tsf`) は、macOS でも `cargo check -p kayoiji-windows --target x86_64-pc-windows-msvc`
  で型を検査できる (リンクはできない)。ビルドとテストは `.github/workflows/windows.yml` の Windows の runner で行う。
  TSF は編集セッションの途中でこちらのメソッドを呼び返すことがあるので、状態の `RefCell` を借りたまま TSF を呼ばない
- Windows 版のインストーラー (`installer/windows/kayoiji.nsi`) は、まだ無い名前の版ごとのフォルダー (同じ版を入れ直す時は
  `<版>-2`) に DLL と辞書を置いてから、登録し直す。入力方式の DLL は動いているアプリが読み込んだまま放さないので、
  同じ場所には上書きできない。前の版のフォルダーは、次のログオンの時に RunOnce で丸ごと消す。リリースでは
  `release.yml` の `build-windows` が作って Release に添付する。Windows 版の依存のライセンスは `THIRD-PARTY-NOTICES-windows.txt`
  (`pnpm notices` が作る。直接の依存が載っていないと `kayoiji-windows` のテストが落ちる)
- リリースは `.github/workflows/release.yml` が行う。main の版 (`Cargo.toml` の workspace、`package.json`、
  `src-tauri/tauri.conf.json`、`pad/src-tauri/tauri.conf.json`。4 か所をそろえる) がまだリリースしていない版なら、
  macOS 26 の runner で辞書と補助プロセスを作り、`Kayoiji.app` を Developer ID で署名・公証して zip を Release に添付する。
  Homebrew の Cask (`cyberneura/homebrew-tap` の `Casks/kayoiji.rb`) は、tap の側が 1 時間ごとに最新のリリースを読んで更新する
- 配布物に入る依存 (Rust のクレート、WebView に入る npm のパッケージ) を足したり上げたりしたら、`pnpm notices` で
  `THIRD-PARTY-NOTICES.txt` を作り直してコミットする。直接の依存が載っていないと、`src-tauri` のテストが落ちる。
  WebView に入る npm のパッケージを足した時は、`scripts/generate-third-party-notices.sh` の `BUNDLED_PACKAGES` にも足す。
  辞書やデータの出典を変えたら `THIRD-PARTY-NOTICES.md` を直し、`pnpm notices` を実行する
- 絵文字のデータは emoji-explorer (`https://emoji.ytyng.com/`) から取得する。ビルド (`kayoiji-core` の
  `emoji::Source`) で、肌の色の違いを基本の絵文字の下にまとめた一覧 (`emoji.json`) と、キーワードから
  基本の絵文字を引く辞書 (`emoji.dict`) を作る。索引には U+FE0F の無い形や一覧に無い記号が混ざるので、
  一覧の絵文字に読み替えてから使う。漢字のキーワードには、Mozc の辞書で読みを足す。
  `data/build` に絵文字のデータが無ければ、エンジンは絵文字を出さずに動く
- 絵文字モード (`Engine::emoji_search`、`emoji_search.rs`) の検索語は、未確定文字列にせず候補ウインドウにだけ出す。
  未確定文字列にすると、入力先の切り替えなどで検索語が確定されてしまう
- コードコメントは日本語で、コードから読み取れない理由だけを書く

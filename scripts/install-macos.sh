#!/usr/bin/env bash
# Kayoiji をビルドして、このユーザーの入力メソッドとしてインストールする。
# 既にインストールしてあれば、新しい版に置き換えて動かし直す。
set -euo pipefail

cd "$(dirname "$0")/.."

for file in kayoiji.dict english.fst symbols.dict; do
  if [[ ! -s "data/build/$file" ]]; then
    pnpm data
    break
  fi
done

pnpm tauri build --bundles app

app="target/release/bundle/macos/Kayoiji.app"
install_dir="$HOME/Library/Input Methods"
destination="$install_dir/Kayoiji.app"

# 辞書の形式が古いと入力メソッドは起動に失敗し、選んでも何も打てなくなる。
# 動いている版を置き換える前に、同梱した辞書でエンジンが開けることを確かめる。
if ! cargo run --release --quiet -p kayoiji-cli -- --data "$app/Contents/Resources/data" type 'nihongo ' >/dev/null; then
  echo "同梱した辞書を開けない。pnpm data で辞書を作り直してから、もう一度実行する。" >&2
  exit 1
fi

tool_dir="$(mktemp -d)"
trap 'rm -rf "$tool_dir"' EXIT
tool="$tool_dir/input-source"
swiftc -O -o "$tool" scripts/input-source.swift

was_enabled="$("$tool" state "$destination")"

# コピーの途中で失敗しても、動いている版が壊れないようにする。
staging="$install_dir/Kayoiji.app.new"
backup="$install_dir/Kayoiji.app.old"
mkdir -p "$install_dir"
rm -rf "$staging" "$backup"
ditto "$app" "$staging"

# 新しい版が使える状態になるまで、今の版を退避して残す。途中で失敗したら今の版に戻す。
had_previous=no
if [[ -e "$destination" ]]; then
  had_previous=yes
fi
restore_previous() {
  if [[ -e "$backup" ]]; then
    rm -rf "$destination"
    mv "$backup" "$destination"
    "$tool" register "$destination" || true
  elif [[ "$had_previous" == no ]]; then
    rm -rf "$destination"
  fi
  # 今の版があって退避がまだなら、インストール先には今の版がそのまま残っている。
  echo "インストールに失敗したので、元の状態に戻した。" >&2
}
trap 'restore_previous; exit 130' INT TERM
if [[ "$had_previous" == yes ]]; then
  mv "$destination" "$backup"
fi
if ! { mv "$staging" "$destination" && "$tool" register "$destination"; }; then
  restore_previous
  exit 1
fi

# 入力ソースとして追加済みだった時は、増えた入力モードも有効にする。
enable_status=0
if [[ -n "$was_enabled" ]]; then
  "$tool" enable "$destination" || enable_status=$?
fi
# 2 は、OS が新しい入力モードをまだ認識していないことを表す。今の版に戻すと次も同じ結果になるので、
# 新しい版を残してログインし直してもらう。それ以外の失敗は、今の版に戻す。
if [[ "$enable_status" -ne 0 && "$enable_status" -ne 2 ]]; then
  restore_previous
  exit 1
fi
trap - INT TERM
rm -rf "$backup"

# 古い版のプロセスが残っていると、置き換えた後も古い版が打鍵を受け続ける。
# 止めておけば、次に入力する時に OS が新しい版を起動する。
pkill -f "$destination/Contents/MacOS/" || true

if [[ -z "$was_enabled" ]]; then
  cat <<'MESSAGE'

Installed: ~/Library/Input Methods/Kayoiji.app

「システム設定 → キーボード → 入力ソース → 編集 → +」で「日本語」を選び、Kayoiji を追加する。
一覧に出ない時は、ログインし直す。
MESSAGE
  exit 0
fi

echo
echo "Updated: ~/Library/Input Methods/Kayoiji.app"
if [[ "$enable_status" -eq 2 ]]; then
  echo "入力モードはまだ有効になっていない。ログインし直してから、もう一度インストールする。" >&2
fi
exit "$enable_status"

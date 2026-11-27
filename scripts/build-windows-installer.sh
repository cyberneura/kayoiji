#!/usr/bin/env bash
# Windows 版のインストーラーを作る。Windows の上の bash (Git Bash) で動かす。makensis が PATH に要る。
# 作ったインストーラーのパスを最後の行に書く。
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

for file in kayoiji.dict english.fst symbols.dict emoji.dict emoji.json; do
  if [[ ! -s "data/build/$file" ]]; then
    bash scripts/fetch-data.sh
    cargo run --locked --release -p kayoiji-cli -- build-data
    break
  fi
done

cargo build --locked --release -p kayoiji-windows --target x86_64-pc-windows-msvc
cargo build --locked --release -p kayoiji-windows --target i686-pc-windows-msvc

stage=target/windows-installer
rm -rf "$stage"
mkdir -p "$stage/x64" "$stage/x86" "$stage/data"
cp target/x86_64-pc-windows-msvc/release/kayoiji_windows.dll "$stage/x64/"
cp target/i686-pc-windows-msvc/release/kayoiji_windows.dll "$stage/x86/"
cp data/build/kayoiji.dict data/build/english.fst data/build/symbols.dict data/build/emoji.dict data/build/emoji.json "$stage/data/"
cp LICENSE "$stage/"
cp THIRD-PARTY-NOTICES-windows.txt "$stage/THIRD-PARTY-NOTICES.txt"

output="target/Kayoiji_${version}_windows_setup.exe"
makensis -V2 -DVERSION="$version" -DSOURCE="$(cygpath -w "$PWD/$stage")" -DOUTFILE="$(cygpath -w "$PWD/$output")" \
  installer/windows/kayoiji.nsi
echo "$output"

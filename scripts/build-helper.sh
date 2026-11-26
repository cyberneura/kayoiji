#!/usr/bin/env bash
# 英文チェックの補助プロセス (helper/english-check.swift) をビルドする。
# アプリには実行ファイルとして同梱する (tauri.conf.json の externalBin)。同梱するファイルの名前には、
# ビルド先のターゲットの名前を付ける決まりになっている。
set -euo pipefail
cd "$(dirname "$0")/.."

target="$(rustc -vV | sed -n 's/^host: //p')"
mkdir -p data/build
swiftc -O -o "data/build/english-check-$target" helper/english-check.swift
echo "data/build/english-check-$target"

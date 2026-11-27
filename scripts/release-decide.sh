#!/usr/bin/env bash
# VERSION を今リリースしてよいかを決め、true か false を 1 行で標準出力に書く。
# 決められない時 (API の失敗、想定外の形) は 1 で終わる。理由は標準エラーに書く。
#
# release.yml の plan (ビルドするか) と publish (公開する直前) の両方から呼ぶ。publish でも聞き直すのは、
# Actions の「失敗したジョブを再実行」が成功済みの plan を再実行せず、その時の結果を使い回すから。
# plan と publish の間に新しい版が公開されていたら、make_latest で最新が巻き戻ってしまう。
#
# 必要な環境変数: GH_TOKEN、GH_REPO (owner/repo)、VERSION (X.Y.Z)
set -euo pipefail

: "${GH_REPO:?GH_REPO is required}"
: "${VERSION:?VERSION is required}"

# 新旧の比較は X.Y.Z どうしの sort -V に頼るので、ほかの形は判断しない。
semver='^[0-9]+\.[0-9]+\.[0-9]+$'
if ! [[ "$VERSION" =~ $semver ]]; then
  echo "::error::version is not X.Y.Z: '${VERSION}'" >&2
  exit 1
fi

http_status() {
  gh api "$1" --silent --include 2>/dev/null | head -n 1 | awk '{print $2}' || true
}

# 知りたいのは「公開済みか」。下書きは未公開として扱う。失敗した実行が残した下書きは、同じ版の再実行で
# 埋め直すし、publish はビルドが作った下書きを前にしてこの判断を呼ぶ。このエンドポイントは下書きに 404 を
# 返すが、下書きの 200 を公開済みと読むと publish が止まったままになるので、200 でも下書きかを確かめる。
#
# 「無い」と読むのは 404 だけ。レート制限や障害をそう読むと、公開済みの版を作り直して二重に公開しようとする。
status=$(http_status "repos/${GH_REPO}/releases/tags/v${VERSION}")
case "$status" in
  404) ;;
  200)
    # 代入で受ける。`[ "$(gh ...)" ]` の中だと、gh の失敗が空文字になり set -e が効かない。
    draft=$(gh api "repos/${GH_REPO}/releases/tags/v${VERSION}" --jq '.draft')
    if [ "$draft" != "true" ] && [ "$draft" != "false" ]; then
      echo "::error::could not read whether v${VERSION} is a draft (got '${draft}'). Not guessing." >&2
      exit 1
    fi
    if [ "$draft" = "false" ]; then
      echo "v${VERSION} is already released." >&2
      echo false
      exit 0
    fi
    ;;
  *)
    echo "::error::could not tell whether v${VERSION} exists (HTTP ${status:-none}). Not guessing." >&2
    exit 1
    ;;
esac

# 未公開でも、公開済みの最新より古い版は出さない。publish は make_latest にするので、出すと最新が巻き戻り、
# Homebrew の tap も古い版に戻る。版を上げたコミットを revert した時や、待っている実行が push と逆の順に
# 処理された時に起きる。後者で飛ばされた版の中身は新しい版に含まれているので、出し直さなくてよい。
status=$(http_status "repos/${GH_REPO}/releases/latest")
case "$status" in
  404)
    echo "Releasing v${VERSION} (no release yet)." >&2
    echo true
    exit 0
    ;;
  200) ;;
  *)
    echo "::error::could not read the latest release (HTTP ${status:-none}). Not guessing." >&2
    exit 1
    ;;
esac

latest=$(gh api "repos/${GH_REPO}/releases/latest" --jq '.tag_name')
latest="${latest#v}"
if ! [[ "$latest" =~ $semver ]]; then
  echo "::error::latest release tag is not vX.Y.Z: 'v${latest}'. Not guessing." >&2
  exit 1
fi

if [ "$(printf '%s\n%s\n' "$latest" "$VERSION" | sort -V | tail -n 1)" != "$VERSION" ]; then
  echo "::warning::v${VERSION} is older than the latest release v${latest}; not releasing it." >&2
  echo false
  exit 0
fi

echo "Releasing v${VERSION} (latest is v${latest})." >&2
echo true

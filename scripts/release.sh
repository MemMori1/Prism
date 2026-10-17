#!/usr/bin/env bash
# Prism 发版脚本：构建带 updater 签名的产物 + 生成 latest.json。
#
# 用法：
#   scripts/release.sh            构建到 release-artifacts/（不上传）
#   scripts/release.sh --publish  构建后经 gh CLI 创建 GitHub Release 并上传
#
# 前置：
#   - 签名私钥在 ~/.prism/updater.key（可用 PRISM_UPDATER_KEY 覆盖路径）
#   - --publish 需要已安装 gh 且已 `gh auth login`
set -euo pipefail
cd "$(dirname "$0")/.."

# 本机 cargo 在 ~/.cargo/bin（不在默认 PATH 里）
export PATH="$HOME/.cargo/bin:$PATH"

REPO="MemMori1/Prism"
KEY="${PRISM_UPDATER_KEY:-$HOME/.prism/updater.key}"
PUBLISH=0
[ "${1:-}" = "--publish" ] && PUBLISH=1

# ---- 版本校验：三处必须一致 ----
VERSION=$(node -p "require('./src-tauri/tauri.conf.json').version")
PKG=$(node -p "require('./package.json').version")
CARGO=$(sed -n 's/^version = "\(.*\)"/\1/p' src-tauri/Cargo.toml | head -n1)
if [ "$VERSION" != "$PKG" ] || [ "$VERSION" != "$CARGO" ]; then
  echo "✗ 版本号不一致：tauri.conf=$VERSION / package.json=$PKG / Cargo.toml=$CARGO" >&2
  exit 1
fi
TAG="v$VERSION"

# ---- 签名密钥 ----
if [ ! -f "$KEY" ]; then
  echo "✗ 缺少签名私钥：$KEY" >&2
  echo "  生成：pnpm tauri signer generate -w ~/.prism/updater.key" >&2
  exit 1
fi
export TAURI_SIGNING_PRIVATE_KEY="$(cat "$KEY")"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD=""

# ---- 构建（createUpdaterArtifacts 仅发版时开启） ----
echo "==> 构建 Prism ${TAG}（含 updater 签名产物）"
pnpm tauri build --config '{"bundle":{"createUpdaterArtifacts":true}}'

BUNDLE="src-tauri/target/release/bundle"
APP_TAR=$(ls "$BUNDLE"/macos/*.app.tar.gz 2>/dev/null | head -n1 || true)
DMG=$(ls "$BUNDLE"/dmg/*.dmg 2>/dev/null | head -n1 || true)
if [ -z "$APP_TAR" ] || [ -z "$DMG" ]; then
  echo "✗ 未找到产物（${BUNDLE}）" >&2
  exit 1
fi

# ---- 收拢产物 ----
ARCH="aarch64"
DIST="release-artifacts"
rm -rf "$DIST"
mkdir -p "$DIST"
TAR_NAME="Prism_${VERSION}_${ARCH}.app.tar.gz"
DMG_NAME="Prism_${VERSION}_${ARCH}.dmg"
cp "$APP_TAR" "$DIST/$TAR_NAME"
cp "$DMG" "$DIST/$DMG_NAME"

URL="https://github.com/$REPO/releases/download/$TAG/$TAR_NAME"
SIG=$(cat "$APP_TAR.sig")
node -e '
const fs = require("fs");
const [version, url, sig, out] = process.argv.slice(1);
const data = {
  version,
  notes: "",
  pub_date: new Date().toISOString(),
  platforms: { "darwin-aarch64": { signature: sig.trim(), url } },
};
fs.writeFileSync(out, JSON.stringify(data, null, 2) + "\n");
' "$VERSION" "$URL" "$SIG" "$DIST/latest.json"

echo "==> 产物已就绪：$DIST/"
ls -1 "$DIST"

# ---- 发布 ----
if [ "$PUBLISH" = "1" ]; then
  if ! command -v gh >/dev/null 2>&1; then
    echo "✗ 未安装 gh CLI（brew install gh）" >&2
    exit 1
  fi
  gh release create "$TAG" "$DIST"/* \
    --repo "$REPO" \
    --title "Prism $TAG" \
    --notes "Prism $TAG"
  echo "==> 已发布：https://github.com/$REPO/releases/tag/$TAG"
else
  echo
  echo "上传（二选一）："
  echo "  1) gh:   scripts/release.sh --publish（首次先 gh auth login）"
  echo "  2) 网页: https://github.com/$REPO/releases/new?tag=${TAG}（上传 release-artifacts/ 全部文件）"
fi

#!/usr/bin/env bash
# vendor-refresh.sh — 升级 gpui-base / gpui-component 时重放 [dsh] 补丁。
#
# 步骤：解包新版本 → 对 .vendor/<crate>-<新版本>.patch 逐文件 patch
#      （--forward --fuzz=2，冲突行打印出来人工处理）→ 报告。
# 不直接改写 vendor/：先在临时目录试放，成功后再整体替换并刷新基线。
#
# 用法: scripts/vendor-refresh.sh gpui-base 0.9.1
#       scripts/vendor-refresh.sh gpui-component 0.9.1
# 前置: .vendor/<crate>-<旧版本>.patch 存在（scripts/vendor-diff.sh 生成）。
set -uo pipefail
cd "$(dirname "$0")/.."

CRATE="${1:?usage: vendor-refresh.sh <crate> <new-version>}"
NEW_VER="${2:?usage: vendor-refresh.sh <crate> <new-version>}"
REGISTRY="${REGISTRY:-$HOME/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f}"
CRATE_FILE="$HOME/.cargo/registry/cache/index.crates.io-1949cf8c6b5b557f/$CRATE-$NEW_VER.crate"

# 选最新一版已有补丁作重放源
PATCH_SRC=$(ls -1 .vendor/"$CRATE"-*.patch 2>/dev/null | sort -V | tail -1)
if [ -z "$PATCH_SRC" ]; then
  echo "no patch for $CRATE in .vendor/; run scripts/vendor-diff.sh first" >&2
  exit 1
fi
echo "replay source: $PATCH_SRC"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
if [ -d "$REGISTRY/$CRATE-$NEW_VER" ]; then
  cp -r "$REGISTRY/$CRATE-$NEW_VER/." "$WORK/"
elif [ -f "$CRATE_FILE" ]; then
  tar -xzf "$CRATE_FILE" -C "$WORK" --strip-components=1
else
  echo "crate $CRATE-$NEW_VER not in local registry; run 'cargo fetch' for it first" >&2
  exit 1
fi

LOG="$WORK/patch.log"
if patch -p1 --forward --fuzz=2 -i "$PATCH_SRC" -d "$WORK" > "$LOG" 2>&1; then
  echo "all hunks applied"
else
  echo "CONFLICTS — resolve manually, then rerun:"
  grep -E "FAILED|Hunk|reject" "$LOG" || true
  echo
  echo "work tree kept at: $WORK"
  trap - EXIT
  exit 1
fi

# 新基线：把重放后的源文件与补丁一起换版本
OLD_BASE=$(ls -d .vendor/"$CRATE"-* 2>/dev/null | grep -v '\.patch$' | sort -V | tail -1)
NEW_BASE=".vendor/$CRATE-$NEW_VER"
rm -rf "$NEW_BASE"
mkdir -p "$NEW_BASE"
for f in $(grep '^--- a/' "$PATCH_SRC" | sed 's|^--- a/||'); do
  mkdir -p "$NEW_BASE/$(dirname "$f")"
  cp "$WORK/$f" "$NEW_BASE/$f"
done
# 用新版原始源刷新补丁（保持可重放）
if [ -d "$REGISTRY/$CRATE-$NEW_VER" ]; then
  REGISTRY="$REGISTRY" bash scripts/vendor-diff.sh >/dev/null || true
fi
echo "baseline written: $NEW_BASE"
echo "next: replace vendor/$CRATE and run scripts/vendor-verify.sh + cargo test --workspace"

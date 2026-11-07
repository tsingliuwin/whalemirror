#!/usr/bin/env bash
# vendor-diff.sh — 由 registry 原始 crate 源与 vendor/ 补丁树生成规范化补丁集。
#
# 补丁路径规范：头两行写成 a/<crate内路径> / b/<crate内路径>（以 crate 根
# 为基准），`patch -p1` 即可应用到解包后的 crate 根。生成物：
#   .vendor/<crate>-<version>.patch   全部 [dsh] 改动的完整补丁
# 校验：scripts/vendor-verify.sh 用同一批补丁反向核对 vendor 树。
#
# 用法: scripts/vendor-diff.sh            # 用 .vendor/ 里记录的版本
#       REGISTRY=<路径> scripts/vendor-diff.sh   # 指定 registry src 根
set -euo pipefail
cd "$(dirname "$0")/.."

REGISTRY="${REGISTRY:-$HOME/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f}"
BASE_BASELINE=$(basename "$(find .vendor -maxdepth 1 -name 'gpui-base-*' -type d | head -1)")
COMP_BASELINE=$(basename "$(find .vendor -maxdepth 1 -name 'gpui-component-*' -type d | head -1)")
BASE_VER="${BASE_BASELINE#gpui-base-}"
COMP_VER="${COMP_BASELINE#gpui-component-}"

gen() { # gen <crate> <ver> <file...>
  local crate="$1" ver="$2"; shift 2
  local out=".vendor/$crate-$ver.patch"
  : > "$out"
  for p in "$@"; do
    diff -u "$REGISTRY/$crate-$ver/$p" "vendor/$crate/$p" \
      --label "a/$p" --label "b/$p" >> "$out" || true
  done
  echo "$out: $(grep -c '^--- ' "$out") file(s), $(grep -c '^@@' "$out") hunk(s)"
}

gen gpui-base "$BASE_VER" \
  src/input/base/state.rs \
  src/text/mod.rs \
  src/text/node.rs \
  src/text/style.rs
gen gpui-component "$COMP_VER" \
  src/text/mod.rs

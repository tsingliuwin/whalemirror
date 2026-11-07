#!/usr/bin/env bash
# vendor-verify.sh — 校验 vendor/ 与「原始 crate 源 + .vendor 补丁」一致。
#
# 判据（任一不满足即非零退出）：
#   1. 每个 crate 的基线目录存在，且其中的文件与 registry 原始源逐字节相同；
#   2. .vendor/<crate>-<ver>.patch-apply 到基线后，与 vendor/<crate> 中
#      被补丁覆盖的文件逐字节相同。
# 该脚本同时被 cargo test（crates/dsh-gpui/tests/vendor_baseline.rs）的
# 前提说明引用：CI/本地跑测试前建议先跑一次。
#
# 用法: scripts/vendor-verify.sh
set -uo pipefail
cd "$(dirname "$0")/.."

REGISTRY="${REGISTRY:-$HOME/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f}"
FAIL=0
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

verify() { # verify <crate>
  local crate="$1"
  local base patch ver tmp
  base=$(ls -d .vendor/"$crate"-* 2>/dev/null | grep -v '\.patch$' | sort -V | tail -1)
  if [ -z "$base" ]; then
    echo "MISSING baseline for $crate in .vendor/" >&2
    FAIL=1
    return
  fi
  ver="${base##*-}"
  patch=".vendor/$crate-$ver.patch"
  if [ ! -f "$patch" ]; then
    echo "MISSING patch $patch" >&2
    FAIL=1
    return
  fi
  # 1) 基线 == registry 原始源
  while IFS= read -r f; do
    local rel="${f#$base/}"
    if ! cmp -s "$f" "$REGISTRY/$crate-$ver/$rel"; then
      echo "BASELINE DRIFT $crate: $rel differs from registry $ver" >&2
      FAIL=1
    fi
  done < <(find "$base" -type f)
  # 2) 基线 + 补丁 == vendor 补丁文件
  tmp="$WORK/$crate"
  cp -r "$base" "$tmp"
  if ! patch -p1 -s --forward -i "$OLDPWD/$patch" -d "$tmp" >/dev/null 2>&1; then
    echo "PATCH FAILED to apply: $patch" >&2
    FAIL=1
    return
  fi
  while IFS= read -r f; do
    local rel="${f#"$tmp"/}"
    if ! cmp -s "$f" "vendor/$crate/$rel"; then
      echo "VENDOR DRIFT $crate: $rel != baseline+patch" >&2
      FAIL=1
    fi
  done < <(find "$tmp" -type f)
}

verify gpui-base
verify gpui-component

if [ "$FAIL" -eq 0 ]; then
  echo "VENDOR-BASELINE: OK (baseline + patches reproduce vendor/)"
else
  echo "VENDOR-BASELINE: FAILED" >&2
fi
exit "$FAIL"

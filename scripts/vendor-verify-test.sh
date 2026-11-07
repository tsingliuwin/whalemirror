#!/usr/bin/env bash
# vendor-verify-test.sh — 自测 scripts/vendor-verify.sh 真的能发现漂移。
#
# 判据：
#   1. 当前树干净 → 退出码 0；
#   2. 往 vendor/ 里塞一个补丁外的字节 → 退出码非 0（并在输出里点名文件）；
#   3. 还原后再次 0（脚本不破坏工作树）。
# 只有本地 registry 里有同版本 crate 时才能做逐字节比对，缺则跳过（退出 0
# 但打印 SKIP），避免在无缓存的 CI 上误报。
set -uo pipefail
cd "$(dirname "$0")/.."

REGISTRY="${REGISTRY:-$HOME/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f}"
if [ ! -d "$REGISTRY/gpui-component-0.9.0" ]; then
  echo "SKIP: no gpui-component-0.9.0 in local registry"
  exit 0
fi

PROBE="vendor/gpui-component/src/text/mod.rs"
BACKUP=$(mktemp)
cp "$PROBE" "$BACKUP"
restored=0
restore() {
  [ "$restored" -eq 1 ] && return 0
  cp "$BACKUP" "$PROBE" && rm -f "$BACKUP" && restored=1
}
trap restore EXIT INT TERM

fail() { echo "VENDOR-VERIFY-TEST: FAILED — $1" >&2; exit 1; }

bash scripts/vendor-verify.sh >/dev/null 2>&1 || fail "clean tree reported drift"
printf '\n// drift probe\n' >> "$PROBE"
if bash scripts/vendor-verify.sh >/tmp/vvendor.out 2>&1; then
  fail "injected drift went undetected"
fi
grep -q "VENDOR DRIFT" /tmp/vvendor.out || fail "drift message missing"
restore
bash scripts/vendor-verify.sh >/dev/null 2>&1 || fail "restore did not return to clean"
echo "VENDOR-VERIFY-TEST: OK (clean=0, drift=1, restore=0)"

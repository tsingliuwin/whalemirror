# vendor 补丁基线与重放约定

`vendor/` 下的 `gpui-base` / `gpui-component` 是带 `[dsh]` 本地改动的补丁树，
通过根 `Cargo.toml` 的 `[patch.crates-io]` 覆盖上游版本。本目录保存它们的
**可重放基线**，使升级 gpui-kit 生态时不必人工重读代码去复原改动。

## 目录

```
.vendor/
  gpui-base-0.9.0/         # 只含被补丁覆盖的文件，内容 == registry 原始源
    src/input/base/state.rs
    src/text/{mod,node,style}.rs
  gpui-component-0.9.0/
    src/text/mod.rs
  gpui-base-0.9.0.patch    # 基线 → vendor 的完整补丁（a/ b/ 相对 crate 根）
  gpui-component-0.9.0.patch
```

基线只存**被改动的文件**（不是整包），故体积远小于 vendor 树；`scripts/vendor-verify.sh`
会从本地 cargo registry 取同版本原始源，逐字节确认基线没有被改动过。

## 三个脚本

| 脚本 | 作用 |
|---|---|
| `scripts/vendor-diff.sh` | 由「registry 原始源 ⇒ vendor 补丁树」生成/刷新 `.vendor/<crate>-<ver>.patch` |
| `scripts/vendor-verify.sh` | 校验：基线 == registry 原始源，且「基线 + 补丁 == vendor 补丁文件」。非零退出即漂移 |
| `scripts/vendor-refresh.sh <crate> <新版本>` | 升级时把上一版补丁重放到新版本源码，成功写新基线；冲突则打印 FAILED hunk 并保留工作树 |

## 升级流程

```bash
# 1. 让新版本 crate 落到本地 registry
cargo update -p gpui-base --precise 0.9.1   # 或手改 Cargo.toml 后 cargo fetch
# 2. 重放补丁（冲突会列出 hunk，人工改完后重跑）
scripts/vendor-refresh.sh gpui-base 0.9.1
# 3. 用重放结果替换补丁树
rm -rf vendor/gpui-base && cp -r <重放出的树> vendor/gpui-base
# 4. 固化新版本号，刷新补丁并校验
#    （Cargo.toml 的 [patch] 指向 path，版本号写在 workspace.dependencies）
scripts/vendor-diff.sh && scripts/vendor-verify.sh
# 5. 回归
cargo test --workspace
```

`scripts/vendor-verify.sh` 也可作为升级前的健康检查：只要它 OK，就说明
vendor 树与基线补丁没有漂移——**这也是目前唯一能发现「有人直接改了 vendor 却
没更新补丁」的手段**。

## 补丁清单（0.9.0）

文件与动机见各处的 `[dsh]` 注释；按性质分两类：

**A. 可上游化**（web 对齐的排版/几何数值——值得给 gpui-kit 提 issue/PR）

- `gpui-base/src/text/style.rs`：`heading_line_height` 按标题级供行高
  （web `--dsw-font-markdown-h*` 标尺 30/28/26/24）。
- `gpui-base/src/text/node.rs`：markdown 排版对齐 web——li 6px 间距、圆点用
  次级色 marker、strong 600、标题边距/字重/行高、`hr`、`blockquote`、表格几何
  （无竖线、首末列贴边、单元格 10×16）、代码块 R16 与复制钮。
- `gpui-component/src/text/mod.rs`：把上述 style 定制接到组件的 markdown 渲染。

**B. 本地专用扩展点**（上游无对应接口，长期保留）

- `gpui-base/src/input/base/state.rs`：粘贴分流（图片落附件而非文本）、相对
  路径 opener 钩子（`try_relative_file_opener` / `dsh_openable`）。

升级时优先确认 A 类是否已进上游：已进则从补丁里删掉对应 hunk（改完跑
`vendor-diff.sh` 重新生成），B 类才需要长期重放。

## 与测试的关系

- `crates/dsh-gpui/tests/vendor_baseline.rs` 守着「基线存在 + 补丁非空」这两件
  结构性事实（不依赖 registry，CI 可跑）；逐字节校验交给
  `scripts/vendor-verify.sh`（需要本地 registry 里的同版本 crate）。
- 排版行为的回归仍由既有测试覆盖：`--test table_geometry`、`--lib ui_metrics`、
  以及 `regression-core.sh` 的实机取证。

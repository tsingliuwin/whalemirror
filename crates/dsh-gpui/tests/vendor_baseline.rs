//! vendor 补丁基线的结构性守门（详见 `.vendor/PATCHES.md`）。
//!
//! 这里刻意只断言**不依赖本地 cargo registry** 的事实：基线文件存在、
//! 每个补丁覆盖的路径都能在基线里找到、补丁里确实带 `[dsh]` 改动。
//! 逐字节核对「基线 == registry 原始源」与「基线 + 补丁 == vendor 树」
//! 需要同版本 crate，交给 `scripts/vendor-verify.sh`。
//!
//! 目的：让人改 `vendor/` 却忘了更新 `.vendor/` 补丁时，至少有一处会响——
//! 下次升级按 `.vendor/` 重放补丁时才不会静默丢掉改动。

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/crates/dsh-gpui
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root above crates/dsh-gpui")
        .to_path_buf()
}

/// 补丁头里 `--- a/<path>` 的行，即该补丁覆盖的 crate 内相对路径。
fn patched_paths(patch: &str) -> Vec<String> {
    patch
        .lines()
        .filter_map(|line| line.strip_prefix("--- a/"))
        .map(|rest| rest.split('\t').next().unwrap_or(rest).trim().to_string())
        .filter(|p| !p.is_empty() && p != "/dev/null" && p != "dev/null")
        .collect()
}

fn newest(dir: &Path, prefix: &str, suffix: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut candidates: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(prefix) && n.ends_with(suffix))
        })
        .collect();
    candidates.sort();
    candidates.pop()
}

fn check_crate(crate_name: &str) {
    let root = repo_root();
    let vendor_dir = root.join(".vendor");
    assert!(
        vendor_dir.is_dir(),
        ".vendor/ missing: vendor patches have no replayable baseline (see .vendor/PATCHES.md)"
    );

    let patch = newest(&vendor_dir, crate_name, ".patch")
        .unwrap_or_else(|| panic!("no .vendor/{crate_name}-<ver>.patch"));
    let patch_text = std::fs::read_to_string(&patch).expect("patch readable");
    let paths = patched_paths(&patch_text);
    assert!(
        !paths.is_empty(),
        "{}: patch lists no target files",
        patch.display()
    );

    // 基线目录：与补丁同版本
    let stem = patch.file_stem().unwrap().to_string_lossy().to_string();
    let baseline = vendor_dir.join(&stem);
    assert!(
        baseline.is_dir(),
        "{}: baseline dir missing next to its patch",
        baseline.display()
    );
    for rel in &paths {
        let original = baseline.join(rel);
        assert!(
            original.is_file(),
            "{crate_name}: baseline lacks {rel} — regenerate with scripts/vendor-diff.sh"
        );
        // 补丁树里也必须存在同名文件（否则补丁与 vendor 不同步）
        let patched = root.join("vendor").join(crate_name).join(rel);
        assert!(
            patched.is_file(),
            "{crate_name}: vendor/{crate_name}/{rel} missing but patch targets it"
        );
    }

    // 补丁确实带本地改动标记（空补丁 = 改动丢了）
    assert!(
        patch_text.contains("[dsh]"),
        "{}: patch contains no [dsh] marker — local changes may have been dropped",
        patch.display()
    );
}

#[test]
fn gpui_base_patch_has_a_baseline() {
    check_crate("gpui-base");
}

#[test]
fn gpui_component_patch_has_a_baseline() {
    check_crate("gpui-component");
}

#[test]
fn patch_parser_ignores_dev_null_and_reads_paths() {
    let sample = "\
--- a/src/text/mod.rs\t2026-11-06
+++ b/src/text/mod.rs\t2026-11-06
at at  -1,1 +1,1 at at 
--- a/dev/null
+++ b/src/x.rs
";
    let paths = patched_paths(sample);
    assert_eq!(paths, vec!["src/text/mod.rs"]);
}

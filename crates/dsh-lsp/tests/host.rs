//! 宿主侧文件访问契约：工作区规范化、包含校验、字节上限与 file URI 形态。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use dsh_lsp::host::{canonicalize_workspace, file_url, read_source};

/// 每个测试一个独立临时目录（不引入 `tempfile` 依赖）。
fn scratch(label: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let unique = format!(
        "dsh-lsp-host-{}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst),
        label
    );
    let path = std::env::temp_dir().join(unique);
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create scratch dir");
    path
}

#[test]
fn workspace_canonicalizes_to_directory_and_file_url() {
    let root = scratch("ws");
    let workspace = canonicalize_workspace(&root.display().to_string()).expect("canonicalize");
    assert!(workspace.canonical_path.is_dir());
    assert!(workspace.file_url.starts_with("file:///"), "{}", workspace.file_url);
    assert!(
        workspace.file_url.contains(&root.file_name().unwrap().to_string_lossy().to_string()),
        "{}",
        workspace.file_url
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn workspace_rejects_file_and_missing_paths() {
    let root = scratch("bad");
    let file = root.join("a.txt");
    std::fs::write(&file, "x").unwrap();

    let error = canonicalize_workspace(&file.display().to_string()).expect_err("文件不是目录");
    assert!(error.contains("is not a directory"), "{error}");

    let missing = root.join("nope");
    let error = canonicalize_workspace(&missing.display().to_string()).expect_err("不存在");
    assert!(error.contains("cannot be resolved"), "{error}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn relative_source_reads_inside_the_workspace() {
    let root = scratch("rel");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.ts"), "export const a = 1\n").unwrap();
    let workspace = canonicalize_workspace(&root.display().to_string()).unwrap();

    let source = read_source("src/a.ts", &workspace, 1_000).expect("read");
    assert_eq!(source.text, "export const a = 1\n");
    assert!(source.file_url.ends_with("/a.ts"), "{}", source.file_url);

    // 绝对路径同样可用。
    let absolute = root.join("src/a.ts").display().to_string();
    assert_eq!(read_source(&absolute, &workspace, 1_000).unwrap().text, source.text);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn source_outside_the_workspace_is_rejected() {
    let root = scratch("outside");
    let sibling = std::env::temp_dir().join(format!("dsh-lsp-sibling-{}", std::process::id()));
    std::fs::create_dir_all(&sibling).unwrap();
    std::fs::write(sibling.join("b.ts"), "b").unwrap();
    let workspace = canonicalize_workspace(&root.display().to_string()).unwrap();

    let outside = sibling.join("b.ts").display().to_string();
    let error = read_source(&outside, &workspace, 1_000).expect_err("越界要拒绝");
    assert!(error.contains("resolves outside the workspace"), "{error}");

    // 前缀兄弟目录（`/w2` 之于 `/w`）不算工作区内。
    let prefixed = PathBuf::from(format!("{}2", root.display()));
    std::fs::create_dir_all(&prefixed).unwrap();
    std::fs::write(prefixed.join("c.ts"), "c").unwrap();
    let error = read_source(&prefixed.join("c.ts").display().to_string(), &workspace, 1_000)
        .expect_err("前缀兄弟不算内部");
    assert!(error.contains("resolves outside the workspace"), "{error}");

    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&sibling);
    let _ = std::fs::remove_dir_all(&prefixed);
}

#[test]
fn missing_source_and_byte_cap_report_exact_messages() {
    let root = scratch("cap");
    std::fs::write(root.join("big.ts"), "0123456789").unwrap();
    let workspace = canonicalize_workspace(&root.display().to_string()).unwrap();

    let error = read_source("nope.ts", &workspace, 1_000).expect_err("不存在");
    assert!(error.contains("cannot be resolved"), "{error}");

    // 超限是错误而不是截断：静默截断会让语言服务器看到一份不存在的文档。
    let error = read_source("big.ts", &workspace, 4).expect_err("超限");
    assert!(
        error.contains("exceeds the 4-byte limit; reading stopped after 10 bytes"),
        "{error}"
    );

    // 边界：恰好等于上限可以通过。
    assert_eq!(read_source("big.ts", &workspace, 10).unwrap().text, "0123456789");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn non_utf8_source_is_reported_as_unreadable() {
    let root = scratch("utf8");
    std::fs::write(root.join("bad.ts"), [0xff, 0xfe, 0xfd]).unwrap();
    let workspace = canonicalize_workspace(&root.display().to_string()).unwrap();

    let error = read_source("bad.ts", &workspace, 1_000).expect_err("非 UTF-8");
    assert!(error.contains("could not be read"), "{error}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn file_url_is_plain_posix_style_on_windows() {
    // 无论平台，URI 里都不能出现 `\\?\` verbatim 前缀（那会被百分号编码）。
    let path = Path::new(env!("CARGO_MANIFEST_DIR"));
    let url = file_url(path).expect("file url");
    assert!(url.starts_with("file:///"), "{url}");
    assert!(!url.contains("%5C%5C%3F"), "{url}");
}
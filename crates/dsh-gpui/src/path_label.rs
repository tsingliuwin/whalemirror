//! 预览头部路径标签的切分（上游 `ui-primitives` `PathLabel` +
//! `util/workspace-path` `pathPartsOf` 的等价实现）。
//!
//! 上游把路径拆成「目录前缀（含分隔符）」与「文件名」两段：目录走
//! `label-tertiary`、文件名走 `label-primary`。分隔符**保持路径自身的写法**
//! （Windows 路径仍是 `\`、POSIX 路径是 `/`、混写各自保留），所以不能在
//! 拼接时补一个固定的 `/`——rustdsh 旧实现就是这么做的，Windows 下会渲染成
//! `E:\rustproject\miaocr/.gitignore` 这种混写。
//!
//! 切分放 lib（不依赖 GPUI）便于单测：bin 靶的 `#[test]` 会让 rustc 宏展开爆栈。

/// 把内部规范路径转成展示形态：Windows 下把 `/` 归一成 `\`。
///
/// rustdsh 内部的 `child_path` 一律用 `/` 拼接（层表键、`expanded` 集合都按
/// 这个形态比对），但上游预览头展示的是 Host 给出的**原生绝对路径**
/// （`E:\rustproject\miaocr\README.md`）。若直接把内部形态贴进头部，就会在
/// 原生前缀后接一段 `/`，渲染成 `E:\rustproject\miaocr/README.md` 这种混写。
/// 归一放在展示边界，不动内部键。
pub fn native_display_path(path: &str) -> String {
    if std::path::MAIN_SEPARATOR == '/' {
        return path.to_string();
    }
    path.replace('/', &std::path::MAIN_SEPARATOR.to_string())
}

/// 把路径拆成「目录前缀（含末尾分隔符）」与「最后一段」。
///
/// 与上游 `pathPartsOf` 逐字对齐：先去掉末尾所有分隔符（`/` 与 `\` 都算），
/// 这样目录路径指的是它自己那一段；再以最后一个分隔符为界切开。没有分隔符、
/// 或整条只有分隔符的路径，全部算文件名。
pub fn path_parts_of(path: &str) -> (String, String) {
    let trimmed = path.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        // 上游：`{ directory: '', name: path }`——原样保留（如 "/"）
        return (String::new(), path.to_string());
    }
    let cut = match trimmed.rfind(['/', '\\']) {
        Some(i) => i + 1,
        None => 0,
    };
    (trimmed[..cut].to_string(), trimmed[cut..].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(path: &str) -> (String, String) {
        path_parts_of(path)
    }

    #[test]
    fn posix_paths_keep_their_separator_in_the_directory() {
        // 上游 spec：`/work/project/notes.md` → 目录带末尾 `/`
        assert_eq!(
            parts("/work/project/notes.md"),
            ("/work/project/".into(), "notes.md".into())
        );
        assert_eq!(parts("notes.md"), (String::new(), "notes.md".into()));
    }

    #[test]
    fn windows_paths_keep_backslashes() {
        // 上游 spec：`C:\work\project\notes.md` → 目录带末尾 `\`
        // rustdsh 旧实现补固定 `/`，Windows 下会输出混写
        assert_eq!(
            parts(r"C:\work\project\notes.md"),
            (r"C:\work\project\".into(), "notes.md".into())
        );
        assert_eq!(
            parts(r"E:\rustproject\miaocr\.gitignore"),
            (r"E:\rustproject\miaocr\".into(), ".gitignore".into())
        );
    }

    #[test]
    fn mixed_separators_keep_the_last_one_as_written() {
        // 上游 spec：`\\host\share/notes.md` → 目录原样保留混写
        assert_eq!(
            parts(r"\\host\share/notes.md"),
            (r"\\host\share/".into(), "notes.md".into())
        );
    }

    #[test]
    fn trailing_separators_are_dropped_so_a_directory_names_its_own_segment() {
        // 上游：先去掉末尾分隔符，因此目录路径的「文件名」是它自己
        assert_eq!(parts("/work/project/"), ("/work/".into(), "project".into()));
        assert_eq!(
            parts(r"C:\work\project\\"),
            (r"C:\work\".into(), "project".into())
        );
    }

    #[test]
    fn separator_only_and_empty_paths_are_all_name() {
        // 上游：trimmed 为空时目录为空、名字是原始入参（含 "/" 本身）
        assert_eq!(parts("/"), (String::new(), "/".into()));
        assert_eq!(parts("///"), (String::new(), "///".into()));
        assert_eq!(parts(""), (String::new(), String::new()));
        // 盘符根：`C:\` 去尾后是 `C:`，没有分隔符 → 全是名字
        assert_eq!(parts(r"C:\"), (String::new(), r"C:".into()));
    }

    #[test]
    fn display_path_uses_the_platform_separator() {
        // 内部 child_path 用 `/` 拼接；展示边界归一成原生分隔符
        let shown = native_display_path("E:/rustproject/miaocr/README.md");
        if std::path::MAIN_SEPARATOR == '\\' {
            assert_eq!(shown, r"E:\rustproject\miaocr\README.md");
            // UNC 前缀的两个 `/` 一并归一
            assert_eq!(native_display_path("//host/share/a.md"), r"\\host\share\a.md");
        } else {
            assert_eq!(shown, "E:/rustproject/miaocr/README.md");
        }
        // 已经是原生的路径不变（幂等）
        let native = r"E:\rustproject\miaocr\README.md";
        assert_eq!(native_display_path(native_display_path(native).as_str()), native_display_path(native));
        // 归一后再切分：目录沿用原生 `\`，不再出现 `.../README.md` 混写
        let (dir, name) = parts(&native_display_path("E:/rustproject/miaocr/README.md"));
        assert_eq!(name, "README.md");
        if std::path::MAIN_SEPARATOR == '\\' {
            assert_eq!(dir, r"E:\rustproject\miaocr\");
        }
    }
}

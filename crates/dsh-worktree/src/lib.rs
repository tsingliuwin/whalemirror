//! dsh-worktree — Git 工作树创建/进入（上游 `experimental/worktree` 的
//! rustdsh 等价）：从本地提交/分支/标签新建分支 + worktree checkout，
//! 然后把调用会话的工作目录切进 checkout（「进入」由宿主工具层执行
//! `Workdir::set`——工具面下一请求起全部落树内）。
//!
//! git 序列与包装逐字对齐上游 `createCheckout`：每步都裹
//! `-c core.hooksPath=/dev/null -c core.fsmonitor=false`，环境剔除全部
//! `GIT_*` 后设 `GIT_TERMINAL_PROMPT=0`/`GIT_LFS_SKIP_SMUDGE=1`/
//! `GIT_NO_LAZY_FETCH=1`；reset 前按本仓 filter 驱动逐个下发
//! clean/smudge/process/required 覆盖（GIT_CONFIG_COUNT/KEY_i/VALUE_i）。
//! restore 面上游仅 forge 消费（rustdsh 无 forge——面外登记）。

use std::path::{Path, PathBuf};

use dsh_subprocess::{run, SpawnSpec, Stdio};

/// checkout 池目录（上游 Config.directory 默认）。
pub const WORKTREE_DIRECTORY: &str = ".agents/worktrees";

/// 分支/目录名前缀（上游 Config.namePrefix 默认）。
pub const NAME_PREFIX: &str = "dsh-";

/// 单步 git 的前台超时（上游 Config.timeoutMs 默认 60s）。
const GIT_TIMEOUT_MS: u64 = 60_000;

/// 终止宽限（上游 Config.graceMs 默认 3s）。
const GIT_GRACE_MS: u64 = 3_000;

/// 一次成功创建的产物（上游工具输出对象 {path, branch, baseCommit,
/// repositoryRoot}）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeCreated {
    pub path: PathBuf,
    pub branch: String,
    pub base_commit: String,
    pub repository_root: String,
}

/// 会话 id → 生成名（上游 `generatedWorktreeName`）：id 中首个 UUID 取
/// 前 8 位小写 + `-` + 随机段；无 UUID → 整段随机。随机段由调用方传入
/// （保持本面纯函数可测；宿主传时间纳斯 hex）。
pub fn generated_worktree_name(prefix: &str, session_id: &str, random_hex: &str) -> String {
    match first_uuid(session_id) {
        Some(u) => format!("{}{}-{}", prefix, u[..8].to_lowercase(), &random_hex[..random_hex.len().min(4)]),
        None => format!("{}{}", prefix, &random_hex[..random_hex.len().min(8)]),
    }
}

/// id 中首个 UUID（上游 line 6 的正则等价：8-4-4-4-12 hex）。
fn first_uuid(id: &str) -> Option<&str> {
    let bytes = id.as_bytes();
    let is_hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b) || (b'A'..=b'F').contains(&b);
    if bytes.len() < 36 {
        return None;
    }
    for start in 0..=bytes.len() - 36 {
        let w = &bytes[start..start + 36];
        let hyphens = [8, 13, 18, 23];
        let ok = hyphens.iter().all(|i| w[*i] == b'-')
            && w.iter()
                .enumerate()
                .all(|(i, b)| hyphens.contains(&i) || is_hex(*b));
        if ok {
            return Some(&id[start..start + 36]);
        }
    }
    None
}

/// baseCommit 形校验（上游 `/^(?:[a-f\d]{40}|[a-f\d]{64})$/i`）。
pub fn base_commit_valid(s: &str) -> bool {
    let ok = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b) || (b'A'..=b'F').contains(&b);
    (s.len() == 40 || s.len() == 64) && s.bytes().all(ok)
}

/// git 环境清洗（上游 process.ts：剔除全部 `GIT_*`（GIT_DIR/GIT_WORK_TREE
/// 等防外泄），设终端不提示/LFS 跳过 smudge/禁止惰性拉取）。
fn git_env(extra: &[(String, String)]) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| !k.starts_with("GIT_"))
        .collect();
    env.push(("GIT_TERMINAL_PROMPT".into(), "0".into()));
    env.push(("GIT_LFS_SKIP_SMUDGE".into(), "1".into()));
    env.push(("GIT_NO_LAZY_FETCH".into(), "1".into()));
    env.extend_from_slice(extra);
    env
}

/// 单步 git（上游每步的 `-c` 包装 + 60s 前台超时；stdout/stderr 均收）。
async fn git(repo_cwd: &Path, step: &str, args: &[&str], env_extra: &[(String, String)]) -> Result<(i32, String, String), String> {
    let mut argv: Vec<String> = vec![
        "git".into(),
        "-c".into(),
        "core.hooksPath=/dev/null".into(),
        "-c".into(),
        "core.fsmonitor=false".into(),
    ];
    argv.extend(args.iter().map(|s| s.to_string()));
    let spec = SpawnSpec {
        argv,
        cwd: Some(repo_cwd.to_path_buf()),
        stdin: Stdio::Ignore,
        stdout: Stdio::Pipe,
        stderr: Stdio::Pipe,
        env: git_env(env_extra),
        grace_ms: GIT_GRACE_MS,
    };
    let completed = tokio::time::timeout(
        std::time::Duration::from_millis(GIT_TIMEOUT_MS),
        run(&spec),
    )
    .await
    .map_err(|_| format!("git step {step} timed out after 60000ms"))?
    .map_err(|error| format!("git step {step} failed to start: {error} [cwd={:?}]", repo_cwd.display()))?;
    let stdout = String::from_utf8_lossy(&completed.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&completed.stderr).trim().to_string();
    Ok((completed.exit_code.unwrap_or(-1), stdout, stderr))
}

/// 本仓 filter 驱动覆盖（上游 286-305）：`git config --null --name-only
/// --get-regexp '^filter\.'` 的每个驱动下发 clean/smudge/process 空 +
/// required=false（reset 期间的 smudge 全部旁路）。
async fn filter_overrides(repo_cwd: &Path) -> Vec<(String, String)> {
    let Ok((code, stdout, _)) = git(
        repo_cwd,
        "list filter drivers",
        &["config", "--null", "--name-only", "--get-regexp", "^filter\\."],
        &[],
    )
    .await
    else {
        return Vec::new();
    };
    if code != 0 {
        return Vec::new();
    }
    // --null 形：`filter.<driver>.<key>\0` 逐条（尾 \0 去重键）
    let mut drivers: Vec<String> = stdout
        .split('\0')
        .filter_map(|entry| entry.strip_prefix("filter."))
        .filter_map(|rest| rest.split('.').next().map(str::to_string))
        .collect();
    drivers.sort();
    drivers.dedup();
    let mut env = vec![("GIT_CONFIG_COUNT".into(), (drivers.len() * 4).to_string())];
    for (i, driver) in drivers.iter().enumerate() {
        let base = i * 4;
        for (offset, key) in ["clean", "smudge", "process", "required"].iter().enumerate() {
            let index = base + offset;
            env.push((format!("GIT_CONFIG_KEY_{index}"), format!("filter.{driver}.{key}")));
            let value = if *key == "required" { "false".to_string() } else { String::new() };
            env.push((format!("GIT_CONFIG_VALUE_{index}"), value));
        }
    }
    env
}

/// 创建并准备一个 worktree（上游 `create` 的 createCheckout 序列）：
/// 分支名校验 → 仓库根 → 基点解析 → 分支占用检查 → 池目录（新池写自忽
/// 略 .gitignore）→ worktree add --no-checkout → filter 覆盖 → reset
/// --hard。失败在路径分配后统一裹上游逐字文案（checkout 与分支保留）。
pub async fn create(
    repo_cwd: &Path,
    name: Option<&str>,
    from: Option<&str>,
    session_id: &str,
) -> Result<WorktreeCreated, String> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let branch = match name {
        Some(given) => given.to_string(),
        None => generated_worktree_name(
            NAME_PREFIX,
            session_id,
            &format!("{:04x}", (nanos & 0xffff) as u16),
        ),
    };
    // ① 分支名合法（git 仲裁；拒绝文案取 git stderr）
    let (code, _, stderr) = git(
        repo_cwd,
        "check-ref-format",
        &["check-ref-format", &format!("refs/heads/{branch}")],
        &[],
    )
    .await?;
    if code != 0 {
        return Err(if stderr.is_empty() { format!("invalid branch name: {branch}") } else { stderr });
    }
    // ② 仓库根
    let (code, root, stderr) = git(repo_cwd, "rev-parse toplevel", &["rev-parse", "--show-toplevel"], &[]).await?;
    if code != 0 {
        return Err(if stderr.is_empty() { "not inside a git repository".into() } else { stderr });
    }
    let repository_root = root;
    // git 输出正斜杠路径；Windows 的 CreateProcess lpCurrentDirectory 不收
    // （os error 123）——归一成平台分隔符
    let repo_root_str = if cfg!(windows) { repository_root.replace('/', "\\") } else { repository_root.clone() };
    let repo_root = PathBuf::from(&repo_root_str);
    // ③ 基点解析（默认 HEAD；不拉取）
    let revision = from.unwrap_or("HEAD");
    let (code, base, stderr) = git(
        &repo_root,
        "rev-parse base", &["rev-parse", "--verify", "--end-of-options", &format!("{revision}^{{commit}}")],
        &[],
    )
    .await?;
    if code != 0 {
        return Err(if stderr.is_empty() {
            format!("could not resolve {revision} to a commit")
        } else {
            stderr
        });
    }
    let base_commit = base;
    // ④ 分支占用（上游：exit 0 = 已存在即失败；1 = 不存在继续）
    let (code, _, _) = git(
        &repo_root,
        "show-ref branch", &["show-ref", "--verify", "--quiet", &format!("refs/heads/{branch}")],
        &[],
    )
    .await?;
    if code == 0 {
        return Err(format!("worktree branch already exists: {branch}"));
    }
    // ⑤ 池目录 + 目的地（上游 PREPARE_DIRECTORY：新池写自忽略 .gitignore；
    // 同名目的地失败——工具描述 "Existing names fail"）
    let pool = repo_root.join(WORKTREE_DIRECTORY);
    if !pool.exists() {
        std::fs::create_dir_all(&pool)
            .map_err(|error| format!("could not create worktree pool: {error}"))?;
        std::fs::write(pool.join(".gitignore"), "*\n")
            .map_err(|error| format!("could not write worktree pool gitignore: {error}"))?;
    }
    let dest = pool.join(&branch);
    if dest.exists() {
        return Err(format!("worktree checkout already exists: {}", dest.display()));
    }
    std::fs::create_dir_all(&dest).map_err(|error| format!("could not create checkout directory: {error}"))?;
    let wrap = |message: String| -> String {
        format!(
            "Could not finish worktree {branch} at {}. Any created checkout and branch are retained. {message}",
            dest.display()
        )
    };
    // ⑥ worktree add --no-checkout -b
    let (code, _, stderr) = git(
        &repo_root,
        "worktree add", &[
            "--work-tree",
            &repository_root,
            "worktree",
            "add",
            "--no-checkout",
            "-b",
            &branch,
            "--",
            &dest.to_string_lossy(),
            &base_commit,
        ],
        &[],
    )
    .await?;
    if code != 0 {
        return Err(wrap(if stderr.is_empty() { "worktree add failed".into() } else { stderr }));
    }
    // ⑦+⑧ filter 覆盖 + reset --hard
    let overrides = filter_overrides(&repo_root).await;
    let (code, _, stderr) = git(
        &repo_root,
        "reset", &[
            "--work-tree",
            &dest.to_string_lossy(),
            "reset",
            "--hard",
            "--no-recurse-submodules",
            &base_commit,
        ],
        &overrides,
    )
    .await?;
    if code != 0 {
        return Err(wrap(if stderr.is_empty() { "reset failed".into() } else { stderr }));
    }
    Ok(WorktreeCreated {
        path: dest,
        branch,
        base_commit,
        repository_root,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_name_uses_first_uuid_head_plus_random() {
        let name = generated_worktree_name(
            "dsh-",
            "session-123e4567-e89b-12d3-a456-426614174000",
            "ab12",
        );
        assert_eq!(name, "dsh-123e4567-ab12");
        // 大写 UUID 归一小写
        let name = generated_worktree_name("dsh-", "session-ABCDEF01-2345-6789-ABCD-EF0123456789", "zz");
        { let _ = &name; assert!(name.starts_with("dsh-abcdef01-"), "{name}"); }
    }

    #[test]
    fn name_without_uuid_falls_back_to_random() {
        let name = generated_worktree_name("dsh-", "not-a-uuid", "feed");
        assert_eq!(name, "dsh-feed");
    }

    #[test]
    fn base_commit_shape_matches_upstream_regex() {
        let sha256 = "a".repeat(64);
        let sha1 = "b".repeat(40);
        assert!(base_commit_valid(&sha1));
        assert!(base_commit_valid(&sha256));
        assert!(base_commit_valid("ABCDEF0123456789ABCDEF0123456789ABCDEF01"));
        assert!(!base_commit_valid(&"c".repeat(39)));
        assert!(!base_commit_valid(&"g".repeat(40)));
    }

    #[test]
    fn first_uuid_scans_embedded_ids() {
        assert_eq!(
            first_uuid("session-123e4567-e89b-12d3-a456-426614174000"),
            Some("123e4567-e89b-12d3-a456-426614174000")
        );
        assert_eq!(first_uuid("plain"), None);
    }

    #[test]
    fn git_env_strips_all_git_vars_and_adds_guards() {
        // 环境清洗在 git_env 内读真实 env——只断言守卫恒在
        let env = git_env(&[("GIT_CONFIG_COUNT".into(), "0".into())]);
        assert!(env.contains(&("GIT_TERMINAL_PROMPT".to_string(), "0".to_string())));
        assert!(env.contains(&("GIT_LFS_SKIP_SMUDGE".to_string(), "1".to_string())));
        assert!(env.contains(&("GIT_NO_LAZY_FETCH".to_string(), "1".to_string())));
    }

    /// 真仓库端到端：init → 提交 → create → 断言结构/分支/树内容。
    #[tokio::test]
    async fn create_builds_a_real_worktree() {
        let root = std::env::temp_dir().join(format!(
            "dsh-worktree-e2e-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let run = |args: &[&str], cwd: &Path| {
            let spec = SpawnSpec {
                argv: args.iter().map(|s| s.to_string()).collect(),
                cwd: Some(cwd.to_path_buf()),
                stdin: Stdio::Ignore,
                stdout: Stdio::Pipe,
                stderr: Stdio::Pipe,
                env: vec![],
                grace_ms: 3_000,
            };
            async move { dsh_subprocess::run(&spec).await }
        };
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        run(&["git", "init", "-q"], &repo).await.unwrap();
        run(&["git", "config", "user.email", "t@t"], &repo).await.unwrap();
        run(&["git", "config", "user.name", "t"], &repo).await.unwrap();
        std::fs::write(repo.join("a.txt"), "hello").unwrap();
        run(&["git", "add", "."], &repo).await.unwrap();
        run(&["git", "commit", "-q", "-m", "init"], &repo).await.unwrap();

        let created = create(&repo, Some("fix-login-retry"), None, "session-x").await.unwrap();
        assert_eq!(created.branch, "fix-login-retry");
        assert!(created.path.ends_with(".agents/worktrees/fix-login-retry"));
        assert!(created.path.join("a.txt").exists(), "reset --hard 已落树内容");
        assert!(base_commit_valid(&created.base_commit));
        assert!(created.repository_root.contains("repo"));

        // 同名分支再次创建失败（上游 already exists 逐字）
        let error = create(&repo, Some("fix-login-retry"), None, "session-x").await.unwrap_err();
        assert_eq!(error, "worktree branch already exists: fix-login-retry");
        let _ = run(&["git", "worktree", "prune"], &repo).await;
    }
}

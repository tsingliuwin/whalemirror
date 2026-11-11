//! create_worktree 宿主面（上游 `experimental/tool-worktree` 的 rustdsh
//! 等价）：建分支 + worktree checkout + 把调用会话的工作目录切进树
//! （「进入」= Workdir 共享句柄 set——fs/bash/grep 与下一次请求组装的
//! sandbox 语境同步跟随）。工具描述与参数描述逐字。
//!
//! 偏差：仅主 agent 注册（headless runtime 工具面无探针槽）；会话桶/
//! 名册 cwd 不随树切换（上游 workingDirectory.set 同为运行时面，不动
//! 会话归属）。

use std::sync::{Arc, Mutex};

use dsh_tools::{Tool, ToolDefinition, ToolExecutionInput, ToolExecutionResult};
use serde_json::{json, Value};

/// 调用者材料：会话 id / 当前 cwd / 该会话的共享工作目录句柄。
#[derive(Clone, Debug)]
pub struct WorktreeActor {
    pub session_id: String,
    pub cwd: String,
    pub workdir: dsh_tools::Workdir,
}

pub type WorktreeProbe = Arc<dyn Fn() -> Option<WorktreeActor> + Send + Sync>;

pub struct WorktreeState {
    probe: Mutex<Option<WorktreeProbe>>,
}

impl WorktreeState {
    pub fn new() -> Self {
        Self { probe: Mutex::new(None) }
    }

    pub fn set_probe(&self, probe: WorktreeProbe) {
        *self.probe.lock().unwrap() = Some(probe);
    }

    fn actor(&self) -> Result<WorktreeActor, String> {
        let probe = self.probe.lock().unwrap().clone();
        let probe = probe.as_ref().ok_or("worktree creation requires a live calling agent")?;
        probe().ok_or_else(|| "worktree creation requires a live calling agent".to_string())
    }
}

impl Default for WorktreeState {
    fn default() -> Self {
        Self::new()
    }
}

/// 工具描述（上游 tool-worktree 逐字）。
pub const CREATE_WORKTREE_DESCRIPTION: &str = "Create a new Git branch and worktree from a local commit, branch, or tag, then change this session's working directory to it. Uncommitted files stay in the source checkout. Existing names fail. Use working_directory with cd to leave; the checkout and branch remain.";

pub struct CreateWorktreeTool {
    state: Arc<WorktreeState>,
}

impl CreateWorktreeTool {
    pub fn new(state: Arc<WorktreeState>) -> Self {
        Self { state }
    }
}

#[async_trait::async_trait]
impl Tool for CreateWorktreeTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "create_worktree".into(),
            description: CREATE_WORKTREE_DESCRIPTION.into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "New branch name, also used as the checkout directory: a few lowercase words joined by hyphens that describe the task, such as fix-login-retry. Omit to generate a unique name." },
                    "from": { "type": "string", "description": "Local commit, branch, or tag to start from. Defaults to HEAD; no fetch is performed." }
                }
            }),
        }
    }

    async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult {
        let actor = match self.state.actor() {
            Ok(actor) => actor,
            Err(message) => return ToolExecutionResult::error(message),
        };
        // 子代理拒答（本仓 id 前缀约定——切工作目录是根会话动作）
        if !actor.session_id.starts_with("session-") {
            return ToolExecutionResult::error(
                "worktree creation requires a root live calling agent".to_string(),
            );
        }
        let name = input.arguments.get("name").and_then(|v| v.as_str());
        let from = input.arguments.get("from").and_then(|v| v.as_str());
        let created = match dsh_worktree::create(
            std::path::Path::new(&actor.cwd),
            name,
            from,
            &actor.session_id,
        )
        .await
        {
            Ok(created) => created,
            Err(message) => return ToolExecutionResult::error(message),
        };
        // 「进入」：调用会话的工具工作目录切进树（上游
        // workingDirectory.set 同语义；下一请求组装的 sandbox 语境跟随）
        actor.workdir.set(&created.path);
        ToolExecutionResult::text(
            json!({
                "path": created.path.to_string_lossy(),
                "branch": created.branch,
                "baseCommit": created.base_commit,
                "repositoryRoot": created.repository_root,
            })
            .to_string(),
        )
    }
}

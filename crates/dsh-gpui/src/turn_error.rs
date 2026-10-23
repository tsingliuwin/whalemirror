//! 轮次失败行呈现（上游 TurnErrorItem / 消息文案映射的等价纯函数）。
//!
//! 上游 `failureMessage`：已知 provider code 用本地化文案（QUOTA/AUTH/
//! ACCOUNT_*），未知 code 保留原始消息；标题上 SIGNED_OUT/INTERRUPTED 是
//! 「任务已停止」，其余「本轮运行失败」；code chip 除 INTERRUPTED 外都显示。
//! 另有 QuotaNoticeHost：live 轮末 QUOTA 错误弹全窗 toast（历史不弹）。

/// 上游 zh locale `message.failure.quota` / toast 文案。
pub const QUOTA_FAILURE_NOTICE: &str = "当前请求的额度已用尽";

/// 失败行呈现视图（标题/消息/code chip 三件）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnFailureView {
    pub title: &'static str,
    pub message: String,
    pub show_code: bool,
}

/// 上游 `failureMessage` + `TurnErrorItem` 的标题/chip 规则。
pub fn turn_failure_view(code: Option<&str>, message: &str) -> TurnFailureView {
    let code = code.unwrap_or("");
    let stopped = code == "ACCOUNT_SIGNED_OUT" || code == "INTERRUPTED";
    let message = match code {
        "ACCOUNT_SIGNED_OUT" => "任务已因退出 DeepSeek 登录而停止。".to_string(),
        "INTERRUPTED" => "DSH 进程在本轮完成前退出（例如被强制结束、崩溃或关闭超时），重新启动后本轮已中止。未完成的工具调用结果未知。".to_string(),
        "ACCOUNT_SIGN_IN_REQUIRED" => "请先登录 DeepSeek，并确认请求地址支持账号认证。".to_string(),
        "QUOTA" | "ACCOUNT_QUOTA" => QUOTA_FAILURE_NOTICE.to_string(),
        "AUTH" => "API 密钥无效".to_string(),
        _ => message.to_string(),
    };
    TurnFailureView {
        title: if stopped { "任务已停止" } else { "本轮运行失败" },
        message,
        show_code: !code.is_empty() && code != "INTERRUPTED",
    }
}

/// 上游 `quotaNotice` 判定：只有这两种 code 触发额度提示。
pub fn is_quota_failure(code: &str) -> bool {
    code == "QUOTA" || code == "ACCOUNT_QUOTA"
}

/// 上游 zh locale `message.maxTokens`。
pub const MAX_TOKENS_TITLE: &str = "已达到输出 token 上限";
/// 上游 zh locale `message.maxTokens.hint`。
pub const MAX_TOKENS_HINT: &str = "回答被截断，已有输出保留在对话中。发送“继续”可让模型接着输出。";

/// 轮终是否需要「达到输出上限」提示行（上游 turn-max-tokens 节点：
/// `turn/end` 的 `reason.kind == 'max-tokens'`；其余收尾无此行）。
pub fn is_max_tokens_end(reason: &dsh_session::TurnEndReason) -> bool {
    matches!(reason, dsh_session::TurnEndReason::MaxTokens)
}

/// 失败条目的内部存储格式 `[CODE] message` → (code, message)。
/// 两个入列路径（实时 AgentEvent::Error 与回放 turn/end error）都经此
/// 格式；无前缀时 code 为 None、全文即消息。
pub fn split_error_text(text: &str) -> (Option<String>, String) {
    if let Some(rest) = text.strip_prefix('[')
        && let Some(end) = rest.find(']')
    {
        let code = &rest[..end];
        let tail = &rest[end + 1..];
        // code 是 provider 大写标识符；带空格的方括号文本不算前缀
        if !code.is_empty()
            && code.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            && let Some(msg) = tail.strip_prefix(' ')
        {
            return (Some(code.to_string()), msg.to_string());
        }
    }
    (None, text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(code: Option<&str>, msg: &str) -> TurnFailureView {
        turn_failure_view(code, msg)
    }

    #[test]
    fn quota_code_uses_localized_copy() {
        let v = view(Some("QUOTA"), "{\"error\":{\"message\":\"Insufficient Balance\"}}");
        assert_eq!(v.title, "本轮运行失败");
        assert_eq!(v.message, "当前请求的额度已用尽");
        assert!(v.show_code);
        let v = view(Some("ACCOUNT_QUOTA"), "raw");
        assert_eq!(v.message, "当前请求的额度已用尽");
        assert!(is_quota_failure("QUOTA") && is_quota_failure("ACCOUNT_QUOTA"));
        assert!(!is_quota_failure("AUTH"));
    }

    #[test]
    fn auth_and_rate_limit_mapping() {
        assert_eq!(view(Some("AUTH"), "secret detail").message, "API 密钥无效");
        // 未知 code 保留原始消息
        assert_eq!(view(Some("RATE_LIMIT"), "429 too many").message, "429 too many");
        assert_eq!(view(None, "boom").message, "boom");
        assert!(!view(None, "boom").show_code);
    }

    #[test]
    fn stopped_codes_switch_title_and_hide_chip() {
        let v = view(Some("ACCOUNT_SIGNED_OUT"), "x");
        assert_eq!(v.title, "任务已停止");
        assert_eq!(v.message, "任务已因退出 DeepSeek 登录而停止。");
        assert!(v.show_code);
        let v = view(Some("INTERRUPTED"), "x");
        assert_eq!(v.title, "任务已停止");
        assert!(v.message.contains("DSH 进程"));
        assert!(!v.show_code, "INTERRUPTED 不显示 code chip");
        let v = view(Some("ACCOUNT_SIGN_IN_REQUIRED"), "x");
        assert_eq!(v.title, "本轮运行失败");
        assert!(v.message.contains("请先登录 DeepSeek"));
    }

    #[test]
    fn error_text_round_trip() {
        let (code, msg) = split_error_text("[QUOTA] {\"error\":1}");
        assert_eq!(code.as_deref(), Some("QUOTA"));
        assert_eq!(msg, "{\"error\":1}");
        let (code, msg) = split_error_text("plain failure");
        assert_eq!(code, None);
        assert_eq!(msg, "plain failure");
        // 非标识符方括号文本不吞
        let (code, msg) = split_error_text("[not a code] body");
        assert_eq!(code, None);
        assert_eq!(msg, "[not a code] body");
        let (code, msg) = split_error_text("[] x");
        assert_eq!(code, None);
        assert_eq!(msg, "[] x");
    }

    #[test]
    fn view_built_from_stored_text() {
        let (code, msg) = split_error_text("[QUOTA] raw detail");
        let v = turn_failure_view(code.as_deref(), &msg);
        assert_eq!(v.message, QUOTA_FAILURE_NOTICE);
        assert!(v.show_code);
    }

    #[test]
    fn max_tokens_only_for_max_tokens_reason() {
        use dsh_session::TurnEndReason;
        assert!(is_max_tokens_end(&TurnEndReason::MaxTokens));
        assert!(!is_max_tokens_end(&TurnEndReason::Completed));
        assert!(!is_max_tokens_end(&TurnEndReason::Blocked));
        assert!(!is_max_tokens_end(&TurnEndReason::Aborted {
            cancel_cause: dsh_session::CancelCause::Legacy
        }));
        assert!(!is_max_tokens_end(&TurnEndReason::Error {
            failure: dsh_llm::LlmFailure {
                message: "x".into(),
                code: "QUOTA".into(),
                status: None,
                provider_retry_after_ms: None,
                request_id: None,
                offload_images: None,
            }
        }));
        // 文案逐字锁（上游 message.maxTokens / .hint）
        assert_eq!(MAX_TOKENS_TITLE, "已达到输出 token 上限");
        assert!(MAX_TOKENS_HINT.contains("发送“继续”"));
        assert!(MAX_TOKENS_HINT.starts_with("回答被截断"));
    }
}

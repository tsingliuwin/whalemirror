//! DeepSeek `GET /models` 列表核心（上游 llm-deepseek `listing.ts` /
//! `listed-catalog.ts` / `listing-config.ts` 的等价纯函数面，#39a）。
//!
//! 官方路由（`deepseek-official`，Messages origin = `https://api.deepseek.com`）
//! 在 `models` 未自定义时跟随 `GET /models` 列表：条目声明能力事实
//! （输入模态/上下文窗口/输出上限/system-prompt 与 tool 更新模式），列表
//! 还会按 `client_requirements["deepseek-harness"].min_version` 做版本门。
//! 本轮只落纯函数与单测；拉取/缓存/接线与 models 页联动留下一批。

use serde_json::Value;

/// 一次列表请求的默认截止（同时约束首次访问等待）。
pub const DEFAULT_MODELS_TIMEOUT_MS: u64 = 5_000;
/// 列表年龄超过该值时下次访问后台刷新。
pub const DEFAULT_MODELS_MAX_AGE_MS: u64 = 600_000;
/// 官方 `/models` 为 deepseek-flash / deepseek-v4-pro 声明的上下文窗口
/// （十进制 1,000,000 与二进制 1,048,576 之差以内建目录同步列表为准）。
pub const OFFICIAL_CONTEXT_WINDOW: u64 = 1_048_576;
/// 列表 `client_requirements` 里本产品的键（上游 APP_IDENTITY.product）。
pub const CLIENT_PRODUCT: &str = "deepseek-harness";

/// 模型接受的请求模态（本构建会发送的两种）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modality {
    Text,
    Image,
}

/// Messages system prompt 更新模式（列表声明；非 `in-history` 一律读作
/// `leading-only`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptUpdateMode {
    InHistory,
    LeadingOnly,
}

/// Messages tool 更新模式（`in-history`/`addition-only` 之外一律
/// `leading-only`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolUpdateMode {
    InHistory,
    AdditionOnly,
    LeadingOnly,
}

/// 列表条目声明的可用能力。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedFacts {
    pub input_modalities: Vec<Modality>,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub system_prompt_update: Option<PromptUpdateMode>,
    pub tool_update: Option<ToolUpdateMode>,
}

/// 本产品声明的最低客户端版本要求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientRequirement {
    None,
    MinVersion(String),
    /// 声明不可读（非对象、min_version 非字符串等）。
    Invalid,
}

/// 条目能力：可用（带事实）或不可用（原因）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListedCapabilities {
    Usable(ListedFacts),
    Unusable(String),
}

/// 一条解码后的列表条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListingEntry {
    pub id: String,
    pub name: Option<String>,
    pub requirement: ClientRequirement,
    pub capabilities: ListedCapabilities,
}

/// 解码后的列表（至少一条可用 id 条目）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelListing {
    pub entries: Vec<ListingEntry>,
    /// 无可用 id 或重复 id 的行数。
    pub skipped: usize,
}

/// 列表解码失败（上游 `INVALID_RESPONSE`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListingError {
    /// 响应体没有 `data` 数组。
    NoData,
    /// `data` 里没有任何可用条目。
    NoEntry,
}

impl ListingError {
    pub fn message(self) -> &'static str {
        match self {
            ListingError::NoData => "DeepSeek model listing has no \"data\" array",
            ListingError::NoEntry => "DeepSeek model listing contains no model entry",
        }
    }
}

/// 列表端点是 Messages 根的 origin 下的 `/models`——唯一会收到该路由
/// 凭据的源（上游 `modelListingURL`）。
pub fn model_listing_url(base_url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(base_url).ok()?;
    let origin = parsed.origin().ascii_serialization();
    (origin != "null").then(|| format!("{origin}/models"))
}

fn fields_of(value: &Value) -> Option<&serde_json::Map<String, Value>> {
    value.as_object()
}

/// 列表顺序里的已知模态、去重（上游 knownModalities）。
fn known_modalities(value: &Value) -> Option<Vec<Modality>> {
    let listed = value.as_array()?;
    let mut out = Vec::new();
    for v in listed {
        match v.as_str() {
            Some("text") if !out.contains(&Modality::Text) => out.push(Modality::Text),
            Some("image") if !out.contains(&Modality::Image) => out.push(Modality::Image),
            _ => {}
        }
    }
    Some(out)
}

/// 声明的容量：缺省 None；非正安全整数 = 解码失败（上游 capacity 的
/// `false` 分支）。
fn capacity(value: Option<&Value>) -> Result<Option<u64>, ()> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_u64() {
            Some(n) if n > 0 => Ok(Some(n)),
            _ => Err(()),
        },
    }
}

fn requirement_of(entry: &serde_json::Map<String, Value>) -> ClientRequirement {
    let Some(requirements) = entry.get("client_requirements") else {
        return ClientRequirement::None;
    };
    let Some(requirements) = fields_of(requirements) else {
        return ClientRequirement::Invalid;
    };
    let Some(declared) = requirements.get(CLIENT_PRODUCT) else {
        return ClientRequirement::None;
    };
    let Some(own) = fields_of(declared) else {
        return ClientRequirement::Invalid;
    };
    let Some(min_version) = own.get("min_version") else {
        return ClientRequirement::None;
    };
    match min_version.as_str() {
        Some(v) if !v.is_empty() => ClientRequirement::MinVersion(v.to_string()),
        _ => ClientRequirement::Invalid,
    }
}

fn capabilities_of(entry: &serde_json::Map<String, Value>) -> ListedCapabilities {
    let Some(messages) = entry
        .get("api_capabilities")
        .and_then(fields_of)
        .and_then(|c| c.get("anthropic_messages"))
        .and_then(fields_of)
    else {
        return ListedCapabilities::Unusable("declares no api_capabilities.anthropic_messages".into());
    };
    // 输出模态：缺省 text；必须含 text
    let outputs = match entry.get("output_modalities") {
        None => Some(vec![Modality::Text]),
        Some(v) => known_modalities(v),
    };
    if !outputs.is_some_and(|o| o.contains(&Modality::Text)) {
        return ListedCapabilities::Unusable("declares no text output".into());
    }
    // 输入模态：缺省 text；未知模态过滤后不得为空
    let inputs = match entry.get("input_modalities") {
        None => Some(vec![Modality::Text]),
        Some(v) => known_modalities(v),
    };
    let Some(inputs) = inputs.filter(|i| !i.is_empty()) else {
        return ListedCapabilities::Unusable(
            "declares no input modality DeepSeek Harness supports".into(),
        );
    };
    let context_window = match capacity(entry.get("context_window")) {
        Ok(v) => v,
        Err(()) => {
            return ListedCapabilities::Unusable(
                "declares an invalid context_window or max_output_tokens".into(),
            )
        }
    };
    let max_output_tokens = match capacity(entry.get("max_output_tokens")) {
        Ok(v) => v,
        Err(()) => {
            return ListedCapabilities::Unusable(
                "declares an invalid context_window or max_output_tokens".into(),
            )
        }
    };
    // null 视同缺省（序列化器把空值写 null 时保持本构建的知识）
    let system_prompt_update = match messages.get("system_prompt_update").and_then(Value::as_str) {
        Some("in-history") => Some(PromptUpdateMode::InHistory),
        Some(_) => Some(PromptUpdateMode::LeadingOnly),
        None => None,
    };
    let tool_update = match messages.get("tool_update").and_then(Value::as_str) {
        Some("in-history") => Some(ToolUpdateMode::InHistory),
        Some("addition-only") => Some(ToolUpdateMode::AdditionOnly),
        Some(_) => Some(ToolUpdateMode::LeadingOnly),
        None => None,
    };
    ListedCapabilities::Usable(ListedFacts {
        input_modalities: inputs,
        context_window,
        max_output_tokens,
        system_prompt_update,
        tool_update,
    })
}

/// 逐条解码 `GET /models` 响应体。无 `data` 数组或无任何可用条目都算
/// 解码失败（上游 `parseModelListing`）。
pub fn parse_model_listing(body: &Value) -> Result<ModelListing, ListingError> {
    let Some(data) = fields_of(body).and_then(|b| b.get("data")).and_then(Value::as_array) else {
        return Err(ListingError::NoData);
    };
    let mut entries = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for row in data {
        let Some(entry) = fields_of(row) else { continue };
        let Some(id) = entry.get("id").and_then(Value::as_str) else { continue };
        if id.is_empty() || !seen.insert(id.to_string()) {
            continue;
        }
        entries.push(ListingEntry {
            id: id.to_string(),
            name: entry
                .get("name")
                .and_then(Value::as_str)
                .filter(|n| !n.is_empty())
                .map(str::to_string),
            requirement: requirement_of(entry),
            capabilities: capabilities_of(entry),
        });
    }
    if entries.is_empty() {
        return Err(ListingError::NoEntry);
    }
    let skipped = data.len() - entries.len();
    Ok(ModelListing { entries, skipped })
}

/// 一条模型在本构建里的能力知识（内建目录或既有覆盖）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelFacts {
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub system_prompt_update: Option<PromptUpdateMode>,
    pub tool_update: Option<ToolUpdateMode>,
    pub input_modalities: Vec<Modality>,
}

/// 把列表事实叠到本构建的知识上（上游 `listed()` 的分层语义）：
/// - 声明的上下文窗口/输出上限覆盖（输出上限受路由 cap 约束）；
/// - system-prompt 模式声明 `leading-only` 时移除继承值，`in-history`
///   覆盖；缺省保留继承；
/// - tool 模式同理（`in-history`/`addition-only` 覆盖）；
/// - 输入模态整体替换。
pub fn apply_listed_facts(base: &ModelFacts, facts: &ListedFacts, route_max_tokens: u64) -> ModelFacts {
    ModelFacts {
        context_window: facts.context_window.or(base.context_window),
        max_output_tokens: facts
            .max_output_tokens
            .map(|n| n.min(route_max_tokens))
            .or(base.max_output_tokens),
        system_prompt_update: match facts.system_prompt_update {
            Some(PromptUpdateMode::InHistory) => Some(PromptUpdateMode::InHistory),
            Some(PromptUpdateMode::LeadingOnly) => None,
            None => base.system_prompt_update,
        },
        tool_update: match facts.tool_update {
            Some(ToolUpdateMode::InHistory) => Some(ToolUpdateMode::InHistory),
            Some(ToolUpdateMode::AdditionOnly) => Some(ToolUpdateMode::AdditionOnly),
            Some(ToolUpdateMode::LeadingOnly) => None,
            None => base.tool_update,
        },
        input_modalities: facts.input_modalities.clone(),
    }
}

/// 目录拒绝原因（网络 I/O 之前判定）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogRefusal {
    UpgradeRequired(String),
    Unsupported,
}

/// 版本门（上游 `gateOf`）：无要求时仅本构建已知 id 可用；声明
/// `min_version` 时按语义化版本比较；不可读要求记一条 problem 并按
/// unsupported 处理。
///
/// 偏差：比较只认 `MAJOR.MINOR.PATCH`（忽略预发布/构建元数据与预发布
/// 排序），rustdsh 无 semver 依赖且当前键位都是正式版本。
pub fn gate_listing_entry(
    entry: &ListingEntry,
    known: bool,
    client_version: &str,
    problems: &mut Vec<String>,
) -> Option<CatalogRefusal> {
    match &entry.requirement {
        ClientRequirement::None => (!known).then_some(CatalogRefusal::Unsupported),
        ClientRequirement::MinVersion(required) => {
            let (Some(current), Some(required)) =
                (parse_semver(client_version), parse_semver(required))
            else {
                problems.push(format!(
                    "model \"{}\" declares an unreadable client requirement",
                    entry.id
                ));
                return Some(CatalogRefusal::Unsupported);
            };
            (current < required).then(|| CatalogRefusal::UpgradeRequired(
                entry.requirement_version().unwrap_or_default().to_string(),
            ))
        }
        ClientRequirement::Invalid => {
            problems.push(format!(
                "model \"{}\" declares an unreadable client requirement",
                entry.id
            ));
            Some(CatalogRefusal::Unsupported)
        }
    }
}

impl ListingEntry {
    /// 声明的原始版本串（诊断文案用）。
    pub fn requirement_version(&self) -> Option<&str> {
        match &self.requirement {
            ClientRequirement::MinVersion(v) => Some(v),
            _ => None,
        }
    }
}

/// 解析 `MAJOR.MINOR.PATCH`（可带 `-pre`/`+build` 后缀，排序时忽略）。
fn parse_semver(text: &str) -> Option<(u64, u64, u64)> {
    let core = text.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// 列表缓存时序（配置一代）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListingTiming {
    pub timeout_ms: u64,
    pub max_age_ms: u64,
}

/// 校验时序（设置更新可能绕过 schema，边界在这里判）。
pub fn resolve_listing_timing(
    timeout_ms: Option<u64>,
    max_age_ms: Option<u64>,
) -> Result<ListingTiming, String> {
    let timeout_ms = timeout_ms.unwrap_or(DEFAULT_MODELS_TIMEOUT_MS);
    let max_age_ms = max_age_ms.unwrap_or(DEFAULT_MODELS_MAX_AGE_MS);
    if timeout_ms == 0 {
        return Err("modelsTimeoutMs must be a positive finite number".into());
    }
    Ok(ListingTiming { timeout_ms, max_age_ms })
}

/// 列表是否过期（下次访问应后台刷新；失败也按同一年龄重试）。
pub fn listing_stale(age_ms: u64, max_age_ms: u64) -> bool {
    age_ms >= max_age_ms
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry_body() -> Value {
        json!({
            "data": [
                {
                    "id": "deepseek-flash",
                    "name": "DeepSeek-V4.1-Flash",
                    "context_window": 1048576,
                    "max_output_tokens": 65536,
                    "input_modalities": ["text", "image"],
                    "output_modalities": ["text"],
                    "api_capabilities": {
                        "anthropic_messages": {
                            "system_prompt_update": "in-history",
                            "tool_update": "addition-only"
                        }
                    },
                    "client_requirements": {
                        "deepseek-harness": { "min_version": "0.2.1" }
                    }
                }
            ]
        })
    }

    #[test]
    fn listing_url_is_origin_models() {
        assert_eq!(
            model_listing_url("https://api.deepseek.com/anthropic").as_deref(),
            Some("https://api.deepseek.com/models")
        );
        // origin 归一：端口与路径保留在 origin 内
        assert_eq!(
            model_listing_url("http://127.0.0.1:8080/v1/messages").as_deref(),
            Some("http://127.0.0.1:8080/models")
        );
        assert_eq!(model_listing_url("not a url"), None);
    }

    #[test]
    fn parses_entry_facts_and_requirement() {
        let listing = parse_model_listing(&entry_body()).unwrap();
        assert_eq!(listing.entries.len(), 1);
        let entry = &listing.entries[0];
        assert_eq!(entry.id, "deepseek-flash");
        assert_eq!(entry.name.as_deref(), Some("DeepSeek-V4.1-Flash"));
        assert_eq!(entry.requirement, ClientRequirement::MinVersion("0.2.1".into()));
        let ListedCapabilities::Usable(facts) = &entry.capabilities else {
            panic!("expected usable capabilities");
        };
        assert_eq!(facts.context_window, Some(1_048_576));
        assert_eq!(facts.max_output_tokens, Some(65_536));
        assert_eq!(facts.input_modalities, vec![Modality::Text, Modality::Image]);
        assert_eq!(facts.system_prompt_update, Some(PromptUpdateMode::InHistory));
        assert_eq!(facts.tool_update, Some(ToolUpdateMode::AdditionOnly));
    }

    #[test]
    fn defaults_and_skips() {
        let body = json!({
            "data": [
                {},
                {"id": ""},
                {"id": "a"},
                {"id": "a"},
                {"id": "b", "input_modalities": ["text"],
                 "api_capabilities": {"anthropic_messages": {}}},
            ]
        });
        let listing = parse_model_listing(&body).unwrap();
        assert_eq!(listing.entries.len(), 2);
        assert_eq!(listing.skipped, 3);
        // 无 api_capabilities 的条目仍解码（id 可见），但能力不可用
        assert_eq!(listing.entries[0].id, "a");
        assert!(matches!(&listing.entries[0].capabilities, ListedCapabilities::Unusable(_)));
        // 缺省：text 输入、无能力事实、无要求
        let ListedCapabilities::Usable(facts) = &listing.entries[1].capabilities else {
            panic!();
        };
        assert_eq!(facts.input_modalities, vec![Modality::Text]);
        assert_eq!(facts.context_window, None);
        assert_eq!(facts.max_output_tokens, None);
        assert_eq!(listing.entries[1].requirement, ClientRequirement::None);
    }

    #[test]
    fn missing_data_or_no_usable_entry_errors() {
        assert_eq!(parse_model_listing(&json!({})).unwrap_err(), ListingError::NoData);
        assert_eq!(parse_model_listing(&json!({"data": "x"})).unwrap_err(), ListingError::NoData);
        assert_eq!(parse_model_listing(&json!({"data": []})).unwrap_err(), ListingError::NoEntry);
        assert_eq!(
            parse_model_listing(&json!({"data": [{"id": ""}]})).unwrap_err(),
            ListingError::NoEntry
        );
    }

    #[test]
    fn unusable_capabilities_are_reported_not_hidden() {
        // 无 anthropic_messages
        let body = json!({"data": [{"id": "x"}]});
        let listing = parse_model_listing(&body).unwrap();
        assert!(matches!(&listing.entries[0].capabilities, ListedCapabilities::Unusable(r) if r.contains("anthropic_messages")));
        // 输出不含 text
        let body = json!({"data": [{
            "id": "x",
            "output_modalities": ["image"],
            "api_capabilities": {"anthropic_messages": {}}
        }]});
        let listing = parse_model_listing(&body).unwrap();
        assert!(matches!(&listing.entries[0].capabilities, ListedCapabilities::Unusable(r) if r.contains("text output")));
        // 输入无可识别模态
        let body = json!({"data": [{
            "id": "x",
            "input_modalities": ["audio"],
            "api_capabilities": {"anthropic_messages": {}}
        }]});
        let listing = parse_model_listing(&body).unwrap();
        assert!(matches!(&listing.entries[0].capabilities, ListedCapabilities::Unusable(r) if r.contains("input modality")));
        // 非法容量
        let body = json!({"data": [{
            "id": "x",
            "context_window": 0,
            "api_capabilities": {"anthropic_messages": {}}
        }]});
        let listing = parse_model_listing(&body).unwrap();
        assert!(matches!(&listing.entries[0].capabilities, ListedCapabilities::Unusable(r) if r.contains("invalid context_window")));
    }

    #[test]
    fn prompt_and_tool_modes_map_like_upstream() {
        // null 视同缺省
        let body = json!({"data": [{
            "id": "x",
            "api_capabilities": {"anthropic_messages": {
                "system_prompt_update": null,
                "tool_update": null
            }}
        }]});
        let listing = parse_model_listing(&body).unwrap();
        let ListedCapabilities::Usable(facts) = &listing.entries[0].capabilities else { panic!() };
        assert_eq!(facts.system_prompt_update, None);
        assert_eq!(facts.tool_update, None);
        // 未知值降级 leading-only
        let body = json!({"data": [{
            "id": "x",
            "api_capabilities": {"anthropic_messages": {
                "system_prompt_update": "weird",
                "tool_update": "weird"
            }}
        }]});
        let listing = parse_model_listing(&body).unwrap();
        let ListedCapabilities::Usable(facts) = &listing.entries[0].capabilities else { panic!() };
        assert_eq!(facts.system_prompt_update, Some(PromptUpdateMode::LeadingOnly));
        assert_eq!(facts.tool_update, Some(ToolUpdateMode::LeadingOnly));
    }

    #[test]
    fn listed_facts_layer_over_builtin_knowledge() {
        let base = ModelFacts {
            context_window: Some(1_000_000),
            max_output_tokens: Some(8_192),
            system_prompt_update: Some(PromptUpdateMode::InHistory),
            tool_update: Some(ToolUpdateMode::AdditionOnly),
            input_modalities: vec![Modality::Text],
        };
        // 声明覆盖：上下文/输出（受路由 cap）/模态；模式缺省继承
        let facts = ListedFacts {
            input_modalities: vec![Modality::Text, Modality::Image],
            context_window: Some(1_048_576),
            max_output_tokens: Some(65_536),
            system_prompt_update: None,
            tool_update: None,
        };
        let merged = apply_listed_facts(&base, &facts, 32_768);
        assert_eq!(merged.context_window, Some(1_048_576));
        assert_eq!(merged.max_output_tokens, Some(32_768), "路由 cap 约束列出的输出上限");
        assert_eq!(merged.system_prompt_update, Some(PromptUpdateMode::InHistory));
        assert_eq!(merged.tool_update, Some(ToolUpdateMode::AdditionOnly));
        assert_eq!(merged.input_modalities, vec![Modality::Text, Modality::Image]);
        // leading-only 移除继承模式
        let facts = ListedFacts {
            input_modalities: vec![Modality::Text],
            context_window: None,
            max_output_tokens: None,
            system_prompt_update: Some(PromptUpdateMode::LeadingOnly),
            tool_update: Some(ToolUpdateMode::LeadingOnly),
        };
        let merged = apply_listed_facts(&base, &facts, 32_768);
        assert_eq!(merged.system_prompt_update, None);
        assert_eq!(merged.tool_update, None);
        assert_eq!(merged.context_window, Some(1_000_000), "未声明保留继承");
    }

    #[test]
    fn version_gate_matches_upstream_rules() {
        let mut problems = Vec::new();
        let entry = |requirement: ClientRequirement| ListingEntry {
            id: "m".into(),
            name: None,
            requirement,
            capabilities: ListedCapabilities::Unusable("x".into()),
        };
        // 无要求且未知 id：unsupported
        assert_eq!(
            gate_listing_entry(&entry(ClientRequirement::None), false, "0.2.1", &mut problems),
            Some(CatalogRefusal::Unsupported)
        );
        // 无要求且已知 id：放行
        assert_eq!(
            gate_listing_entry(&entry(ClientRequirement::None), true, "0.2.1", &mut problems),
            None
        );
        // min_version 满足 / 不满足
        assert_eq!(
            gate_listing_entry(
                &entry(ClientRequirement::MinVersion("0.2.1".into())),
                false,
                "0.2.1",
                &mut problems
            ),
            None
        );
        assert_eq!(
            gate_listing_entry(
                &entry(ClientRequirement::MinVersion("0.2.10".into())),
                false,
                "0.2.1",
                &mut problems
            ),
            Some(CatalogRefusal::UpgradeRequired("0.2.10".into())),
            "按数值比较而非字典序"
        );
        // 不可读要求：problem + unsupported
        assert_eq!(
            gate_listing_entry(&entry(ClientRequirement::Invalid), true, "0.2.1", &mut problems),
            Some(CatalogRefusal::Unsupported)
        );
        assert_eq!(problems.len(), 1);
        assert_eq!(
            gate_listing_entry(
                &entry(ClientRequirement::MinVersion("v1".into())),
                true,
                "1.0.0",
                &mut problems
            ),
            Some(CatalogRefusal::Unsupported)
        );
        assert_eq!(problems.len(), 2);
    }

    #[test]
    fn timing_defaults_and_validation() {
        let timing = resolve_listing_timing(None, None).unwrap();
        assert_eq!(timing.timeout_ms, DEFAULT_MODELS_TIMEOUT_MS);
        assert_eq!(timing.max_age_ms, DEFAULT_MODELS_MAX_AGE_MS);
        assert!(resolve_listing_timing(Some(0), None).is_err());
        assert_eq!(
            resolve_listing_timing(Some(1_000), Some(0)).unwrap(),
            ListingTiming { timeout_ms: 1_000, max_age_ms: 0 }
        );
        assert!(listing_stale(600_000, 600_000));
        assert!(!listing_stale(599_999, 600_000));
    }
}
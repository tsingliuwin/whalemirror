//! 运行时纯面与管理服务词表（A1.4 批②⑤-a）。
//!
//! 对齐上游 `delivery-history.ts` + `runtime.ts` 的纯函数 +
//! `index.ts` 的服务词表：投递回执落历史（保留窗口与条数上限在唯一
//! 写路径裁剪）、投递间隔地板（`new` 目的地用独立间隔，其余 60s）、
//! 投递后任务推进（target/involved/updatedAt/pending/历史 + 循环规则
//! 前进到下一目标）、目录条目形与排序、列表分页、身份生成。宿主
//! 耦合面（会话激活/入站投递/idle 等待）由 rustdsh 宿主（批②⑤-b）
//! 组合这些纯函数。

use serde::Serialize;
use serde_json::Value;

use crate::{
    render_recurring_reminder_batch_framing, render_reminder_framing, resolve_daily_occurrence,
    resolve_every_occurrence, resolve_weekly_occurrence, DeliveryHistory, DeliveryHistoryRecord,
    DeliveryReceipt, ScheduleInputError, ScheduleInputErrorCode, ScheduleTarget, ScheduleTask,
    ScheduleTaskStatus,
};

/// 每天的毫秒数（保留窗口的陈述单位）。
const DAY_MS: i64 = 86_400_000;

/// 投递历史默认保留天数（上游 `DEFAULT_DELIVERY_HISTORY_DAYS`）。
pub const DEFAULT_DELIVERY_HISTORY_DAYS: u64 = 30;

/// 投递历史默认保留条数（上游 `DEFAULT_DELIVERY_HISTORY_RECORDS`）。
pub const DEFAULT_DELIVERY_HISTORY_RECORDS: usize = 200;

/// `new` 目的地投递的默认最小间隔秒（上游
/// `DEFAULT_NEW_SESSION_INTERVAL_SECONDS`）。
pub const DEFAULT_NEW_SESSION_INTERVAL_SECONDS: u64 = 60;

/// 未声明页大小的任务列表页大小（上游 `DEFAULT_LIST_LIMIT`）。
pub const DEFAULT_LIST_LIMIT: usize = 100;

/// 任务列表请求的最大页大小（上游 `MAX_LIST_LIMIT`）。
pub const MAX_LIST_LIMIT: usize = 500;

/// 目标会话属于子代理路由的稳定拒文案（上游 `SUBAGENT_SESSION_REFUSAL`）。
pub const SUBAGENT_SESSION_REFUSAL: &str =
    "This Session belongs to subagent routing, which never receives reminder delivery.";

/// 投递历史的保留窗口与条数上限（上游 `DeliveryRetentionBounds`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeliveryRetentionBounds {
    /// 保留窗口天数。
    pub days: u64,
    /// 保留条数上限。
    pub records: usize,
}

impl Default for DeliveryRetentionBounds {
    fn default() -> Self {
        Self {
            days: DEFAULT_DELIVERY_HISTORY_DAYS,
            records: DEFAULT_DELIVERY_HISTORY_RECORDS,
        }
    }
}

/// 一个运行时的已解析部署限制（上游 `ScheduleRuntimeLimits`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScheduleRuntimeLimits {
    /// 每次投递提交时套用的保留窗口与条数上限。
    pub retention: DeliveryRetentionBounds,
    /// `new` 目的地任务的投递地板间隔（毫秒）。
    pub new_session_interval_ms: i64,
}

impl Default for ScheduleRuntimeLimits {
    fn default() -> Self {
        Self {
            retention: DeliveryRetentionBounds::default(),
            new_session_interval_ms: (DEFAULT_NEW_SESSION_INTERVAL_SECONDS as i64) * 1_000,
        }
    }
}

/// 生成宿主侧任务身份（上游 `ScheduleId(\`schedule-${randomUUID()}\`)`）。
pub fn new_schedule_id() -> String {
    format!("schedule-{}", uuid::Uuid::new_v4())
}

/// 生成保留会话身份（上游 `session-${randomUUID()}`）。
pub fn new_reserved_session_id() -> String {
    format!("session-{}", uuid::Uuid::new_v4())
}

/// canonical 串的字典序比较键（上游直接 `localeCompare`——canonical
/// RFC 3339 形的字典序即时间序）。
fn instant_key(instant: &str) -> &str {
    instant
}

/// actionable 任务优先、相等时间戳下分页保持稳定（上游 `compareEntries`）。
pub fn compare_entries(left: &ScheduleCatalogEntry, right: &ScheduleCatalogEntry) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let status_order = |active: bool| if active { 0 } else { 1 };
    let order = status_order(left.is_active()).cmp(&status_order(right.is_active()));
    if order != Ordering::Equal {
        return order;
    }
    if left.is_active() && right.is_active() {
        return instant_key(left.scheduled_at())
            .cmp(instant_key(right.scheduled_at()))
            .then_with(|| left.id().cmp(right.id()));
    }
    let recent = |entry: &ScheduleCatalogEntry| -> String {
        entry
            .last_delivery
            .as_ref()
            .map(|receipt| receipt.delivered_at.clone())
            .or_else(|| entry.updated_at.clone())
            .or_else(|| entry.created_at.clone())
            .unwrap_or_else(|| entry.scheduled_at().to_string())
    };
    instant_key(&recent(right))
        .cmp(instant_key(&recent(left)))
        .then_with(|| left.id().cmp(right.id()))
}

/// 列表请求的页界（上游 `resolveListPage`：limit 1-500、offset 非负）。
pub fn resolve_list_page(
    limit: Option<usize>,
    offset: Option<usize>,
) -> Result<(usize, usize), ScheduleInputError> {
    let limit = limit.unwrap_or(DEFAULT_LIST_LIMIT);
    let offset = offset.unwrap_or(0);
    if limit < 1 || limit > MAX_LIST_LIMIT {
        return Err(ScheduleInputError::new(
            ScheduleInputErrorCode::InvalidRule,
            format!("limit must be 1-{MAX_LIST_LIMIT} and offset must be a non-negative integer."),
        ));
    }
    Ok((limit, offset))
}

/// 把发起会话与既有/保留目标会话并入 involved（首涉入序；
/// 上游 `involved`——`new` 目的地不记录，见 delivered 的无界增长防）。
pub fn involved(
    actor_session_id: Option<&str>,
    target: &ScheduleTarget,
    previous: &[String],
) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    let mut push = |id: &str| {
        if !ids.iter().any(|existing| existing == id) {
            ids.push(id.to_string());
        }
    };
    for id in previous {
        push(id);
    }
    if let Some(actor) = actor_session_id {
        push(actor);
    }
    if let Some(session_id) = target.session_id() {
        push(session_id);
    }
    ids
}

/// 严格晚于存量值的变更时刻（时钟回拨下 `expectedUpdatedAt` 也能检出
/// 每次后变更；上游 `revisedAt`）。
pub fn revised_at(task: &ScheduleTask, now: i64) -> String {
    let previous = task.updated_at.as_ref().and_then(|text| text.parse::<jiff::Timestamp>().ok());
    let previous_ms = previous.map(|timestamp| timestamp.as_millisecond());
    let instant = jiff::Timestamp::from_millisecond(now.max(
        previous_ms.map_or(now, |previous| previous + 1),
    ))
    .expect("调用方已限四位年");
    crate::format_instant(instant.as_millisecond())
}

/// 编辑请求（上游 `ScheduleEditRequest` 的服务形）。
#[derive(Clone, Debug)]
pub struct ScheduleEditRequest {
    pub id: String,
    pub action: ScheduleEditAction,
    /// `expectedUpdatedAt`（缺省 None = 不做乐观并发检查）。
    pub expected_updated_at: Option<String>,
}

/// 编辑动作（上游 update/delete 闭集）。
#[derive(Clone, Debug)]
pub enum ScheduleEditAction {
    Update {
        patch: SchedulePatchRequest,
        target: Option<ScheduleTarget>,
        active: Option<bool>,
    },
    Delete,
}

/// 一个编辑补丁的服务形（title/prompt/timing/target/active；上游
/// `SchedulePatch`）。
#[derive(Clone, Debug, Default)]
pub struct SchedulePatchRequest {
    pub title: Option<String>,
    pub prompt: Option<String>,
    pub timing: Option<Value>,
    pub target: Option<ScheduleTarget>,
    pub active: Option<bool>,
}

/// 一个管理原子编辑（上游 `revisedTask`：同一 timing 求值器；规则或
/// resume 变更时重算；`expectedUpdatedAt` 用 updatedAt 单调性检出冲突）。
pub fn revised_task(
    current: &ScheduleTask,
    request: &SchedulePatchRequest,
    target: Option<ScheduleTarget>,
    actor_session_id: Option<&str>,
    now: i64,
) -> Result<ScheduleTask, ScheduleInputError> {
    let next_target = target.clone().unwrap_or_else(|| current.target.clone());
    let record = crate::update::update_schedule_record(
        &current.record,
        &crate::update::SchedulePatch {
            title: request.title.clone(),
            prompt: request.prompt.clone(),
            timing: request.timing.clone(),
        },
        now,
        request.active == Some(true),
    )?;
    // changedRule：record 与 current.record 除描述字段外不等。
    let changed_rule = {
        let mut left = record.clone();
        left.set_title_prompt(current.record.title(), current.record.prompt());
        let mut right = current.record.clone();
        right.set_title_prompt(current.record.title(), current.record.prompt());
        left != right
    };
    let updated_at = revised_at(current, now);
    let retargeted = target.as_ref().is_some_and(|next| *next != current.target);
    let mut status = current.status;
    if request.active == Some(false) {
        status = ScheduleTaskStatus::Paused;
    } else if request.active == Some(true) || (changed_rule && current.status == ScheduleTaskStatus::Ended) {
        status = ScheduleTaskStatus::Active;
    }
    // 经 schema 往返规范化（上游 scheduleTaskSchema.parse 等价）。
    let mut draft = serde_json::json!({
        "record": serde_json::to_value(&record).unwrap_or(Value::Null),
        "target": serde_json::to_value(&next_target).unwrap_or(Value::Null),
        "involvedSessionIds": involved(actor_session_id, &next_target, &current.involved_session_ids),
        "status": status_wire(status),
        "createdAt": current.created_at,
        "updatedAt": updated_at,
        "lastDelivery": current.last_delivery,
        "deliveryHistory": current.delivery_history,
    });
    // pendingDelivery 保留条件：存在且规则与目的地都未变（上游同款）。
    if let (Some(pending), false, false) = (&current.pending_delivery, changed_rule, retargeted) {
        draft.as_object_mut().expect("对象形").insert(
            "pendingDelivery".into(),
            serde_json::to_value(pending).unwrap_or(Value::Null),
        );
    }
    crate::decode_schedule_task(&draft)
        .map_err(|error| ScheduleInputError::new(ScheduleInputErrorCode::InvalidRule, error.0))
}

/// 状态的线形（上游枚举小写）。
fn status_wire(status: ScheduleTaskStatus) -> &'static str {
    match status {
        ScheduleTaskStatus::Active => "active",
        ScheduleTaskStatus::Paused => "paused",
        ScheduleTaskStatus::Ended => "ended",
        ScheduleTaskStatus::Inactive => "inactive",
    }
}

/// 目录条目（上游 `ScheduleCatalogEntry`）：记录权威 timing + 目的地 +
/// 管理 bookkeeping，无执行内部态。
#[derive(Clone, Debug, Serialize)]
pub struct ScheduleCatalogEntry {
    #[serde(flatten)]
    pub record: crate::ScheduleRecord,
    pub target: ScheduleTarget,
    #[serde(rename = "involvedSessionIds")]
    pub involved_session_ids: Vec<String>,
    pub status: ScheduleTaskStatus,
    #[serde(rename = "createdAt", skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(rename = "updatedAt", skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(rename = "executionActive", skip_serializing_if = "std::ops::Not::not")]
    pub execution_active: bool,
    #[serde(rename = "deliveryFloorEndsAt", skip_serializing_if = "Option::is_none")]
    pub delivery_floor_ends_at: Option<String>,
    #[serde(rename = "lastDelivery", skip_serializing_if = "Option::is_none")]
    pub last_delivery: Option<DeliveryReceipt>,
}

impl ScheduleCatalogEntry {
    fn is_active(&self) -> bool {
        self.status == ScheduleTaskStatus::Active
    }

    /// 任务身份（展平记录的 id）。
    pub fn id(&self) -> &str {
        self.record.id()
    }

    /// 已承诺目标（展平记录的 scheduledAt）。
    pub fn scheduled_at(&self) -> &str {
        self.record.scheduled_at()
    }
}

/// 由耐久任务值塑出目录条目（上游 `entry`；执行态由宿主运行时传入）。
pub fn catalog_entry(
    task: &ScheduleTask,
    execution_active: bool,
    delivery_floor_ends_at: Option<String>,
) -> ScheduleCatalogEntry {
    ScheduleCatalogEntry {
        record: task.record.clone(),
        target: task.target.clone(),
        involved_session_ids: task.involved_session_ids.clone(),
        status: task.status,
        created_at: task.created_at.clone(),
        updated_at: task.updated_at.clone(),
        execution_active,
        delivery_floor_ends_at,
        last_delivery: task.last_delivery.clone(),
    }
}

/// 在记录的权威时区渲染 scheduledAt（rrule 时区随规则解析；上游
/// `formatScheduleLocalTime(record, ...)` 的任务形）。
pub fn format_task_local_time(task: &ScheduleTask, instant: &str) -> String {
    let time_zone = match &task.record {
        crate::ScheduleRecord::RRule(record) => crate::RRule::parse(&record.rrule, false)
            .map(|rule| rule.time_zone().to_string())
            .unwrap_or_else(|_| "UTC".to_string()),
        crate::ScheduleRecord::After(_) => "UTC".to_string(),
        crate::ScheduleRecord::At(_) => "UTC".to_string(),
        other => other
            .time_zone()
            .unwrap_or("UTC")
            .to_string(),
    };
    crate::format_schedule_local_time(&time_zone, instant)
}

/// 目的地为其会话的到期循环任务共用一条消息（上游 `batchSessionOf`）。
pub fn batch_session_of(task: &ScheduleTask) -> Option<&str> {
    match (&task.target, &task.record) {
        (ScheduleTarget::Session { session_id }, record) if record.is_recurring() => {
            Some(session_id.as_str())
        }
        _ => None,
    }
}

/// 一个任务可再接纳投递的最早墙钟时刻（上游 `nextAdmissionAt`）：
/// 后继目标已守住间隔时以 occurrence 锚定，确认延迟既不累积也不顺延。
pub fn next_admission_at(task: &ScheduleTask, limits: &ScheduleRuntimeLimits) -> i64 {
    let Some(receipt) = &task.last_delivery else {
        return i64::MIN;
    };
    let interval = match task.target {
        ScheduleTarget::New { .. } => limits.new_session_interval_ms,
        _ => crate::MIN_EVERY_INTERVAL_SECONDS as i64 * 1_000,
    };
    let Ok(occurrence) = receipt.scheduled_at.parse::<jiff::Timestamp>() else {
        return i64::MIN;
    };
    let Ok(delivered) = receipt.delivered_at.parse::<jiff::Timestamp>() else {
        return i64::MIN;
    };
    let Ok(scheduled) = task.record.scheduled_at().parse::<jiff::Timestamp>() else {
        return i64::MIN;
    };
    let anchor = if scheduled.as_millisecond() - occurrence.as_millisecond() >= interval {
        occurrence.as_millisecond()
    } else {
        delivered.as_millisecond()
    };
    anchor + interval
}

/// 该任务当前投递地板的结束时刻；仍在未来才返回（上游
/// `deliveryFloorEnd`）。
pub fn delivery_floor_end(task: &ScheduleTask, limits: &ScheduleRuntimeLimits, now: i64) -> Option<i64> {
    let end = next_admission_at(task, limits);
    (end > now).then_some(end)
}

/// 把一条回执套用到目的地、involved、历史并保持已承诺 occurrence
/// （上游 `delivered`；`new` 目的地每次投递开会话，全记录会无界增长）。
/// 回执缺目的地时由任务目的地补齐（运行时 receipt 填充的同位）。
pub fn delivered(
    task: &ScheduleTask,
    mut receipt: DeliveryReceipt,
    bounds: &DeliveryRetentionBounds,
) -> ScheduleTask {
    if receipt.target_session_id.is_none() {
        receipt.target_session_id = task.target.session_id().map(str::to_string);
    }
    let target_session = receipt.target_session_id.clone().unwrap_or_default();
    let target = if matches!(task.target, ScheduleTarget::NewOnce { .. }) {
        ScheduleTarget::Session { session_id: target_session.clone() }
    } else {
        task.target.clone()
    };
    let involved_session_ids = match task.target {
        ScheduleTarget::New { .. } => task.involved_session_ids.clone(),
        _ => {
            let mut ids = task.involved_session_ids.clone();
            if !ids.iter().any(|id| *id == target_session) {
                ids.push(target_session);
            }
            ids
        }
    };
    let delivered_ms = receipt
        .delivered_at
        .parse::<jiff::Timestamp>()
        .map(|timestamp| timestamp.as_millisecond())
        .unwrap_or_default();
    let updated_at = task.updated_at.as_ref().and_then(|text| text.parse::<jiff::Timestamp>().ok());
    let updated_ms = updated_at.map_or(delivered_ms, |timestamp| timestamp.as_millisecond() + 1);
    let updated_at = crate::format_instant(delivered_ms.max(updated_ms));
    let (last_receipt, appended_history) = append_delivery(task, receipt, bounds);
    ScheduleTask {
        record: task.record.clone(),
        target,
        involved_session_ids,
        status: task.status,
        created_at: task.created_at.clone(),
        updated_at: Some(updated_at),
        pending_delivery: None,
        last_delivery: Some(last_receipt),
        delivery_history: appended_history,
    }
}

/// 套用一条定时回执并推进拥有派发的规则到其下一目标（上游
/// `advanced`：无下一目标即终结）。
pub fn advanced(
    task: &ScheduleTask,
    occurrence: crate::RecurringOccurrence,
    receipt: DeliveryReceipt,
    bounds: &DeliveryRetentionBounds,
) -> ScheduleTask {
    let scheduled_at = occurrence
        .next_scheduled_at
        .clone()
        .unwrap_or_else(|| occurrence.occurrence_at.clone());
    let mut delivered_task = delivered(task, receipt, bounds);
    delivered_task.record.set_scheduled_at(scheduled_at);
    delivered_task.status = if occurrence.next_scheduled_at.is_none() {
        ScheduleTaskStatus::Ended
    } else {
        ScheduleTaskStatus::Active
    };
    delivered_task
}

/// 为同一条投递准备一条真实收件箱确认（上游 `appendDelivery`）：仅在
/// 唯一写路径裁剪到配置窗口与条数上限，任务记录不随确认数增长。
pub fn append_delivery(
    task: &ScheduleTask,
    receipt: DeliveryReceipt,
    bounds: &DeliveryRetentionBounds,
) -> (DeliveryReceipt, DeliveryHistory) {
    let history = &task.delivery_history;
    let mut appended: Vec<DeliveryHistoryRecord> = history.records.clone();
    appended.push(DeliveryHistoryRecord {
        receipt: receipt.clone(),
        prompt: Some(task.record.prompt().to_string()),
    });
    let delivered_ms = receipt
        .delivered_at
        .parse::<jiff::Timestamp>()
        .map(|timestamp| timestamp.as_millisecond())
        .unwrap_or_default();
    let floor = delivered_ms - (bounds.days as i64) * DAY_MS;
    let before = appended.len();
    let retained: Vec<DeliveryHistoryRecord> = appended
        .into_iter()
        .filter(|record| {
            record
                .receipt
                .delivered_at
                .parse::<jiff::Timestamp>()
                .map(|timestamp| timestamp.as_millisecond() >= floor)
                .unwrap_or(true)
        })
        .collect();
    let retained = if bounds.records < retained.len() {
        retained[retained.len() - bounds.records..].to_vec()
    } else {
        retained
    };
    let pruned = retained.len() != before;
    (
        receipt,
        DeliveryHistory {
            earlier_records_unavailable: history.earlier_records_unavailable || pruned,
            earlier_records_pruned: history.earlier_records_pruned || pruned,
            records: retained,
        },
    )
}

/// 投递历史的一页（上游 `deliveryHistoryPage`：按追加序取最新一页，
/// 倒序返回；游标按 messageId 定位）。
pub fn delivery_history_page(
    task: &ScheduleTask,
    id: &str,
    limit: usize,
    before: Option<&str>,
    retention: &DeliveryRetentionBounds,
) -> Result<Value, ScheduleHistoryPageError> {
    let history = &task.delivery_history;
    let end = match before {
        None => history.records.len(),
        Some(cursor) => match history.records.iter().position(|record| record.receipt.message_id == cursor) {
            Some(position) => position,
            None => return Err(ScheduleHistoryPageError::CursorNotFound),
        },
    };
    let start = end.saturating_sub(limit);
    let records: Vec<Value> = history.records[start..end]
        .iter()
        .rev()
        .map(|record| {
            let mut receipt = serde_json::to_value(&record.receipt).unwrap_or(Value::Null);
            if let (Some(object), Some(prompt)) = (receipt.as_object_mut(), &record.prompt) {
                object.insert("prompt".into(), Value::String(prompt.clone()));
            }
            receipt
        })
        .collect();
    let oldest = records.last().cloned();
    let mut result = serde_json::json!({
        "id": id,
        "records": records,
        "earlierRecordsUnavailable": history.earlier_records_unavailable,
        "earlierRecordsPruned": history.earlier_records_pruned,
        "retention": { "days": retention.days, "records": retention.records },
    });
    if let (Some(oldest), true) = (oldest, start > 0) {
        if let Some(object) = result.as_object_mut() {
            object.insert("nextBefore".into(), oldest["messageId"].clone());
        }
    }
    Ok(result)
}

/// 历史页查询的失败闭集。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScheduleHistoryPageError {
    /// 游标不在保留历史中。
    CursorNotFound,
}

/// 解析循环规则的一次决策（上游 `resolveRecurringOccurrence`：五形态
/// 分派——daily/weekly/every 直连，cron/rrule 经引擎解析后 resolve）。
pub fn resolve_recurring_occurrence(
    record: &crate::ScheduleRecord,
    accepted_at: i64,
) -> Result<crate::RecurringOccurrence, crate::ScheduleLogError> {
    let rule_error = |what: &str| crate::ScheduleLogError(format!("invalid schedule {what}"));
    match record {
        crate::ScheduleRecord::Daily(record) => resolve_daily_occurrence(record, accepted_at),
        crate::ScheduleRecord::Weekly(record) => resolve_weekly_occurrence(record, accepted_at),
        crate::ScheduleRecord::Every(record) => {
            resolve_every_occurrence(&record.scheduled_at, record.every_seconds, accepted_at)
        }
        // 一次性规则不经循环决策（运行时以 scheduledAt 直投）。
        crate::ScheduleRecord::After(_) | crate::ScheduleRecord::At(_) => Err(crate::ScheduleLogError(
            "schedule record is not a recurring rule".into(),
        )),
        crate::ScheduleRecord::Cron(record) => {
            let rule = crate::Cron::parse(&record.expression, &record.time_zone)
                .map_err(|_| rule_error("cron"))?;
            Ok(rule.resolve(&record.scheduled_at, accepted_at))
        }
        crate::ScheduleRecord::RRule(record) => Ok(crate::RRule::parse(&record.rrule, false)
            .map_err(|_| rule_error("rrule"))?
            .resolve(&record.scheduled_at, accepted_at)),
    }
}

/// 渲染一次到期投递的模型可见文本（单条或同目标批；上游
/// `deliverDue` 的 framing 选择）。
pub fn render_delivery_text(
    batch: bool,
    deliveries: &[(ScheduleTask, crate::RecurringOccurrence)],
) -> String {
    if batch {
        let reminders: Vec<(String, String, String)> = deliveries
            .iter()
            .map(|(task, occurrence)| {
                (
                    task.record.id().to_string(),
                    occurrence.occurrence_at.clone(),
                    task.record.prompt().to_string(),
                )
            })
            .collect();
        render_recurring_reminder_batch_framing(&reminders)
    } else {
        let (task, occurrence) = &deliveries[0];
        render_reminder_framing(task.record.id(), task.record.prompt(), &occurrence.occurrence_at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{create_schedule_record, ScheduleTaskStatus};
    use serde_json::json;

    /// 2026-01-10T08:00:00.000Z（周六）。
    const NOW: i64 = 1_768_032_000_000;

    fn session_task(id: &str) -> ScheduleTask {
        let record = create_schedule_record(
            id,
            "t",
            "p",
            &json!({"kind": "daily", "daily": {"time": "09:30:00", "time_zone": "UTC"}}),
            NOW,
        )
        .unwrap();
        ScheduleTask {
            record,
            target: ScheduleTarget::Session { session_id: "session-1".into() },
            involved_session_ids: vec!["session-1".into()],
            status: ScheduleTaskStatus::Active,
            created_at: Some("2026-01-10T08:00:00.000Z".into()),
            updated_at: None,
            last_delivery: None,
            delivery_history: DeliveryHistory {
                records: Vec::new(),
                earlier_records_unavailable: false,
                earlier_records_pruned: false,
            },
            pending_delivery: None,
        }
    }

    fn receipt(delivered_at: &str) -> DeliveryReceipt {
        DeliveryReceipt {
            scheduled_at: "2026-01-10T09:30:00.000Z".into(),
            delivered_at: delivered_at.into(),
            message_id: "msg-1".into(),
            target_session_id: Some("session-1".into()),
        }
    }

    #[test]
    fn append_delivery_prunes_to_window_and_cap() {
        let mut task = session_task("t1");
        task.record.set_scheduled_at("2026-01-10T09:30:00.000Z".into());
        // 三条历史，间隔超过窗口的回执被裁剪。
        task.delivery_history.records = vec![
            DeliveryHistoryRecord {
                receipt: DeliveryReceipt {
                    scheduled_at: "2026-01-01T09:30:00.000Z".into(),
                    delivered_at: "2026-01-01T09:30:01.000Z".into(),
                    message_id: "old".into(),
                    target_session_id: Some("session-1".into()),
                },
                prompt: Some("p".into()),
            },
            DeliveryHistoryRecord {
                receipt: receipt("2026-01-09T09:30:01.000Z"),
                prompt: Some("p".into()),
            },
        ];
        let bounds = DeliveryRetentionBounds { days: 2, records: 10 };
        let (last, history) = append_delivery(&task, receipt("2026-01-10T09:30:01.000Z"), &bounds);
        assert_eq!(last.message_id, "msg-1");
        // 01-01 的记录在 2 天窗口外被裁剪；01-09 与新条目保留。
        assert_eq!(history.records.len(), 2);
        assert!(history.earlier_records_pruned);
        assert!(history.earlier_records_unavailable);
        // 条数上限裁剪。
        let bounds = DeliveryRetentionBounds { days: 3650, records: 1 };
        let (_, history) = append_delivery(&task, receipt("2026-01-10T09:30:01.000Z"), &bounds);
        assert_eq!(history.records.len(), 1);
        assert_eq!(history.records[0].receipt.message_id, "msg-1");
    }

    #[test]
    fn delivery_floor_anchors_on_occurrence_successor() {
        let limits = ScheduleRuntimeLimits::default();
        let mut task = session_task("t1");
        // 无回执：地板为 -∞（恒可投递）。
        assert_eq!(next_admission_at(&task, &limits), i64::MIN);
        // 情形一：已承诺目标 = 当日 09:30 = occurrence 本身（间隔 0 <
        // 60s）→ 锚点切 delivered 09:31，地板 = 09:32:00Z。
        task.last_delivery = Some(receipt("2026-01-10T09:31:00.000Z"));
        assert_eq!(next_admission_at(&task, &limits), 1_768_037_520_000);
        // 情形二：目标已前进到次日 09:30（后继守住间隔 24h ≥ 60s）→
        // 以 occurrence 09:30 锚定，地板 = 09:31:00Z——确认延迟既不
        // 累积也不顺延（delivered 即使远晚也不改变锚点）。
        task.record.set_scheduled_at("2026-01-11T09:30:00.000Z".into());
        task.last_delivery = Some(receipt("2026-01-10T10:31:00.000Z"));
        assert_eq!(next_admission_at(&task, &limits), 1_768_037_460_000);
        let _ = delivery_floor_end(&task, &limits, 0);
    }

    #[test]
    fn delivered_advances_target_and_history() {
        let mut task = session_task("t1");
        task.record.set_scheduled_at("2026-01-10T09:30:00.000Z".into());
        task.target = ScheduleTarget::NewOnce {
            session_id: Some("reserved".into()),
            create: None,
        };
        task.involved_session_ids = vec!["caller".into(), "reserved".into()];
        let bounds = DeliveryRetentionBounds::default();
        let delivered_task = delivered(
            &task,
            DeliveryReceipt {
                target_session_id: Some("created-1".into()),
                ..receipt("2026-01-10T09:30:01.000Z")
            },
            &bounds,
        );
        // new_once 交付后固化为 session 目的地。
        assert_eq!(
            delivered_task.target,
            ScheduleTarget::Session { session_id: "created-1".into() }
        );
        // involved 追加目的地会话（去重保序）。
        assert_eq!(
            delivered_task.involved_session_ids,
            vec![
                "caller".to_string(),
                "reserved".to_string(),
                "created-1".to_string()
            ]
        );
        assert_eq!(delivered_task.pending_delivery, None);
        assert_eq!(delivered_task.delivery_history.records.len(), 1);
        // 循环规则前进：下一目标 + 保持 active。
        let occurrence =
            resolve_recurring_occurrence(&delivered_task.record, 1_768_040_000_000).unwrap();
        let advanced_task = advanced(&delivered_task, occurrence, receipt("2026-01-11T09:30:01.000Z"), &bounds);
        assert_eq!(advanced_task.record.scheduled_at(), "2026-01-11T09:30:00.000Z");
        assert_eq!(advanced_task.status, ScheduleTaskStatus::Active);
        assert_eq!(advanced_task.delivery_history.records.len(), 2);
        // 耗尽（COUNT 型无下一目标）→ ended。手工构造 every 一次后耗尽：
        let record = create_schedule_record(
            "t2",
            "t",
            "p",
            &json!({"kind": "every", "every_seconds": 60}),
            NOW,
        )
        .unwrap();
        let mut task = session_task("t2");
        task.record = record;
        task.record.set_scheduled_at("2026-01-10T08:59:00.000Z".into());
        // 决策 08:59:30（目标后）：最新到期 08:59、下次 09:00。
        let occurrence = resolve_recurring_occurrence(&task.record, 1_768_035_570_000).unwrap();
        assert_eq!(occurrence.occurrence_at, "2026-01-10T08:59:00.000Z");
        assert_eq!(occurrence.next_scheduled_at.as_deref(), Some("2026-01-10T09:00:00.000Z"));
        let advanced_task = advanced(&task, occurrence, receipt("2026-01-10T08:59:31.000Z"), &bounds);
        assert_eq!(advanced_task.record.scheduled_at(), "2026-01-10T09:00:00.000Z");
        assert_eq!(advanced_task.status, ScheduleTaskStatus::Active);
        // 耗尽（无下一目标）→ ended。
        let exhausted = crate::RecurringOccurrence {
            occurrence_at: "2026-01-10T09:00:00.000Z".into(),
            next_scheduled_at: None,
        };
        let ended_task = advanced(&advanced_task, exhausted, receipt("2026-01-10T09:00:31.000Z"), &bounds);
        assert_eq!(ended_task.status, ScheduleTaskStatus::Ended);
    }

    #[test]
    fn batch_delivery_text_uses_batch_framing() {
        let task = session_task("t1");
        let occurrence = crate::RecurringOccurrence {
            occurrence_at: "2026-01-10T09:30:00.000Z".into(),
            next_scheduled_at: None,
        };
        let text = render_delivery_text(false, &[(task.clone(), occurrence.clone())]);
        assert!(text.starts_with("[SCHEDULE REMINDER]\n"));
        let text = render_delivery_text(true, &[(task, occurrence)]);
        assert!(text.starts_with("[SCHEDULE REMINDER BATCH]\n"));
    }

    #[test]
    fn list_page_and_ordering() {
        let (limit, offset) = resolve_list_page(None, None).unwrap();
        assert_eq!((limit, offset), (DEFAULT_LIST_LIMIT, 0));
        let error = resolve_list_page(Some(0), None).unwrap_err();
        assert_eq!(
            error.message,
            "limit must be 1-500 and offset must be a non-negative integer."
        );
        assert!(resolve_list_page(Some(501), None).is_err());
        // involved：actor + 目的地并入，首涉入序。
        let target = ScheduleTarget::Session { session_id: "s2".into() };
        assert_eq!(
            involved(Some("s1"), &target, &["s2".into(), "s1".into()]),
            vec!["s2".to_string(), "s1".to_string()]
        );
        // new 目的地不记录会话。
        let new_target = ScheduleTarget::New { create: None };
        assert_eq!(involved(Some("s1"), &new_target, &[]), vec!["s1".to_string()]);
        // 身份生成形。
        assert!(new_schedule_id().starts_with("schedule-"));
        assert!(new_reserved_session_id().starts_with("session-"));
        // 单条投递文本 + 目录条目形。
        let task = session_task("t1");
        let entry = catalog_entry(&task, false, None);
        let wire = serde_json::to_value(&entry).unwrap();
        assert!(wire.get("kind").and_then(Value::as_str) == Some("daily"));
        assert!(wire.get("involvedSessionIds").is_some());
        let _ = format_task_local_time(&task, "2026-01-10T09:30:00.000Z");
    }
}

//! 宿主级 Schedule 任务值与耐久解码（A1.4 批②④）。
//!
//! 对齐上游 `storage.ts` 的 `scheduleTaskSchema`：v2 形状、v1 行兼容
//! 读取（不写启动迁移——下一次编辑或投递即写当前形状，无 `sessionId`）。
//! 解码即规范化：target 推导（显式 target 压过旧 Session 绑定；缺失时
//! 用绑定或稳定的 create-once 目的地）、回执补目的地、`inactive` →
//! `ended`、`involvedSessionIds` 缺省。自定义诊断逐字；zod 结构性诊断
//! 以等义英文近似（登记偏差——上游 zod 消息非 Schedule 自有词表）。

use serde::{Deserialize, Serialize};

use crate::{
    recurrence::four_digit_year_epoch, decode_schedule_record, ScheduleLogError, ScheduleRecord,
};

/// canonical 四位年 UTC instant 的耐久诊断（上游 zod refine 消息逐字）。
const INSTANT_MESSAGE: &str = "Expected a canonical four-digit-year UTC calendar instant";

/// 缺 target 时旧 Session 绑定必须合法（上游自定义 issue 逐字）。
const BINDING_MESSAGE: &str = "A missing target requires a valid stored Session binding";

/// 无既有目的地的已存投递必须指名目标会话（上游自定义 issue 逐字）。
const RECEIPT_TARGET_MESSAGE: &str =
    "A saved delivery without an existing destination must name its target Session";

/// lastDelivery 必须等于最新历史回执（上游 refine 消息逐字）。
const LAST_DELIVERY_MESSAGE: &str = "Last delivery must match the latest saved delivery receipt";

/// 任务幂等键闭集（上游 zod strict 的完整键集）。
const TASK_KEYS: [&str; 10] = [
    "record",
    "sessionId",
    "target",
    "involvedSessionIds",
    "status",
    "createdAt",
    "updatedAt",
    "lastDelivery",
    "deliveryHistory",
    "pendingDelivery",
];

/// 非空且无首尾空白的标识串（上游 messageId/sessionId schema 的公共面）。
fn valid_id_str(text: &str) -> Option<String> {
    (!text.is_empty() && text.trim() == text).then(|| text.to_string())
}

/// 非空且无首尾空白的标识串（上游 messageId/sessionId schema 的公共面）。
fn trimmed_id(value: &serde_json::Value) -> Option<String> {
    value.as_str().and_then(valid_id_str)
}

/// 解码 canonical 四位年 UTC instant（上游 `instantSchema`：恒 3 位毫秒
/// Z 形、非 0000 年；jiff 可表示为登记偏差面）。
fn decode_canonical_instant(value: &serde_json::Value) -> Result<String, ScheduleLogError> {
    let Some(text) = value.as_str() else {
        return Err(ScheduleLogError(INSTANT_MESSAGE.into()));
    };
    let epoch =
        four_digit_year_epoch(text).ok_or_else(|| ScheduleLogError(INSTANT_MESSAGE.into()))?;
    if crate::format_instant(epoch) != text {
        return Err(ScheduleLogError(INSTANT_MESSAGE.into()));
    }
    Ok(text.to_string())
}

/// 无已存目的地任务的稳定保留目的地（上游 `defaultSessionId` 逐字）：
/// `session-schedule-<sha256(id)>`，读取永不分配或持久化会话。
pub fn default_session_id(id: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(id.as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("session-schedule-{hex}")
}

/// 目的地的创建规格（上游 `createSpecSchema`：cwd/agentPreset 可选严格）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(
        rename = "agentPreset",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub agent_preset: Option<String>,
}

/// 投递目的地闭集（上游 `targetSchema` 判别联合）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScheduleTarget {
    /// 既有会话。
    Session {
        #[serde(rename = "sessionId")]
        session_id: String,
    },
    /// 新会话一次性（回执后保持绑定；`sessionId` 缺省时由任务 id 推导）。
    NewOnce {
        #[serde(rename = "sessionId", default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        create: Option<CreateSpec>,
    },
    /// 每次投递都开新会话（无固定目的地，回执必须自带目标会话）。
    New {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        create: Option<CreateSpec>,
    },
}

impl ScheduleTarget {
    /// 规范化后的固定目的地会话；`New` 为 None（上游 receipt 填充面）。
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::Session { session_id } => Some(session_id),
            Self::NewOnce { session_id, .. } => session_id.as_deref(),
            Self::New { .. } => None,
        }
    }
}

/// 一次投递的回执（上游 `deliveryReceiptSchema`）；不记录模型执行完成。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryReceipt {
    #[serde(rename = "scheduledAt")]
    pub scheduled_at: String,
    #[serde(rename = "deliveredAt")]
    pub delivered_at: String,
    #[serde(rename = "messageId")]
    pub message_id: String,
    #[serde(
        rename = "targetSessionId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub target_session_id: Option<String>,
}

/// 历史回执行：回执 + 可选投递 prompt（上游 records 元素形）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryHistoryRecord {
    #[serde(flatten)]
    pub receipt: DeliveryReceipt,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

/// 任务投递历史（上游 `deliveryHistorySchema`：messageId 任务内唯一）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryHistory {
    pub records: Vec<DeliveryHistoryRecord>,
    #[serde(rename = "earlierRecordsUnavailable")]
    pub earlier_records_unavailable: bool,
    #[serde(rename = "earlierRecordsPruned", default)]
    pub earlier_records_pruned: bool,
}

/// 一次待投递的保留（上游 `pendingDelivery`）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingDelivery {
    #[serde(rename = "targetSessionId")]
    pub target_session_id: String,
}

/// 任务状态闭集（上游枚举；`inactive` 仅在磁盘形出现，规范化为 `ended`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScheduleTaskStatus {
    Active,
    Paused,
    Ended,
    Inactive,
}

/// 规范化后的耐久任务值（上游 `ScheduleTask`）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleTask {
    pub record: ScheduleRecord,
    pub target: ScheduleTarget,
    #[serde(rename = "involvedSessionIds")]
    pub involved_session_ids: Vec<String>,
    pub status: ScheduleTaskStatus,
    #[serde(
        rename = "createdAt",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub created_at: Option<String>,
    #[serde(
        rename = "updatedAt",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub updated_at: Option<String>,
    #[serde(
        rename = "lastDelivery",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub last_delivery: Option<DeliveryReceipt>,
    #[serde(rename = "deliveryHistory")]
    pub delivery_history: DeliveryHistory,
    #[serde(
        rename = "pendingDelivery",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub pending_delivery: Option<PendingDelivery>,
}

/// 严格键集检查：值集恰为期望键（上游各 zod strict 的等价判定）。
fn has_exact_keys(value: &serde_json::Map<String, serde_json::Value>, expected: &[&str]) -> bool {
    value.keys().all(|key| expected.contains(&key.as_str()))
}

/// 读可选对象字段（非对象即拒绝）。
fn object_field<'a>(
    value: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<&'a serde_json::Map<String, serde_json::Value>> {
    value.get(key).and_then(serde_json::Value::as_object)
}

/// 解码可选 canonical instant 字段。
fn decode_optional_instant(
    value: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Option<String>, ScheduleLogError> {
    match value.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(raw) => Ok(Some(decode_canonical_instant(raw)?)),
    }
}

/// 解码回执（scheduledAt/deliveredAt/messageId 必填，targetSessionId 可选；
/// 键闭集）。
fn decode_receipt(value: &serde_json::Map<String, serde_json::Value>) -> Result<DeliveryReceipt, ScheduleLogError> {
    if !has_exact_keys(value, &["scheduledAt", "deliveredAt", "messageId", "targetSessionId"])
        || !value.contains_key("scheduledAt")
        || !value.contains_key("deliveredAt")
        || !value.contains_key("messageId")
    {
        return Err(ScheduleLogError(
            "delivery receipt must contain exactly scheduledAt, deliveredAt, messageId, and optional targetSessionId"
                .into(),
        ));
    }
    let Some(message_id) = value.get("messageId").and_then(trimmed_id) else {
        return Err(ScheduleLogError(
            "delivery receipt messageId must be a non-empty string without surrounding whitespace"
                .into(),
        ));
    };
    let target_session_id = match value.get("targetSessionId") {
        None | Some(serde_json::Value::Null) => None,
        Some(raw) => Some(trimmed_id(raw).ok_or_else(|| {
            ScheduleLogError(BINDING_MESSAGE.into())
        })?),
    };
    Ok(DeliveryReceipt {
        scheduled_at: decode_canonical_instant(
            value.get("scheduledAt").expect("上面已判必填"),
        )?,
        delivered_at: decode_canonical_instant(
            value.get("deliveredAt").expect("上面已判必填"),
        )?,
        message_id,
        target_session_id,
    })
}

/// 解码投递历史（键闭集 + messageId 任务内唯一）。
fn decode_delivery_history(
    value: &serde_json::Map<String, serde_json::Value>,
) -> Result<DeliveryHistory, ScheduleLogError> {
    if !has_exact_keys(
        value,
        &["records", "earlierRecordsUnavailable", "earlierRecordsPruned"],
    ) || !value.contains_key("records")
        || !value.contains_key("earlierRecordsUnavailable")
    {
        return Err(ScheduleLogError(
            "delivery history must contain exactly records, earlierRecordsUnavailable, and earlierRecordsPruned"
                .into(),
        ));
    }
    let Some(records) = value.get("records").and_then(serde_json::Value::as_array) else {
        return Err(ScheduleLogError("delivery history records must be an array".into()));
    };
    let mut decoded = Vec::with_capacity(records.len());
    for record in records {
        let Some(map) = record.as_object() else {
            return Err(ScheduleLogError("delivery history records must be objects".into()));
        };
        // 行形 = 回执 + 可选 prompt（键闭集）。
        if !has_exact_keys(
            map,
            &["scheduledAt", "deliveredAt", "messageId", "targetSessionId", "prompt"],
        ) {
            return Err(ScheduleLogError(
                "delivery history records must contain exactly a delivery receipt and an optional prompt"
                    .into(),
            ));
        }
        // 回执解码剥掉 prompt 键（回执键闭集不含它）。
        let mut receipt_map = map.clone();
        receipt_map.remove("prompt");
        let receipt = decode_receipt(&receipt_map)?;
        let prompt = match map.get("prompt") {
            None | Some(serde_json::Value::Null) => None,
            Some(text) => Some(text.as_str().ok_or_else(|| {
                ScheduleLogError("delivery history record prompt must be a string".into())
            })?.to_string()),
        };
        decoded.push(DeliveryHistoryRecord { receipt, prompt });
    }
    let unique: std::collections::BTreeSet<&str> =
        decoded.iter().map(|record| record.receipt.message_id.as_str()).collect();
    if unique.len() != decoded.len() {
        return Err(ScheduleLogError(
            "Delivery history message identities must be unique within a task".into(),
        ));
    }
    let Some(unavailable) = value
        .get("earlierRecordsUnavailable")
        .and_then(serde_json::Value::as_bool)
    else {
        return Err(ScheduleLogError(
            "delivery history earlierRecordsUnavailable must be a boolean".into(),
        ));
    };
    let pruned = value
        .get("earlierRecordsPruned")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    Ok(DeliveryHistory {
        records: decoded,
        earlier_records_unavailable: unavailable,
        earlier_records_pruned: pruned,
    })
}

/// 解码投递目的地判别联合（键闭集 + create 规格）。
fn decode_target(value: &serde_json::Map<String, serde_json::Value>) -> Result<ScheduleTarget, ScheduleLogError> {
    let decode_create = |map: &serde_json::Map<String, serde_json::Value>| -> Result<CreateSpec, ScheduleLogError> {
        match map.get("create") {
            None | Some(serde_json::Value::Null) => Ok(CreateSpec::default()),
            Some(create) => {
                let Some(map) = create.as_object() else {
                    return Err(ScheduleLogError("target create spec must be an object".into()));
                };
                if !has_exact_keys(map, &["cwd", "agentPreset"]) {
                    return Err(ScheduleLogError(
                        "target create spec must contain exactly cwd and agentPreset".into(),
                    ));
                }
                let text_field = |key: &str| -> Result<Option<String>, ScheduleLogError> {
                    match map.get(key) {
                        None | Some(serde_json::Value::Null) => Ok(None),
                        Some(raw) => Ok(Some(raw.as_str().ok_or_else(|| {
                            ScheduleLogError("target create spec fields must be strings".into())
                        })?.to_string())),
                    }
                };
                Ok(CreateSpec {
                    cwd: text_field("cwd")?,
                    agent_preset: text_field("agentPreset")?,
                })
            }
        }
    };
    match value.get("kind").and_then(serde_json::Value::as_str) {
        Some("session") => {
            if !has_exact_keys(value, &["kind", "sessionId"]) {
                return Err(ScheduleLogError("session target must contain exactly kind and sessionId".into()));
            }
            let session_id = value
                .get("sessionId")
                .and_then(trimmed_id)
                .ok_or_else(|| ScheduleLogError(BINDING_MESSAGE.into()))?;
            Ok(ScheduleTarget::Session { session_id })
        }
        Some("new_once") => {
            if !has_exact_keys(value, &["kind", "sessionId", "create"]) {
                return Err(ScheduleLogError(
                    "new_once target must contain exactly kind, optional sessionId, and optional create".into(),
                ));
            }
            let session_id = match value.get("sessionId") {
                None | Some(serde_json::Value::Null) => None,
                Some(raw) => Some(trimmed_id(raw).ok_or_else(|| ScheduleLogError(BINDING_MESSAGE.into()))?),
            };
            let create = decode_create(value)?;
            Ok(ScheduleTarget::NewOnce { session_id, create: Some(create) })
        }
        Some("new") => {
            if !has_exact_keys(value, &["kind", "create"]) {
                return Err(ScheduleLogError("new target must contain exactly kind and optional create".into()));
            }
            let create = decode_create(value)?;
            Ok(ScheduleTarget::New { create: Some(create) })
        }
        _ => Err(ScheduleLogError(
            "target kind must be \"session\", \"new_once\", or \"new\"".into(),
        )),
    }
}

/// 解码一个耐久任务值并规范化（上游 `scheduleTaskSchema` 的 transform）。
pub fn decode_schedule_task(
    value: &serde_json::Value,
) -> Result<ScheduleTask, ScheduleLogError> {
    let Some(map) = value.as_object() else {
        return Err(ScheduleLogError("schedule task must be an object".into()));
    };
    if !has_exact_keys(map, &TASK_KEYS) || !map.contains_key("record") {
        return Err(ScheduleLogError(
            "schedule task must contain a record and only known metadata keys".into(),
        ));
    }
    let record = decode_schedule_record(map.get("record").expect("上面已判必填"))?;
    let saved_target = match map.get("target") {
        None | Some(serde_json::Value::Null) => None,
        Some(raw) => Some(decode_target(raw.as_object().ok_or_else(|| {
            ScheduleLogError("schedule task target must be an object".into())
        })?)?),
    };
    let status = match map.get("status") {
        None | Some(serde_json::Value::Null) => ScheduleTaskStatus::Active,
        Some(raw) => {
            let Some(status) = raw
                .as_str()
                .and_then(|text| match text {
                    "active" => Some(ScheduleTaskStatus::Active),
                    "paused" => Some(ScheduleTaskStatus::Paused),
                    "ended" => Some(ScheduleTaskStatus::Ended),
                    "inactive" => Some(ScheduleTaskStatus::Inactive),
                    _ => None,
                })
            else {
                return Err(ScheduleLogError(
                    "schedule task status must be \"active\", \"paused\", \"ended\", or \"inactive\"".into(),
                ));
            };
            status
        }
    };
    let involved_session_ids = match map.get("involvedSessionIds") {
        None | Some(serde_json::Value::Null) => None,
        Some(raw) => {
            let Some(list) = raw.as_array() else {
                return Err(ScheduleLogError(
                    "schedule task involvedSessionIds must be an array".into(),
                ));
            };
            let mut ids = Vec::with_capacity(list.len());
            for entry in list {
                ids.push(entry.as_str().and_then(valid_id_str).ok_or_else(|| {
                    ScheduleLogError("involved session ids must be non-empty trimmed strings".into())
                })?);
            }
            Some(ids)
        }
    };
    let last_delivery = match map.get("lastDelivery") {
        None | Some(serde_json::Value::Null) => None,
        Some(raw) => Some(decode_receipt(raw.as_object().ok_or_else(|| {
            ScheduleLogError("schedule task lastDelivery must be an object".into())
        })?)?),
    };
    let delivery_history = match object_field(map, "deliveryHistory") {
        None => None,
        Some(map) => Some(decode_delivery_history(map)?),
    };
    let pending_delivery = match object_field(map, "pendingDelivery") {
        None => None,
        Some(map) => {
            if !has_exact_keys(map, &["targetSessionId"]) || !map.contains_key("targetSessionId") {
                return Err(ScheduleLogError(
                    "pending delivery must contain exactly targetSessionId".into(),
                ));
            }
            Some(PendingDelivery {
                target_session_id: map
                    .get("targetSessionId")
                    .and_then(trimmed_id)
                    .ok_or_else(|| ScheduleLogError(BINDING_MESSAGE.into()))?,
            })
        }
    };
    let created_at = decode_optional_instant(map, "createdAt")?;
    let updated_at = decode_optional_instant(map, "updatedAt")?;
    let binding = if saved_target.is_none() {
        map.get("sessionId").and_then(trimmed_id)
    } else {
        None
    };
    if saved_target.is_none() && map.get("sessionId").is_some() && binding.is_none() {
        return Err(ScheduleLogError(BINDING_MESSAGE.into()));
    }
    // 回执目的地检查在填充前（上游 hasReceiptTarget 语义）：new_once
    // 存盘时未带 sessionId 的旧行，其无目标回执会被拒。
    let has_receipt_target = match &saved_target {
        None => binding.is_some(),
        Some(ScheduleTarget::New { .. }) => false,
        Some(ScheduleTarget::Session { .. }) => true,
        Some(ScheduleTarget::NewOnce { session_id, .. }) => session_id.is_some(),
    };
    // 规范化 target：显式 target 压过旧绑定；缺失用绑定或稳定的
    // create-once 目的地；new_once 缺 sessionId 由任务 id 推导。
    let target = match saved_target {
        None => match binding {
            Some(session_id) => ScheduleTarget::Session { session_id },
            None => ScheduleTarget::NewOnce {
                session_id: Some(default_session_id(record.id())),
                create: Some(CreateSpec::default()),
            },
        },
        Some(ScheduleTarget::NewOnce { session_id, create }) => ScheduleTarget::NewOnce {
            session_id: Some(session_id.unwrap_or_else(|| default_session_id(record.id()))),
            create,
        },
        Some(target) => target,
    };
    let mut receipts_missing_target = last_delivery
        .as_ref()
        .is_some_and(|receipt| receipt.target_session_id.is_none());
    if let Some(history) = &delivery_history {
        receipts_missing_target |= history
            .records
            .iter()
            .any(|record| record.receipt.target_session_id.is_none());
    }
    if !has_receipt_target && receipts_missing_target {
        return Err(ScheduleLogError(RECEIPT_TARGET_MESSAGE.into()));
    }
    let receipt_target = |receipt: &DeliveryReceipt| -> DeliveryReceipt {
        DeliveryReceipt {
            target_session_id: receipt
                .target_session_id
                .clone()
                .or_else(|| target.session_id().map(str::to_string)),
            ..receipt.clone()
        }
    };
    let last = last_delivery.map(|receipt| receipt_target(&receipt));
    let history = match delivery_history {
        None => DeliveryHistory {
            records: last.iter().map(|receipt| DeliveryHistoryRecord {
                receipt: receipt.clone(),
                prompt: None,
            }).collect(),
            earlier_records_unavailable: true,
            earlier_records_pruned: false,
        },
        Some(history) => DeliveryHistory {
            earlier_records_pruned: history.earlier_records_pruned,
            earlier_records_unavailable: history.earlier_records_unavailable,
            records: history
                .records
                .into_iter()
                .map(|mut record| {
                    record.receipt = receipt_target(&record.receipt);
                    record
                })
                .collect(),
        },
    };
    let task = ScheduleTask {
        record,
        target: target.clone(),
        involved_session_ids: involved_session_ids.unwrap_or_else(|| match &target {
            ScheduleTarget::New { .. } => Vec::new(),
            _ => vec![target.session_id().expect("非 New 恒有目的地").to_string()],
        }),
        status: if status == ScheduleTaskStatus::Inactive {
            ScheduleTaskStatus::Ended
        } else {
            status
        },
        created_at,
        updated_at,
        last_delivery: last,
        delivery_history: history,
        pending_delivery,
    };
    // refine：lastDelivery 必须等于最新历史回执（忽略 prompt）。
    let matches_latest = match task.delivery_history.records.last() {
        None => task.last_delivery.is_none(),
        Some(latest) => task.last_delivery.as_ref() == Some(&latest.receipt),
    };
    if !matches_latest {
        return Err(ScheduleLogError(LAST_DELIVERY_MESSAGE.into()));
    }
    Ok(task)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create_schedule_record;
    use serde_json::json;

    fn after_record(id: &str) -> serde_json::Value {
        let record = create_schedule_record(
            id,
            "t",
            "p",
            &json!({"kind": "after", "after_seconds": 90}),
            1_768_032_000_000,
        )
        .unwrap();
        serde_json::to_value(record).unwrap()
    }

    /// v2 完整行（显式 session target + 回执带目标）。
    fn v2_task(record: serde_json::Value) -> serde_json::Value {
        json!({
            "record": record,
            "target": {"kind": "session", "sessionId": "session-1"},
            "involvedSessionIds": ["session-1"],
            "status": "active",
            "createdAt": "2026-01-10T08:00:00.000Z",
            "lastDelivery": {
                "scheduledAt": "2026-01-10T08:01:30.000Z",
                "deliveredAt": "2026-01-10T08:01:31.000Z",
                "messageId": "msg-1",
                "targetSessionId": "session-1"
            },
            "deliveryHistory": {
                "records": [{
                    "scheduledAt": "2026-01-10T08:01:30.000Z",
                    "deliveredAt": "2026-01-10T08:01:31.000Z",
                    "messageId": "msg-1",
                    "targetSessionId": "session-1"
                }],
                "earlierRecordsUnavailable": false
            }
        })
    }

    #[test]
    fn v2_task_decodes_normalized() {
        let task = decode_schedule_task(&v2_task(after_record("t1"))).unwrap();
        assert_eq!(task.status, ScheduleTaskStatus::Active);
        assert_eq!(task.involved_session_ids, vec!["session-1".to_string()]);
        assert_eq!(
            task.last_delivery.as_ref().unwrap().target_session_id.as_deref(),
            Some("session-1")
        );
        assert!(!task.delivery_history.earlier_records_pruned);
        assert!(task.pending_delivery.is_none());
        // 回写线形保持 v2 键。
        let wire = serde_json::to_value(&task).unwrap();
        assert!(wire.get("target").unwrap().get("kind").is_some());
        assert!(wire.get("sessionId").is_none());
    }

    #[test]
    fn v1_row_binds_session_and_fills_receipts() {
        // v1 形：无 target，只有旧 sessionId 绑定；回执无目标会话。
        let value = json!({
            "record": after_record("t1"),
            "sessionId": "session-v1",
            "status": "active",
            "lastDelivery": {
                "scheduledAt": "2026-01-10T08:01:30.000Z",
                "deliveredAt": "2026-01-10T08:01:31.000Z",
                "messageId": "msg-1"
            }
        });
        let task = decode_schedule_task(&value).unwrap();
        assert_eq!(task.target, ScheduleTarget::Session {
            session_id: "session-v1".into(),
        });
        // 回执补上绑定目的地；历史缺省生成单行 + unavailable。
        assert_eq!(
            task.last_delivery.unwrap().target_session_id.unwrap(),
            "session-v1"
        );
        assert_eq!(task.delivery_history.records.len(), 1);
        assert!(task.delivery_history.earlier_records_unavailable);
        // involved 缺省 = [绑定]。
        assert_eq!(task.involved_session_ids, vec!["session-v1".to_string()]);
    }

    #[test]
    fn default_target_is_stable_create_once() {
        let value = json!({ "record": after_record("t1") });
        let task = decode_schedule_task(&value).unwrap();
        let ScheduleTarget::NewOnce { session_id, create } = &task.target else {
            panic!("缺省应为 new_once");
        };
        assert_eq!(
            session_id.as_deref(),
            Some(default_session_id("t1").as_str())
        );
        assert!(session_id.as_deref().unwrap().starts_with("session-schedule-"));
        assert_eq!(*create, Some(CreateSpec::default()));
        // 哈希稳定。
        assert_eq!(default_session_id("t1"), default_session_id("t1"));
    }

    #[test]
    fn new_once_missing_session_fills_and_rejects_receipts() {
        // 存盘时未带 sessionId 的 new_once：其无目标回执被拒（填充前检查）。
        let value = json!({
            "record": after_record("t1"),
            "target": {"kind": "new_once"},
            "lastDelivery": {
                "scheduledAt": "2026-01-10T08:01:30.000Z",
                "deliveredAt": "2026-01-10T08:01:31.000Z",
                "messageId": "msg-1"
            }
        });
        let error = decode_schedule_task(&value).unwrap_err();
        assert_eq!(error.0, RECEIPT_TARGET_MESSAGE);
        // 无回执则通过并填默认目的地。
        let mut clean = value.as_object().unwrap().clone();
        clean.remove("lastDelivery");
        let task = decode_schedule_task(&serde_json::Value::Object(clean)).unwrap();
        assert_eq!(
            match &task.target {
                ScheduleTarget::NewOnce { session_id, .. } => session_id.clone(),
                _ => None,
            },
            Some(default_session_id("t1"))
        );
    }

    #[test]
    fn inactive_becomes_ended_and_bad_status_rejects() {
        let mut value = v2_task(after_record("t1"));
        value.as_object_mut().unwrap().insert("status".into(), json!("inactive"));
        let task = decode_schedule_task(&value).unwrap();
        assert_eq!(task.status, ScheduleTaskStatus::Ended);
        value.as_object_mut().unwrap().insert("status".into(), json!("runing"));
        let error = decode_schedule_task(&value).unwrap_err();
        assert_eq!(
            error.0,
            "schedule task status must be \"active\", \"paused\", \"ended\", or \"inactive\""
        );
    }

    #[test]
    fn refine_requires_last_delivery_to_match_history() {
        let mut value = v2_task(after_record("t1"));
        // 删历史 → 历史缺省合成单行 = lastDelivery，放行（上游同款）。
        value.as_object_mut().unwrap().remove("deliveryHistory");
        assert!(decode_schedule_task(&value).is_ok());
        // 历史多一行（最新 ≠ lastDelivery）应拒。
        value.as_object_mut().unwrap().insert(
            "deliveryHistory".into(),
            json!({
                "records": [
                    {
                        "scheduledAt": "2026-01-10T08:01:30.000Z",
                        "deliveredAt": "2026-01-10T08:01:31.000Z",
                        "messageId": "msg-0",
                        "targetSessionId": "session-1"
                    },
                    {
                        "scheduledAt": "2026-01-10T09:01:30.000Z",
                        "deliveredAt": "2026-01-10T09:01:31.000Z",
                        "messageId": "msg-1",
                        "targetSessionId": "session-1"
                    }
                ],
                "earlierRecordsUnavailable": false
            }),
        );
        let error = decode_schedule_task(&value).unwrap_err();
        assert_eq!(error.0, LAST_DELIVERY_MESSAGE);
    }

    #[test]
    fn duplicate_message_ids_and_bad_instants_reject() {
        let mut value = v2_task(after_record("t1"));
        // 历史重复 messageId。
        value.as_object_mut().unwrap().insert(
            "deliveryHistory".into(),
            json!({
                "records": [
                    {
                        "scheduledAt": "2026-01-10T08:01:30.000Z",
                        "deliveredAt": "2026-01-10T08:01:31.000Z",
                        "messageId": "msg-1",
                        "targetSessionId": "session-1"
                    },
                    {
                        "scheduledAt": "2026-01-10T09:01:30.000Z",
                        "deliveredAt": "2026-01-10T09:01:31.000Z",
                        "messageId": "msg-1",
                        "targetSessionId": "session-1",
                        "prompt": "p"
                    }
                ],
                "earlierRecordsUnavailable": false,
                "earlierRecordsPruned": true
            }),
        );
        // lastDelivery 与最新回执一致（含 prompt 不参与比较）。
        value.as_object_mut().unwrap().insert(
            "lastDelivery".into(),
            json!({
                "scheduledAt": "2026-01-10T09:01:30.000Z",
                "deliveredAt": "2026-01-10T09:01:31.000Z",
                "messageId": "msg-1",
                "targetSessionId": "session-1"
            }),
        );
        let error = decode_schedule_task(&value).unwrap_err();
        assert_eq!(error.0, "Delivery history message identities must be unique within a task");
        // 非规范 instant。
        let mut value = v2_task(after_record("t1"));
        value.as_object_mut().unwrap().insert("createdAt".into(), json!("2026-01-10T08:00:00Z"));
        let error = decode_schedule_task(&value).unwrap_err();
        assert_eq!(error.0, INSTANT_MESSAGE);
    }

    #[test]
    fn missing_binding_and_unknown_keys_reject() {
        // v1 绑定非法。
        let value = json!({ "record": after_record("t1"), "sessionId": " padded " });
        let error = decode_schedule_task(&value).unwrap_err();
        assert_eq!(error.0, BINDING_MESSAGE);
        // 未知键（strict）。
        let mut value = v2_task(after_record("t1"));
        value.as_object_mut().unwrap().insert("extra".into(), json!(1));
        let error = decode_schedule_task(&value).unwrap_err();
        assert_eq!(error.0, "schedule task must contain a record and only known metadata keys");
    }
}

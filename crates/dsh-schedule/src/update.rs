//! 定时构造与编辑的统一分派（A1.4 批②④）。
//!
//! 对齐上游 `update.ts`：`validateTiming` 键闭集、`timingOf`（一次性
//! 保留已承诺绝对目标）、`normalizedTiming`（等价归一后才判变更）、
//! `updateScheduleRecord`（未变 timing 保留已承诺目标；变了或 resume
//! 走创建同款构造器；resume 无未来 occurrence 的逐字换文案）。

use serde_json::Value;

use crate::{
    create_after_schedule_record, create_at_schedule_record, create_cron_schedule_record,
    create_daily_schedule_record, create_every_schedule_record, create_rrule_schedule_record,
    create_weekly_schedule_record, normalize_rrule, parse_at_input, parse_cron_input,
    parse_daily_input, parse_weekly_input, schedule_title, ScheduleInputError,
    ScheduleInputErrorCode, ScheduleRecord,
};

/// 七种 timing 选择器（上游 `ScheduleTiming` 的 Rust 形）。
#[derive(Clone, Debug)]
pub enum ScheduleTiming {
    After { after_seconds: u64 },
    At { at: Value },
    Every { every_seconds: u64 },
    Daily { daily: Value },
    Weekly { weekly: Value },
    Cron { cron: Value },
    RRule { rrule: String },
}

impl ScheduleTiming {
    /// 上游 selectors 表的 kind → 选择器键名。
    fn selector_key(&self) -> &'static str {
        match self {
            Self::After { .. } => "after_seconds",
            Self::At { .. } => "at",
            Self::Every { .. } => "every_seconds",
            Self::Daily { .. } => "daily",
            Self::Weekly { .. } => "weekly",
            Self::Cron { .. } => "cron",
            Self::RRule { .. } => "rrule",
        }
    }

    /// 上游 kind 字面值。
    fn kind(&self) -> &'static str {
        match self {
            Self::After { .. } => "after",
            Self::At { .. } => "at",
            Self::Every { .. } => "every",
            Self::Daily { .. } => "daily",
            Self::Weekly { .. } => "weekly",
            Self::Cron { .. } => "cron",
            Self::RRule { .. } => "rrule",
        }
    }

    /// 线形（构造校验用原始值直通）。
    fn to_wire(&self) -> Value {
        let mut object = serde_json::Map::new();
        object.insert("kind".into(), serde_json::json!(self.kind()));
        match self {
            Self::After { after_seconds } => {
                object.insert("after_seconds".into(), serde_json::json!(after_seconds));
            }
            Self::At { at } => {
                object.insert("at".into(), at.clone());
            }
            Self::Every { every_seconds } => {
                object.insert("every_seconds".into(), serde_json::json!(every_seconds));
            }
            Self::Daily { daily } => {
                object.insert("daily".into(), daily.clone());
            }
            Self::Weekly { weekly } => {
                object.insert("weekly".into(), weekly.clone());
            }
            Self::Cron { cron } => {
                object.insert("cron".into(), cron.clone());
            }
            Self::RRule { rrule } => {
                object.insert("rrule".into(), serde_json::json!(rrule));
            }
        }
        Value::Object(object)
    }
}

/// 在不可信管理边界读恰好一个匹配的 timing 载荷（上游 `validateTiming`）。
pub fn validate_timing(value: &Value) -> Result<ScheduleTiming, ScheduleInputError> {
    let error = || {
        ScheduleInputError::new(
            ScheduleInputErrorCode::InvalidRule,
            "timing must contain exactly kind and its matching selector.",
        )
    };
    let Some(map) = value.as_object() else {
        return Err(error());
    };
    let kind = map.get("kind").and_then(Value::as_str).ok_or_else(error)?;
    let selector_key = match kind {
        "after" => "after_seconds",
        "at" => "at",
        "every" => "every_seconds",
        "daily" => "daily",
        "weekly" => "weekly",
        "cron" => "cron",
        "rrule" => "rrule",
        _ => return Err(error()),
    };
    if map.len() != 2 || !map.contains_key(selector_key) {
        return Err(error());
    }
    let selector = map.get(selector_key).expect("上面已判存在");
    Ok(match kind {
        "after" => ScheduleTiming::After {
            after_seconds: selector.as_u64().ok_or_else(error)?,
        },
        "at" => ScheduleTiming::At { at: selector.clone() },
        "every" => ScheduleTiming::Every {
            every_seconds: selector.as_u64().ok_or_else(error)?,
        },
        "daily" => ScheduleTiming::Daily { daily: selector.clone() },
        "weekly" => ScheduleTiming::Weekly { weekly: selector.clone() },
        "cron" => ScheduleTiming::Cron { cron: selector.clone() },
        _ => ScheduleTiming::RRule {
            rrule: selector.as_str().ok_or_else(error)?.to_string(),
        },
    })
}

/// 把记录的规则表达为可编辑 timing；一次性保留已承诺绝对目标
/// （上游 `timingOf`）。
pub fn timing_of(record: &ScheduleRecord) -> ScheduleTiming {
    match record {
        ScheduleRecord::After(record) => ScheduleTiming::At {
            at: serde_json::json!(record.scheduled_at),
        },
        ScheduleRecord::At(record) => ScheduleTiming::At {
            at: serde_json::json!(record.scheduled_at),
        },
        ScheduleRecord::Every(record) => ScheduleTiming::Every {
            every_seconds: record.every_seconds,
        },
        ScheduleRecord::Daily(record) => ScheduleTiming::Daily {
            daily: serde_json::json!({"time": record.time, "time_zone": record.time_zone}),
        },
        ScheduleRecord::Weekly(record) => ScheduleTiming::Weekly {
            weekly: serde_json::json!({"time": record.time, "time_zone": record.time_zone, "weekdays": record.weekdays}),
        },
        ScheduleRecord::Cron(record) => ScheduleTiming::Cron {
            cron: serde_json::json!({"expression": record.expression, "time_zone": record.time_zone}),
        },
        ScheduleRecord::RRule(record) => ScheduleTiming::RRule {
            rrule: record.rrule.clone(),
        },
    }
}

/// 计算任何新目标前的等价归一（上游 `normalizedTiming`）。
pub fn normalized_timing(value: &ScheduleTiming) -> Result<Value, ScheduleInputError> {
    let normalized = match value {
        ScheduleTiming::After { after_seconds } => serde_json::json!({
            "kind": "after", "after_seconds": after_seconds,
        }),
        ScheduleTiming::At { at } => serde_json::json!({
            "kind": "at", "at": parse_at_input(at)?,
        }),
        ScheduleTiming::Every { every_seconds } => serde_json::json!({
            "kind": "every", "every_seconds": every_seconds,
        }),
        ScheduleTiming::Daily { daily } => {
            let (time, time_zone) = parse_daily_input(daily)?;
            serde_json::json!({"kind": "daily", "time": time, "time_zone": time_zone})
        }
        ScheduleTiming::Weekly { weekly } => {
            let (time, time_zone, weekdays) = parse_weekly_input(weekly)?;
            serde_json::json!({
                "kind": "weekly", "time": time, "time_zone": time_zone, "weekdays": weekdays,
            })
        }
        ScheduleTiming::Cron { cron } => {
            let (expression, time_zone) = parse_cron_input(cron)?;
            serde_json::json!({"kind": "cron", "expression": expression, "time_zone": time_zone})
        }
        ScheduleTiming::RRule { rrule } => serde_json::json!({
            "kind": "rrule", "rrule": normalize_rrule(rrule)?,
        }),
    };
    Ok(normalized)
}

/// 用一个权威 timing 形态构造带未来已承诺目标的当前记录（上游
/// `createScheduleRecord`）。
pub fn create_schedule_record(
    id: &str,
    title: &str,
    prompt: &str,
    value: &Value,
    now: i64,
) -> Result<ScheduleRecord, ScheduleInputError> {
    match validate_timing(value)? {
        ScheduleTiming::After { after_seconds } => Ok(ScheduleRecord::After(create_after_schedule_record(
            id, prompt, after_seconds, now, title,
        )?)),
        ScheduleTiming::At { at } => Ok(ScheduleRecord::At(create_at_schedule_record(
            id, prompt, &at, now, title,
        )?)),
        ScheduleTiming::Every { every_seconds } => Ok(ScheduleRecord::Every(
            create_every_schedule_record(id, prompt, every_seconds, now, title)?,
        )),
        ScheduleTiming::Daily { daily } => Ok(ScheduleRecord::Daily(
            create_daily_schedule_record(id, prompt, &daily, now, title)?,
        )),
        ScheduleTiming::Weekly { weekly } => Ok(ScheduleRecord::Weekly(
            create_weekly_schedule_record(id, prompt, &weekly, now, title)?,
        )),
        ScheduleTiming::Cron { cron } => Ok(ScheduleRecord::Cron(
            create_cron_schedule_record(id, prompt, &cron, now, title)?,
        )),
        ScheduleTiming::RRule { rrule } => Ok(ScheduleRecord::RRule(
            create_rrule_schedule_record(id, title, prompt, &rrule, now)?,
        )),
    }
}

/// 编辑补丁（上游 `SchedulePatch` 的描述字段 + 可选 timing 替换）。
#[derive(Clone, Debug, Default)]
pub struct SchedulePatch {
    pub title: Option<String>,
    pub prompt: Option<String>,
    pub timing: Option<Value>,
}

/// 保持未变的 timing 与已承诺目标，或走创建同款构造器重新武装
/// （上游 `updateScheduleRecord`）。
pub fn update_schedule_record(
    current: &ScheduleRecord,
    patch: &SchedulePatch,
    now: i64,
    resume: bool,
) -> Result<ScheduleRecord, ScheduleInputError> {
    let title = schedule_title(patch.title.as_deref().unwrap_or(current.title()))?;
    let prompt = patch.prompt.as_deref().unwrap_or(current.prompt());
    if prompt.trim().is_empty() {
        return Err(ScheduleInputError::new(
            ScheduleInputErrorCode::InvalidPrompt,
            "prompt must be non-empty after trimming.",
        ));
    }
    let timing = match &patch.timing {
        Some(value) => validate_timing(value)?,
        None => timing_of(current),
    };
    let changed = patch.timing.is_some()
        && normalized_timing(&timing)? != normalized_timing(&timing_of(current))?;
    let outcome =
        if matches!(current, ScheduleRecord::RRule(_)) && matches!(timing, ScheduleTiming::RRule { .. }) {
        let ScheduleTiming::RRule { rrule } = &timing else {
            unreachable!("上面已判 rrule 形")
        };
        update_rrule_schedule_record(
            match current {
                ScheduleRecord::RRule(record) => record,
                _ => unreachable!("上面已判 rrule 形"),
            },
            &title,
            prompt,
            changed.then_some(rrule.as_str()),
            now,
            changed || resume,
        )
        .map(ScheduleRecord::RRule)
    } else if changed || resume {
        create_schedule_record(current.id(), &title, prompt, &timing.to_wire(), now)
    } else {
        // 保留当前记录：仅描述字段可能变。
        let trimmed = prompt.trim();
        if title == current.title() && trimmed == current.prompt() {
            Ok(current.clone())
        } else {
            Ok(with_title_prompt(current, &title, trimmed))
        }
    };
    match outcome {
        Err(error)
            if resume && !changed && error.code == ScheduleInputErrorCode::NotFuture =>
        {
            Err(ScheduleInputError::new(
                ScheduleInputErrorCode::NotFuture,
                "This task has no future occurrence. Set a future schedule before resuming it.",
            ))
        }
        outcome => outcome,
    }
}

/// 更新 rrule 记录的描述字段并可选重算下一 occurrence（上游
/// `updateRRuleScheduleRecord`）。
#[allow(clippy::too_many_arguments)]
fn update_rrule_schedule_record(
    current: &crate::RRuleScheduleRecord,
    title: &str,
    prompt: &str,
    rrule: Option<&str>,
    now: i64,
    recompute: bool,
) -> Result<crate::RRuleScheduleRecord, ScheduleInputError> {
    let title = schedule_title(title)?;
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return Err(ScheduleInputError::new(
            ScheduleInputErrorCode::InvalidPrompt,
            "prompt must be non-empty after trimming.",
        ));
    }
    let prepared = match rrule {
        Some(rrule) => crate::record::read_recurrence(crate::RRule::parse(rrule, true))?,
        None => crate::record::read_recurrence(crate::RRule::parse(&current.rrule, false))?,
    };
    if recompute {
        return crate::record::first_rrule_record(current.id.as_str(), title, prompt.to_string(), &prepared, now);
    }
    Ok(crate::RRuleScheduleRecord {
        id: current.id.clone(),
        title,
        prompt: prompt.to_string(),
        rrule: prepared.expression().to_string(),
        scheduled_at: current.scheduled_at.clone(),
    })
}

/// 仅替换描述字段（timing 保留；上游 spread 同款）。
fn with_title_prompt(record: &ScheduleRecord, title: &str, prompt: &str) -> ScheduleRecord {
    match record {
        ScheduleRecord::After(r) => ScheduleRecord::After(crate::AfterScheduleRecord {
            title: title.to_string(),
            prompt: prompt.to_string(),
            ..r.clone()
        }),
        ScheduleRecord::At(r) => ScheduleRecord::At(crate::AtScheduleRecord {
            title: title.to_string(),
            prompt: prompt.to_string(),
            ..r.clone()
        }),
        ScheduleRecord::Every(r) => ScheduleRecord::Every(crate::EveryScheduleRecord {
            title: title.to_string(),
            prompt: prompt.to_string(),
            ..r.clone()
        }),
        ScheduleRecord::Daily(r) => ScheduleRecord::Daily(crate::DailyScheduleRecord {
            title: title.to_string(),
            prompt: prompt.to_string(),
            ..r.clone()
        }),
        ScheduleRecord::Weekly(r) => ScheduleRecord::Weekly(crate::WeeklyScheduleRecord {
            title: title.to_string(),
            prompt: prompt.to_string(),
            ..r.clone()
        }),
        ScheduleRecord::Cron(r) => ScheduleRecord::Cron(crate::CronScheduleRecord {
            title: title.to_string(),
            prompt: prompt.to_string(),
            ..r.clone()
        }),
        ScheduleRecord::RRule(r) => ScheduleRecord::RRule(crate::RRuleScheduleRecord {
            title: title.to_string(),
            prompt: prompt.to_string(),
            ..r.clone()
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 2026-01-10T08:00:00.000Z（周六）。
    const NOW: i64 = 1_768_032_000_000;

    fn daily_record() -> ScheduleRecord {
        create_schedule_record(
            "t1",
            "t",
            "p",
            &json!({"kind": "daily", "daily": {"time": "09:30:00", "time_zone": "UTC"}}),
            NOW,
        )
        .unwrap()
    }

    #[test]
    fn validate_timing_requires_exact_keys() {
        let error = validate_timing(&json!({"kind": "daily"})).unwrap_err();
        assert_eq!(error.message, "timing must contain exactly kind and its matching selector.");
        let error = validate_timing(&json!({"kind": "daily", "daily": {}, "extra": 1})).unwrap_err();
        assert_eq!(error.message, "timing must contain exactly kind and its matching selector.");
        let error = validate_timing(&json!({"kind": "hourly", "hourly": {}})).unwrap_err();
        assert_eq!(error.message, "timing must contain exactly kind and its matching selector.");
        // 合法形态直通。
        assert!(validate_timing(&json!({"kind": "after", "after_seconds": 90})).is_ok());
    }

    #[test]
    fn unchanged_timing_keeps_committed_target() {
        let current = daily_record();
        let patch = SchedulePatch {
            title: Some("renamed".into()),
            prompt: Some("new prompt".into()),
            timing: None,
        };
        let updated = update_schedule_record(&current, &patch, NOW + 1, false).unwrap();
        // timing 未变 → 已承诺目标保留。
        assert_eq!(updated.scheduled_at(), current.scheduled_at());
        assert_eq!(updated.title(), "renamed");
        assert_eq!(updated.prompt(), "new prompt");
        // 描述完全未变 → 返回原记录。
        let same = update_schedule_record(&current, &SchedulePatch::default(), NOW, false).unwrap();
        assert_eq!(same, current);
    }

    #[test]
    fn changed_timing_re_arms_via_constructor() {
        let current = daily_record();
        let patch = SchedulePatch {
            title: None,
            prompt: None,
            timing: Some(json!({"kind": "daily", "daily": {"time": "10:30:00", "time_zone": "UTC"}})),
        };
        let updated = update_schedule_record(&current, &patch, NOW, false).unwrap();
        assert_eq!(updated.scheduled_at(), "2026-01-10T10:30:00.000Z");
        // 等价归一：09:30 vs 09:30:00.000（语义同形）不算变更。
        let patch = SchedulePatch {
            title: None,
            prompt: None,
            timing: Some(json!({"kind": "daily", "daily": {"time": "09:30:00.000", "time_zone": "UTC"}})),
        };
        let updated2 = update_schedule_record(&current, &patch, NOW + 60_000, false).unwrap();
        // timing 未实质变更 → 保留旧承诺目标。
        assert_eq!(updated2.scheduled_at(), current.scheduled_at());
    }

    #[test]
    fn resume_without_future_gets_verbatim_copy() {
        // at 形目标已过 + resume：创建端 not_future 换文案。
        let current = create_schedule_record(
            "t1",
            "t",
            "p",
            &json!({"kind": "at", "at": "2026-01-10T09:00:00.000Z"}),
            NOW,
        )
        .unwrap();
        let later = NOW + 3_600_000;
        let error =
            update_schedule_record(&current, &SchedulePatch::default(), later, true).unwrap_err();
        assert_eq!(error.code, ScheduleInputErrorCode::NotFuture);
        assert_eq!(
            error.message,
            "This task has no future occurrence. Set a future schedule before resuming it."
        );
        // 非 resume 的等价情形：timing 未变 + 描述未变 → 保留，不报错。
        let retained =
            update_schedule_record(&current, &SchedulePatch::default(), later, false).unwrap();
        assert_eq!(retained.scheduled_at(), current.scheduled_at());
        // timing 实际变更（改成过去时刻）→ 创建端报原始文案。
        let patch = SchedulePatch {
            title: None,
            prompt: None,
            timing: Some(json!({"kind": "at", "at": "2026-01-10T08:30:00.000Z"})),
        };
        let error = update_schedule_record(&current, &patch, later, false).unwrap_err();
        assert_eq!(error.message, "The scheduled time must be strictly in the future.");
    }

    #[test]
    fn one_shot_timing_becomes_absolute_at() {
        // after 记录的 timingOf = at(已承诺目标)；等价归一保持稳定。
        let current = create_schedule_record(
            "t1",
            "t",
            "p",
            &json!({"kind": "after", "after_seconds": 90}),
            NOW,
        )
        .unwrap();
        let timing = timing_of(&current);
        let normalized = normalized_timing(&timing).unwrap();
        assert_eq!(
            normalized,
            serde_json::json!({"kind": "at", "at": 1_768_032_090_000i64})
        );
        // 同形 at patch 不算变更。
        let patch = SchedulePatch {
            title: None,
            prompt: None,
            timing: Some(serde_json::json!({"kind": "at", "at": "2026-01-10T08:01:30.000Z"})),
        };
        let updated = update_schedule_record(&current, &patch, NOW + 60_000, false).unwrap();
        assert_eq!(updated.scheduled_at(), current.scheduled_at());
    }

    #[test]
    fn rrule_edit_keeps_or_recomputes() {
        let current = create_schedule_record(
            "t1",
            "t",
            "p",
            &json!({"kind": "rrule", "rrule": "DTSTART:20260110T090000Z\nRRULE:FREQ=DAILY"}),
            NOW,
        )
        .unwrap();
        // 仅改描述：rrule 与承诺目标保留。
        let patch = SchedulePatch {
            title: Some("renamed".into()),
            prompt: None,
            timing: None,
        };
        let updated = update_schedule_record(&current, &patch, NOW + 60_000, false).unwrap();
        assert_eq!(updated.title(), "renamed");
        assert_eq!(updated.scheduled_at(), current.scheduled_at());
        // 改 rrule：重算首个未来 occurrence。
        let patch = SchedulePatch {
            title: None,
            prompt: None,
            timing: Some(json!({
                "kind": "rrule",
                "rrule": "DTSTART:20260110T090000Z\nRRULE:FREQ=DAILY;INTERVAL=2"
            })),
        };
        let updated = update_schedule_record(&current, &patch, NOW, false).unwrap();
        assert_eq!(updated.scheduled_at(), "2026-01-10T09:00:00.000Z");
    }
}

//! 任务构造与耐久解码（A1.4 批②②）。
//!
//! 对齐上游 `domain.ts` 的构造器（`create*ScheduleRecord`）与逐形态
//! 严格解码器（`decode*Record`）：构造器按「prompt → 选择器 → now 可
//! 表示 → title → 目标」的顺序映射逐字文案；解码器先 title 再键闭集
//! 再逐字段，全部文案逐字。cron/rrule 两形态的构造与解码在批②③
//! 落引擎时接入（`decode_schedule_record` 七形态闭集包装同步落）。

use serde_json::{Map, Value};

use crate::{
    AtScheduleRecord, DailyScheduleRecord, EveryScheduleRecord, ScheduleInputError,
    ScheduleInputErrorCode, ScheduleLogError, AfterScheduleRecord, WeeklyScheduleRecord,
    canonicalize_time_zone, decode_id, decode_instant, decode_stored_title, future_instant,
    is_four_digit_year_instant, local_clock_time, normalize_weekdays, next_daily_target,
    next_weekly_target, parse, schedule_title, MIN_EVERY_INTERVAL_SECONDS,
};
use parse::{InstantRejection, LocalCalendarRejection};

/// JS `Number.MAX_SAFE_INTEGER`（上游安全整数上限）。
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// canonical 形拒绝的稳定诊断（上游 `NON_CANONICAL_INSTANT_MESSAGE`）。
pub(crate) const NON_CANONICAL_INSTANT_MESSAGE: &str =
    "scheduledAt must be a canonical four-digit-year RFC 3339 UTC instant";

/// 不可表示目标的稳定诊断（上游 `futureInstant` 的越界分支）。
fn time_out_of_range_error() -> ScheduleInputError {
    ScheduleInputError::new(
        ScheduleInputErrorCode::TimeOutOfRange,
        "The scheduled time must be representable as a four-digit-year RFC 3339 UTC instant.",
    )
}

/// prompt 校验（上游各 create* 共同的 trim 分支）。
fn normalized_prompt(prompt: &str) -> Result<String, ScheduleInputError> {
    let trimmed = prompt.trim();
    if trimmed.is_empty() {
        return Err(ScheduleInputError::new(
            ScheduleInputErrorCode::InvalidPrompt,
            "prompt must be non-empty after trimming.",
        ));
    }
    Ok(trimmed.to_string())
}

/// 是否恰好携带全部期望键、且无额外键（上游 `hasExactKeys`）。
pub(crate) fn has_exact_keys(value: &Map<String, Value>, expected: &[&str]) -> bool {
    expected.iter().all(|key| value.contains_key(*key))
        && value.keys().all(|key| expected.contains(&key.as_str()))
}

/// 解码记录 title，再要求键闭集（上游 `decodeRecordTitle`：title 先行，
/// 缺 title 报 title 诊断而非键清单）。
fn decode_record_title(
    value: &Map<String, Value>,
    expected: &[&str],
    message: &str,
) -> Result<String, ScheduleLogError> {
    let Some(title) = value.get("title").and_then(Value::as_str) else {
        return Err(ScheduleLogError(crate::REQUIRED_TITLE_MESSAGE.into()));
    };
    let title = decode_stored_title(title)?;
    if !has_exact_keys(value, expected) {
        return Err(ScheduleLogError(message.into()));
    }
    Ok(title)
}

/// 解码必填且已 trim 的耐久 prompt 字段（上游各 decode* 的共同分支）。
fn decode_prompt_field(value: &Map<String, Value>, selector: &str) -> Result<String, ScheduleLogError> {
    let Some(prompt) = value.get("prompt").and_then(Value::as_str) else {
        return Err(ScheduleLogError(format!(
            "{selector} prompt must be non-empty and already trimmed"
        )));
    };
    if prompt.is_empty() || prompt.trim() != prompt {
        return Err(ScheduleLogError(format!(
            "{selector} prompt must be non-empty and already trimmed"
        )));
    }
    Ok(prompt.to_string())
}

/// 解码耐久 id 字段（非串同报 id 诊断）。
fn decode_id_field(value: &Map<String, Value>) -> Result<String, ScheduleLogError> {
    let Some(id) = value.get("id").and_then(Value::as_str) else {
        return Err(ScheduleLogError(
            "schedule id must be a non-empty string without surrounding whitespace".into(),
        ));
    };
    decode_id(id)
}

/// 解码耐久 scheduledAt 字段（非串同报 canonical 诊断——上游 `decodeInstant`）。
fn decode_scheduled_at_field(value: &Map<String, Value>) -> Result<String, ScheduleLogError> {
    let Some(text) = value.get("scheduledAt").and_then(Value::as_str) else {
        return Err(ScheduleLogError(NON_CANONICAL_INSTANT_MESSAGE.into()));
    };
    decode_instant(text)
}

/// 延迟一次性提醒（上游 `createAfterScheduleRecord`）。
pub fn create_after_schedule_record(
    id: &str,
    prompt: &str,
    after_seconds: u64,
    now: i64,
    title: &str,
) -> Result<AfterScheduleRecord, ScheduleInputError> {
    let prompt = normalized_prompt(prompt)?;
    if after_seconds == 0 || after_seconds > MAX_SAFE_INTEGER {
        return Err(ScheduleInputError::new(
            ScheduleInputErrorCode::InvalidRule,
            "after_seconds must be a positive safe integer.",
        ));
    }
    let title = schedule_title(title)?;
    let delay = after_seconds as i64 * 1_000;
    let target = now.checked_add(delay).ok_or_else(time_out_of_range_error)?;
    Ok(AfterScheduleRecord {
        id: id.into(),
        title,
        prompt,
        after_seconds,
        scheduled_at: future_instant(target, now)?,
    })
}

/// 解析绝对选择器，不要求未来（上游 `parseAtInput`：显式偏移串或本地
/// 日历对象，键闭集 `date/time/time_zone`）。
pub fn parse_at_input(at: &Value) -> Result<i64, ScheduleInputError> {
    if let Some(text) = at.as_str() {
        return match parse::parse_offset_instant(text) {
            Ok(epoch) => Ok(epoch),
            Err(InstantRejection::NotRfc3339) => Err(ScheduleInputError::new(
                ScheduleInputErrorCode::InvalidRule,
                "at must use YYYY-MM-DDTHH:mm:ss with optional 1-3 digit fractional seconds and an explicit Z or numeric offset.",
            )),
            Err(InstantRejection::InvalidNumericOffset) => Err(ScheduleInputError::new(
                ScheduleInputErrorCode::InvalidRule,
                "The at numeric offset is invalid.",
            )),
            Err(InstantRejection::NotACalendarDate | InstantRejection::OutOfRange) => Err(
                ScheduleInputError::new(
                    ScheduleInputErrorCode::InvalidRule,
                    "The at value must be a real ISO calendar date and time.",
                ),
            ),
        };
    }
    let Some(object) = at.as_object() else {
        return Err(ScheduleInputError::new(
            ScheduleInputErrorCode::InvalidRule,
            "at must be an explicit-offset string or local calendar object.",
        ));
    };
    if !has_exact_keys(object, &["date", "time", "time_zone"]) {
        return Err(ScheduleInputError::new(
            ScheduleInputErrorCode::InvalidRule,
            "Local at must contain exactly date, time, and time_zone.",
        ));
    }
    let Some(date) = object.get("date").and_then(Value::as_str) else {
        return Err(ScheduleInputError::new(
            ScheduleInputErrorCode::InvalidRule,
            "Local at date and time must be strings.",
        ));
    };
    let Some(time) = object.get("time").and_then(Value::as_str) else {
        return Err(ScheduleInputError::new(
            ScheduleInputErrorCode::InvalidRule,
            "Local at date and time must be strings.",
        ));
    };
    let Some(raw_zone) = object.get("time_zone").and_then(Value::as_str) else {
        return Err(ScheduleInputError::new(
            ScheduleInputErrorCode::InvalidTimeZone,
            "time_zone must be a string.",
        ));
    };
    let (date, time) = match parse::parse_local_calendar(date, time) {
        Ok(parts) => parts,
        Err(LocalCalendarRejection::Malformed) => {
            return Err(ScheduleInputError::new(
                ScheduleInputErrorCode::InvalidRule,
                "Local at requires date YYYY-MM-DD and time HH:mm:ss with optional one-to-three digit milliseconds.",
            ));
        }
        Err(LocalCalendarRejection::NotACalendarDate) => {
            return Err(ScheduleInputError::new(
                ScheduleInputErrorCode::InvalidRule,
                "The at value must be a real ISO calendar date and time.",
            ));
        }
        Err(LocalCalendarRejection::OutOfRange) => {
            return Err(ScheduleInputError::new(
                ScheduleInputErrorCode::InvalidRule,
                "The local at value must be a real ISO calendar date and time.",
            ));
        }
    };
    let zone = canonicalize_time_zone(raw_zone)?;
    crate::occurrence::resolve_local_instant(date, time, &zone).ok_or_else(|| {
        ScheduleInputError::new(
            ScheduleInputErrorCode::InvalidRule,
            "The local at time does not exist in the selected time zone.",
        )
    })
}

/// 绝对一次性提醒（上游 `createAtScheduleRecord`：title 在 at 解析前校验）。
pub fn create_at_schedule_record(
    id: &str,
    prompt: &str,
    at: &Value,
    now: i64,
    title: &str,
) -> Result<AtScheduleRecord, ScheduleInputError> {
    let prompt = normalized_prompt(prompt)?;
    let title = schedule_title(title)?;
    let target = parse_at_input(at)?;
    Ok(AtScheduleRecord {
        id: id.into(),
        title,
        prompt,
        scheduled_at: future_instant(target, now)?,
    })
}

/// 固定间隔提醒（上游 `createEveryScheduleRecord`）。
pub fn create_every_schedule_record(
    id: &str,
    prompt: &str,
    every_seconds: u64,
    now: i64,
    title: &str,
) -> Result<EveryScheduleRecord, ScheduleInputError> {
    let prompt = normalized_prompt(prompt)?;
    if every_seconds > MAX_SAFE_INTEGER {
        return Err(ScheduleInputError::new(
            ScheduleInputErrorCode::InvalidRule,
            "every_seconds must be a safe integer.",
        ));
    }
    if every_seconds < MIN_EVERY_INTERVAL_SECONDS {
        return Err(ScheduleInputError::new(
            ScheduleInputErrorCode::FrequencyTooHigh,
            format!("every_seconds must be at least {MIN_EVERY_INTERVAL_SECONDS}."),
        ));
    }
    let title = schedule_title(title)?;
    let interval = every_seconds as i64 * 1_000;
    let target = now.checked_add(interval).ok_or_else(time_out_of_range_error)?;
    Ok(EveryScheduleRecord {
        id: id.into(),
        title,
        prompt,
        every_seconds,
        scheduled_at: future_instant(target, now)?,
    })
}

/// 归一 daily 选择器，不算新承诺目标（上游 `parseDailyInput`）。
pub fn parse_daily_input(daily: &Value) -> Result<(String, String), ScheduleInputError> {
    let time_error = || {
        ScheduleInputError::new(
            ScheduleInputErrorCode::InvalidRule,
            "daily must contain exactly time and time_zone, with time HH:mm:ss and optional 1-3 fractional digits.",
        )
    };
    let Some(object) = daily.as_object() else {
        return Err(time_error());
    };
    if !has_exact_keys(object, &["time", "time_zone"])
        || !object.get("time").is_some_and(Value::is_string)
    {
        return Err(time_error());
    }
    let Some(raw_zone) = object.get("time_zone").and_then(Value::as_str) else {
        return Err(ScheduleInputError::new(
            ScheduleInputErrorCode::InvalidTimeZone,
            "time_zone must be a string.",
        ));
    };
    let time = object.get("time").and_then(Value::as_str).expect("上面已校验为串");
    let time = local_clock_time(time, "daily")?;
    let time_zone = canonicalize_time_zone(raw_zone)?;
    Ok((time, time_zone))
}

/// 每日墙钟提醒（上游 `createDailyScheduleRecord`）。
pub fn create_daily_schedule_record(
    id: &str,
    prompt: &str,
    daily: &Value,
    now: i64,
    title: &str,
) -> Result<DailyScheduleRecord, ScheduleInputError> {
    let prompt = normalized_prompt(prompt)?;
    let (time, time_zone) = parse_daily_input(daily)?;
    if !is_four_digit_year_instant(now) {
        return Err(ScheduleInputError::new(
            ScheduleInputErrorCode::TimeOutOfRange,
            "Daily creation time must be a representable four-digit-year UTC instant.",
        ));
    }
    let title = schedule_title(title)?;
    let Some(scheduled_at) = next_daily_target(&time, &time_zone, now)? else {
        return Err(ScheduleInputError::new(
            ScheduleInputErrorCode::TimeOutOfRange,
            "No future daily occurrence is representable as a four-digit-year UTC instant.",
        ));
    };
    Ok(DailyScheduleRecord {
        id: id.into(),
        title,
        prompt,
        time,
        time_zone,
        scheduled_at,
    })
}

/// 归一 weekly 选择器，不算新承诺目标（上游 `parseWeeklyInput`）。
pub fn parse_weekly_input(
    weekly: &Value,
) -> Result<(String, String, Vec<u8>), ScheduleInputError> {
    let time_error = || {
        ScheduleInputError::new(
            ScheduleInputErrorCode::InvalidRule,
            "weekly must contain exactly time, time_zone, and weekdays, with time HH:mm:ss and optional 1-3 fractional digits.",
        )
    };
    let Some(object) = weekly.as_object() else {
        return Err(time_error());
    };
    if !has_exact_keys(object, &["time", "time_zone", "weekdays"])
        || !object.get("time").is_some_and(Value::is_string)
    {
        return Err(time_error());
    }
    let Some(raw_zone) = object.get("time_zone").and_then(Value::as_str) else {
        return Err(ScheduleInputError::new(
            ScheduleInputErrorCode::InvalidTimeZone,
            "time_zone must be a string.",
        ));
    };
    let time = object.get("time").and_then(Value::as_str).expect("上面已校验为串");
    let time = local_clock_time(time, "weekly")?;
    let time_zone = canonicalize_time_zone(raw_zone)?;
    let weekdays = normalize_weekdays(object.get("weekdays").unwrap_or(&Value::Null))?;
    Ok((time, time_zone, weekdays))
}

/// 每周墙钟提醒（上游 `createWeeklyScheduleRecord`）。
pub fn create_weekly_schedule_record(
    id: &str,
    prompt: &str,
    weekly: &Value,
    now: i64,
    title: &str,
) -> Result<WeeklyScheduleRecord, ScheduleInputError> {
    let prompt = normalized_prompt(prompt)?;
    let (time, time_zone, weekdays) = parse_weekly_input(weekly)?;
    if !is_four_digit_year_instant(now) {
        return Err(ScheduleInputError::new(
            ScheduleInputErrorCode::TimeOutOfRange,
            "Weekly creation time must be a representable four-digit-year UTC instant.",
        ));
    }
    let title = schedule_title(title)?;
    let Some(scheduled_at) = next_weekly_target(&time, &time_zone, &weekdays, now)? else {
        return Err(ScheduleInputError::new(
            ScheduleInputErrorCode::TimeOutOfRange,
            "No future weekly occurrence is representable as a four-digit-year UTC instant.",
        ));
    };
    Ok(WeeklyScheduleRecord {
        id: id.into(),
        title,
        prompt,
        time,
        time_zone,
        weekdays,
        scheduled_at,
    })
}

/// 解码延迟一次性耐久记录（上游 `decodeAfterRecord`）。
pub fn decode_after_record(value: &Map<String, Value>) -> Result<AfterScheduleRecord, ScheduleLogError> {
    let title = decode_record_title(
        value,
        &["id", "kind", "title", "prompt", "afterSeconds", "scheduledAt"],
        "after schedule must contain exactly id, kind, title, prompt, afterSeconds, and scheduledAt",
    )?;
    let prompt = decode_prompt_field(value, "after")?;
    let Some(after_seconds) = value.get("afterSeconds").and_then(Value::as_u64) else {
        return Err(ScheduleLogError("afterSeconds must be a positive safe integer".into()));
    };
    if after_seconds == 0 || after_seconds > MAX_SAFE_INTEGER {
        return Err(ScheduleLogError("afterSeconds must be a positive safe integer".into()));
    }
    Ok(AfterScheduleRecord {
        id: decode_id_field(value)?,
        title,
        prompt,
        after_seconds,
        scheduled_at: decode_scheduled_at_field(value)?,
    })
}

/// 解码绝对一次性耐久记录（上游 `decodeAtRecord`）。
pub fn decode_at_record(value: &Map<String, Value>) -> Result<AtScheduleRecord, ScheduleLogError> {
    let title = decode_record_title(
        value,
        &["id", "kind", "title", "prompt", "scheduledAt"],
        "at schedule must contain exactly id, kind, title, prompt, and scheduledAt",
    )?;
    let prompt = decode_prompt_field(value, "at")?;
    Ok(AtScheduleRecord {
        id: decode_id_field(value)?,
        title,
        prompt,
        scheduled_at: decode_scheduled_at_field(value)?,
    })
}

/// 解码固定间隔耐久记录（上游 `decodeEveryRecord`）。
pub fn decode_every_record(value: &Map<String, Value>) -> Result<EveryScheduleRecord, ScheduleLogError> {
    let title = decode_record_title(
        value,
        &["id", "kind", "title", "prompt", "everySeconds", "scheduledAt"],
        "every schedule must contain exactly id, kind, title, prompt, everySeconds, and scheduledAt",
    )?;
    let prompt = decode_prompt_field(value, "every")?;
    let Some(every_seconds) = value.get("everySeconds").and_then(Value::as_u64) else {
        return Err(ScheduleLogError(format!(
            "everySeconds must be a safe integer of at least {MIN_EVERY_INTERVAL_SECONDS}"
        )));
    };
    if every_seconds < MIN_EVERY_INTERVAL_SECONDS || every_seconds > MAX_SAFE_INTEGER {
        return Err(ScheduleLogError(format!(
            "everySeconds must be a safe integer of at least {MIN_EVERY_INTERVAL_SECONDS}"
        )));
    }
    Ok(EveryScheduleRecord {
        id: decode_id_field(value)?,
        title,
        prompt,
        every_seconds,
        scheduled_at: decode_scheduled_at_field(value)?,
    })
}

/// 解码 daily 耐久规则，不重解已承诺目标（上游 `decodeDailyRecord`）。
pub fn decode_daily_record(value: &Map<String, Value>) -> Result<DailyScheduleRecord, ScheduleLogError> {
    let title = decode_record_title(
        value,
        &["id", "kind", "title", "prompt", "time", "timeZone", "scheduledAt"],
        "daily schedule must contain exactly id, kind, title, prompt, time, timeZone, and scheduledAt",
    )?;
    let prompt = decode_prompt_field(value, "daily")?;
    let Some(time) = value.get("time").and_then(Value::as_str) else {
        return Err(ScheduleLogError("daily time and timeZone must be strings".into()));
    };
    let Some(time_zone) = value.get("timeZone").and_then(Value::as_str) else {
        return Err(ScheduleLogError("daily time and timeZone must be strings".into()));
    };
    // 上游同一 try 内先归一时间再规范时区，输入错重抛为耐久错误。
    let normalized = local_clock_time(time, "daily").map_err(|error| ScheduleLogError(error.to_string()))?;
    canonicalize_time_zone(time_zone).map_err(|error| ScheduleLogError(error.to_string()))?;
    if time != normalized {
        return Err(ScheduleLogError("daily time must be normalized to HH:mm:ss.SSS".into()));
    }
    Ok(DailyScheduleRecord {
        id: decode_id_field(value)?,
        title,
        prompt,
        time: normalized,
        time_zone: time_zone.into(),
        scheduled_at: decode_scheduled_at_field(value)?,
    })
}

/// 解码 weekly 耐久规则，不重解已承诺目标（上游 `decodeWeeklyRecord`：
/// 先时区后时间；weekdays 与存储逐元素比对）。
pub fn decode_weekly_record(value: &Map<String, Value>) -> Result<WeeklyScheduleRecord, ScheduleLogError> {
    let title = decode_record_title(
        value,
        &["id", "kind", "title", "prompt", "time", "timeZone", "weekdays", "scheduledAt"],
        "weekly schedule must contain exactly id, kind, title, prompt, time, timeZone, weekdays, and scheduledAt",
    )?;
    let prompt = decode_prompt_field(value, "weekly")?;
    let Some(time) = value.get("time").and_then(Value::as_str) else {
        return Err(ScheduleLogError("weekly time and timeZone must be strings".into()));
    };
    let Some(time_zone) = value.get("timeZone").and_then(Value::as_str) else {
        return Err(ScheduleLogError("weekly time and timeZone must be strings".into()));
    };
    canonicalize_time_zone(time_zone).map_err(|error| ScheduleLogError(error.to_string()))?;
    let normalized = local_clock_time(time, "weekly").map_err(|error| ScheduleLogError(error.to_string()))?;
    if time != normalized {
        return Err(ScheduleLogError("weekly time must be normalized to HH:mm:ss.SSS".into()));
    }
    let weekdays = normalize_weekdays(value.get("weekdays").unwrap_or(&Value::Null))
        .map_err(|error| ScheduleLogError(error.to_string()))?;
    // normalize_weekdays 已证明该字段是数值数组，这里与归一结果逐位比对。
    let stored = value.get("weekdays").and_then(Value::as_array);
    let stored_matches = stored.is_some_and(|list| {
        list.len() == weekdays.len()
            && list
                .iter()
                .zip(&weekdays)
                .all(|(entry, weekday)| entry.as_u64() == Some(u64::from(*weekday)))
    });
    if !stored_matches {
        return Err(ScheduleLogError(
            "weekly weekdays must be normalized to unique ascending ISO weekday numbers".into(),
        ));
    }
    Ok(WeeklyScheduleRecord {
        id: decode_id_field(value)?,
        title,
        prompt,
        time: normalized,
        time_zone: time_zone.into(),
        weekdays,
        scheduled_at: decode_scheduled_at_field(value)?,
    })
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn as_map(value: &Value) -> &Map<String, Value> {
        value.as_object().expect("记录序列化恒为对象")
    }

    /// 2026-01-10T08:00:00.000Z（周六）。
    const NOW: i64 = 1_768_032_000_000;

    #[test]
    fn after_record_happy_path_and_messages() {
        let record =
            create_after_schedule_record("t1", " run the report ", 90, NOW, " daily report ").unwrap();
        assert_eq!(record.title, "daily report");
        assert_eq!(record.prompt, "run the report");
        assert_eq!(record.after_seconds, 90);
        assert_eq!(record.scheduled_at, "2026-01-10T08:01:30.000Z");
        // prompt 空。
        let error = create_after_schedule_record("t1", "   ", 90, NOW, "t").unwrap_err();
        assert_eq!(error.message, "prompt must be non-empty after trimming.");
        // 零延迟。
        let error = create_after_schedule_record("t1", "p", 0, NOW, "t").unwrap_err();
        assert_eq!(error.code, ScheduleInputErrorCode::InvalidRule);
        assert_eq!(error.message, "after_seconds must be a positive safe integer.");
        // title 空（在延迟校验之后）。
        let error = create_after_schedule_record("t1", "p", 90, NOW, "  ").unwrap_err();
        assert_eq!(error.message, crate::REQUIRED_TITLE_MESSAGE);
        // 目标越界（安全整数内、9999 年外）。
        let far =
            create_after_schedule_record("t1", "p", 9_007_199_254_740_991, NOW, "t");
        assert_eq!(far.unwrap_err().code, ScheduleInputErrorCode::TimeOutOfRange);
    }

    #[test]
    fn every_record_frequency_floor() {
        let error = create_every_schedule_record("t1", "p", 59, NOW, "t").unwrap_err();
        assert_eq!(error.code, ScheduleInputErrorCode::FrequencyTooHigh);
        assert_eq!(error.message, "every_seconds must be at least 60.");
        let record = create_every_schedule_record("t1", "p", 60, NOW, "t").unwrap();
        assert_eq!(record.scheduled_at, "2026-01-10T08:01:00.000Z");
    }

    #[test]
    fn at_record_offset_and_local_forms() {
        // 显式偏移串：+05:30 → UTC 09:30。
        let record = create_at_schedule_record(
            "t1",
            "p",
            &json!("2026-01-10T15:00:00+05:30"),
            NOW,
            "t",
        )
        .unwrap();
        assert_eq!(record.scheduled_at, "2026-01-10T09:30:00.000Z");
        // 本地日历形态。
        let record = create_at_schedule_record(
            "t1",
            "p",
            &json!({"date": "2026-01-10", "time": "09:00:00", "time_zone": "UTC"}),
            NOW,
            "t",
        )
        .unwrap();
        assert_eq!(record.scheduled_at, "2026-01-10T09:00:00.000Z");
        // 键闭集。
        let error = create_at_schedule_record(
            "t1",
            "p",
            &json!({"date": "2026-01-10", "time": "09:00:00"}),
            NOW,
            "t",
        )
        .unwrap_err();
        assert_eq!(error.message, "Local at must contain exactly date, time, and time_zone.");
        // DST gap 拒绝。
        let error = create_at_schedule_record(
            "t1",
            "p",
            &json!({"date": "2026-03-08", "time": "02:30:00", "time_zone": "America/New_York"}),
            NOW,
            "t",
        )
        .unwrap_err();
        assert_eq!(error.message, "The local at time does not exist in the selected time zone.");
        // 非串非对象。
        let error = create_at_schedule_record("t1", "p", &json!(42), NOW, "t").unwrap_err();
        assert_eq!(error.message, "at must be an explicit-offset string or local calendar object.");
    }

    #[test]
    fn daily_record_normalizes_and_commits() {
        let record = create_daily_schedule_record(
            "t1",
            "p",
            &json!({"time": "09:30:00", "time_zone": "UTC"}),
            NOW,
            "t",
        )
        .unwrap();
        assert_eq!(record.time, "09:30:00.000");
        assert_eq!(record.scheduled_at, "2026-01-10T09:30:00.000Z");
        // 年内无未来目标（jiff 上界尾段）——且不 panic。
        let now_max = crate::occurrence::max_four_digit_year_ms();
        let error = create_daily_schedule_record(
            "t1",
            "p",
            &json!({"time": "23:59:59", "time_zone": "UTC"}),
            now_max,
            "t",
        )
        .unwrap_err();
        assert_eq!(
            error.message,
            "No future daily occurrence is representable as a four-digit-year UTC instant."
        );
    }

    #[test]
    fn weekly_record_orders_weekdays() {
        let record = create_weekly_schedule_record(
            "t1",
            "p",
            &json!({"time": "09:30:00", "time_zone": "UTC", "weekdays": [3, 1]}),
            NOW,
            "t",
        )
        .unwrap();
        assert_eq!(record.weekdays, vec![1, 3]);
        // 决策周六 1/10：下一匹配周一 1/12。
        assert_eq!(record.scheduled_at, "2026-01-12T09:30:00.000Z");
    }

    #[test]
    fn after_decode_requires_exact_keys() {
        // 走 ScheduleRecord 枚举序列化——wire 形才带 kind 标签。
        let value = serde_json::to_value(crate::ScheduleRecord::After(
            create_after_schedule_record("t1", "p", 90, NOW, "t").unwrap(),
        ))
        .unwrap();
        let decoded = decode_after_record(as_map(&value)).unwrap();
        assert_eq!(decoded.id, "t1");
        // 额外键拒。
        let mut padded = value;
        padded
            .as_object_mut()
            .unwrap()
            .insert("extra".into(), json!(1));
        let error = decode_after_record(as_map(&padded)).unwrap_err();
        assert_eq!(
            error.0,
            "after schedule must contain exactly id, kind, title, prompt, afterSeconds, and scheduledAt"
        );
        // 缺键拒（title 诊断先行）。
        padded.as_object_mut().unwrap().remove("title");
        let error = decode_after_record(as_map(&padded)).unwrap_err();
        assert_eq!(error.0, crate::REQUIRED_TITLE_MESSAGE);
    }

    #[test]
    fn after_decode_rejects_zero_delay() {
        let mut value = serde_json::to_value(crate::ScheduleRecord::After(
            create_after_schedule_record("t1", "p", 90, NOW, "t").unwrap(),
        ))
        .unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("afterSeconds".into(), json!(0));
        let error = decode_after_record(as_map(&value)).unwrap_err();
        assert_eq!(error.0, "afterSeconds must be a positive safe integer");
    }

    #[test]
    fn at_and_every_decode_prompt_and_floor() {
        let value = serde_json::to_value(crate::ScheduleRecord::At(
            create_at_schedule_record("t1", "p", &json!("2026-01-10T09:00:00Z"), NOW, "t").unwrap(),
        ))
        .unwrap();
        assert!(decode_at_record(as_map(&value)).is_ok());
        let mut padded = value;
        padded
            .as_object_mut()
            .unwrap()
            .insert("prompt".into(), json!(" padded "));
        let error = decode_at_record(as_map(&padded)).unwrap_err();
        assert_eq!(error.0, "at prompt must be non-empty and already trimmed");

        let value = serde_json::to_value(crate::ScheduleRecord::Every(
            create_every_schedule_record("t1", "p", 120, NOW, "t").unwrap(),
        ))
        .unwrap();
        assert!(decode_every_record(as_map(&value)).is_ok());
        let mut low = value;
        low.as_object_mut()
            .unwrap()
            .insert("everySeconds".into(), json!(30));
        let error = decode_every_record(as_map(&low)).unwrap_err();
        assert_eq!(error.0, "everySeconds must be a safe integer of at least 60");
    }

    #[test]
    fn daily_decode_requires_normalized_time_and_zone() {
        let value = serde_json::to_value(crate::ScheduleRecord::Daily(
            create_daily_schedule_record(
                "t1",
                "p",
                &json!({"time": "09:30:00", "time_zone": "UTC"}),
                NOW,
                "t",
            )
            .unwrap(),
        ))
        .unwrap();
        assert!(decode_daily_record(as_map(&value)).is_ok());
        // 未归一时间拒（过正则但非 .SSS 归一形）。
        let mut raw = value;
        raw.as_object_mut()
            .unwrap()
            .insert("time".into(), json!("09:30:00.5"));
        let error = decode_daily_record(as_map(&raw)).unwrap_err();
        assert_eq!(error.0, "daily time must be normalized to HH:mm:ss.SSS");
        // 构造端坏时区在 parse_daily_input 即拒。
        let record = create_daily_schedule_record(
            "t1",
            "p",
            &json!({"time": "09:30:00", "time_zone": "Mars/Olympus"}),
            NOW,
            "t",
        );
        assert!(record.is_err());
    }

    #[test]
    fn weekly_decode_requires_ascending_normalized_weekdays() {
        let value = serde_json::to_value(crate::ScheduleRecord::Weekly(
            create_weekly_schedule_record(
                "t1",
                "p",
                &json!({"time": "09:30:00", "time_zone": "UTC", "weekdays": [1, 3]}),
                NOW,
                "t",
            )
            .unwrap(),
        ))
        .unwrap();
        assert!(decode_weekly_record(as_map(&value)).is_ok());
        // 存储乱序拒。
        let mut shuffled = value;
        shuffled
            .as_object_mut()
            .unwrap()
            .insert("weekdays".into(), json!([3, 1]));
        let error = decode_weekly_record(as_map(&shuffled)).unwrap_err();
        assert_eq!(
            error.0,
            "weekly weekdays must be normalized to unique ascending ISO weekday numbers"
        );
    }

    #[test]
    fn time_zone_normalizes_case_to_tzdb_spelling() {
        // jiff iana_name 回 tzdb 规范大小写（上游 Intl 同款）。登记偏差：
        // jiff-tzdb 把别名（如 Asia/Calcutta）存为独立条目，不回 tzdb
        // 主名（上游 Intl 会回 Asia/Kolkata）——功能等价，仅落盘串形。
        assert_eq!(crate::canonicalize_time_zone("asia/kolkata").unwrap(), "Asia/Kolkata");
        assert_eq!(crate::canonicalize_time_zone("Asia/Calcutta").unwrap(), "Asia/Calcutta");
        assert_eq!(crate::canonicalize_time_zone("UTC").unwrap(), "UTC");
    }
}

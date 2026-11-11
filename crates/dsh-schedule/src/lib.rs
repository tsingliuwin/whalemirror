//! 定时任务域（A1.4 批①）：类型与校验纯面。
//!
//! 对齐 [`packages/schedule/schedule/src/types.ts`](https://github.com/deepseek-ai/deepseek-harness/blob/main/packages/schedule/schedule/src/types.ts)
//! 与 `domain.ts` 的校验函数：七种 timing 记录（after/at/every/daily/weekly/
//! cron/rrule）、错误码闭集、title/prompt 校验、canonical 时区解析、
//! 未来时刻校验。cron 与 rrule 的规则引擎（`dsh-util-time` 的 Cron/RRule）
//! 在批② 落——本批只做记录形态与通用校验，不评估 occurrence。

mod cron;
mod occurrence;
mod parse;
mod record;
mod recurrence;
mod rrule;
mod service;
mod store;
mod task;
mod update;

pub use cron::{Cron, CronFields, CRON_SEARCH_HORIZON_YEARS};
pub use rrule::{RRule, MAX_PERIOD_CANDIDATES, MAX_RRULE_COUNT, MAX_SET_POSITIONS, MIN_RRULE_YEAR};
pub use service::{
    advanced, append_delivery, batch_session_of, catalog_entry, compare_entries, delivered,
    delivery_floor_end, delivery_history_page, format_task_local_time, involved,
    new_reserved_session_id, new_schedule_id, next_admission_at, resolve_list_page,
    resolve_recurring_occurrence, revised_at, revised_task, render_delivery_text,
    DeliveryRetentionBounds, ScheduleCatalogEntry, ScheduleEditAction, ScheduleEditRequest,
    ScheduleHistoryPageError, SchedulePatchRequest, ScheduleRuntimeLimits,
    DEFAULT_DELIVERY_HISTORY_DAYS, DEFAULT_DELIVERY_HISTORY_RECORDS, DEFAULT_LIST_LIMIT,
    DEFAULT_NEW_SESSION_INTERVAL_SECONDS, MAX_LIST_LIMIT, SUBAGENT_SESSION_REFUSAL,
};
pub use store::{
    ScheduleListing, ScheduleStore, ScheduleStoreError, SCHEDULE_COMPATIBLE_VERSIONS,
    SCHEDULE_DOMAIN_VERSION,
};
pub use task::{
    decode_schedule_task, default_session_id, CreateSpec, DeliveryHistory, DeliveryHistoryRecord,
    DeliveryReceipt, PendingDelivery, ScheduleTarget, ScheduleTask, ScheduleTaskStatus,
};
pub use update::{
    create_schedule_record, normalized_timing, timing_of, update_schedule_record, validate_timing,
    SchedulePatch, ScheduleTiming,
};
pub use occurrence::{
    RecurringOccurrence, max_four_digit_year_ms, min_four_digit_year_ms, next_daily_target,
    next_weekly_target, resolve_daily_occurrence, resolve_every_occurrence,
    resolve_weekly_occurrence,
};
pub use parse::{InstantRejection, LocalCalendarRejection, civil_to_epoch_ms, parse_local_calendar, parse_offset_instant};
pub use record::{
    canonicalize_cron_expression, create_after_schedule_record, create_at_schedule_record,
    create_cron_schedule_record, create_daily_schedule_record, create_every_schedule_record,
    create_rrule_schedule_record, create_weekly_schedule_record, decode_after_record,
    decode_at_record, decode_cron_record, decode_daily_record, decode_every_record,
    decode_rrule_record, decode_schedule_record, decode_weekly_record, normalize_rrule,
    parse_at_input, parse_cron_input, parse_daily_input, parse_weekly_input,
};

use serde::{Deserialize, Serialize};

/// 最小 every 间隔（秒；上游 `MIN_EVERY_INTERVAL_SECONDS`）。
pub const MIN_EVERY_INTERVAL_SECONDS: u64 = 60;

/// title 长度上限（上游 `MAX_TITLE_LENGTH`）。
pub const MAX_TITLE_LENGTH: usize = 120;

/// title 必填诊断（上游 `REQUIRED_TITLE_MESSAGE` 逐字）。
pub const REQUIRED_TITLE_MESSAGE: &str = "title is required and must be non-empty after trimming.";

/// 输入错误码闭集（上游 `ScheduleInputErrorCode`——管理错误去掉 internal_error）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScheduleInputErrorCode {
    InvalidPrompt,
    InvalidSelector,
    InvalidRule,
    InvalidTimeZone,
    NotFuture,
    TimeOutOfRange,
    FrequencyTooHigh,
    SubagentSession,
}

impl ScheduleInputErrorCode {
    /// wire 码（上游小写蛇形）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::InvalidPrompt => "invalid_prompt",
            Self::InvalidSelector => "invalid_selector",
            Self::InvalidRule => "invalid_rule",
            Self::InvalidTimeZone => "invalid_time_zone",
            Self::NotFuture => "not_future",
            Self::TimeOutOfRange => "time_out_of_range",
            Self::FrequencyTooHigh => "frequency_too_high",
            Self::SubagentSession => "subagent_session",
        }
    }
}

/// 输入失败：稳定 code + 文案（上游 `ScheduleInputError`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduleInputError {
    pub code: ScheduleInputErrorCode,
    pub message: String,
}

impl ScheduleInputError {
    pub fn new(code: ScheduleInputErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
}

impl std::fmt::Display for ScheduleInputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ScheduleInputError {}

/// 耐久数据损坏错误（上游 `ScheduleLogError`，code 恒 corrupt_schedule_log）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduleLogError(pub String);

impl std::fmt::Display for ScheduleLogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ScheduleLogError {}

/// 一次性提醒：正延迟（上游 `AfterScheduleRecord`）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AfterScheduleRecord {
    pub id: String,
    pub title: String,
    pub prompt: String,
    /// 正安全整数延迟（秒）。
    #[serde(rename = "afterSeconds")]
    pub after_seconds: u64,
    /// 四位数年 RFC 3339 UTC 目标。
    #[serde(rename = "scheduledAt")]
    pub scheduled_at: String,
}

/// 一次性提醒：绝对时刻（上游 `AtScheduleRecord`）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtScheduleRecord {
    pub id: String,
    pub title: String,
    pub prompt: String,
    #[serde(rename = "scheduledAt")]
    pub scheduled_at: String,
}

/// 固定间隔提醒（上游 `EveryScheduleRecord`）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EveryScheduleRecord {
    pub id: String,
    pub title: String,
    pub prompt: String,
    /// 恒 ≥ 60 秒。
    #[serde(rename = "everySeconds")]
    pub every_seconds: u64,
    #[serde(rename = "scheduledAt")]
    pub scheduled_at: String,
}

/// 每日墙钟提醒（上游 `DailyScheduleRecord`；time 归一 HH:mm:ss.SSS）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DailyScheduleRecord {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub time: String,
    #[serde(rename = "timeZone")]
    pub time_zone: String,
    #[serde(rename = "scheduledAt")]
    pub scheduled_at: String,
}

/// 每周墙钟提醒（上游 `WeeklyScheduleRecord`；weekdays 唯一升序 ISO 1-7）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeeklyScheduleRecord {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub time: String,
    #[serde(rename = "timeZone")]
    pub time_zone: String,
    pub weekdays: Vec<u8>,
    #[serde(rename = "scheduledAt")]
    pub scheduled_at: String,
}

/// 五字段 cron 提醒（上游 `CronScheduleRecord`；批② 落规则引擎）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CronScheduleRecord {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub expression: String,
    #[serde(rename = "timeZone")]
    pub time_zone: String,
    #[serde(rename = "scheduledAt")]
    pub scheduled_at: String,
}

/// RFC 5545 提醒（上游 `RRuleScheduleRecord`；批② 落规则引擎）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RRuleScheduleRecord {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub rrule: String,
    #[serde(rename = "scheduledAt")]
    pub scheduled_at: String,
}

/// 七种 timing 记录闭集（上游 `ScheduleRecord`）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ScheduleRecord {
    After(AfterScheduleRecord),
    At(AtScheduleRecord),
    Every(EveryScheduleRecord),
    Daily(DailyScheduleRecord),
    Weekly(WeeklyScheduleRecord),
    Cron(CronScheduleRecord),
    RRule(RRuleScheduleRecord),
}

impl ScheduleRecord {
    /// 记录身份。
    pub fn id(&self) -> &str {
        match self {
            Self::After(r) => &r.id,
            Self::At(r) => &r.id,
            Self::Every(r) => &r.id,
            Self::Daily(r) => &r.id,
            Self::Weekly(r) => &r.id,
            Self::Cron(r) => &r.id,
            Self::RRule(r) => &r.id,
        }
    }

    /// 承诺的下一时刻（或终态最后一现）。
    pub fn scheduled_at(&self) -> &str {
        match self {
            Self::After(r) => &r.scheduled_at,
            Self::At(r) => &r.scheduled_at,
            Self::Every(r) => &r.scheduled_at,
            Self::Daily(r) => &r.scheduled_at,
            Self::Weekly(r) => &r.scheduled_at,
            Self::Cron(r) => &r.scheduled_at,
            Self::RRule(r) => &r.scheduled_at,
        }
    }

    /// title（七形态共字段）。
    pub fn title(&self) -> &str {
        match self {
            Self::After(r) => &r.title,
            Self::At(r) => &r.title,
            Self::Every(r) => &r.title,
            Self::Daily(r) => &r.title,
            Self::Weekly(r) => &r.title,
            Self::Cron(r) => &r.title,
            Self::RRule(r) => &r.title,
        }
    }

    /// 提醒内容（七形态共字段）。
    pub fn prompt(&self) -> &str {
        match self {
            Self::After(r) => &r.prompt,
            Self::At(r) => &r.prompt,
            Self::Every(r) => &r.prompt,
            Self::Daily(r) => &r.prompt,
            Self::Weekly(r) => &r.prompt,
            Self::Cron(r) => &r.prompt,
            Self::RRule(r) => &r.prompt,
        }
    }

    /// 是否循环规则（上游 `isRecurringScheduleRecord`）。
    pub fn is_recurring(&self) -> bool {
        !matches!(self, Self::After(_) | Self::At(_))
    }

    /// 记录的权威时区（after/at 与 rrule 的时区在规则内，返回 None/
    /// 解析形；上游 `'timeZone' in record` 的服务面）。
    pub fn time_zone(&self) -> Option<&str> {
        match self {
            Self::Daily(r) => Some(&r.time_zone),
            Self::Weekly(r) => Some(&r.time_zone),
            Self::Cron(r) => Some(&r.time_zone),
            _ => None,
        }
    }

    /// 替换已承诺目标（runtime 前进与编辑重算用）。
    pub fn set_scheduled_at(&mut self, scheduled_at: String) {
        match self {
            Self::After(r) => r.scheduled_at = scheduled_at,
            Self::At(r) => r.scheduled_at = scheduled_at,
            Self::Every(r) => r.scheduled_at = scheduled_at,
            Self::Daily(r) => r.scheduled_at = scheduled_at,
            Self::Weekly(r) => r.scheduled_at = scheduled_at,
            Self::Cron(r) => r.scheduled_at = scheduled_at,
            Self::RRule(r) => r.scheduled_at = scheduled_at,
        }
    }

    /// 替换描述字段（编辑保留分支用）。
    pub fn set_title_prompt(&mut self, title: impl Into<String>, prompt: impl Into<String>) {
        match self {
            Self::After(r) => {
                r.title = title.into();
                r.prompt = prompt.into();
            }
            Self::At(r) => {
                r.title = title.into();
                r.prompt = prompt.into();
            }
            Self::Every(r) => {
                r.title = title.into();
                r.prompt = prompt.into();
            }
            Self::Daily(r) => {
                r.title = title.into();
                r.prompt = prompt.into();
            }
            Self::Weekly(r) => {
                r.title = title.into();
                r.prompt = prompt.into();
            }
            Self::Cron(r) => {
                r.title = title.into();
                r.prompt = prompt.into();
            }
            Self::RRule(r) => {
                r.title = title.into();
                r.prompt = prompt.into();
            }
        }
    }
}

/// 校验创建时的 title（上游 `scheduleTitle` 逐字）。
pub fn schedule_title(title: &str) -> Result<String, ScheduleInputError> {
    if title.trim().is_empty() {
        return Err(ScheduleInputError::new(
            ScheduleInputErrorCode::InvalidPrompt,
            REQUIRED_TITLE_MESSAGE,
        ));
    }
    let normalized = title.trim();
    if normalized.chars().count() > MAX_TITLE_LENGTH {
        return Err(ScheduleInputError::new(
            ScheduleInputErrorCode::InvalidPrompt,
            format!("title must be at most {MAX_TITLE_LENGTH} characters."),
        ));
    }
    Ok(normalized.to_string())
}

/// 耐久边界校验已存 title（上游 `decodeStoredTitle` 逐字——不归一只报错）。
pub fn decode_stored_title(value: &str) -> Result<String, ScheduleLogError> {
    if value.trim().is_empty() {
        return Err(ScheduleLogError(REQUIRED_TITLE_MESSAGE.into()));
    }
    if value.chars().count() > MAX_TITLE_LENGTH {
        return Err(ScheduleLogError(format!(
            "title must be at most {MAX_TITLE_LENGTH} characters"
        )));
    }
    if value.trim() != value {
        return Err(ScheduleLogError("title must be a trimmed string".into()));
    }
    Ok(value.to_string())
}

/// 耐久边界校验任务 id（上游 `decodeId` 逐字）。
pub fn decode_id(value: &str) -> Result<String, ScheduleLogError> {
    if value.is_empty() || value.trim() != value {
        return Err(ScheduleLogError(
            "schedule id must be a non-empty string without surrounding whitespace".into(),
        ));
    }
    Ok(value.to_string())
}

/// 解析并规范化一个 IANA 时区名（上游 `canonicalizeTimeZone`）。
///
/// 空、带空白边、非 `Area/Location` 形（缩写/数字偏移/单段名）前置拒绝；
/// `UTC` 恒合法；别名返回 tzdb 规范拼写（上游 `Intl.resolvedOptions`
/// 同语义——jiff `iana_name()` 给规范名）。
pub fn canonicalize_time_zone(value: &str) -> Result<String, ScheduleInputError> {
    if value.is_empty()
        || value.trim() != value
        || (value != "UTC" && !parse::iana_zone_shape(value))
    {
        return Err(time_zone_error());
    }
    match jiff::tz::TimeZone::get(value) {
        Ok(zone) => Ok(zone
            .iana_name()
            .map(str::to_string)
            .unwrap_or_else(|| value.to_string())),
        Err(_) => Err(time_zone_error()),
    }
}

/// 上游 `time_zone must be UTC or a valid IANA Area/Location name.` 逐字。
fn time_zone_error() -> ScheduleInputError {
    ScheduleInputError::new(
        ScheduleInputErrorCode::InvalidTimeZone,
        "time_zone must be UTC or a valid IANA Area/Location name.",
    )
}

/// 解析 canonical RFC 3339 UTC 时刻（上游 `decodeInstant`——必须 Z 结尾、
/// 四位数年、恒 3 位毫秒）。
///
/// 拒绝分支逐字：形不合法（含 0000 年）→ canonical 诊断；形过而日期
/// 不存在（2 月 30）→ `not a real UTC calendar instant` 诊断。形与日期
/// 都过但 jiff 不可表示（9999 尾段，登记偏差）→ canonical 诊断。
pub fn decode_instant(value: &str) -> Result<String, ScheduleLogError> {
    let non_canonical = || ScheduleLogError(record::NON_CANONICAL_INSTANT_MESSAGE.into());
    if !parse::matches_utc_instant_shape(value) {
        return Err(non_canonical());
    }
    let year: i64 = value[0..4].parse().expect("形已校验数位");
    let month: i64 = value[5..7].parse().expect("形已校验数位");
    let day: i64 = value[8..10].parse().expect("形已校验数位");
    if jiff::civil::Date::new(year as i16, month as i8, day as i8).is_err() {
        return Err(ScheduleLogError("scheduledAt is not a real UTC calendar instant".into()));
    }
    let parsed: Result<jiff::Timestamp, _> = value.parse();
    let Ok(parsed) = parsed else { return Err(non_canonical()) };
    // canonical 形 = 毫秒恒 3 位的 Z 形（防御性重格式化对比）。
    if format_instant(parsed.as_millisecond()) != value {
        return Err(non_canonical());
    }
    Ok(value.to_string())
}

/// epoch 毫秒 → canonical RFC 3339 UTC 串（恒 3 位毫秒、Z 后缀——上游
/// `toISOString()` 形）。
pub fn format_instant(epoch_ms: i64) -> String {
    let timestamp = jiff::Timestamp::from_millisecond(epoch_ms)
        .expect("epoch 毫秒在 jiff 支持范围（调用方已限四位数年）");
    let utc = timestamp.to_zoned(jiff::tz::TimeZone::UTC);
    let (hour, minute, second, millisecond) = (
        utc.hour(),
        utc.minute(),
        utc.second(),
        utc.millisecond(),
    );
    let date = utc.date();
    format!(
        "{:04}-{:02}-{:02}T{hour:02}:{minute:02}:{second:02}.{millisecond:03}Z",
        date.year(),
        date.month() as u8,
        date.day()
    )
}

/// epoch 毫秒是否可用四位数年表示（上游 `isFourDigitYearInstant`）。
pub fn is_four_digit_year_instant(epoch_ms: i64) -> bool {
    jiff::Timestamp::from_millisecond(epoch_ms).is_ok()
}

/// RFC 3339 串 → epoch 毫秒（上游 `Date.parse` 的 rustdsh 等价入口；
/// 宿主面不直接依赖 jiff）。
pub fn instant_epoch_ms(text: &str) -> Option<i64> {
    text.parse::<jiff::Timestamp>().ok().map(|timestamp| timestamp.as_millisecond())
}

/// 校验一个严格未来的 UTC 目标（上游 `futureInstant` 逐字）。
///
/// 返回 canonical 串；越界或过去都报错。
pub fn future_instant(epoch_ms: i64, now_ms: i64) -> Result<String, ScheduleInputError> {
    if !is_four_digit_year_instant(epoch_ms) {
        return Err(ScheduleInputError::new(
            ScheduleInputErrorCode::TimeOutOfRange,
            "The scheduled time must be representable as a four-digit-year RFC 3339 UTC instant.",
        ));
    }
    if epoch_ms <= now_ms {
        return Err(ScheduleInputError::new(
            ScheduleInputErrorCode::NotFuture,
            "The scheduled time must be strictly in the future.",
        ));
    }
    Ok(format_instant(epoch_ms))
}

/// 校验提示内容（上游各 create* 的 prompt 分支逐字）。
pub fn schedule_prompt(prompt: &str) -> Result<String, ScheduleInputError> {
    let trimmed = prompt.trim();
    if trimmed.is_empty() {
        return Err(ScheduleInputError::new(
            ScheduleInputErrorCode::InvalidPrompt,
            "prompt must be non-empty after trimming.",
        ));
    }
    Ok(trimmed.to_string())
}

/// 本地墙钟时刻串校验（上游 `LOCAL_CLOCK_TIME`：HH:mm:ss 可带 1-3 位小数）。
///
/// 返回归一 HH:mm:ss.SSS（上游 `toString({fractionalSecondDigits: 3})`）。
pub fn local_clock_time(value: &str, selector: &str) -> Result<String, ScheduleInputError> {
    let (hh, rest) = value.split_once(':').unwrap_or(("", ""));
    let (mm, ss) = rest.split_once(':').unwrap_or(("", ""));
    let valid_hours = hh.len() == 2 && hh.parse::<u8>().is_ok_and(|h| h < 24);
    let valid_minutes = mm.len() == 2 && mm.parse::<u8>().is_ok_and(|m| m < 60);
    let (seconds, millis) = match ss.split_once('.') {
        Some((s, frac)) => (s, Some(frac)),
        None => (ss, None),
    };
    let valid_seconds = seconds.len() == 2 && seconds.parse::<u8>().is_ok_and(|s| s < 60);
    let valid_millis = match millis {
        None => true,
        Some(frac) => (1..=3).contains(&frac.len()) && frac.bytes().all(|b| b.is_ascii_digit()),
    };
    if !valid_hours || !valid_minutes || !valid_seconds || !valid_millis {
        return Err(ScheduleInputError::new(
            ScheduleInputErrorCode::InvalidRule,
            format!(
                "{selector}.time must use HH:mm:ss with optional 1-3 fractional digits, without leap seconds or 24:00."
            ),
        ));
    }
    let millis = match millis {
        Some(frac) => format!("{:0<3}", frac).chars().take(3).collect::<String>(),
        None => "000".to_string(),
    };
    Ok(format!("{hh}:{mm}:{seconds}.{millis}"))
}

/// 归一化周几集合（上游 `normalizeWeekdays` 逐字文案；收原始 JSON 值，
/// 非数组与空数组同报非空数组诊断）。
pub fn normalize_weekdays(weekdays: &serde_json::Value) -> Result<Vec<u8>, ScheduleInputError> {
    let empty_error = || {
        ScheduleInputError::new(
            ScheduleInputErrorCode::InvalidRule,
            "weekly.weekdays must be a non-empty array of ISO weekday numbers.",
        )
    };
    let Some(list) = weekdays.as_array() else {
        return Err(empty_error());
    };
    if list.is_empty() {
        return Err(empty_error());
    }
    let mut unique = std::collections::BTreeSet::new();
    for weekday in list {
        let Some(number) = weekday.as_u64() else {
            return Err(ScheduleInputError::new(
                ScheduleInputErrorCode::InvalidRule,
                "Each weekly.weekdays entry must be an integer from 1 (Monday) through 7 (Sunday).",
            ));
        };
        if !(1..=7).contains(&number) {
            return Err(ScheduleInputError::new(
                ScheduleInputErrorCode::InvalidRule,
                "Each weekly.weekdays entry must be an integer from 1 (Monday) through 7 (Sunday).",
            ));
        }
        let value = number as u8;
        if unique.contains(&value) {
            return Err(ScheduleInputError::new(
                ScheduleInputErrorCode::InvalidRule,
                format!("weekly.weekdays must not repeat weekday {value}."),
            ));
        }
        unique.insert(value);
    }
    Ok(unique.into_iter().collect())
}

/// 渲染一次性提醒的模型可见框架（上游 `renderReminderFraming` 逐字）。
pub fn render_reminder_framing(id: &str, prompt: &str, scheduled_at: &str) -> String {
    [
        "[SCHEDULE REMINDER]",
        "This is a scheduled message from the user",
        &format!("schedule_id_json: {}", serde_json::json!(id)),
        &format!("occurrence_at: {scheduled_at}"),
        &format!("reminder_prompt_json: {}", serde_json::json!(prompt)),
    ]
    .join("\n")
}

/// 渲染循环提醒批量框架（上游 `renderRecurringReminderBatchFraming` 逐字）。
pub fn render_recurring_reminder_batch_framing(
    reminders: &[(String, String, String)],
) -> String {
    let payload: Vec<serde_json::Value> = reminders
        .iter()
        .map(|(id, occurrence_at, prompt)| {
            serde_json::json!({
                "schedule_id": id,
                "occurrence_at": occurrence_at,
                "reminder_prompt": prompt,
            })
        })
        .collect();
    [
        "[SCHEDULE REMINDER BATCH]",
        "This is a scheduled message from the user",
        &format!("reminders_json: {}", serde_json::json!(payload)),
    ]
    .join("\n")
}

/// 在记录的权威时区渲染时刻（上游 `formatScheduleLocalTime`：
/// `toString({timeZoneName:'never', smallestUnit:'second'})`——秒精度、
/// 数字偏移、无区域注记，如 `2026-01-10T17:00:00+08:00`）。
pub fn format_schedule_local_time(time_zone: &str, instant: &str) -> String {
    let timestamp: jiff::Timestamp = instant.parse().unwrap_or_default();
    let zoned = match jiff::tz::TimeZone::get(time_zone) {
        Ok(zone) => timestamp.to_zoned(zone),
        // 未知时区防御性回退 UTC（调用方已校验时区为前提）。
        Err(_) => timestamp.to_zoned(jiff::tz::TimeZone::UTC),
    };
    jiff::fmt::strtime::format("%Y-%m-%dT%H:%M:%S%:z", &zoned)
        .unwrap_or_else(|_| zoned.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_validation_is_verbatim() {
        assert_eq!(schedule_title("  daily report  ").unwrap(), "daily report");
        let error = schedule_title("   ").unwrap_err();
        assert_eq!(error.code, ScheduleInputErrorCode::InvalidPrompt);
        assert_eq!(error.message, REQUIRED_TITLE_MESSAGE);
        let long = "x".repeat(121);
        let error = schedule_title(&long).unwrap_err();
        assert_eq!(error.message, "title must be at most 120 characters.");
    }

    #[test]
    fn stored_title_rejects_untrimmed() {
        assert!(decode_stored_title("ok").is_ok());
        assert!(decode_stored_title(" padded ").is_err());
        assert!(decode_stored_title("").is_err());
    }

    #[test]
    fn id_rejects_whitespace_padding() {
        assert!(decode_id("abc").is_ok());
        assert!(decode_id(" abc").is_err());
        assert!(decode_id("").is_err());
    }

    #[test]
    fn instant_validation_requires_canonical_form() {
        assert!(decode_instant("2026-01-02T03:04:05.000Z").is_ok());
        // 偏移形式非 canonical。
        assert!(decode_instant("2026-01-02T03:04:05.000+00:00").is_err());
        // 秒精度非 canonical。
        assert!(decode_instant("2026-01-02T03:04:05Z").is_err());
        // 非法串。
        assert!(decode_instant("nonsense").is_err());
        // 零年。
        assert!(decode_instant("0000-01-02T03:04:05.000Z").is_err());
    }

    #[test]
    fn future_instant_rejects_past_and_out_of_range() {
        let now = 1_762_700_000_000i64;
        let error = future_instant(now - 1, now).unwrap_err();
        assert_eq!(error.code, ScheduleInputErrorCode::NotFuture);
        assert_eq!(error.message, "The scheduled time must be strictly in the future.");
        // 同刻不算未来。
        assert!(future_instant(now, now).is_err());
        // 四位数年范围外。
        let error = future_instant(640_000_000_000_000, now).unwrap_err();
        assert_eq!(error.code, ScheduleInputErrorCode::TimeOutOfRange);
        // 正常未来（now=14:53:20Z → +60s = 14:54:20Z）。
        assert_eq!(
            future_instant(now + 60_000, now).unwrap(),
            "2025-11-09T14:54:20.000Z"
        );
    }

    #[test]
    fn local_clock_time_normalizes_to_millis() {
        assert_eq!(local_clock_time("09:30:00", "daily").unwrap(), "09:30:00.000");
        assert_eq!(local_clock_time("23:59:59.5", "daily").unwrap(), "23:59:59.500");
        assert_eq!(local_clock_time("00:00:00.123", "weekly").unwrap(), "00:00:00.123");
        // 24:00 与 60 秒拒绝。
        assert!(local_clock_time("24:00:00", "daily").is_err());
        assert!(local_clock_time("09:30:60", "daily").is_err());
        // 四位小数拒绝。
        assert!(local_clock_time("09:30:00.1234", "daily").is_err());
        // 缺秒拒绝。
        assert!(local_clock_time("09:30", "daily").is_err());
        // 文案逐字（带 selector 前缀）。
        let error = local_clock_time("9:30", "daily").unwrap_err();
        assert_eq!(
            error.message,
            "daily.time must use HH:mm:ss with optional 1-3 fractional digits, without leap seconds or 24:00."
        );
    }

    #[test]
    fn weekdays_normalize_unique_ascending() {
        let error = normalize_weekdays(&serde_json::json!([3, 1, 5, 1])).unwrap_err();
        assert_eq!(error.message, "weekly.weekdays must not repeat weekday 1.");
        assert_eq!(normalize_weekdays(&serde_json::json!([7, 1, 3])).unwrap(), vec![1, 3, 7]);
        // 0 与 8 拒绝。
        assert!(normalize_weekdays(&serde_json::json!([0])).is_err());
        assert!(normalize_weekdays(&serde_json::json!([8])).is_err());
        // 空集与非数组同诊断。
        assert!(normalize_weekdays(&serde_json::json!([])).is_err());
        assert!(normalize_weekdays(&serde_json::json!("weekdays")).is_err());
    }

    #[test]
    fn time_zone_canonicalization() {
        assert!(canonicalize_time_zone("UTC").is_ok());
        assert!(canonicalize_time_zone("Asia/Shanghai").is_ok());
        let error = canonicalize_time_zone("Mars/Olympus").unwrap_err();
        assert_eq!(
            error.message,
            "time_zone must be UTC or a valid IANA Area/Location name."
        );
    }

    #[test]
    fn reminder_framing_is_verbatim() {
        let text = render_reminder_framing("task-1", "run the report", "2026-01-02T03:04:05.000Z");
        assert_eq!(
            text,
            "[SCHEDULE REMINDER]\nThis is a scheduled message from the user\nschedule_id_json: \"task-1\"\noccurrence_at: 2026-01-02T03:04:05.000Z\nreminder_prompt_json: \"run the report\""
        );
    }

    #[test]
    fn batch_framing_is_verbatim() {
        let text = render_recurring_reminder_batch_framing(&[(
            "task-1".into(),
            "2026-01-02T03:04:05.000Z".into(),
            "run".into(),
        )]);
        assert!(text.starts_with(
            "[SCHEDULE REMINDER BATCH]\nThis is a scheduled message from the user\nreminders_json: "
        ));
        assert!(text.contains(r#""schedule_id":"task-1""#));
        assert!(text.contains(r#""occurrence_at":"2026-01-02T03:04:05.000Z""#));
    }
}

//! 有界 RFC 5545 循环（A1.4 批②③）。
//!
//! 上游结构同构：`dsh-util-time` 的 `rrule.ts` = npm `rrule` 包 + 适配
//! 层；本面 = rust `rrule` crate（0.14）+ 适配层。分工一致——**严格
//! 校验、决策语义、DST 换算全在本层（逐字文案），库只做 UTC 形墙钟的
//! 循环算术**。库规则恒以 UTC 构建（DTSTART 的时区不进库），墙钟 →
//! 真实 instant 的换算走 [`resolve_local_instant`]（DST gap 跳过 /
//! overlap 取早），与上游「UTC 形枚举 + resolveLocalInstant」同一套
//! 语义。
//!
//! 库适配差异（登记）：crate 校验要求 DAILY+ 显式 BYHOUR、MINUTELY/
//! HOURLY 显式时间部件——上游 npm 库隐式继承 DTSTART，本层显式注入
//! 同值部件（RFC 5545 时间部件默认继承 DTSTART，语义不变）。

use std::collections::BTreeSet;

use chrono::{DateTime, Month as LibraryMonth, Utc, Weekday as LibraryWeekday};
use rrule::{
    Frequency as LibraryFrequency, NWeekday, RRule as LibraryRuleBuilder, RRuleSet,
};

use crate::{
    canonicalize_time_zone, format_instant, is_four_digit_year_instant,
    occurrence::{max_four_digit_year_ms, resolve_local_instant},
    parse::civil_to_epoch_ms as civil_epoch,
    RecurringOccurrence,
};
use crate::recurrence::{recurrence_decision, RecurrenceInputError, RecurrenceRejection};

/// 最大接受的 COUNT；更长有限循环用 UNTIL（上游 `MAX_RRULE_COUNT`）。
pub const MAX_RRULE_COUNT: u64 = 1_000;

/// 一个 FREQ 周期可生成的最大 occurrence 数：一周内每分钟（上游
/// `MAX_PERIOD_CANDIDATES`）。
pub const MAX_PERIOD_CANDIDATES: u64 = 10_080;

/// BYSETPOS 值数上限（上游 `MAX_SET_POSITIONS`）。
pub const MAX_SET_POSITIONS: usize = 31;

/// DTSTART 与 UNTIL 的最早年份；循环库把 0000-0099 年读作 1900-1999
/// （上游 `MIN_RRULE_YEAR`）。
pub const MIN_RRULE_YEAR: i64 = 100;

const DAY_MS: i64 = 86_400_000;

/// 循环库能生成/读取为 DTSTART 的最晚墙钟（上游 `LAST_RRULE_WALL_MS`）。
const LAST_RRULE_WALL_MS: i64 = civil_epoch(9999, 12, 31, 23, 59, 59, 0);

/// 各 FREQ 在一个 400 年格里高利循环内的周期数；过一循环后周几与闰日
/// 重复（上游 `CYCLE_PERIODS`）。
const CYCLE_PERIODS: [(Frequency, i64); 6] = [
    (Frequency::Minutely, 210_379_680),
    (Frequency::Hourly, 3_506_328),
    (Frequency::Daily, 146_097),
    (Frequency::Weekly, 20_871),
    (Frequency::Monthly, 4_800),
    (Frequency::Yearly, 400),
];

fn cycle_periods(frequency: Frequency) -> i64 {
    CYCLE_PERIODS
        .iter()
        .find(|(key, _)| *key == frequency)
        .map(|(_, periods)| *periods)
        .expect("闭集恒命中")
}

/// 一个 400 年格里高利循环的毫秒长度。
const CYCLE_MS: i64 = 146_097 * DAY_MS;

/// RFC 5545 周几码；序号即库枚举序（周一 0）（上游 `WEEKDAYS`）。
const WEEKDAYS: [&str; 7] = ["MO", "TU", "WE", "TH", "FR", "SA", "SU"];

/// 支持的 RRULE 部件闭集（上游 `SUPPORTED_PARTS` 逐字）。
const SUPPORTED_PARTS: [&str; 14] = [
    "FREQ", "INTERVAL", "COUNT", "UNTIL", "WKST", "BYSECOND", "BYMINUTE", "BYHOUR", "BYDAY",
    "BYMONTHDAY", "BYYEARDAY", "BYWEEKNO", "BYMONTH", "BYSETPOS",
];

/// 一个数值 BY 列表的闭区间（负下限的区间排除 0 由语义检查承担）。
struct ListRange {
    min: i64,
    max: i64,
}

const LIST_RANGES: [(&str, ListRange); 8] = [
    ("BYSECOND", ListRange { min: 0, max: 59 }),
    ("BYMINUTE", ListRange { min: 0, max: 59 }),
    ("BYHOUR", ListRange { min: 0, max: 23 }),
    ("BYMONTHDAY", ListRange { min: -31, max: 31 }),
    ("BYYEARDAY", ListRange { min: -366, max: 366 }),
    ("BYWEEKNO", ListRange { min: -53, max: 53 }),
    ("BYMONTH", ListRange { min: 1, max: 12 }),
    ("BYSETPOS", ListRange { min: -366, max: 366 }),
];

/// 库会把值组合进一天时刻的 BY 列表（上游 `TIME_PARTS`）。
const TIME_PARTS: [&str; 3] = ["BYHOUR", "BYMINUTE", "BYSECOND"];

/// 本面枚举的 FREQ 闭集（SECONDLY 合法但违反 60 秒间隔，单独拒绝）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Frequency {
    Minutely,
    Hourly,
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

/// BYDAY 条目计数；带序数条目（如 `2TU`）在其月/年内至多选一天
/// （上游 `ByDayCounts`）。
#[derive(Clone, Copy, Debug)]
struct ByDayCounts {
    entries: usize,
    ordinals: usize,
}

/// 组合与成本检查用的已解码 RRULE 值（上游 `RuleValues`）。
#[derive(Clone, Debug)]
struct RuleValues {
    frequency: Frequency,
    interval: u64,
    count: Option<u64>,
    /// UNTIL instant 的 epoch 毫秒。
    until: Option<i64>,
    /// WKST 序号；周一 0。
    week_start: usize,
    /// 出现过的 BY 列表（键 → 值）。
    lists: Vec<(&'static str, Vec<i64>)>,
    by_day: Option<ByDayCounts>,
}

impl RuleValues {
    fn list(&self, key: &str) -> Option<&[i64]> {
        self.lists
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, values)| values.as_slice())
    }

    fn has_list(&self, key: &str) -> bool {
        self.lists.iter().any(|(name, _)| *name == key)
    }

    fn lists_size(&self) -> usize {
        self.lists.len()
    }
}

/// 库规则部件（构建 rust rrule crate 规则用）。
#[derive(Clone, Debug)]
struct LibraryParts {
    frequency: Frequency,
    interval: u64,
    week_start: usize,
    by_second: Vec<i64>,
    by_minute: Vec<i64>,
    by_hour: Vec<i64>,
    /// (序数, 周几码序号 0=周一)。
    by_weekday: Vec<(Option<i64>, usize)>,
    by_month_day: Vec<i64>,
    by_year_day: Vec<i64>,
    by_week_no: Vec<i64>,
    by_month: Vec<i64>,
    by_set_pos: Vec<i64>,
}

/// 一个校验过的规则与 UTC 形墙钟枚举所需的库输入（上游 `PreparedRRule`）。
#[derive(Clone, Debug)]
pub struct PreparedRRule {
    /// Trim 后的两行输入；存储记录保留此文本。
    pub(crate) normalized: String,
    /// canonical 时区；DTSTART 以 Z 结尾时为 `UTC`。
    pub(crate) time_zone: String,
    /// DTSTART 墙钟的 UTC 形 epoch 毫秒。
    pub(crate) start: i64,
    pub(crate) frequency: Frequency,
    pub(crate) interval: u64,
    /// WKST 序号；周一 0。
    pub(crate) week_start: usize,
    pub(crate) count: Option<u64>,
    /// UNTIL instant，或最晚四位年 instant。
    pub(crate) limit: i64,
    /// 库规则部件（不含 COUNT/UNTIL——本层在实际 instant 上施加）。
    pub(crate) library: LibraryParts,
    /// 一个完整「日历 + INTERVAL」循环的墙钟长度；更长的 gap 墙钟串
    /// 永不终结（上游 `invalidRunLimit`）。
    pub(crate) invalid_run_limit: i64,
}

/// 决策时刻附近的一窗 occurrence（上游 `OccurrenceWindow`）。
#[derive(Default)]
struct OccurrenceWindow {
    latest: Option<i64>,
    next: Option<i64>,
}

/// 字节段是否恰为非空 ASCII 数字串（上游 `\d+` 正则的等价判定）。
fn text_is_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

/// 读一个匹配基础日期时间 `YYYYMMDDTHHMMSS` 的 UTC 形 epoch 毫秒，
/// 拒绝非规范化值（上游 `basicTime`）。
fn basic_time(field: &str, text: &str) -> Result<i64, RecurrenceInputError> {
    let message = format!("{field} must be a real calendar date and time in years 0100 through 9999.");
    let invalid = || RecurrenceInputError::new(RecurrenceRejection::Syntax, message.clone());
    let bytes = text.as_bytes();
    if bytes.len() != 15 || bytes[8] != b'T' {
        return Err(invalid());
    }
    let digits = |slice: &str| -> Option<i64> {
        if text_is_digits(slice) {
            slice.parse::<i64>().ok()
        } else {
            None
        }
    };
    let (Some(year), Some(month), Some(day)) = (
        digits(&text[0..4]),
        digits(&text[4..6]),
        digits(&text[6..8]),
    ) else {
        return Err(invalid());
    };
    let (Some(hour), Some(minute), Some(second)) = (
        digits(&text[9..11]),
        digits(&text[11..13]),
        digits(&text[13..15]),
    ) else {
        return Err(invalid());
    };
    if year < MIN_RRULE_YEAR {
        return Err(invalid());
    }
    // 上游 PlainDateTime.from({overflow: reject})——不存在于日历的
    // 日期与越界钟字段同拒。
    if hour > 23 || minute > 59 || second > 59
        || jiff::civil::Date::new(year as i16, month as i8, day as i8).is_err()
    {
        return Err(invalid());
    }
    Ok(civil_epoch(year, month, day, hour, minute, second, 0))
}

/// 拆分为 Trim 后的 DTSTART 与 RRULE 两行（上游 `recurrenceLines`）。
fn recurrence_lines(value: &str) -> Result<(String, String), RecurrenceInputError> {
    let normalized = value.replace("\r\n", "\n");
    let lines: Vec<&str> = normalized
        .trim()
        .split('\n')
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect();
    let message = "rrule must contain one DTSTART line followed by one RRULE line.";
    if lines.len() != 2 || !lines[0].starts_with("DTSTART") || !lines[1].starts_with("RRULE:") {
        return Err(RecurrenceInputError::new(RecurrenceRejection::Syntax, message));
    }
    Ok((lines[0].to_string(), lines[1].to_string()))
}

/// DTSTART 的 UTC 形前置（上游 `DTSTART_UTC`：`DTSTART:YYYYMMDDTHHMMSSZ`）。
fn matches_dtstart_utc(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("DTSTART:") else {
        return false;
    };
    let bytes = rest.as_bytes();
    bytes.len() == 16 && bytes[8] == b'T' && bytes[15] == b'Z'
        && text_is_digits(&rest[0..8])
        && text_is_digits(&rest[9..15])
}

/// DTSTART 的带时区前置（上游 `DTSTART_ZONED`：`DTSTART;TZID=X:墙钟`）。
fn matches_dtstart_zoned(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("DTSTART;TZID=") else {
        return false;
    };
    let Some((tzid, wall)) = rest.rsplit_once(':') else {
        return false;
    };
    !tzid.is_empty()
        && !tzid.contains(';')
        && wall.len() == 15
        && wall.as_bytes().get(8) == Some(&b'T')
        && text_is_digits(&wall[0..8])
        && text_is_digits(&wall[9..15])
}

/// 校验 canonical 时区并保留解析结果（上游 `validatedZone`）。
fn validated_zone(value: &str) -> Result<String, RecurrenceInputError> {
    canonicalize_time_zone(value).map_err(|_| {
        RecurrenceInputError::new(
            RecurrenceRejection::TimeZone,
            "TZID must be UTC or a valid IANA Area/Location name.",
        )
    })
}

/// 读 DTSTART 为 canonical 时区与 UTC 形墙钟（上游 `parseStart`）。
fn parse_start(line: &str) -> Result<(String, i64), RecurrenceInputError> {
    if matches_dtstart_utc(line) {
        return Ok(("UTC".into(), basic_time("DTSTART", &line[8..23])?));
    }
    if !matches_dtstart_zoned(line) {
        return Err(RecurrenceInputError::new(
            RecurrenceRejection::Syntax,
            "DTSTART must use UTC with Z or a local date-time with an explicit TZID.",
        ));
    }
    let rest = line.strip_prefix("DTSTART;TZID=").expect("形已匹配");
    let (tzid, wall) = rest.rsplit_once(':').expect("形已含冒号");
    let zone = validated_zone(tzid)?;
    Ok((zone, basic_time("DTSTART", wall)?))
}

/// 拆 RRULE 行为输入序的部件表（上游 `ruleParts`）。
fn rule_parts(line: &str) -> Result<Vec<(String, String)>, RecurrenceInputError> {
    let mut parts: Vec<(String, String)> = Vec::new();
    let body = line.strip_prefix("RRULE:").expect("调用方已保证前缀");
    for component in body.split(';') {
        let Some(separator) = component.find('=') else {
            return Err(non_empty_unique());
        };
        let key = &component[..separator];
        let text = &component[separator + 1..];
        if separator == 0 || text.is_empty() || parts.iter().any(|(name, _)| name == key) {
            return Err(non_empty_unique());
        }
        if !SUPPORTED_PARTS.contains(&key) {
            return Err(RecurrenceInputError::new(
                RecurrenceRejection::Syntax,
                format!("RRULE supports only {}.", SUPPORTED_PARTS.join(", ")),
            ));
        }
        parts.push((key.to_string(), text.to_string()));
    }
    Ok(parts)
}

fn non_empty_unique() -> RecurrenceInputError {
    RecurrenceInputError::new(
        RecurrenceRejection::Syntax,
        "RRULE fields must be non-empty and unique.",
    )
}

/// 读 FREQ；SECONDLY 合法但违反 60 秒 occurrence 间隔（上游 `frequencyOf`）。
fn frequency_of(text: Option<&str>) -> Result<Frequency, RecurrenceInputError> {
    let Some(text) = text else {
        return Err(RecurrenceInputError::new(
            RecurrenceRejection::Syntax,
            "RRULE must contain FREQ.",
        ));
    };
    if text == "SECONDLY" {
        return Err(RecurrenceInputError::new(
            RecurrenceRejection::Density,
            "FREQ=SECONDLY is not supported; occurrences must be at least 60 seconds apart.",
        ));
    }
    let frequency = match text {
        "MINUTELY" => Frequency::Minutely,
        "HOURLY" => Frequency::Hourly,
        "DAILY" => Frequency::Daily,
        "WEEKLY" => Frequency::Weekly,
        "MONTHLY" => Frequency::Monthly,
        "YEARLY" => Frequency::Yearly,
        _ => {
            return Err(RecurrenceInputError::new(
                RecurrenceRejection::Syntax,
                "FREQ must be MINUTELY, HOURLY, DAILY, WEEKLY, MONTHLY, or YEARLY.",
            ));
        }
    };
    Ok(frequency)
}

/// 读可选的正整数部件 1..=max（上游 `positiveValue`；`^\d+$` 允许前导零）。
fn positive_value(
    text: Option<&str>,
    max: u64,
    message: &str,
) -> Result<Option<u64>, RecurrenceInputError> {
    let Some(text) = text else {
        return Ok(None);
    };
    if !text_is_digits(text) {
        return Err(RecurrenceInputError::new(RecurrenceRejection::Syntax, message));
    }
    let Ok(value) = text.parse::<u64>() else {
        return Err(RecurrenceInputError::new(RecurrenceRejection::Syntax, message));
    };
    if value < 1 || value > max {
        return Err(RecurrenceInputError::new(RecurrenceRejection::Syntax, message));
    }
    Ok(Some(value))
}

/// 读可选 UNTIL 为真实 UTC instant（上游 `untilOf`）。
fn until_of(text: Option<&str>) -> Result<Option<i64>, RecurrenceInputError> {
    let Some(text) = text else {
        return Ok(None);
    };
    let Some(body) = text.strip_suffix('Z') else {
        return Err(until_error());
    };
    if body.len() != 15 || body.as_bytes().get(8) != Some(&b'T') || !text_is_digits(&body[0..8]) || !text_is_digits(&body[9..15]) {
        return Err(until_error());
    }
    Ok(Some(basic_time("UNTIL", body)?))
}

fn until_error() -> RecurrenceInputError {
    RecurrenceInputError::new(
        RecurrenceRejection::Syntax,
        "UNTIL must be an absolute UTC date-time ending in Z.",
    )
}

/// 读 WKST 为周几序号；RFC 5545 默认周一（上游 `weekStartOf`）。
fn week_start_of(text: Option<&str>) -> Result<usize, RecurrenceInputError> {
    let name = text.unwrap_or("MO");
    WEEKDAYS
        .iter()
        .position(|candidate| *candidate == name)
        .ok_or_else(|| {
            RecurrenceInputError::new(
                RecurrenceRejection::Syntax,
                "WKST must be MO, TU, WE, TH, FR, SA, or SU.",
            )
        })
}

/// 读一个数值 BY 列表，值唯一且在范围（上游 `listValues`）。
fn list_values(
    key: &'static str,
    text: &str,
    range: &ListRange,
) -> Result<Vec<i64>, RecurrenceInputError> {
    let signed = range.min < 0;
    let mut values = Vec::new();
    for item in text.split(',') {
        // 有符号形收 `^[+-]?\d+$`（`+5`、`-5`、`05`），无符号形收 `^\d+$`。
        let shape_ok = if signed { item.parse::<i64>().is_ok() } else { text_is_digits(item) };
        let Ok(value) = item.parse::<i64>() else {
            return Err(out_of_range(key, range, signed));
        };
        if !shape_ok || value < range.min || value > range.max || (signed && value == 0) {
            return Err(out_of_range(key, range, signed));
        }
        values.push(value);
    }
    let unique: BTreeSet<i64> = values.iter().copied().collect();
    if unique.len() != values.len() {
        return Err(RecurrenceInputError::new(
            RecurrenceRejection::Syntax,
            format!("{key} values must be unique."),
        ));
    }
    Ok(values)
}

fn out_of_range(key: &str, range: &ListRange, signed: bool) -> RecurrenceInputError {
    RecurrenceInputError::new(
        RecurrenceRejection::Syntax,
        format!(
            "{key} values must be integers from {} to {}{}.",
            range.min,
            range.max,
            if signed { ", excluding 0" } else { "" }
        ),
    )
}

/// 读可选 BYDAY 列表；`+2TU` 与 `2TU` 同选一天（上游 `byDayOf`）。
fn by_day_of(text: Option<&str>) -> Result<Option<ByDayCounts>, RecurrenceInputError> {
    let Some(text) = text else {
        return Ok(None);
    };
    let selectors: Vec<&str> = text.split(',').collect();
    let invalid = || {
        RecurrenceInputError::new(
            RecurrenceRejection::Syntax,
            "BYDAY contains an invalid weekday selector.",
        )
    };
    let mut seen = BTreeSet::new();
    let mut ordinals = 0usize;
    for selector in &selectors {
        if selector.len() < 2 {
            return Err(invalid());
        }
        let (prefix, day) = selector.split_at(selector.len() - 2);
        let Some(index) = WEEKDAYS.iter().position(|name| *name == day) else {
            return Err(invalid());
        };
        // 无序数条目的键沿用上游 `Number('')` 形（"NaN" + 周几码）。
        let key = if prefix.is_empty() {
            format!("NaN{day}")
        } else {
            let value_text = prefix.strip_prefix('+').unwrap_or(prefix);
            if !text_is_digits(value_text) {
                return Err(invalid());
            }
            let Ok(number) = value_text.parse::<i64>() else {
                return Err(invalid());
            };
            // 上游正则把序数限在 1-53。
            if !(1..=53).contains(&number) {
                return Err(invalid());
            }
            ordinals += 1;
            format!("{number}{day}")
        };
        seen.insert((key, index));
    }
    if seen.len() != selectors.len() {
        return Err(RecurrenceInputError::new(
            RecurrenceRejection::Syntax,
            "BYDAY values must be unique.",
        ));
    }
    Ok(Some(ByDayCounts { entries: selectors.len(), ordinals }))
}

/// 读每个 RRULE 部件值（上游 `ruleValues`）。
fn rule_values(parts: &[(String, String)]) -> Result<RuleValues, RecurrenceInputError> {
    let get = |key: &str| parts.iter().find(|(name, _)| name == key).map(|(_, text)| text.as_str());
    let frequency = frequency_of(get("FREQ"))?;
    let interval = positive_value(
        get("INTERVAL"),
        u64::MAX,
        "INTERVAL must be a positive safe integer.",
    )?;
    let count = positive_value(
        get("COUNT"),
        MAX_RRULE_COUNT,
        &format!("COUNT must be an integer from 1 to {MAX_RRULE_COUNT}; use UNTIL for a longer finite recurrence."),
    )?;
    let until = until_of(get("UNTIL"))?;
    let week_start = week_start_of(get("WKST"))?;
    let mut lists = Vec::new();
    for (key, range) in &LIST_RANGES {
        if let Some(text) = get(key) {
            lists.push((*key, list_values(key, text, range)?));
        }
    }
    let by_day = by_day_of(get("BYDAY"))?;
    Ok(RuleValues {
        frequency,
        interval: interval.unwrap_or(1),
        count,
        until,
        week_start,
        lists,
        by_day,
    })
}

/// 拒绝 RFC 5545 禁止的部件组合与破坏 60 秒间隔的组合（上游
/// `checkCombination`）。
fn check_combination(values: &RuleValues) -> Result<(), RecurrenceInputError> {
    if values.count.is_some() && values.until.is_some() {
        return Err(RecurrenceInputError::new(
            RecurrenceRejection::Syntax,
            "RRULE must not contain both COUNT and UNTIL.",
        ));
    }
    if values.list("BYSECOND").is_some_and(|list| list.len() > 1) {
        return Err(RecurrenceInputError::new(
            RecurrenceRejection::Density,
            "BYSECOND must contain one value; occurrences must be at least 60 seconds apart.",
        ));
    }
    if (values.frequency == Frequency::Minutely || values.frequency == Frequency::Hourly)
        && (values.lists_size() > 0 || values.by_day.is_some())
    {
        return Err(RecurrenceInputError::new(
            RecurrenceRejection::Syntax,
            format!(
                "FREQ={} does not support BY rule parts; use FREQ=DAILY or FREQ=WEEKLY with BYHOUR and BYMINUTE.",
                frequency_name(values.frequency)
            ),
        ));
    }
    for key in ["BYWEEKNO", "BYYEARDAY"] {
        if values.has_list(key) && values.frequency != Frequency::Yearly {
            return Err(RecurrenceInputError::new(
                RecurrenceRejection::Syntax,
                format!("{key} requires FREQ=YEARLY."),
            ));
        }
    }
    if values.has_list("BYMONTHDAY")
        && values.frequency != Frequency::Monthly
        && values.frequency != Frequency::Yearly
    {
        return Err(RecurrenceInputError::new(
            RecurrenceRejection::Syntax,
            "BYMONTHDAY requires FREQ=MONTHLY or FREQ=YEARLY.",
        ));
    }
    let ordinals = values.by_day.map(|counts| counts.ordinals).unwrap_or(0);
    if ordinals > 0 && values.frequency != Frequency::Monthly && values.frequency != Frequency::Yearly {
        return Err(RecurrenceInputError::new(
            RecurrenceRejection::Syntax,
            "BYDAY ordinals require FREQ=MONTHLY or FREQ=YEARLY.",
        ));
    }
    if ordinals > 0 && values.has_list("BYWEEKNO") {
        return Err(RecurrenceInputError::new(
            RecurrenceRejection::Syntax,
            "BYDAY ordinals cannot be combined with BYWEEKNO.",
        ));
    }
    if values.has_list("BYSETPOS") && values.lists_size() == 1 && values.by_day.is_none() {
        return Err(RecurrenceInputError::new(
            RecurrenceRejection::Syntax,
            "BYSETPOS requires another BY rule part.",
        ));
    }
    Ok(())
}

fn frequency_name(frequency: Frequency) -> &'static str {
    match frequency {
        Frequency::Minutely => "MINUTELY",
        Frequency::Hourly => "HOURLY",
        Frequency::Daily => "DAILY",
        Frequency::Weekly => "WEEKLY",
        Frequency::Monthly => "MONTHLY",
        Frequency::Yearly => "YEARLY",
    }
}

/// 一个 FREQ 周期所选日数的上界；无 BYDAY 即无界（上游 `byDayDays`）。
fn by_day_days(by_day: Option<ByDayCounts>, per_weekday: i64) -> Option<i64> {
    match by_day {
        None => None,
        Some(counts) => {
            Some(counts.ordinals as i64 + per_weekday * (counts.entries - counts.ordinals) as i64)
        }
    }
}

fn min_optional(values: &[Option<i64>]) -> Option<i64> {
    values
        .into_iter()
        .copied()
        .flatten()
        .min_by(|left, right| left.cmp(right))
}

/// 无界 × 数 = 无界；有界 × 有界饱和乘（上游 JS 的 `Infinity` 语义）。
fn mul_optional(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(x), Some(y)) => Some(x.saturating_mul(y)),
        _ => None,
    }
}

/// 一个 FREQ 周期所选日数的实际上界（规则显式携带 DTSTART 默认值）；
/// 一个 BYWEEKNO 值至多选一年的 9 天（相邻年同周号日期也可入选）
/// （上游 `periodDays`）。
fn period_days(values: &RuleValues) -> Option<i64> {
    let size = |key: &str| -> Option<i64> { values.list(key).map(|list| list.len() as i64) };
    let months = values.list("BYMONTH").map(|list| list.len() as i64);
    match values.frequency {
        Frequency::Minutely | Frequency::Hourly | Frequency::Daily => Some(1),
        Frequency::Weekly => min_optional(&[Some(7), by_day_days(values.by_day, 1)]),
        Frequency::Monthly => min_optional(&[
            Some(31),
            size("BYMONTHDAY"),
            by_day_days(values.by_day, 5),
        ]),
        Frequency::Yearly => {
            if months.is_none() {
                min_optional(&[
                    Some(366),
                    size("BYYEARDAY"),
                    size("BYMONTHDAY").and_then(|n| 12i64.checked_mul(n)),
                    size("BYWEEKNO").and_then(|n| 9i64.checked_mul(n)),
                    by_day_days(values.by_day, 53),
                ])
            } else {
                min_optional(&[
                    months.map(|months| 31i64.saturating_mul(months)),
                    size("BYYEARDAY"),
                    mul_optional(months, size("BYMONTHDAY")),
                    // 上游此位不乘 months（一个 BYWEEKNO 值至多 9 天）。
                    size("BYWEEKNO").and_then(|n| 9i64.checked_mul(n)),
                    mul_optional(months, by_day_days(values.by_day, 5)),
                ])
            }
        }
    }
}

/// 拒绝 FREQ 周期能生成超过一次枚举可访问量的规则（上游 `checkCost`）。
fn check_cost(
    values: &RuleValues,
    reject_ineffective_set_positions: bool,
) -> Result<(), RecurrenceInputError> {
    let times = ["BYHOUR", "BYMINUTE", "BYSECOND"]
        .iter()
        .map(|key| values.list(key).map_or(1, |list| list.len() as u64))
        .product::<u64>();
    let candidates = period_days(values).unwrap_or(0) as u64 * times;
    if candidates > MAX_PERIOD_CANDIDATES {
        return Err(RecurrenceInputError::new(
            RecurrenceRejection::Density,
            format!(
                "One FREQ period of this RRULE can contain up to {candidates} occurrences; the limit is {MAX_PERIOD_CANDIDATES}."
            ),
        ));
    }
    let positions = values.list("BYSETPOS").unwrap_or(&[]).to_vec();
    if reject_ineffective_set_positions && candidates == 1 && !positions.is_empty() {
        return Err(RecurrenceInputError::new(
            RecurrenceRejection::Syntax,
            format!(
                "BYSETPOS has no effect because one FREQ={} period of this RRULE contains at most 1 occurrence. \
                 BYSETPOS selects among the occurrences of one period; for example, \
                 FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1 selects the last weekday of each month.",
                frequency_name(values.frequency)
            ),
        ));
    }
    if positions.len() > MAX_SET_POSITIONS {
        return Err(RecurrenceInputError::new(
            RecurrenceRejection::Syntax,
            format!("BYSETPOS must contain at most {MAX_SET_POSITIONS} values."),
        ));
    }
    if positions.iter().any(|position| position.unsigned_abs() > candidates) {
        return Err(RecurrenceInputError::new(
            RecurrenceRejection::Syntax,
            format!(
                "BYSETPOS values must be from 1 to {candidates} or from -{candidates} to -1; \
                 one FREQ period of this RRULE contains at most {candidates} occurrences."
            ),
        ));
    }
    Ok(())
}

/// 库在 DAILY 及更长的规则缺省部件时从 DTSTART 派生的值
/// （上游 `startDefaults`；返回键 → 文本）。
fn start_defaults(values: &RuleValues, start: i64) -> Vec<(&'static str, String)> {
    let mut defaults = Vec::new();
    if values.frequency == Frequency::Minutely || values.frequency == Frequency::Hourly {
        return defaults;
    }
    let (year, month, day, hour, minute, second) = civil_fields(start);
    let _ = (year, month, day);
    let has = |key: &str| values.has_list(key);
    if !has("BYHOUR") {
        defaults.push(("BYHOUR", hour.to_string()));
    }
    if !has("BYMINUTE") {
        defaults.push(("BYMINUTE", minute.to_string()));
    }
    if !has("BYSECOND") {
        defaults.push(("BYSECOND", second.to_string()));
    }
    let day_codes = values.by_day.is_some()
        || has("BYMONTHDAY")
        || has("BYYEARDAY")
        || has("BYWEEKNO");
    if day_codes {
        return defaults;
    }
    if values.frequency == Frequency::Weekly {
        // 上游 `new Weekday((getUTCDay() + 6) % 7)`：周日 0 → 周一 0 序。
        let weekday_index = ((weekday_number(start) + 6) % 7) as usize;
        defaults.push(("BYDAY", WEEKDAYS[weekday_index].to_string()));
    }
    if values.frequency == Frequency::Monthly || values.frequency == Frequency::Yearly {
        defaults.push(("BYMONTHDAY", day.to_string()));
    }
    if values.frequency == Frequency::Yearly && !has("BYMONTH") {
        defaults.push(("BYMONTH", month.to_string()));
    }
    defaults
}

/// UTC 形墙钟的 (年, 月, 日, 时, 分, 秒)（上游 `new Date(epoch)` 的
/// 各 getUTC* 组合；纯算术实现，wall 值可越 jiff `Timestamp` 上界）。
fn civil_fields(epoch_ms: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = epoch_ms.div_euclid(DAY_MS);
    let rem = epoch_ms.rem_euclid(DAY_MS);
    let (year, month, day) = civil_from_days(days);
    let hour = rem / 3_600_000;
    let minute = rem / 60_000 % 60;
    let second = rem / 1_000 % 60;
    (year, month, day, hour, minute, second)
}

/// epoch 天数 → (年, 月, 日)（Hinnant civil_from_days）。
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (year + (month <= 2) as i64, month, day)
}

/// UTC 形墙钟的 cron 序周几（周日 0；epoch 天 0 是周四）。
fn weekday_number(epoch_ms: i64) -> i64 {
    (epoch_ms.div_euclid(DAY_MS) + 4).rem_euclid(7)
}

/// 两个正整数的最大公约数（上游 `gcd`）。
fn gcd(left: i64, right: i64) -> i64 {
    if right == 0 {
        left
    } else {
        gcd(right, left % right)
    }
}

/// 完全校验一个规则并导出枚举输入（上游 `prepareRRule`）。
fn prepare_rrule(
    value: &str,
    reject_ineffective_set_positions: bool,
) -> Result<PreparedRRule, RecurrenceInputError> {
    let (start_line, rule_line) = recurrence_lines(value)?;
    let (time_zone, start) = parse_start(&start_line)?;
    let parts = rule_parts(&rule_line)?;
    let values = rule_values(&parts)?;
    check_combination(&values)?;
    // DTSTART 的真实本地存在性：UTC 形墙钟在规则时区解析（gap 即拒，
    // 上游 `instantOf(new Date(start), timeZone) === undefined`）。
    if !dtstart_exists(start, &time_zone) {
        return Err(RecurrenceInputError::new(
            RecurrenceRejection::Syntax,
            "DTSTART must be a real local date and time in its time zone.",
        ));
    }
    // 库输入 = 输入部件 + DTSTART 派生默认值（上游 recurrenceParts）。
    let mut recurrence_parts: Vec<(String, String)> = parts.clone();
    for (key, text) in start_defaults(&values, start) {
        if !recurrence_parts.iter().any(|(name, _)| name == key) {
            recurrence_parts.push((key.to_string(), text));
        }
    }
    let recurrence_values = rule_values(&recurrence_parts)?;
    check_cost(&recurrence_values, reject_ineffective_set_positions)?;
    // COUNT/UNTIL 由本层在实际 instant 上施加，不进库规则。
    recurrence_parts.retain(|(name, _)| name != "COUNT" && name != "UNTIL");
    // 库按给定序组合时间列表，升序列表保证逐日按时间序枚举。
    for key in TIME_PARTS {
        if let Some(list) = recurrence_values.list(key) {
            let mut sorted = list.to_vec();
            sorted.sort_unstable();
            if let Some(entry) = recurrence_parts.iter_mut().find(|(name, _)| name == key) {
                entry.1 = sorted
                    .iter()
                    .map(|value| value.to_string())
                    .collect::<Vec<_>>()
                    .join(",");
            }
        }
    }
    // 库部件从升序后的 recurrence_parts 重建。
    let get = |key: &str| -> Vec<i64> {
        recurrence_parts
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, text)| text.split(',').filter_map(|item| item.parse::<i64>().ok()).collect())
            .unwrap_or_default()
    };
    let mut by_weekday = Vec::new();
    if let Some((_, day_text)) = recurrence_parts.iter().find(|(name, _)| name == "BYDAY") {
        for selector in day_text.split(',') {
            let (prefix, day) = selector.split_at(selector.len() - 2);
            let index = WEEKDAYS.iter().position(|name| *name == day).expect("已校验");
            let ordinal = if prefix.is_empty() {
                None
            } else {
                Some(prefix.strip_prefix('+').unwrap_or(prefix).parse::<i64>().expect("已校验"))
            };
            by_weekday.push((ordinal, index));
        }
    }
    let library = LibraryParts {
        frequency: recurrence_values.frequency,
        interval: recurrence_values.interval,
        week_start: recurrence_values.week_start,
        by_second: get("BYSECOND"),
        by_minute: get("BYMINUTE"),
        by_hour: get("BYHOUR"),
        by_weekday,
        by_month_day: get("BYMONTHDAY"),
        by_year_day: get("BYYEARDAY"),
        by_week_no: get("BYWEEKNO"),
        by_month: get("BYMONTH"),
        by_set_pos: get("BYSETPOS"),
    };
    Ok(PreparedRRule {
        normalized: format!("{start_line}\n{rule_line}"),
        time_zone,
        start,
        frequency: values.frequency,
        interval: values.interval,
        week_start: values.week_start,
        count: values.count,
        limit: values.until.unwrap_or_else(max_four_digit_year_ms),
        library,
        invalid_run_limit: CYCLE_MS * values.interval as i64
            / gcd(values.interval as i64, cycle_periods(values.frequency)),
    })
}

/// UTC 形墙钟在规则时区的真实存在性（上游 `instantOf(new Date(start))`）。
fn dtstart_exists(start: i64, time_zone: &str) -> bool {
    let (year, month, day, hour, minute, second) = civil_fields(start);
    let Ok(date) = jiff::civil::Date::new(year as i16, month as i8, day as i8) else {
        return false;
    };
    let time = jiff::civil::time(hour as i8, minute as i8, second as i8, 0);
    resolve_local_instant(date, time, time_zone).is_some()
}

/// UTC 形墙钟 → 该时区 epoch 偏移毫秒（上游 `wallOf` 的 offset 部分）。
fn zone_offset_ms(epoch: i64, time_zone: &str) -> i64 {
    let timestamp = jiff::Timestamp::from_millisecond(epoch).expect("调用方已限四位年");
    let zone = jiff::tz::TimeZone::get(time_zone).expect("调用方已校验时区");
    (timestamp.to_zoned(zone).offset().seconds() as i64) * 1_000
}

/// 真实 instant → UTC 形墙钟（上游 `wallOf`）。
fn wall_of(epoch: i64, time_zone: &str) -> i64 {
    epoch + zone_offset_ms(epoch, time_zone)
}

/// UTC 形墙钟 → 真实 instant 的早解释；前向跳变（gap）内为 None
/// （上游 `instantOf`）。
fn instant_of(wall: i64, time_zone: &str) -> Option<i64> {
    let (year, month, day, hour, minute, second) = civil_fields(wall);
    let date = jiff::civil::Date::new(year as i16, month as i8, day as i8).ok()?;
    let time = jiff::civil::time(hour as i8, minute as i8, second as i8, 0);
    resolve_local_instant(date, time, time_zone)
}

/// UTC 形墙钟的 epoch 天数（上游 `dayOf`）。

/// UTC 形墙钟的 epoch 天数（上游 `dayOf`）。
fn day_of(epoch_ms: i64) -> i64 {
    epoch_ms.div_euclid(DAY_MS)
}

/// 周起始日（周一 0 序）在或先于某 epoch 天；epoch 天 0 是周四
/// （上游 `weekStartDay`）。
fn week_start_day(day: i64, week_start: usize) -> i64 {
    day - (day + 3 - week_start as i64).rem_euclid(7)
}

/// UTC 形墙钟的绝对月号（上游 `monthOf`）。
fn month_of(epoch_ms: i64) -> i64 {
    let (year, month, _, _, _, _) = civil_fields(epoch_ms);
    year * 12 + month - 1
}

/// UTC 形墙钟的年（上游 `yearOf`）。
fn year_of(epoch_ms: i64) -> i64 {
    civil_fields(epoch_ms).0
}

/// `origin + k * step` 在不大于 value 处的最大点（上游 `floorStep`）。
fn floor_step(origin: i64, value: i64, step: i64) -> i64 {
    origin + (value - origin).div_euclid(step) * step
}

/// 包含一个墙钟的 INTERVAL 对齐 FREQ 周期起点（上游 `periodStart`）。
fn period_start(prepared: &PreparedRRule, wall: i64) -> i64 {
    let (start, interval, week_start) = (prepared.start, prepared.interval as i64, prepared.week_start);
    match prepared.frequency {
        Frequency::Minutely => floor_step(start, wall, interval * 60_000),
        Frequency::Hourly => floor_step(start, wall, interval * 3_600_000),
        Frequency::Daily => floor_step(day_of(start), day_of(wall), interval) * DAY_MS,
        Frequency::Weekly => {
            floor_step(
                week_start_day(day_of(start), week_start),
                week_start_day(day_of(wall), week_start),
                7 * interval,
            ) * DAY_MS
        }
        Frequency::Monthly => {
            let month = floor_step(month_of(start), month_of(wall), interval);
            let (year, month) = (month.div_euclid(12), month.rem_euclid(12) + 1);
            civil_epoch(year, month, 1, 0, 0, 0, 0)
        }
        Frequency::Yearly => {
            let year = floor_step(year_of(start), year_of(wall), interval);
            civil_epoch(year, 1, 1, 0, 0, 0, 0)
        }
    }
}

/// 含一个墙钟的周期的枚举起点；首个周期保留原 DTSTART（库会把首个
/// WEEKLY 周期截断在 DTSTART）（上游 `periodAnchor`）。
fn period_anchor(prepared: &PreparedRRule, wall: i64) -> i64 {
    if wall > prepared.start {
        period_start(prepared, wall).max(prepared.start)
    } else {
        prepared.start
    }
}

/// 库周几映射（本面枚举周一 0 → crate Weekday）。
fn library_weekday(index: usize) -> LibraryWeekday {
    match index {
        0 => LibraryWeekday::Mon,
        1 => LibraryWeekday::Tue,
        2 => LibraryWeekday::Wed,
        3 => LibraryWeekday::Thu,
        4 => LibraryWeekday::Fri,
        5 => LibraryWeekday::Sat,
        6 => LibraryWeekday::Sun,
        _ => unreachable!("闭集内"),
    }
}

/// 从周期起点枚举 UTC 形墙钟的库规则（上游 `libraryRule`）。
fn library_rule(prepared: &PreparedRRule, anchor: i64) -> RRuleSet {
    let anchor_dt = DateTime::<Utc>::from_timestamp_millis(anchor)
        .expect("四位年墙钟恒可表示")
        .with_timezone(&rrule::Tz::UTC);
    let parts = &prepared.library;
    let frequency = match parts.frequency {
        Frequency::Yearly => LibraryFrequency::Yearly,
        Frequency::Monthly => LibraryFrequency::Monthly,
        Frequency::Weekly => LibraryFrequency::Weekly,
        Frequency::Daily => LibraryFrequency::Daily,
        Frequency::Hourly => LibraryFrequency::Hourly,
        Frequency::Minutely => LibraryFrequency::Minutely,
    };
    let mut builder = LibraryRuleBuilder::new(frequency)
        .interval(parts.interval as u16)
        .week_start(library_weekday(parts.week_start));
    if !parts.by_set_pos.is_empty() {
        builder = builder.by_set_pos(parts.by_set_pos.iter().map(|value| *value as i32).collect());
    }
    if !parts.by_month.is_empty() {
        let months: Vec<LibraryMonth> = parts
            .by_month
            .iter()
            .map(|value| match value {
                1 => LibraryMonth::January,
                2 => LibraryMonth::February,
                3 => LibraryMonth::March,
                4 => LibraryMonth::April,
                5 => LibraryMonth::May,
                6 => LibraryMonth::June,
                7 => LibraryMonth::July,
                8 => LibraryMonth::August,
                9 => LibraryMonth::September,
                10 => LibraryMonth::October,
                11 => LibraryMonth::November,
                12 => LibraryMonth::December,
                _ => unreachable!("校验范围 1-12"),
            })
            .collect();
        builder = builder.by_month(&months);
    }
    if !parts.by_month_day.is_empty() {
        builder = builder.by_month_day(parts.by_month_day.iter().map(|value| *value as i8).collect());
    }
    if !parts.by_year_day.is_empty() {
        builder = builder.by_year_day(parts.by_year_day.iter().map(|value| *value as i16).collect());
    }
    if !parts.by_week_no.is_empty() {
        builder = builder.by_week_no(parts.by_week_no.iter().map(|value| *value as i8).collect());
    }
    if !parts.by_weekday.is_empty() {
        let weekdays = parts
            .by_weekday
            .iter()
            .map(|(ordinal, index)| {
                NWeekday::new(ordinal.map(|value| value as i16), library_weekday(*index as usize))
            })
            .collect();
        builder = builder.by_weekday(weekdays);
    }
    if !parts.by_hour.is_empty() {
        builder = builder.by_hour(parts.by_hour.iter().map(|value| *value as u8).collect());
    }
    if !parts.by_minute.is_empty() {
        builder = builder.by_minute(parts.by_minute.iter().map(|value| *value as u8).collect());
    }
    if !parts.by_second.is_empty() {
        builder = builder.by_second(parts.by_second.iter().map(|value| *value as u8).collect());
    }
    builder
        .build(anchor_dt)
        .expect("已校验部件恒可构建库规则")
}

/// 报告规则能否生成墙钟。整 400 年循环保持格里高利日历，扫描从
/// 9999 年前留出一个完整「日历 + INTERVAL」循环的最新等价周期开始；
/// 无墙钟规则的成本至多两个循环（上游 `hasCandidate`）。
fn has_candidate(prepared: &PreparedRRule) -> bool {
    if prepared.frequency == Frequency::Minutely || prepared.frequency == Frequency::Hourly {
        return true;
    }
    let first = period_start(prepared, prepared.start);
    let first_year = year_of(first);
    let years = cycle_periods(Frequency::Yearly);
    let cycle_years = years * prepared.interval as i64
        / gcd(prepared.interval as i64, cycle_periods(prepared.frequency));
    let cycles = ((9999 - first_year - cycle_years - 1)
        .div_euclid(years))
    .max(if first_year < MIN_RRULE_YEAR { 1 } else { 0 });
    let anchor = first + cycles * CYCLE_MS;
    let set = library_rule(prepared, anchor);
    // crate 的迭代器以 UTC 产出墙钟；`after(anchor, inc=true)` 的首个结果。
    (&set)
        .into_iter()
        .find(|dt| dt.timestamp_millis() >= anchor)
        .is_some()
}

/// 从一个起点枚举的墙钟中找决策前的最新 occurrence 与其后第一个
/// occurrence。不晚于决策墙钟的墙钟只从最新一个往回换算
/// （上游 `scanWindow`）。
fn scan_window(prepared: &PreparedRRule, from_wall: i64, decision: i64) -> OccurrenceWindow {
    let mut window = OccurrenceWindow::default();
    if from_wall > LAST_RRULE_WALL_MS {
        return window;
    }
    let bound = decision.min(prepared.limit);
    let bound_wall = wall_of(bound, &prepared.time_zone);
    let mut below: Vec<i64> = Vec::new();
    let mut last_valid_wall = bound_wall;
    let set = library_rule(prepared, period_anchor(prepared, from_wall));
    for date in &set {
        let wall = date.timestamp_millis();
        if wall < from_wall {
            continue;
        }
        if wall <= bound_wall {
            below.push(wall);
            continue;
        }
        // UTC 偏移短于一天。
        if wall >= prepared.limit + DAY_MS {
            break;
        }
        let instant = instant_of(wall, &prepared.time_zone);
        if instant.is_none() {
            if wall - last_valid_wall > prepared.invalid_run_limit {
                break;
            }
            continue;
        }
        let instant = instant.expect("上面已判存在");
        last_valid_wall = wall;
        if instant <= bound {
            window.latest = Some(instant);
            continue;
        }
        if instant <= prepared.limit {
            window.next = Some(instant);
        }
        break;
    }
    while window.latest.is_none() {
        let Some(wall) = below.pop() else {
            break;
        };
        if let Some(instant) = instant_of(wall, &prepared.time_zone) {
            if instant <= bound {
                window.latest = Some(instant);
            }
        }
    }
    window
}

/// 找最新到期 occurrence；窗口从决策前两天起指数倍扩展直到命中
/// （上游 `dispatchWindow`）。
fn dispatch_window(prepared: &PreparedRRule, saved: i64, decision: i64) -> OccurrenceWindow {
    let saved_wall = wall_of(saved, &prepared.time_zone);
    let bound_wall = wall_of(decision.min(prepared.limit), &prepared.time_zone);
    let mut span = 2 * DAY_MS;
    loop {
        let from_wall = (bound_wall - span).max(saved_wall);
        let window = scan_window(prepared, from_wall, decision);
        if window.latest.is_some() || from_wall == saved_wall {
            return window;
        }
        span *= 2;
    }
}

/// 从 DTSTART 起枚举 COUNT 个实例至决策后的第一个 occurrence
/// （上游 `countWindow`）。
fn count_window(prepared: &PreparedRRule, decision: i64, count: u64) -> OccurrenceWindow {
    let mut window = OccurrenceWindow::default();
    let mut remaining = count;
    let mut last_valid_wall = prepared.start;
    let set = library_rule(prepared, prepared.start);
    for date in &set {
        let wall = date.timestamp_millis();
        let Some(instant) = instant_of(wall, &prepared.time_zone) else {
            if wall - last_valid_wall > prepared.invalid_run_limit {
                break;
            }
            continue;
        };
        if window.latest == Some(instant) {
            continue;
        }
        last_valid_wall = wall;
        if instant > decision {
            if instant <= max_four_digit_year_ms() {
                window.next = Some(instant);
            }
            break;
        }
        window.latest = Some(instant);
        remaining -= 1;
        if remaining == 0 {
            break;
        }
    }
    window
}

/// 格式化严格晚于一个可表示参考时刻的首个 occurrence
/// （上游 `nextOccurrence`）。
fn next_occurrence(prepared: &PreparedRRule, after: i64) -> Option<String> {
    if !has_candidate(prepared) {
        return None;
    }
    let next = if prepared.count.is_none() {
        scan_window(prepared, wall_of(after, &prepared.time_zone), after).next
    } else {
        count_window(prepared, after, prepared.count.expect("上面已判存在")).next
    };
    next.map(format_instant)
}

/// 解析后的两行 DTSTART/RRULE 值，带有界 occurrence 算术（上游 `RRule`）。
#[derive(Clone, Debug)]
pub struct RRule {
    prepared: PreparedRRule,
}

impl RRule {
    /// 校验语法与枚举成本后再做 occurrence 搜索（上游 `RRule.parse`）。
    pub fn parse(
        expression: &str,
        reject_ineffective_set_positions: bool,
    ) -> Result<Self, RecurrenceInputError> {
        Ok(Self {
            prepared: prepare_rrule(expression, reject_ineffective_set_positions)?,
        })
    }

    /// Trim 后的两行表达式；DTSTART 拥有时区与循环锚点。
    pub fn expression(&self) -> &str {
        &self.prepared.normalized
    }

    /// DTSTART 的 canonical 时区；Z 起点即 `UTC`。
    pub fn time_zone(&self) -> &str {
        &self.prepared.time_zone
    }

    /// 严格晚于显式下界的首个 occurrence，耗尽为 None（上游 `RRule.next`）。
    ///
    /// # Panics
    ///
    /// 下界不可表示时（上游 `RangeError`）——调用方传已验时刻。
    pub fn next(&self, after: i64) -> Option<String> {
        assert!(
            is_four_digit_year_instant(after),
            "RRULE lower bound must be a four-digit-year instant"
        );
        next_occurrence(&self.prepared, after)
    }

    /// 最新到期 occurrence 与首个未来 occurrence，保留既决下界
    /// （上游 `RRule.resolve`）。
    ///
    /// # Panics
    ///
    /// 决策不可表示或先于既决目标时（上游 `RangeError`）——调用方先验。
    pub fn resolve(&self, scheduled_at: &str, accepted_at: i64) -> RecurringOccurrence {
        let saved_target = recurrence_decision(scheduled_at, accepted_at);
        let prepared = &self.prepared;
        let window = if prepared.count.is_none() {
            dispatch_window(prepared, saved_target, accepted_at)
        } else {
            count_window(prepared, accepted_at, prepared.count.expect("上面已判存在"))
        };
        let occurrence_at = format_instant(window.latest.unwrap_or(saved_target).max(saved_target));
        let next_scheduled_at = window.next.map(format_instant);
        RecurringOccurrence { occurrence_at, next_scheduled_at }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DTSTART: &str = "DTSTART:20260110T090000Z";

    fn parse(text: &str) -> RRule {
        RRule::parse(text, false).unwrap()
    }

    fn instant(text: &str) -> i64 {
        text.parse::<jiff::Timestamp>().unwrap().as_millisecond()
    }

    #[test]
    fn daily_rule_next_and_resolve() {
        let rule = parse(&format!("{DTSTART}\nRRULE:FREQ=DAILY"));
        assert_eq!(rule.expression(), format!("{DTSTART}\nRRULE:FREQ=DAILY"));
        assert_eq!(rule.time_zone(), "UTC");
        let after = instant("2026-01-10T10:00:00.000Z");
        assert_eq!(rule.next(after).unwrap(), "2026-01-11T09:00:00.000Z");
        // resolve：决策 01-12 12:00Z → 最新到期当天 09:00、下次次日 09:00。
        let decision = instant("2026-01-12T12:00:00.000Z");
        let occurrence = rule.resolve("2026-01-10T09:00:00.000Z", decision);
        assert_eq!(occurrence.occurrence_at, "2026-01-12T09:00:00.000Z");
        assert_eq!(occurrence.next_scheduled_at.unwrap(), "2026-01-13T09:00:00.000Z");
    }

    #[test]
    fn weekly_byday_selects_next_weekday() {
        // 周六决策：MO,WE,FR → 周一。
        let rule = parse(&format!("{DTSTART}\nRRULE:FREQ=WEEKLY;BYDAY=MO,WE,FR"));
        let after = instant("2026-01-10T12:00:00.000Z");
        assert_eq!(rule.next(after).unwrap(), "2026-01-12T09:00:00.000Z");
        // 带序数 BYDAY：2026-01 第二个周一 = 01-12（决策 01-10 后仍严格
        // 未来即命中）。
        let rule = parse("DTSTART:20260101T090000Z\nRRULE:FREQ=MONTHLY;BYDAY=+2MO");
        assert_eq!(rule.next(after).unwrap(), "2026-01-12T09:00:00.000Z");
    }

    #[test]
    fn interval_skips_periods() {
        let rule = parse(&format!("{DTSTART}\nRRULE:FREQ=DAILY;INTERVAL=2"));
        let after = instant("2026-01-10T10:00:00.000Z");
        assert_eq!(rule.next(after).unwrap(), "2026-01-12T09:00:00.000Z");
    }

    #[test]
    fn count_exhausts_and_holds_latest() {
        let rule = parse(&format!("{DTSTART}\nRRULE:FREQ=DAILY;COUNT=3"));
        // 三次：01-10、01-11、01-12。决策 02-01：最新到期 01-12、无下次。
        let decision = instant("2026-02-01T00:00:00.000Z");
        let occurrence = rule.resolve("2026-01-10T09:00:00.000Z", decision);
        assert_eq!(occurrence.occurrence_at, "2026-01-12T09:00:00.000Z");
        assert_eq!(occurrence.next_scheduled_at, None);
    }

    #[test]
    fn until_bounds_the_search() {
        let rule = parse(&format!("{DTSTART}\nRRULE:FREQ=DAILY;UNTIL=20260111T090000Z"));
        let after = instant("2026-01-11T10:00:00.000Z");
        assert_eq!(rule.next(after), None);
    }

    #[test]
    fn tzid_dtstart_skips_dst_gap() {
        // 02:30 每日；2026-03-08 该时刻不存在（America/New_York）→ 次日。
        let rule = parse("DTSTART;TZID=America/New_York:20260307T023000\nRRULE:FREQ=DAILY");
        assert_eq!(rule.time_zone(), "America/New_York");
        let after = instant("2026-03-08T07:00:00.000Z");
        // 03-09 02:30 EDT = 06:30Z。
        assert_eq!(rule.next(after).unwrap(), "2026-03-09T06:30:00.000Z");
        // gap 内的 DTSTART 拒绝。
        let error =
            RRule::parse("DTSTART;TZID=America/New_York:20260308T023000\nRRULE:FREQ=DAILY", false)
                .unwrap_err();
        assert_eq!(
            error.message,
            "DTSTART must be a real local date and time in its time zone."
        );
    }

    #[test]
    fn rejections_are_verbatim() {
        // SECONDLY 密度。
        let error = RRule::parse(&format!("{DTSTART}\nRRULE:FREQ=SECONDLY"), false).unwrap_err();
        assert_eq!(error.reason, RecurrenceRejection::Density);
        assert_eq!(
            error.message,
            "FREQ=SECONDLY is not supported; occurrences must be at least 60 seconds apart."
        );
        // 行形。
        let error = RRule::parse("RRULE:FREQ=DAILY", false).unwrap_err();
        assert_eq!(
            error.message,
            "rrule must contain one DTSTART line followed by one RRULE line."
        );
        // 缺 FREQ。
        let error = RRule::parse(&format!("{DTSTART}\nRRULE:INTERVAL=2"), false).unwrap_err();
        assert_eq!(error.message, "RRULE must contain FREQ.");
        // COUNT + UNTIL。
        let error = RRule::parse(
            &format!("{DTSTART}\nRRULE:FREQ=DAILY;COUNT=2;UNTIL=20260112T090000Z"),
            false,
        )
        .unwrap_err();
        assert_eq!(error.message, "RRULE must not contain both COUNT and UNTIL.");
        // BYSECOND 多值。
        let error =
            RRule::parse(&format!("{DTSTART}\nRRULE:FREQ=YEARLY;BYSECOND=10,20"), false).unwrap_err();
        assert_eq!(
            error.message,
            "BYSECOND must contain one value; occurrences must be at least 60 seconds apart."
        );
        // MINUTELY 不收 BY 部件。
        let error =
            RRule::parse(&format!("{DTSTART}\nRRULE:FREQ=MINUTELY;BYDAY=MO"), false).unwrap_err();
        assert_eq!(
            error.message,
            "FREQ=MINUTELY does not support BY rule parts; use FREQ=DAILY or FREQ=WEEKLY with BYHOUR and BYMINUTE."
        );
        // BYWEEKNO 需 YEARLY。
        let error =
            RRule::parse(&format!("{DTSTART}\nRRULE:FREQ=MONTHLY;BYWEEKNO=10"), false).unwrap_err();
        assert_eq!(error.message, "BYWEEKNO requires FREQ=YEARLY.");
        // BYMONTHDAY 需 MONTHLY/YEARLY。
        let error =
            RRule::parse(&format!("{DTSTART}\nRRULE:FREQ=WEEKLY;BYMONTHDAY=10"), false).unwrap_err();
        assert_eq!(error.message, "BYMONTHDAY requires FREQ=MONTHLY or FREQ=YEARLY.");
        // BYDAY 序数需 MONTHLY/YEARLY。
        let error =
            RRule::parse(&format!("{DTSTART}\nRRULE:FREQ=WEEKLY;BYDAY=2MO"), false).unwrap_err();
        assert_eq!(error.message, "BYDAY ordinals require FREQ=MONTHLY or FREQ=YEARLY.");
        // BYSETPOS 需另一 BY 部件。
        let error =
            RRule::parse(&format!("{DTSTART}\nRRULE:FREQ=DAILY;BYSETPOS=1"), false).unwrap_err();
        assert_eq!(error.message, "BYSETPOS requires another BY rule part.");
        // 不支持部件。
        let error =
            RRule::parse(&format!("{DTSTART}\nRRULE:FREQ=DAILY;BYWEEKDAY=MO"), false).unwrap_err();
        assert_eq!(
            error.message,
            "RRULE supports only FREQ, INTERVAL, COUNT, UNTIL, WKST, BYSECOND, BYMINUTE, BYHOUR, BYDAY, BYMONTHDAY, BYYEARDAY, BYWEEKNO, BYMONTH, BYSETPOS."
        );
        // COUNT 超上限。
        let error = RRule::parse(&format!("{DTSTART}\nRRULE:FREQ=DAILY;COUNT=1001"), false).unwrap_err();
        assert_eq!(
            error.message,
            "COUNT must be an integer from 1 to 1000; use UNTIL for a longer finite recurrence."
        );
        // 早期年。
        let error = RRule::parse("DTSTART:00990101T090000Z\nRRULE:FREQ=DAILY", false).unwrap_err();
        assert_eq!(
            error.message,
            "DTSTART must be a real calendar date and time in years 0100 through 9999."
        );
        // DTSTART 形。
        let error = RRule::parse("DTSTART:20260110T090000\nRRULE:FREQ=DAILY", false).unwrap_err();
        assert_eq!(
            error.message,
            "DTSTART must use UTC with Z or a local date-time with an explicit TZID."
        );
    }

    #[test]
    fn cost_and_setpos_checks() {
        // YEARLY × 12 月 × 31 日 × 24 时 × 60 分 → 535680 超限（显式
        // BYMONTH/BYMONTHDAY 防默认部件把周期压小）。
        let range = |end: i64| -> String {
            (1..=end).map(|n| n.to_string()).collect::<Vec<_>>().join(",")
        };
        let hours: Vec<String> = (0..24).map(|h| h.to_string()).collect();
        let minutes: Vec<String> = (0..60).map(|m| m.to_string()).collect();
        let text = format!(
            "{DTSTART}\nRRULE:FREQ=YEARLY;BYMONTH={};BYMONTHDAY={};BYHOUR={};BYMINUTE={}",
            range(12),
            range(31),
            hours.join(","),
            minutes.join(",")
        );
        let error = RRule::parse(&text, false).unwrap_err();
        assert_eq!(error.reason, RecurrenceRejection::Density);
        assert_eq!(
            error.message,
            "One FREQ period of this RRULE can contain up to 535680 occurrences; the limit is 10080."
        );
        // BYSETPOS 超候选数：WEEKLY;BYDAY=MO,TU → 候选 2 → ±2 内。
        let error = RRule::parse(
            &format!("{DTSTART}\nRRULE:FREQ=WEEKLY;BYDAY=MO,TU;BYSETPOS=5"),
            false,
        )
        .unwrap_err();
        assert_eq!(
            error.message,
            "BYSETPOS values must be from 1 to 2 or from -2 to -1; one FREQ period of this RRULE contains at most 2 occurrences."
        );
        // 无效 BYSETPOS（单候选）默认放行、strict 拒。
        let text = format!("{DTSTART}\nRRULE:FREQ=DAILY;BYHOUR=12;BYSETPOS=1");
        assert!(RRule::parse(&text, false).is_ok());
        let error = RRule::parse(&text, true).unwrap_err();
        assert!(error.message.starts_with(
            "BYSETPOS has no effect because one FREQ=DAILY period of this RRULE contains at most 1 occurrence."
        ));
    }

    #[test]
    fn monthly_bymonthday_walks() {
        // 每月 31 号：DTSTART 2026-01-10 → 01-31。
        let rule = parse(&format!("{DTSTART}\nRRULE:FREQ=MONTHLY;BYMONTHDAY=31"));
        let after = instant("2026-01-10T12:00:00.000Z");
        assert_eq!(rule.next(after).unwrap(), "2026-01-31T09:00:00.000Z");
        // 负值：每月最后一天（2 月 → 02-28）。
        let rule = parse("DTSTART:20260101T090000Z\nRRULE:FREQ=MONTHLY;BYMONTHDAY=-1");
        let after = instant("2026-02-01T00:00:00.000Z");
        assert_eq!(rule.next(after).unwrap(), "2026-02-28T09:00:00.000Z");
    }
}

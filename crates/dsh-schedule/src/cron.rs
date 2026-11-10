//! 严格五字段 Cron 解析与有界、时区感知的 occurrence 算术（A1.4 批②③）。
//!
//! 对齐上游 `dsh-util-time` 的 `cron.ts`（上游即纯自写引擎）：五字段
//! 方言值集、Vixie 星标记与 DOM/DOW 日或语义、canonical 文本再生
//! （星字段保持星形拼写）、400 年地平线的界限走查。时区语义由
//! jiff 承载（DST gap 跳过 / overlap 取早）。

use std::collections::BTreeSet;

use crate::{
    canonicalize_time_zone, format_instant, is_four_digit_year_instant,
    occurrence::{local_date, max_four_digit_year_ms, min_four_digit_year_ms, resolve_local_instant},
    RecurringOccurrence,
};
use crate::recurrence::{recurrence_decision, RecurrenceInputError, RecurrenceRejection};

/// 前后向搜索共用的格里高利重复地平线（上游 `CRON_SEARCH_HORIZON_YEARS`）。
pub const CRON_SEARCH_HORIZON_YEARS: i64 = 400;

/// 支持的五字段 cron 方言的字段界限，按求值顺序（上游 `CRON_FIELDS`）。
struct CronFieldSpec {
    /// 诊断用字段名。
    name: &'static str,
    /// 输入方言接受的最低值。
    min: i64,
    /// 输入方言接受的最高值（周日收 0 与 7 两形）。
    max: i64,
    /// 周日折叠后保留的最高值。
    canonical_max: i64,
}

const CRON_FIELDS: [CronFieldSpec; 5] = [
    CronFieldSpec { name: "minute", min: 0, max: 59, canonical_max: 59 },
    CronFieldSpec { name: "hour", min: 0, max: 23, canonical_max: 23 },
    CronFieldSpec { name: "day-of-month", min: 1, max: 31, canonical_max: 31 },
    CronFieldSpec { name: "month", min: 1, max: 12, canonical_max: 12 },
    CronFieldSpec { name: "day-of-week", min: 0, max: 7, canonical_max: 6 },
];

/// 一个解析后的 cron 字段：值集、canonical 拼写与 Vixie 星标记。
#[derive(Clone, Debug, PartialEq, Eq)]
struct ParsedCronField {
    /// 周日折叠后唯一升序值集。
    values: Vec<i64>,
    /// 恰为这些值的 canonical 拼写。
    canonical: String,
    /// 字段文本是否以 `*` 开头（Vixie `DOM_STAR`/`DOW_STAR` 判定）。
    star: bool,
}

/// 一个解析后的五字段 cron 规则：canonical 文本与各字段值集。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CronFields {
    /// 存入耐久记录的 canonical 表达式。
    pub expression: String,
    /// 唯一升序分钟集。
    pub minutes: Vec<i64>,
    /// 唯一升序小时集。
    pub hours: Vec<i64>,
    /// 唯一升序日期集。
    pub days_of_month: Vec<i64>,
    /// 唯一升序月份集。
    pub months: Vec<i64>,
    /// 唯一升序 cron 周几集（周日 0 至周六 6）。
    pub days_of_week: Vec<i64>,
    /// 日期字段文本是否以 `*` 开头（Vixie `DOM_STAR`）。
    pub day_of_month_star: bool,
    /// 周几字段文本是否以 `*` 开头（Vixie `DOW_STAR`）。
    pub day_of_week_star: bool,
}

/// 一个畸形 cron 字段元素的稳定诊断（上游 `invalidCronField` 逐字）。
fn invalid_cron_field(spec: &CronFieldSpec, element: &str) -> RecurrenceInputError {
    RecurrenceInputError::new(
        RecurrenceRejection::Syntax,
        format!(
            "cron.expression {} field element {} must be *, a value, a-b, */n, a-b/n, or a comma-separated list of those.",
            spec.name,
            serde_json::json!(element)
        ),
    )
}

/// 读一个范围内的 cron 字段值（上游 `cronFieldValue`；上游正则保证
/// 只喂进数位串，非数位由调用方映射到元素诊断）。
fn cron_field_value(text: &str, spec: &CronFieldSpec) -> Result<i64, RecurrenceInputError> {
    let value = text.parse::<i64>().unwrap_or_else(|_| {
        panic!("调用方已校验数位串")
    });
    if value < spec.min || value > spec.max {
        return Err(RecurrenceInputError::new(
            RecurrenceRejection::Syntax,
            format!("cron.expression {} field value {text} is outside {}-{}.", spec.name, spec.min, spec.max),
        ));
    }
    Ok(value)
}

/// 读一个正的 cron 字段步长（上游 `cronFieldStep`）。
fn cron_field_step(text: &str, spec: &CronFieldSpec) -> Result<i64, RecurrenceInputError> {
    let Ok(step) = text.parse::<i64>() else {
        return Err(RecurrenceInputError::new(
            RecurrenceRejection::Syntax,
            format!("cron.expression {} field step must be a positive integer.", spec.name),
        ));
    };
    if step < 1 {
        return Err(RecurrenceInputError::new(
            RecurrenceRejection::Syntax,
            format!("cron.expression {} field step must be a positive integer.", spec.name),
        ));
    }
    Ok(step)
}

/// 展开一个逗号分隔的 cron 字段为匹配值集（上游 `cronFieldValues`）。
fn cron_field_values(raw: &str, spec: &CronFieldSpec) -> Result<BTreeSet<i64>, RecurrenceInputError> {
    let mut matched = BTreeSet::new();
    for element in raw.split(',') {
        if element.is_empty() {
            return Err(RecurrenceInputError::new(
                RecurrenceRejection::Syntax,
                format!("cron.expression {} field must not contain an empty list element.", spec.name),
            ));
        }
        if element == "*" {
            for value in spec.min..=spec.max {
                matched.insert(value);
            }
            continue;
        }
        if let Some(step_text) = element.strip_prefix('*') {
            let Some(step_text) = step_text.strip_prefix('/') else {
                return Err(invalid_cron_field(spec, element));
            };
            if !is_digit_run(step_text) {
                return Err(invalid_cron_field(spec, element));
            }
            let step = cron_field_step(step_text, spec)?;
            let mut value = spec.min;
            while value <= spec.max {
                matched.insert(value);
                value += step;
            }
            continue;
        }
        // `a`、`a-b`、`a/b`、`a-b/b`（上游正则 `^(\d+)(-(\d+))?(/(\d+))?$`
        // 的等价切分——每段必须恰为数位，多段/空段落到数位检查拒绝）。
        let (range_text, step_text) = match element.split_once('/') {
            Some((range, step)) => (range, Some(step)),
            None => (element, None),
        };
        let (start_text, end_text) = match range_text.split_once('-') {
            Some((start, end)) => (start, Some(end)),
            None => (range_text, None),
        };
        if !is_digit_run(start_text)
            || end_text.is_some_and(|text| !is_digit_run(text))
            || step_text.is_some_and(|text| !is_digit_run(text))
        {
            return Err(invalid_cron_field(spec, element));
        }
        let start = cron_field_value(start_text, spec)?;
        let Some(end_text) = end_text else {
            if step_text.is_some() {
                return Err(invalid_cron_field(spec, element));
            }
            matched.insert(start);
            continue;
        };
        let last = cron_field_value(end_text, spec)?;
        if start > last {
            return Err(RecurrenceInputError::new(
                RecurrenceRejection::Syntax,
                format!("cron.expression {} field range {start}-{last} is inverted.", spec.name),
            ));
        }
        let step = match step_text {
            Some(text) => cron_field_step(text, spec)?,
            None => 1,
        };
        let mut value = start;
        while value <= last {
            matched.insert(value);
            value += step;
        }
    }
    // 周日折叠：7 → 0（仅 day-of-week 字段 canonical_max ≠ max）。
    Ok(matched
        .into_iter()
        .map(|value| if spec.canonical_max != spec.max && value == spec.max { spec.min } else { value })
        .collect())
}

/// 字节段是否恰为非空 ASCII 数字串（上游元素正则 `\d+` 组的等价判定）。
fn is_digit_run(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

/// 读一个编码器已证明存在的值（上游 `cronFieldValueAt`）。
fn cron_field_value_at(values: &[i64], index: usize) -> i64 {
    values[index]
}

/// 把匹配值集编码为最短的等价逗号分隔 cron 字段，永不以 `*` 拼写
/// （上游 `encodeCronField`）。
fn encode_cron_field(values: &[i64]) -> String {
    let first = cron_field_value_at(values, 0);
    if values.len() == 1 {
        return first.to_string();
    }
    let last = cron_field_value_at(values, values.len() - 1);
    let step = cron_field_value_at(values, 1) - first;
    // 与上游一致：从 index 2 起逐对与等差比对（空窗口即单差对不短路）。
    let uniform = values[2..]
        .iter()
        .zip(values[1..].iter())
        .all(|(current, previous)| current - previous == step);
    if uniform {
        return if step == 1 { format!("{first}-{last}") } else { format!("{first}-{last}/{step}") };
    }
    let mut parts: Vec<String> = Vec::new();
    let mut run_start = first;
    for index in 1..values.len() {
        let current = values[index];
        let previous = values[index - 1];
        if current == previous + 1 {
            continue;
        }
        parts.push(if run_start == previous { run_start.to_string() } else { format!("{run_start}-{previous}") });
        run_start = current;
    }
    let final_value = cron_field_value_at(values, values.len() - 1);
    parts.push(if run_start == final_value { run_start.to_string() } else { format!("{run_start}-{final_value}") });
    parts.join(",")
}

/// 从字段最小值起的 uniform 走查，折叠方式与解析折叠一致（上游 `starWalk`）。
fn star_walk(spec: &CronFieldSpec, step: i64) -> Vec<i64> {
    let mut walked = BTreeSet::new();
    let mut value = spec.min;
    while value <= spec.max {
        walked.insert(if spec.canonical_max != spec.max && value == spec.max { spec.min } else { value });
        value += step;
    }
    walked.into_iter().collect()
}

/// 文本以 `*` 开头的字段的 canonical 拼写（上游 `encodeStarCronField`）：
/// 裸星（全覆盖）拼 `*`，否则取最宽的可含星步长走查加余值列表——再解析
/// 可复现星标记与值集，保住 DOM/DOW AND/OR 判定跨存储稳定。
fn encode_star_cron_field(
    values: &[i64],
    spec: &CronFieldSpec,
) -> Result<String, RecurrenceInputError> {
    if values.len() == (spec.canonical_max - spec.min + 1) as usize {
        return Ok("*".into());
    }
    let present: BTreeSet<i64> = values.iter().copied().collect();
    let mut best_step: Option<i64> = None;
    let mut best_walk: Vec<i64> = Vec::new();
    for step in 1..=(spec.max - spec.min + 1) {
        let walk = star_walk(spec, step);
        if walk.len() > best_walk.len() && walk.iter().all(|value| present.contains(value)) {
            best_step = Some(step);
            best_walk = walk;
        }
    }
    let best_step = best_step
        .ok_or_else(|| {
            RecurrenceInputError::new(
                RecurrenceRejection::Syntax,
                format!("{} cannot keep a leading `*`.", spec.name),
            )
        })?;
    let walked: BTreeSet<i64> = best_walk.iter().copied().collect();
    let remaining: Vec<i64> = values.iter().copied().filter(|value| !walked.contains(value)).collect();
    let walk_text = format!("*/{best_step}");
    Ok(if remaining.is_empty() { walk_text } else { format!("{walk_text},{}", encode_cron_field(&remaining)) })
}

/// 解析一个 cron 字段为值集、canonical 拼写与星标记（上游 `parseCronField`）。
fn parse_cron_field(raw: &str, spec: &CronFieldSpec) -> Result<ParsedCronField, RecurrenceInputError> {
    let mut values: Vec<i64> = cron_field_values(raw, spec)?.into_iter().collect();
    let star = raw.starts_with('*');
    let canonical = if star { encode_star_cron_field(&values, spec)? } else { encode_cron_field(&values) };
    values.shrink_to_fit();
    Ok(ParsedCronField { values, canonical, star })
}

/// 解析并规范化一个严格五字段 cron 表达式（上游 `parseCronExpression`）。
pub fn parse_cron_expression(expression: &str) -> Result<CronFields, RecurrenceInputError> {
    if expression.is_empty() || expression.trim() != expression {
        return Err(RecurrenceInputError::new(
            RecurrenceRejection::Syntax,
            "cron.expression must be a non-empty trimmed string.",
        ));
    }
    let fields: Vec<&str> = expression.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(RecurrenceInputError::new(
            RecurrenceRejection::Syntax,
            "cron.expression must contain exactly five whitespace-separated fields: minute hour day-of-month month day-of-week.",
        ));
    }
    let mut parsed = Vec::with_capacity(5);
    for (index, field) in fields.iter().enumerate() {
        parsed.push(parse_cron_field(field, &CRON_FIELDS[index])?);
    }
    let [minute, hour, day_of_month, month, day_of_week] = parsed.try_into().ok().expect("五字段已数");
    Ok(CronFields {
        expression: [minute.canonical, hour.canonical, day_of_month.canonical, month.canonical, day_of_week.canonical]
            .join(" "),
        minutes: minute.values,
        hours: hour.values,
        days_of_month: day_of_month.values,
        months: month.values,
        days_of_week: day_of_week.values,
        day_of_month_star: day_of_month.star,
        day_of_week_star: day_of_week.star,
    })
}

/// 一个解析后 cron 规则在 Vixie DOM/DOW 语义下的本地日期谓词
/// （上游 `cronDateMatch`）：星标记只选分支、不免除值集——任一字段
/// 文本以 `*` 开头则须同时满足两字段；两者都非星则任一匹配即可。
fn cron_date_matches(parsed: &CronFields, date: jiff::civil::Date) -> bool {
    let months: BTreeSet<i64> = parsed.months.iter().copied().collect();
    let days_of_month: BTreeSet<i64> = parsed.days_of_month.iter().copied().collect();
    let days_of_week: BTreeSet<i64> = parsed.days_of_week.iter().copied().collect();
    let day_of_month_restricted = !parsed.day_of_month_star;
    let day_of_week_restricted = !parsed.day_of_week_star;
    if !months.contains(&(date.month() as i64)) {
        return false;
    }
    let day_of_month_match = days_of_month.contains(&(date.day() as i64));
    // ISO 周一=1…周日=7；%7 折叠后周日=0（上游 `dayOfWeek % 7` 同款）。
    let day_of_week_match = days_of_week.contains(&((date.weekday().to_monday_zero_offset() + 1) as i64 % 7));
    if day_of_month_restricted && day_of_week_restricted {
        day_of_month_match || day_of_week_match
    } else {
        day_of_month_match && day_of_week_match
    }
}

/// 该字段的本地时刻序列（小时 × 分钟；reverse 供回走）。
fn cron_times(parsed: &CronFields, reverse: bool) -> Vec<(i64, i64)> {
    let mut times = Vec::with_capacity(parsed.hours.len() * parsed.minutes.len());
    for &hour in &parsed.hours {
        for &minute in &parsed.minutes {
            times.push((hour, minute));
        }
    }
    if reverse {
        times.reverse();
    }
    times
}

/// 日期 → 下月 1 日（跳月推进；替代上游 `add({months:1}).with({day:1})`）。
/// 上界附近（9999-12 月跳到 10000-01） civil 不可表示 → None（上游
/// 此处 RangeError；本面回答耗尽）。
fn first_of_next_month(date: jiff::civil::Date) -> Option<jiff::civil::Date> {
    let (year, month) = if date.month() == 12 {
        (date.year() + 1, 1)
    } else {
        (date.year(), date.month() + 1)
    };
    jiff::civil::Date::new(year, month, 1).ok()
}

/// 日期 → 上月末日（跳月回退；替代上游 `with({day:1}).subtract({days:1})`）。
fn last_of_previous_month(date: jiff::civil::Date) -> jiff::civil::Date {
    let (year, month) = if date.month() == 1 {
        (date.year() - 1, 12)
    } else {
        (date.year(), date.month() - 1)
    };
    let first = jiff::civil::Date::new(year, month, 1).expect("civil 年范围宽于四位年，回退恒可表示");
    let last_day = first.days_in_month();
    jiff::civil::Date::new(year, month, last_day as i8).expect("当月末日恒存在")
}

/// 年份平移，2 月 29 收敛到 2 月 28（上游 Temporal constrain 同款）；
/// 越出 civil 范围 → None。
fn shift_years(date: jiff::civil::Date, years: i64) -> Option<jiff::civil::Date> {
    let year = date.year() + years as i16;
    match jiff::civil::Date::new(year, date.month(), date.day()) {
        Ok(shifted) => Some(shifted),
        // 仅 2 月 29 会因日不存在失败——收敛 2 月 28 重试。
        Err(_) => jiff::civil::Date::new(year, 2, 28).ok(),
    }
}

/// 决策时刻在其时区的本地日期与当日分钟数（上游 localFloor 的两个分量）。
fn local_floor(floor: i64, time_zone: &str) -> (jiff::civil::Date, i64) {
    let timestamp = jiff::Timestamp::from_millisecond(floor).expect("调用方已限四位年");
    let zone = jiff::tz::TimeZone::get(time_zone).expect("调用方已校验时区");
    let zoned = timestamp.to_zoned(zone);
    let minute_of_day = zoned.hour() as i64 * 60 + zoned.minute() as i64;
    (zoned.date(), minute_of_day)
}

/// 找第一个严格未来的 cron occurrence，不越四位年范围（上游
/// `nextCronTarget`）：地平线先于四位年上界时停在地平线——地平线内
/// 无匹配的规则即耗尽。
fn next_cron_target(
    parsed: &CronFields,
    time_zone: &str,
    floor: i64,
) -> Option<String> {
    let months: BTreeSet<i64> = parsed.months.iter().copied().collect();
    let (first_date, floor_minute) = local_floor(floor, time_zone);
    let mut date = first_date;
    let horizon = shift_years(first_date, CRON_SEARCH_HORIZON_YEARS).unwrap_or(jiff::civil::Date::MAX);
    // 四位年上界的本地日期即 civil 上界（对它 +1 天不可能表示）。
    let last_date = jiff::civil::Date::MAX.min(horizon);
    while date <= last_date {
        if !months.contains(&(date.month() as i64)) {
            match first_of_next_month(date) {
                Some(next) => date = next,
                None => return None,
            }
            continue;
        }
        if cron_date_matches(parsed, date) {
            for &(hour, minute) in &cron_times(parsed, false) {
                if date == first_date && hour * 60 + minute < floor_minute {
                    continue;
                }
                let time = jiff::civil::time(hour as i8, minute as i8, 0, 0);
                let Some(target) = resolve_local_instant(date, time, time_zone) else {
                    continue;
                };
                if target > max_four_digit_year_ms() {
                    return None;
                }
                if target >= min_four_digit_year_ms() && target > floor {
                    return Some(format_instant(target));
                }
            }
        }
        date = match date.tomorrow() {
            Ok(next) => next,
            Err(_) => break,
        };
    }
    None
}

/// 找决策时刻之前的最新 cron occurrence，不越已承诺下界（上游
/// `latestCronOccurrence`）：回走在地平线先于四位年下界时停——地平线内
/// 无匹配则保住已承诺目标；决策前的候选跳过。
fn latest_cron_occurrence(
    parsed: &CronFields,
    time_zone: &str,
    decision: i64,
    saved_target: i64,
) -> i64 {
    let months: BTreeSet<i64> = parsed.months.iter().copied().collect();
    // 偏移 < 24h：日期线回卷可让到期本地日期晚于 UTC 决策日期，从
    // 决策 UTC 日期 +1 起回走（上游注释同款）。
    let mut date = local_date(decision, "UTC").tomorrow().expect("决策在四位年内恒 +1");
    // 下界是一个 instant：回走可达的最早本地日期是规则时区承载它的
    // 那一天；其下的候选跳过，负偏移仍可解析早于 UTC 下界日期的到期分钟。
    let floor_date = local_date(min_four_digit_year_ms(), time_zone);
    let horizon = shift_years(date, -CRON_SEARCH_HORIZON_YEARS).unwrap_or(jiff::civil::Date::MIN);
    let first_date = floor_date.max(horizon);
    while date >= first_date {
        if !months.contains(&(date.month() as i64)) {
            date = last_of_previous_month(date);
            continue;
        }
        if cron_date_matches(parsed, date) {
            // 该日最早存在墙钟先于决策即整日可能到期；gap 去掉最早
            // 时刻时以第一个存在的时刻为界，全 gap 日保留完整走查。
            let mut earliest: Option<i64> = None;
            for &(hour, minute) in &cron_times(parsed, false) {
                let time = jiff::civil::time(hour as i8, minute as i8, 0, 0);
                earliest = resolve_local_instant(date, time, time_zone);
                if earliest.is_some() {
                    break;
                }
            }
            if earliest.is_some_and(|earliest| earliest > decision) {
                date = match date.yesterday() {
                    Ok(previous) => previous,
                    Err(_) => break,
                };
                continue;
            }
            for &(hour, minute) in &cron_times(parsed, true) {
                let time = jiff::civil::time(hour as i8, minute as i8, 0, 0);
                let Some(candidate) = resolve_local_instant(date, time, time_zone) else {
                    continue;
                };
                if candidate > decision || candidate < min_four_digit_year_ms() {
                    continue;
                }
                // 已承诺目标即使被新 tzdata 提前也保持到期。
                return candidate.max(saved_target);
            }
        }
        date = match date.yesterday() {
            Ok(previous) => previous,
            Err(_) => break,
        };
    }
    saved_target
}

/// 解析后的 Cron 值；表达式与时区独立于任何任务而权威（上游 `Cron`）。
#[derive(Clone, Debug)]
pub struct Cron {
    parsed: CronFields,
    time_zone: String,
}

impl Cron {
    /// 解析一个五字段表达式与其显式 IANA 时区（上游 `Cron.parse`）。
    pub fn parse(expression: &str, time_zone: &str) -> Result<Self, RecurrenceInputError> {
        let parsed = parse_cron_expression(expression)?;
        let zone = match canonicalize_time_zone(time_zone) {
            Ok(zone) => zone,
            Err(_) => {
                return Err(RecurrenceInputError::new(
                    RecurrenceRejection::TimeZone,
                    "Cron requires a valid UTC or IANA Area/Location zone.",
                ));
            }
        };
        Ok(Self { parsed, time_zone: zone })
    }

    /// canonical 五字段表达式，保留 Vixie 星语义。
    pub fn expression(&self) -> &str {
        &self.parsed.expression
    }

    /// 解析后字段值集与 canonical 文本。
    pub fn fields(&self) -> &CronFields {
        &self.parsed
    }

    /// 权威时区（规范拼写）。
    pub fn time_zone(&self) -> &str {
        &self.time_zone
    }

    /// 第一个严格未来 occurrence，跳 gap、overlap 取早（上游 `Cron.next`）。
    ///
    /// # Panics
    ///
    /// 下界不可表示时（上游 `RangeError`）——调用方传已验决策时刻。
    pub fn next(&self, after: i64) -> Option<String> {
        assert!(
            is_four_digit_year_instant(after),
            "Cron lower bound must be a four-digit-year instant"
        );
        next_cron_target(&self.parsed, &self.time_zone, after)
    }

    /// 最新到期 occurrence 与第一个未来 occurrence，不重放积压
    /// （上游 `Cron.resolve`）。
    ///
    /// # Panics
    ///
    /// 决策不可表示或先于既决目标时（上游 `RangeError`）——调用方先验。
    pub fn resolve(&self, scheduled_at: &str, accepted_at: i64) -> RecurringOccurrence {
        let saved_target = recurrence_decision(scheduled_at, accepted_at);
        let occurrence_at =
            format_instant(latest_cron_occurrence(&self.parsed, &self.time_zone, accepted_at, saved_target));
        let next_scheduled_at = self.next(accepted_at);
        RecurringOccurrence { occurrence_at, next_scheduled_at }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(expression: &str, zone: &str) -> Cron {
        Cron::parse(expression, zone).unwrap()
    }

    /// 2026-01-10T00:00:00.000Z（周六）。
    const SAT: i64 = 1_768_003_200_000;

    #[test]
    fn canonical_form_and_value_sets() {
        let cron = parse("*/15 0 1 1 0", "UTC");
        assert_eq!(cron.expression(), "*/15 0 1 1 0");
        assert_eq!(cron.fields().minutes, vec![0, 15, 30, 45]);
        // 周日 7 折叠为 0。
        let cron = parse("0 9 * * 7", "UTC");
        assert_eq!(cron.expression(), "0 9 * * 0");
        // 非均匀列表走 run-length 编码。
        let cron = parse("1,2,5 * * * *", "UTC");
        assert_eq!(cron.expression(), "1-2,5 * * * *");
        // 带步长区间。
        let cron = parse("5-10/2 * * * *", "UTC");
        assert_eq!(cron.fields().minutes, vec![5, 7, 9]);
        assert_eq!(cron.expression(), "5-9/2 * * * *");
        // 星字段保持星形拼写；星步长收敛到最宽走查。
        let cron = parse("* */2 * * *", "UTC");
        assert_eq!(cron.expression(), "* */2 * * *");
        let cron = parse("*/7 * * * *", "UTC");
        assert_eq!(cron.expression(), "*/7 * * * *");
    }

    #[test]
    fn parse_rejections_are_verbatim() {
        // 非 Trimmed / 空串。
        let error = Cron::parse(" 0 9 * * *", "UTC").unwrap_err();
        assert_eq!(error.message, "cron.expression must be a non-empty trimmed string.");
        // 字段数。
        let error = Cron::parse("0 9 * *", "UTC").unwrap_err();
        assert_eq!(
            error.message,
            "cron.expression must contain exactly five whitespace-separated fields: minute hour day-of-month month day-of-week."
        );
        // 畸形元素。
        let error = Cron::parse("x 9 * * *", "UTC").unwrap_err();
        assert_eq!(
            error.message,
            r#"cron.expression minute field element "x" must be *, a value, a-b, */n, a-b/n, or a comma-separated list of those."#
        );
        // 空列表元素。
        let error = Cron::parse("1,,2 * * * *", "UTC").unwrap_err();
        assert_eq!(
            error.message,
            "cron.expression minute field must not contain an empty list element."
        );
        // 值越界。
        let error = Cron::parse("60 9 * * *", "UTC").unwrap_err();
        assert_eq!(error.message, "cron.expression minute field value 60 is outside 0-59.");
        let error = Cron::parse("* * * * 8", "UTC").unwrap_err();
        assert_eq!(error.message, "cron.expression day-of-week field value 8 is outside 0-7.");
        // 倒序区间。
        let error = Cron::parse("10-5 * * * *", "UTC").unwrap_err();
        assert_eq!(error.message, "cron.expression minute field range 10-5 is inverted.");
        // 非正步长。
        let error = Cron::parse("*/0 * * * *", "UTC").unwrap_err();
        assert_eq!(error.message, "cron.expression minute field step must be a positive integer.");
        // 时区。
        let error = Cron::parse("* * * * *", "Mars/Olympus").unwrap_err();
        assert_eq!(error.reason, RecurrenceRejection::TimeZone);
        assert_eq!(error.message, "Cron requires a valid UTC or IANA Area/Location zone.");
    }

    #[test]
    fn next_target_finds_future_occurrence() {
        // 决策周六 15:00Z：14:30 已过 → 明天 14:30。
        let cron = parse("30 14 * * *", "UTC");
        assert_eq!(cron.next(SAT + 15 * 3_600_000).unwrap(), "2026-01-11T14:30:00.000Z");
        // 同日未来时刻。
        let cron = parse("30 14 * * *", "UTC");
        assert_eq!(cron.next(SAT).unwrap(), "2026-01-10T14:30:00.000Z");
        // 周几限制：周六决策 → 周一。
        let cron = parse("0 9 * * 1-5", "UTC");
        assert_eq!(cron.next(SAT).unwrap(), "2026-01-12T09:00:00.000Z");
    }

    #[test]
    fn vixie_day_semantics() {
        // DOM 与 DOW 都受限 → 日或：1 月 13 日（周二）先于周五 16 日。
        let cron = parse("0 9 13 * 5", "UTC");
        assert_eq!(cron.next(SAT).unwrap(), "2026-01-13T09:00:00.000Z");
        // DOW 星 → AND（只有周五）。
        let cron = parse("0 9 * * 5", "UTC");
        assert_eq!(cron.next(SAT).unwrap(), "2026-01-16T09:00:00.000Z");
        // DOM 星步长 `*/2` 仍限制日期（星不免除值集；min=1 起步 → 奇数日）。
        let cron = parse("0 9 */2 * 5", "UTC");
        // 1/16 周五为偶数日 ✗ → 下一个奇数周五 1/23。
        assert_eq!(cron.next(SAT).unwrap(), "2026-01-23T09:00:00.000Z");
    }

    #[test]
    fn dst_gap_is_skipped_and_zone_offsets_resolve() {
        // America/New_York 2026-03-08 02:30 不存在 → 次日 02:30 EDT。
        let now = "2026-03-07T12:00:00.000Z".parse::<jiff::Timestamp>().unwrap().as_millisecond();
        let cron = parse("30 2 * * *", "America/New_York");
        assert_eq!(cron.next(now).unwrap(), "2026-03-09T06:30:00.000Z");
        // 时区偏移换算：东京 09:30 = UTC 同日 00:30（决策周六 00:00Z
        // 即东京本地 09:00，当日 09:30 仍严格未来）。
        let cron = parse("30 9 * * *", "Asia/Tokyo");
        assert_eq!(cron.next(SAT).unwrap(), "2026-01-10T00:30:00.000Z");
    }

    #[test]
    fn resolve_returns_latest_due_and_next() {
        // 目标 09:00（已承诺）；决策 09:05 → 最新到期 09:00、下次明天 09:00。
        let cron = parse("0 9 * * *", "UTC");
        let decision = "2026-01-10T09:05:00.000Z".parse::<jiff::Timestamp>().unwrap().as_millisecond();
        let occurrence = cron.resolve("2026-01-10T09:00:00.000Z", decision);
        assert_eq!(occurrence.occurrence_at, "2026-01-10T09:00:00.000Z");
        assert_eq!(occurrence.next_scheduled_at.unwrap(), "2026-01-11T09:00:00.000Z");
        // 决策 10:30、规则 */30 分钟：最新到期 10:30、下次 11:00。
        let cron = parse("*/30 * * * *", "UTC");
        let decision = "2026-01-10T10:30:00.000Z".parse::<jiff::Timestamp>().unwrap().as_millisecond();
        let occurrence = cron.resolve("2026-01-10T09:00:00.000Z", decision);
        assert_eq!(occurrence.occurrence_at, "2026-01-10T10:30:00.000Z");
        assert_eq!(occurrence.next_scheduled_at.unwrap(), "2026-01-10T11:00:00.000Z");
    }

    #[test]
    fn month_skip_walks_terminate() {
        // 只在 2 月匹配：从 3 月决策 → 明年 2 月 1 日。
        let cron = parse("0 9 * 2 *", "UTC");
        let now = "2026-03-01T00:00:00.000Z".parse::<jiff::Timestamp>().unwrap().as_millisecond();
        assert_eq!(cron.next(now).unwrap(), "2027-02-01T09:00:00.000Z");
        // 回走同样终结（决策 3 月 → 最近到期 2 月 28 日）。
        let decision = "2026-03-15T00:00:00.000Z".parse::<jiff::Timestamp>().unwrap().as_millisecond();
        let occurrence = cron.resolve("2026-02-01T09:00:00.000Z", decision);
        assert_eq!(occurrence.occurrence_at, "2026-02-28T09:00:00.000Z");
    }
}

//! 墙钟 occurrence 评估（A1.4 批②）：daily/weekly 的下一次与最近到期。
//!
//! 对齐上游 `domain.ts` 的 `nextMatchingTarget`/`latestDueOccurrence`/
//! `resolveDailyOccurrence`/`resolveWeeklyOccurrence`：
//! - 「下一次」：从决策时刻的本地日期起逐日推进，匹配周几、墙钟时刻在
//!   时区里真实存在（DST gap 跳过）、严格未来、四位数年范围内。
//! - 「最近到期」：从决策 UTC 日期+1 往回退，找 ≤ 决策时刻的最大真实
//!   到期，但**不低于已承诺目标**（tzdata 更新把规则提前时已承诺的仍算
//!   到期——上游 `Math.max(candidate, savedTarget)`）。
//! - DST 语义：gap（不存在）→ None；overlap（重复）→ 取较早（上游
//!   `disambiguation: 'earlier'`——jiff `Earlier` 同语义）。

use crate::{
    DailyScheduleRecord, ScheduleInputError, ScheduleLogError, WeeklyScheduleRecord,
    local_clock_time, format_instant, is_four_digit_year_instant,
};

/// 四位数年 epoch 毫秒下界（`0001-01-01T00:00:00.000Z`——上游
/// `MIN_FOUR_DIGIT_YEAR_MS`；jiff 解析取得）。
pub fn min_four_digit_year_ms() -> i64 {
    "0001-01-01T00:00:00.000Z"
        .parse::<jiff::Timestamp>()
        .expect("固定串恒可解析")
        .as_millisecond()
}

/// 四位数年 epoch 毫秒上界——上游 `MAX_FOUR_DIGIT_YEAR_MS` 是
/// `9999-12-31T23:59:59.999Z`，但 jiff `Timestamp` 需为最大偏移留尾量，
/// 实际上界 `9999-12-31T22:00:00Z`。**登记偏差**：年内最后 ~2h59m 的
/// 目标在本面视为不可表示（上游可），后续以 jiff 能力观察面登记。
pub fn max_four_digit_year_ms() -> i64 {
    // `Timestamp::MAX` 自带纳秒尾量（ns=999_999_999），`as_millisecond`
    // 截出的值喂不回 `from_millisecond`（毫秒闭界 = 整秒×1000）——取
    // 整秒换算，即 `9999-12-31T22:00:00.000Z`。
    jiff::Timestamp::MAX.as_second() * 1_000
}

/// 一次循环决策的结果（上游 `RecurringOccurrence`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecurringOccurrence {
    /// 决策时刻已到期的最新一现。
    pub occurrence_at: String,
    /// 决策后的第一个未来目标；耗尽（四位数年末日）为 None。
    pub next_scheduled_at: Option<String>,
}

/// 决策时刻是否可表示（上游 `acceptedDecision` 逐字）。
fn accepted_decision(selector: &str, accepted_at: i64) -> Result<i64, ScheduleLogError> {
    if !is_four_digit_year_instant(accepted_at) {
        return Err(ScheduleLogError(format!(
            "{selector} acceptedAt must be a representable four-digit-year instant"
        )));
    }
    Ok(accepted_at)
}

/// epoch → 该时区的本地日历日期（上游 `localDate`）。
fn local_date(epoch_ms: i64, time_zone: &str) -> jiff::civil::Date {
    let timestamp =
        jiff::Timestamp::from_millisecond(epoch_ms).expect("调用方已限四位数年");
    let zone = jiff::tz::TimeZone::get(time_zone).expect("调用方已校验时区");
    timestamp.to_zoned(zone).date()
}

/// 解析 HH:mm:ss.SSS 为 civil time（调用方已归一）。
fn parse_clock(time: &str) -> jiff::civil::Time {
    let (hour, rest) = time.split_once(':').expect("归一形态恒有小时");
    let (minute, second) = rest.split_once(':').expect("归一形态恒有分钟");
    let second: i8 = second.split('.').next().unwrap_or(second).parse().expect("归一形态恒合法");
    jiff::civil::time(
        hour.parse().expect("归一形态恒合法"),
        minute.parse().expect("归一形态恒合法"),
        second,
        0,
    )
}

/// 把「日期 + 时刻」在该时区解析为 epoch 毫秒（上游 `resolveLocalInstant`）。
///
/// 返回 `None` = 墙钟时刻不存在（DST gap）；overlap 取较早。
pub(crate) fn resolve_local_instant(
    date: jiff::civil::Date,
    time: jiff::civil::Time,
    time_zone: &str,
) -> Option<i64> {
    let zone = jiff::tz::TimeZone::get(time_zone).expect("调用方已校验时区");
    // disambiguation Earlier：gap 会前移（见下）、overlap 取早（上游
    // `disambiguation: 'earlier'`——jiff `Earlier` 同语义）。
    let datetime = zone
        .to_ambiguous_zoned(date.to_datetime(time))
        .disambiguate(jiff::tz::Disambiguation::Earlier)
        .ok()?;
    // gap 时 jiff 给出的墙钟时刻与请求不同（前移到 gap 后）——上游语义是
    // 「不等于即不存在」。Earlier 模式下 jiff 对 gap 不调整日期时刻而是
    // 保持 earlier 解释（1:30 在 2:00 跳变中取 1:30-旧偏移）；严格对照
    // 解析回来的墙钟值，不等即 None。
    let round_trip = datetime.with_time_zone(zone.clone());
    if round_trip.hour() != time.hour()
        || round_trip.minute() != time.minute()
        || round_trip.second() != time.second()
    {
        return None;
    }
    Some(datetime.timestamp().as_millisecond())
}

/// 周几匹配器（上游 `WeekdayMatch`；daily 恒 true，weekly 按集合）。
enum WeekdayMatch {
    EveryDate,
    WeekdaySet(std::collections::BTreeSet<u8>),
}

impl WeekdayMatch {
    fn matches(&self, date: jiff::civil::Date) -> bool {
        match self {
            Self::EveryDate => true,
            Self::WeekdaySet(set) => {
                // ISO 周一=1…周日=7（jiff weekday() 给 Monday..Sunday 枚举）。
                let iso = (date.weekday().to_monday_zero_offset() + 1) as u8;
                set.contains(&iso)
            }
        }
    }
}

/// 找决策之后的第一个真实 occurrence（上游 `nextMatchingTarget`）。
fn next_matching_target(
    time: jiff::civil::Time,
    time_zone: &str,
    now_ms: i64,
    matches: &WeekdayMatch,
    after_date: Option<jiff::civil::Date>,
) -> Option<String> {
    let mut date = local_date(now_ms, time_zone);
    // 循环上界直接用 civil 日期上界 9999-12-31；上界日当天不可表示的
    // 目标走 resolve_local_instant 的 None 路径自然跳过。
    let last_date = jiff::civil::Date::MAX;
    if let Some(after) = after_date
        && date <= after
    {
        date = match after.tomorrow() {
            Ok(next) => next,
            // 已有 occurrence 落在年内最后一天：不存在更晚目标
            //（上游 PlainDate.add 此处会 throw；本面回答耗尽）。
            Err(_) => return None,
        };
    }
    while date <= last_date {
        if matches.matches(date) {
            if let Some(target) = resolve_local_instant(date, time, time_zone) {
                if target > max_four_digit_year_ms() {
                    return None;
                }
                if target >= min_four_digit_year_ms() && target > now_ms {
                    return Some(format_instant(target));
                }
            }
        }
        date = match date.tomorrow() {
            Ok(next) => next,
            // 9999-12-31 走完即耗尽（上游同位会 throw；本面回答 None）。
            Err(_) => break,
        };
    }
    None
}

/// 找决策时刻之前的最新真实到期（上游 `latestDueOccurrence`）。
fn latest_due_occurrence(
    time_zone: &str,
    matches: &WeekdayMatch,
    time: jiff::civil::Time,
    accepted_at: i64,
    saved_target: i64,
) -> i64 {
    // 偏移 < 24h：日期线回卷可让当前本地日期早于到期日期，从 UTC 日期+1
    // 起往回走（上游注释同款）。
    let mut date = local_date(accepted_at, "UTC").tomorrow().expect("四位数年内恒 +1");
    loop {
        if matches.matches(date) {
            if let Some(candidate) = resolve_local_instant(date, time, time_zone) {
                if candidate <= accepted_at {
                    // 已承诺目标即使被新 tzdata 提前也保持到期。
                    return candidate.max(saved_target);
                }
            }
        }
        date = date.yesterday().expect("往回走必命中（决策 ≥ 已承诺 ≥ 某个真实时刻）");
    }
}

/// daily 决策（上游 `resolveDailyOccurrence`）。
pub fn resolve_daily_occurrence(
    record: &DailyScheduleRecord,
    accepted_at: i64,
) -> Result<RecurringOccurrence, ScheduleLogError> {
    let decision = accepted_decision("daily", accepted_at)?;
    let saved_target: i64 = record
        .scheduled_at
        .parse::<jiff::Timestamp>()
        .map_err(|_| ScheduleLogError("daily scheduledAt 不可解析".into()))?
        .as_millisecond();
    if decision < saved_target {
        return Err(ScheduleLogError(
            "daily dispatch cannot precede the active scheduledAt".into(),
        ));
    }
    let time = parse_clock(&record.time);
    let occurrence =
        latest_due_occurrence(&record.time_zone, &WeekdayMatch::EveryDate, time, decision, saved_target);
    let occurrence_at = format_instant(occurrence);
    let next = next_matching_target(
        time,
        &record.time_zone,
        decision,
        &WeekdayMatch::EveryDate,
        Some(local_date(occurrence, &record.time_zone)),
    );
    Ok(RecurringOccurrence {
        occurrence_at,
        next_scheduled_at: next,
    })
}

/// weekly 决策（上游 `resolveWeeklyOccurrence`）。
pub fn resolve_weekly_occurrence(
    record: &WeeklyScheduleRecord,
    accepted_at: i64,
) -> Result<RecurringOccurrence, ScheduleLogError> {
    let decision = accepted_decision("weekly", accepted_at)?;
    let saved_target: i64 = record
        .scheduled_at
        .parse::<jiff::Timestamp>()
        .map_err(|_| ScheduleLogError("weekly scheduledAt 不可解析".into()))?
        .as_millisecond();
    if decision < saved_target {
        return Err(ScheduleLogError(
            "weekly dispatch cannot precede the active scheduledAt".into(),
        ));
    }
    let time = parse_clock(&record.time);
    let set: std::collections::BTreeSet<u8> =
        record.weekdays.iter().copied().collect();
    let matches = WeekdayMatch::WeekdaySet(set);
    let occurrence =
        latest_due_occurrence(&record.time_zone, &matches, time, decision, saved_target);
    let occurrence_at = format_instant(occurrence);
    let next = next_matching_target(
        time,
        &record.time_zone,
        decision,
        &matches,
        Some(local_date(occurrence, &record.time_zone)),
    );
    Ok(RecurringOccurrence {
        occurrence_at,
        next_scheduled_at: next,
    })
}

/// every（固定间隔）决策（上游 `resolveEveryOccurrence`）。
pub fn resolve_every_occurrence(
    scheduled_at: &str,
    every_seconds: u64,
    accepted_at: i64,
) -> Result<RecurringOccurrence, ScheduleLogError> {
    let target: i64 = scheduled_at
        .parse::<jiff::Timestamp>()
        .map_err(|_| ScheduleLogError("every scheduledAt 不可解析".into()))?
        .as_millisecond();
    if !is_four_digit_year_instant(accepted_at) {
        return Err(ScheduleLogError(
            "every acceptedAt must be a representable four-digit-year instant".into(),
        ));
    }
    let interval = (every_seconds as i64) * 1_000;
    if accepted_at < target {
        return Err(ScheduleLogError(
            "every dispatch cannot precede the active scheduledAt".into(),
        ));
    }
    let steps = (accepted_at - target) / interval;
    let occurrence = target + steps * interval;
    let occurrence_at = format_instant(occurrence);
    let next = occurrence + interval;
    let next_scheduled_at =
        is_four_digit_year_instant(next).then(|| format_instant(next));
    Ok(RecurringOccurrence { occurrence_at, next_scheduled_at })
}

/// daily/weekly 的下一次目标（创建/重排用；上游 `nextDailyTarget`/
/// `nextWeeklyTarget` 的公开形态）。
pub fn next_daily_target(
    time: &str,
    time_zone: &str,
    now_ms: i64,
) -> Result<Option<String>, ScheduleInputError> {
    let normalized = local_clock_time(time, "daily")?;
    Ok(next_matching_target(
        parse_clock(&normalized),
        time_zone,
        now_ms,
        &WeekdayMatch::EveryDate,
        None,
    ))
}

/// weekly 的下一次目标。
pub fn next_weekly_target(
    time: &str,
    time_zone: &str,
    weekdays: &[u8],
    now_ms: i64,
) -> Result<Option<String>, ScheduleInputError> {
    let normalized = local_clock_time(time, "weekly")?;
    let set: std::collections::BTreeSet<u8> = weekdays.iter().copied().collect();
    Ok(next_matching_target(
        parse_clock(&normalized),
        time_zone,
        now_ms,
        &WeekdayMatch::WeekdaySet(set),
        None,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn daily(time: &str, zone: &str, scheduled_at: &str) -> DailyScheduleRecord {
        DailyScheduleRecord {
            id: "t1".into(),
            title: "t".into(),
            prompt: "p".into(),
            time: time.into(),
            time_zone: zone.into(),
            scheduled_at: scheduled_at.into(),
        }
    }

    fn weekly(time: &str, zone: &str, weekdays: Vec<u8>, scheduled_at: &str) -> WeeklyScheduleRecord {
        WeeklyScheduleRecord {
            id: "t1".into(),
            title: "t".into(),
            prompt: "p".into(),
            time: time.into(),
            time_zone: zone.into(),
            weekdays,
            scheduled_at: scheduled_at.into(),
        }
    }

    /// 2026-01-10T00:00:00.000Z（周六）。
    const SAT: i64 = 1_768_003_200_000;

    #[test]
    fn daily_next_target_finds_tomorrow_when_today_passed() {
        // 决策时刻已过当天 09:00 UTC：下一次在明天。
        let now = "2026-01-10T10:00:00.000Z".parse::<jiff::Timestamp>().unwrap().as_millisecond();
        let next = next_daily_target("09:00:00", "UTC", now).unwrap().unwrap();
        assert_eq!(next, "2026-01-11T09:00:00.000Z");
    }

    #[test]
    fn daily_next_target_same_day_when_future() {
        let now = "2026-01-10T08:00:00.000Z".parse::<jiff::Timestamp>().unwrap().as_millisecond();
        let next = next_daily_target("09:00:00", "UTC", now).unwrap().unwrap();
        assert_eq!(next, "2026-01-10T09:00:00.000Z");
    }

    #[test]
    fn weekly_next_target_skips_non_selected_days() {
        // 决策周六；周几集 {1, 3}（周一、周三）→ 下一个是周一。
        let next = next_weekly_target("09:00:00", "UTC", &[1, 3], SAT).unwrap().unwrap();
        assert_eq!(next, "2026-01-12T09:00:00.000Z");
    }

    #[test]
    fn dst_gap_local_time_is_skipped() {
        // America/New_York 2026-03-08 02:30 不存在（02:00→03:00 跳变）。
        // 规则 02:30 每日：当天无 occurrence，跳到次日 02:30。
        // 决策时刻选 03-07 深夜，本地日期已是 03-08。
        let now = "2026-03-07T12:00:00.000Z".parse::<jiff::Timestamp>().unwrap().as_millisecond();
        let next = next_daily_target("02:30:00", "America/New_York", now)
            .unwrap()
            .unwrap();
        // 次日 03-09 02:30 EDT = UTC 06:30。
        assert_eq!(next, "2026-03-09T06:30:00.000Z");
    }

    #[test]
    fn every_occurrence_advances_by_interval() {
        // 目标 09:00、间隔 60s、决策 09:02:30 → 最新到期 09:02、下次 09:03。
        let scheduled = "2026-01-10T09:00:00.000Z";
        let accepted = "2026-01-10T09:02:30.000Z".parse::<jiff::Timestamp>().unwrap().as_millisecond();
        let result = resolve_every_occurrence(scheduled, 60, accepted).unwrap();
        assert_eq!(result.occurrence_at, "2026-01-10T09:02:00.000Z");
        assert_eq!(result.next_scheduled_at.unwrap(), "2026-01-10T09:03:00.000Z");
    }

    #[test]
    fn daily_decision_resolves_latest_due_and_next() {
        // scheduledAt 09:00（已承诺）；决策 09:05 → 最新到期 09:00（今天）、
        // 下一次明天 09:00。
        let record = daily("09:00:00", "UTC", "2026-01-10T09:00:00.000Z");
        let accepted = "2026-01-10T09:05:00.000Z".parse::<jiff::Timestamp>().unwrap().as_millisecond();
        let result = resolve_daily_occurrence(&record, accepted).unwrap();
        assert_eq!(result.occurrence_at, "2026-01-10T09:00:00.000Z");
        assert_eq!(result.next_scheduled_at.unwrap(), "2026-01-11T09:00:00.000Z");
    }

    #[test]
    fn daily_decision_before_saved_target_is_rejected() {
        let record = daily("09:00:00", "UTC", "2026-01-10T09:00:00.000Z");
        let early = "2026-01-10T08:00:00.000Z".parse::<jiff::Timestamp>().unwrap().as_millisecond();
        let error = resolve_daily_occurrence(&record, early).unwrap_err();
        assert_eq!(
            error.0,
            "daily dispatch cannot precede the active scheduledAt"
        );
    }

    #[test]
    fn weekly_decision_resolves_latest_due() {
        // 周几 {6}（周六）、09:00 UTC；决策 2026-01-10（周六）12:00 →
        // 最新到期当天 09:00、下一次 2026-01-17（下周六）。
        let record = weekly("09:00:00", "UTC", vec![6], "2026-01-10T09:00:00.000Z");
        let accepted = "2026-01-10T12:00:00.000Z".parse::<jiff::Timestamp>().unwrap().as_millisecond();
        let result = resolve_weekly_occurrence(&record, accepted).unwrap();
        assert_eq!(result.occurrence_at, "2026-01-10T09:00:00.000Z");
        assert_eq!(result.next_scheduled_at.unwrap(), "2026-01-17T09:00:00.000Z");
    }

    #[test]
    fn saved_target_stays_due_under_tzdata_rollback() {
        // 已承诺 09:00；tzdata 把 09:00 解释得「更早」也不能低于承诺。
        // 构造：decision 正好 = saved_target → 最新到期 = saved_target
        //（latest_due_occurrence 的 max 语义）。
        let record = daily("09:00:00", "UTC", "2026-01-10T09:00:00.000Z");
        let accepted = "2026-01-10T09:00:00.000Z".parse::<jiff::Timestamp>().unwrap().as_millisecond();
        let result = resolve_daily_occurrence(&record, accepted).unwrap();
        assert_eq!(result.occurrence_at, "2026-01-10T09:00:00.000Z");
    }
}
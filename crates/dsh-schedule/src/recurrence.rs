//! Cron / RRule 共享的循环规则验证与决策词表（A1.4 批②③）。
//!
//! 对齐上游 `dsh-util-time` 的 `recurrence.ts`：解析失败以拒绝原因闭集
//! （syntax / time_zone / density）承载，不携带消费方错误码；决策校验
//! 以既决目标为下界（上游抛 `RangeError`——本面由调用方先验，越约即
//! 前提破坏）。

/// 为什么循环内容不能被有界引擎评估（上游 `RecurrenceRejection`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecurrenceRejection {
    Syntax,
    TimeZone,
    Density,
}

/// 内部验证失败，被公开解析器收容（上游 `RecurrenceInputError`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecurrenceInputError {
    /// 拒绝原因闭集。
    pub reason: RecurrenceRejection,
    /// 逐字诊断。
    pub message: String,
}

impl RecurrenceInputError {
    pub(crate) fn new(reason: RecurrenceRejection, message: impl Into<String>) -> Self {
        Self { reason, message: message.into() }
    }
}

impl std::fmt::Display for RecurrenceInputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

/// canonical RFC 3339 UTC 串 → epoch 毫秒（上游 `fourDigitYearEpoch` 的
/// 接受面：形过 + 日历存在 + jiff 可表示；9999 尾段为登记偏差返回 None）。
pub(crate) fn four_digit_year_epoch(value: &str) -> Option<i64> {
    if !crate::parse::matches_utc_instant_shape(value) {
        return None;
    }
    let year: i64 = value[0..4].parse().expect("形已校验数位");
    let month: i64 = value[5..7].parse().expect("形已校验数位");
    let day: i64 = value[8..10].parse().expect("形已校验数位");
    if jiff::civil::Date::new(year as i16, month as i8, day as i8).is_err() {
        return None;
    }
    let parsed: jiff::Timestamp = value.parse().ok()?;
    Some(parsed.as_millisecond())
}

/// 决策时刻与既决下界校验（上游 `recurrenceDecision`——上游抛
/// `RangeError`；本面调用方已先验同一条件，越约即前提破坏）。
pub(crate) fn recurrence_decision(scheduled_at: &str, accepted_at: i64) -> i64 {
    let saved_target = four_digit_year_epoch(scheduled_at)
        .expect("Recurrence decision must be representable and at or after its committed target");
    assert!(
        crate::is_four_digit_year_instant(accepted_at) && accepted_at >= saved_target,
        "Recurrence decision must be representable and at or after its committed target"
    );
    saved_target
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_digit_year_epoch_accepts_canonical_only() {
        assert_eq!(four_digit_year_epoch("2026-01-10T09:00:00.000Z"), Some(1_768_035_600_000));
        assert_eq!(four_digit_year_epoch("2026-01-10T09:00:00Z"), None);
        assert_eq!(four_digit_year_epoch("2026-02-30T09:00:00.000Z"), None);
        assert_eq!(four_digit_year_epoch("0000-01-10T09:00:00.000Z"), None);
    }

    #[test]
    fn recurrence_decision_holds_floor() {
        assert_eq!(
            recurrence_decision("2026-01-10T09:00:00.000Z", 1_768_035_900_000),
            1_768_035_600_000
        );
    }

    #[test]
    #[should_panic(expected = "Recurrence decision must be representable")]
    fn recurrence_decision_panics_below_floor() {
        recurrence_decision("2026-01-10T09:00:00.000Z", 1_768_035_300_000);
    }
}

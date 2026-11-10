//! 严格 instant 与本地日历词表（A1.4 批②②）。
//!
//! 对齐上游 `dsh-util-time` 的 `instant.ts`：所有读不可信文本的入口回答
//! 拒绝原因闭集而非直接抛错，由调用方映射到各自领域码与诊断（逐字文案
//! 在 `record.rs` / `lib.rs` 的映射层）。jiff 承载 Temporal 语义。

/// 为什么一个 RFC 3339 instant 串不可用（上游 `InstantRejection`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstantRejection {
    NotRfc3339,
    OutOfRange,
    NotACalendarDate,
    InvalidNumericOffset,
}

/// 为什么一个本地日历选择器不可用（上游 `LocalCalendarRejection`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalCalendarRejection {
    Malformed,
    OutOfRange,
    NotACalendarDate,
}

/// 字节段是否全为 ASCII 数字。
fn is_digits(bytes: &[u8]) -> bool {
    bytes.iter().all(|b| b.is_ascii_digit())
}

/// 读定长 ASCII 数字段为数值（调用方已校验形与位数）。
fn digits(bytes: &[u8]) -> i64 {
    let text = std::str::from_utf8(bytes).expect("调用方已校验 ASCII");
    text.parse::<i64>().expect("调用方已校验数位")
}

/// 规范四位年 RFC 3339 UTC instant：形如 `YYYY-MM-DDTHH:MM:SS.mmmZ`，
/// 年非 0000、月 01-12、日 01-31、时 00-23、分/秒 00-59、毫秒恰 3 位
/// （上游 `UTC_INSTANT` 正则的等价判定）。
pub(crate) fn matches_utc_instant_shape(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 24
        || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T'
        || bytes[13] != b':' || bytes[16] != b':' || bytes[19] != b'.'
        || bytes[23] != b'Z'
    {
        return false;
    }
    if !is_digits(&bytes[0..4]) || !is_digits(&bytes[5..7]) || !is_digits(&bytes[8..10])
        || !is_digits(&bytes[11..13]) || !is_digits(&bytes[14..16]) || !is_digits(&bytes[17..19])
        || !is_digits(&bytes[20..23])
    {
        return false;
    }
    let (month, day) = (digits(&bytes[5..7]), digits(&bytes[8..10]));
    let (hour, minute, second) = (
        digits(&bytes[11..13]),
        digits(&bytes[14..16]),
        digits(&bytes[17..19]),
    );
    &value[0..4] != "0000"
        && (1..=12).contains(&month)
        && (1..=31).contains(&day)
        && hour <= 23
        && minute <= 59
        && second <= 59
}

/// 本地日历日期 `YYYY-MM-DD`（上游 `LOCAL_DATE` 形）。
fn parse_local_date_shape(value: &str) -> Option<(i64, i64, i64)> {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    if !is_digits(&bytes[0..4]) || !is_digits(&bytes[5..7]) || !is_digits(&bytes[8..10]) {
        return None;
    }
    Some((digits(&bytes[0..4]), digits(&bytes[5..7]), digits(&bytes[8..10])))
}

/// 本地墙钟时刻 `HH:MM:SS` 可带 1-3 位小数（上游 `LOCAL_TIME` 形）。
fn parse_local_time_shape(value: &str) -> Option<(i64, i64, i64, i64)> {
    let bytes = value.as_bytes();
    if bytes.len() < 8 || bytes[2] != b':' || bytes[5] != b':' {
        return None;
    }
    let millisecond = if bytes.len() == 8 {
        0i64
    } else {
        let len = bytes.len();
        if len < 10 || len > 12 || bytes[8] != b'.' || !is_digits(&bytes[9..len]) {
            return None;
        }
        let mut ms = digits(&bytes[9..len]);
        for _ in (len - 9)..3 {
            ms *= 10;
        }
        ms
    };
    if !is_digits(&bytes[0..2]) || !is_digits(&bytes[3..5]) || !is_digits(&bytes[6..8]) {
        return None;
    }
    Some((digits(&bytes[0..2]), digits(&bytes[3..5]), digits(&bytes[6..8]), millisecond))
}

/// 严格读一个本地日历选择器，不咨询进程时区（上游 `parseLocalCalendar`）。
///
/// 形不合法、不存在于日历的日期（如 2 月 30 日）与超范围钟字段回答不同原因。
pub fn parse_local_calendar(
    date: &str,
    time: &str,
) -> Result<(jiff::civil::Date, jiff::civil::Time), LocalCalendarRejection> {
    let Some((year, month, day)) = parse_local_date_shape(date) else {
        return Err(LocalCalendarRejection::Malformed);
    };
    let Some((hour, minute, second, millisecond)) = parse_local_time_shape(time) else {
        return Err(LocalCalendarRejection::Malformed);
    };
    if year == 0 || hour > 23 || minute > 59 || second > 59 {
        return Err(LocalCalendarRejection::OutOfRange);
    }
    // 形与范围已过，剩下的拒绝只有日历上不存在的日期。
    let date = jiff::civil::Date::new(year as i16, month as i8, day as i8)
        .map_err(|_| LocalCalendarRejection::NotACalendarDate)?;
    let time = jiff::civil::time(
        hour as i8,
        minute as i8,
        second as i8,
        millisecond as i32 * 1_000_000,
    );
    Ok((date, time))
}

/// civil 字段 → epoch 毫秒（Hinnant days_from_civil 算术；不经 jiff
/// `Timestamp`，9999 年尾段也可精确计算——表示范围检查留在调用方）。
pub fn civil_to_epoch_ms(
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
    millisecond: i64,
) -> i64 {
    fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
        let y = if m <= 2 { y - 1 } else { y };
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = y - era * 400;
        let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146097 + doe - 719468
    }
    days_from_civil(year, month, day) * 86_400_000
        + (hour * 3_600 + minute * 60 + second) * 1_000
        + millisecond
}

/// 解析一个数字偏移在输入内的 RFC 3339 instant（上游 `parseOffsetInstant`）。
///
/// 裸 `Z` 与显式数字偏移都收，偏移从本地墙钟值中扣除。负零偏移、偏移
/// 时/分超范围、不存在于日历的日期各有独立拒绝。
pub fn parse_offset_instant(value: &str) -> Result<i64, InstantRejection> {
    let bytes = value.as_bytes();
    // 基座 19 字节（date T time），小数 1-4 字节，区 1（Z）或 6（±HH:MM）字节。
    if bytes.len() < 20
        || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T'
        || bytes[13] != b':' || bytes[16] != b':'
        || !is_digits(&bytes[0..4]) || !is_digits(&bytes[5..7]) || !is_digits(&bytes[8..10])
        || !is_digits(&bytes[11..13]) || !is_digits(&bytes[14..16]) || !is_digits(&bytes[17..19])
    {
        return Err(InstantRejection::NotRfc3339);
    }
    let mut cursor = 19usize;
    let mut millisecond = 0i64;
    if bytes.get(cursor) == Some(&b'.') {
        cursor += 1;
        let start = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() && cursor - start < 3 {
            cursor += 1;
        }
        let frac_len = cursor - start;
        if frac_len == 0 {
            return Err(InstantRejection::NotRfc3339);
        }
        millisecond = digits(&bytes[start..cursor]);
        for _ in frac_len..3 {
            millisecond *= 10;
        }
    }
    #[derive(Clone, Copy)]
    enum Zone {
        Utc,
        Offset { direction: i64, hour: i64, minute: i64 },
    }
    let zone = match bytes.get(cursor) {
        Some(b'Z') => Zone::Utc,
        Some(&sign) if sign == b'+' || sign == b'-' => {
            let Some(rest) = bytes.get(cursor + 1..cursor + 6) else {
                return Err(InstantRejection::NotRfc3339);
            };
            if rest[2] != b':' || !is_digits(&rest[0..2]) || !is_digits(&rest[3..5]) {
                return Err(InstantRejection::NotRfc3339);
            }
            Zone::Offset {
                direction: if sign == b'+' { 1 } else { -1 },
                hour: digits(&rest[0..2]),
                minute: digits(&rest[3..5]),
            }
        }
        _ => return Err(InstantRejection::NotRfc3339),
    };
    let expected_len = cursor + if matches!(zone, Zone::Utc) { 1 } else { 6 };
    if bytes.len() != expected_len {
        return Err(InstantRejection::NotRfc3339);
    }
    let year = digits(&bytes[0..4]);
    let month = digits(&bytes[5..7]);
    let day = digits(&bytes[8..10]);
    let (hour, minute, second) = (
        digits(&bytes[11..13]),
        digits(&bytes[14..16]),
        digits(&bytes[17..19]),
    );
    if year == 0 || hour > 23 || minute > 59 || second > 59 {
        return Err(InstantRejection::OutOfRange);
    }
    // 形与范围已过，剩下的拒绝只有日历上不存在的日期。
    if jiff::civil::Date::new(year as i16, month as i8, day as i8).is_err() {
        return Err(InstantRejection::NotACalendarDate);
    }
    let local_epoch = civil_to_epoch_ms(year, month, day, hour, minute, second, millisecond);
    match zone {
        Zone::Utc => Ok(local_epoch),
        Zone::Offset { direction, hour, minute } => {
            if hour > 23 || minute > 59 || (direction == -1 && hour == 0 && minute == 0) {
                return Err(InstantRejection::InvalidNumericOffset);
            }
            Ok(local_epoch - direction * (hour * 60 + minute) * 60_000)
        }
    }
}

/// IANA 形前置：`Area/Location` 样两段以上标识符（上游 `IANA_TIME_ZONE`
/// 正则的等价判定——缩写、数字偏移、单段名在此拒绝；`UTC` 不走此判）。
pub(crate) fn iana_zone_shape(value: &str) -> bool {
    let segments: Vec<&str> = value.split('/').collect();
    if segments.len() < 2 || segments.iter().any(|segment| segment.is_empty()) {
        return false;
    }
    let in_charset = |text: &str| {
        text.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'+' || b == b'.' || b == b'-')
    };
    let first = segments[0];
    first.bytes().next().is_some_and(|b| b.is_ascii_alphabetic())
        && in_charset(first)
        && segments[1..].iter().all(|segment| in_charset(segment))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_instant_shape_matches_upstream_profile() {
        assert!(matches_utc_instant_shape("2026-01-02T03:04:05.000Z"));
        assert!(matches_utc_instant_shape("0001-01-01T00:00:00.000Z"));
        // 0000 年拒。
        assert!(!matches_utc_instant_shape("0000-01-02T03:04:05.000Z"));
        // 月 13 与 00 拒。
        assert!(!matches_utc_instant_shape("2026-13-02T03:04:05.000Z"));
        assert!(!matches_utc_instant_shape("2026-00-02T03:04:05.000Z"));
        // 时 24 拒。
        assert!(!matches_utc_instant_shape("2026-01-02T24:04:05.000Z"));
        // 秒精度（无 .mmm）拒。
        assert!(!matches_utc_instant_shape("2026-01-02T03:04:05Z"));
        // 2 位小数拒。
        assert!(!matches_utc_instant_shape("2026-01-02T03:04:05.00Z"));
        // 偏移形拒（canonical 只收 Z）。
        assert!(!matches_utc_instant_shape("2026-01-02T03:04:05.000+00:00"));
    }

    #[test]
    fn local_calendar_rejections_are_distinct() {
        // malformed：形不合法。
        assert_eq!(
            parse_local_calendar("2026-1-02", "03:04:05"),
            Err(LocalCalendarRejection::Malformed)
        );
        assert_eq!(
            parse_local_calendar("2026-01-02", "03:04"),
            Err(LocalCalendarRejection::Malformed)
        );
        assert_eq!(
            parse_local_calendar("2026-01-02", "03:04:05.1234"),
            Err(LocalCalendarRejection::Malformed)
        );
        // out_of_range：钟字段越界（年 0 / 时 24 / 秒 60）。
        assert_eq!(
            parse_local_calendar("0000-01-02", "03:04:05"),
            Err(LocalCalendarRejection::OutOfRange)
        );
        assert_eq!(
            parse_local_calendar("2026-01-02", "24:04:05"),
            Err(LocalCalendarRejection::OutOfRange)
        );
        assert_eq!(
            parse_local_calendar("2026-01-02", "03:04:60"),
            Err(LocalCalendarRejection::OutOfRange)
        );
        // not_a_calendar_date：2 月 30 / 月 13 / 日 0。
        assert_eq!(
            parse_local_calendar("2026-02-30", "03:04:05"),
            Err(LocalCalendarRejection::NotACalendarDate)
        );
        assert_eq!(
            parse_local_calendar("2026-13-02", "03:04:05"),
            Err(LocalCalendarRejection::NotACalendarDate)
        );
        assert_eq!(
            parse_local_calendar("2026-01-00", "03:04:05"),
            Err(LocalCalendarRejection::NotACalendarDate)
        );
        // 正常：小数 1-3 位归一到毫秒。
        let (date, time) = parse_local_calendar("2026-01-02", "03:04:05.5").unwrap();
        assert_eq!(time.millisecond(), 500);
        let (_, time) = parse_local_calendar("2026-01-02", "03:04:05.123").unwrap();
        assert_eq!(time.millisecond(), 123);
        assert_eq!(date.to_string(), "2026-01-02");
    }

    #[test]
    fn civil_to_epoch_ms_agrees_with_jiff() {
        assert_eq!(civil_to_epoch_ms(1970, 1, 1, 0, 0, 0, 0), 0);
        let epoch = "2026-01-10T12:34:56.789Z"
            .parse::<jiff::Timestamp>()
            .unwrap()
            .as_millisecond();
        assert_eq!(civil_to_epoch_ms(2026, 1, 10, 12, 34, 56, 789), epoch);
        let epoch = "0001-01-01T00:00:00.000Z"
            .parse::<jiff::Timestamp>()
            .unwrap()
            .as_millisecond();
        assert_eq!(civil_to_epoch_ms(1, 1, 1, 0, 0, 0, 0), epoch);
        // 9999 年尾段（jiff Timestamp 不可表示）仍可精确计算。
        assert_eq!(civil_to_epoch_ms(9999, 12, 31, 23, 59, 59, 999), 253_402_300_799_999);
    }

    #[test]
    fn offset_instant_accepts_z_and_numeric_offsets() {
        assert_eq!(
            parse_offset_instant("2026-01-10T09:30:00Z").unwrap(),
            civil_to_epoch_ms(2026, 1, 10, 9, 30, 0, 0)
        );
        // +05:30：本地 15:00 → UTC 09:30。
        assert_eq!(
            parse_offset_instant("2026-01-10T15:00:00+05:30").unwrap(),
            civil_to_epoch_ms(2026, 1, 10, 9, 30, 0, 0)
        );
        // -08:00：本地 01:30 → UTC 09:30。
        assert_eq!(
            parse_offset_instant("2026-01-10T01:30:00-08:00").unwrap(),
            civil_to_epoch_ms(2026, 1, 10, 9, 30, 0, 0)
        );
        // 小数 1 位归一毫秒。
        assert_eq!(
            parse_offset_instant("2026-01-10T09:30:00.5Z").unwrap(),
            civil_to_epoch_ms(2026, 1, 10, 9, 30, 0, 500)
        );
    }

    #[test]
    fn offset_instant_rejections_are_distinct() {
        // not_rfc3339：缺区 / 4 位小数 / 尾余。
        assert_eq!(
            parse_offset_instant("2026-01-10T09:30:00"),
            Err(InstantRejection::NotRfc3339)
        );
        assert_eq!(
            parse_offset_instant("2026-01-10T09:30:00.1234Z"),
            Err(InstantRejection::NotRfc3339)
        );
        assert_eq!(
            parse_offset_instant("2026-01-10T09:30:00Zx"),
            Err(InstantRejection::NotRfc3339)
        );
        // out_of_range：年 0 / 时 24 / 秒 60。
        assert_eq!(
            parse_offset_instant("0000-01-10T09:30:00Z"),
            Err(InstantRejection::OutOfRange)
        );
        assert_eq!(
            parse_offset_instant("2026-01-10T24:30:00Z"),
            Err(InstantRejection::OutOfRange)
        );
        assert_eq!(
            parse_offset_instant("2026-01-10T09:30:60Z"),
            Err(InstantRejection::OutOfRange)
        );
        // not_a_calendar_date：2 月 30。
        assert_eq!(
            parse_offset_instant("2026-02-30T09:30:00Z"),
            Err(InstantRejection::NotACalendarDate)
        );
        // invalid_numeric_offset：负零偏移 / 偏移时 24 / 偏移分 60。
        assert_eq!(
            parse_offset_instant("2026-01-10T09:30:00-00:00"),
            Err(InstantRejection::InvalidNumericOffset)
        );
        assert_eq!(
            parse_offset_instant("2026-01-10T09:30:00+24:00"),
            Err(InstantRejection::InvalidNumericOffset)
        );
        assert_eq!(
            parse_offset_instant("2026-01-10T09:30:00+05:60"),
            Err(InstantRejection::InvalidNumericOffset)
        );
    }

    #[test]
    fn iana_zone_shape_gates_abbreviations_and_offsets() {
        assert!(iana_zone_shape("Asia/Shanghai"));
        assert!(iana_zone_shape("Etc/GMT+5"));
        assert!(iana_zone_shape("America/Argentina/Buenos_Aires"));
        // 单段 / 缩写 / 数字偏移 / 空段 / 数字开头拒。
        assert!(!iana_zone_shape("UTC"));
        assert!(!iana_zone_shape("EST5EDT"));
        assert!(!iana_zone_shape("+05:30"));
        assert!(!iana_zone_shape("Asia/"));
        assert!(!iana_zone_shape("/Shanghai"));
        assert!(!iana_zone_shape("1abc/def"));
    }
}

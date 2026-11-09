//! 时间上下文注入（上游 `context/time-context` 的等价面）。
//!
//! 每次注入是**一条持久化的 user-role 上下文消息**（source =
//! `{kind: plugin, plugin: "time-context", form: "snapshot", sections: [...]}`），
//! 文本三行（上游逐字）：
//!
//! ```text
//! Time sampled while preparing turn {turn}, step {step}: {iso}[{zone}]
//! Browser time zone for this request: {zone}. Interpret otherwise-unqualified dates and times in this zone.
//! Elapsed since the preceding {baseline}: {duration}.
//! ```
//!
//! 基线：step 1 = 上一条模型可见消息；之后 = 本轮上一个时间上下文。
//! 缺失基线报 `unavailable`；时钟回拨按 0 clamp。默认 10 分钟节流
//! （上游 `refreshIntervalMs` 默认 600000）。

/// 注入节流配置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimeContextConfig {
    /// 两次注入之间的最小间隔（毫秒；0 = 每个合格的步都注入）。
    pub refresh_interval_ms: u64,
    /// 展示时区（IANA 名；None = 进程本地时区）。
    pub time_zone: Option<String>,
}

impl Default for TimeContextConfig {
    fn default() -> Self {
        Self { refresh_interval_ms: 600_000, time_zone: None }
    }
}

/// 注入状态（agent 持有）：配置 + 上次注入 / 上次读数时刻。
#[derive(Clone, Debug)]
pub struct TimeContextState {
    pub config: TimeContextConfig,
    /// 上次注入的 epoch ms（节流用）。
    pub last_injection_ms: Option<u64>,
    /// 上次读数的 epoch ms（step > 1 的 elapsed 基线）。
    pub last_reading_ms: Option<u64>,
}

/// 一次注入的读数。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimeReading {
    /// 格式化后的时间戳：`YYYY-MM-DDTHH:MM:SS±HH:MM[zone]`。
    pub timestamp: String,
    /// 本轮展示的时区标签（时间戳方括号内同一个）。
    pub zone: String,
}

/// 本地时区标签：`TZ` 给了 IANA 名就用它，否则用 `Etc/GMT±N` 这类合法 IANA
/// 固定偏移名（Windows 拿不到 IANA 名，宁可用真偏移也不编造城市名）。
pub fn local_zone_label(offset_seconds: i32) -> String {
    if let Ok(tz) = std::env::var("TZ") {
        let tz = tz.trim().to_string();
        if !tz.is_empty() {
            return tz;
        }
    }
    let hours = offset_seconds / 3600;
    // IANA 的 Etc/GMT 符号与常识相反：UTC+8 = Etc/GMT-8、UTC-5 = Etc/GMT+5
    if hours == 0 {
        return "Etc/GMT".to_string();
    }
    if hours > 0 {
        format!("Etc/GMT-{hours}")
    } else {
        format!("Etc/GMT+{}", -hours)
    }
}

/// 用固定偏移渲染时间戳（上游 `Intl.DateTimeFormat` + `longOffset` 的等价）。
pub fn format_timestamp(utc_secs: i64, offset_seconds: i32, zone: &str) -> String {
    let local_secs = utc_secs + offset_seconds as i64;
    let (y, mo, d, h, mi, s) = civil_from_unix(local_secs);
    let sign = if offset_seconds < 0 { '-' } else { '+' };
    let abs = offset_seconds.abs();
    format!(
        "{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}{sign}{:02}:{:02}[{zone}]",
        abs / 3600,
        (abs % 3600) / 60
    )
}

/// 秒 → 公历年月日时分秒（Howard Hinnant 的 days-to-civil 算法）。
fn civil_from_unix(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (h, mi, s) = (
        (tod / 3600) as u32,
        ((tod % 3600) / 60) as u32,
        (tod % 60) as u32,
    );
    // 1970-01-01 起的日数 → (y, m, d)
    let z = days + 719_468;
    let era = if z >= 0 { z / 146_097 } else { (z - 146_096) / 146_097 };
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d, h, mi, s)
}

/// 紧凑整秒时长（上游 `formatDuration`：`1d 2h 3m 4s`，只列非零高位 + 秒）。
pub fn format_duration(ms: u64) -> String {
    let total = ms / 1000;
    let days = total / 86_400;
    let hours = (total % 86_400) / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;
    let mut parts: Vec<String> = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    parts.push(format!("{seconds}s"));
    parts.join(" ")
}

/// 一次注入的完整文本（上游 `renderText` 逐字）。
pub fn render_reading(
    turn: u64,
    step: u64,
    reading: &TimeReading,
    previous_ms: Option<u64>,
    now_ms: u64,
) -> String {
    let elapsed = match previous_ms {
        Some(previous) => format_duration(now_ms.saturating_sub(previous)),
        None => "unavailable".to_string(),
    };
    let baseline = if step == 1 { "model-visible message" } else { "step context" };
    format!(
        "Time sampled while preparing turn {turn}, step {step}: {}\n\
         Browser time zone for this request: {}. Interpret otherwise-unqualified dates and times in this zone.\n\
         Elapsed since the preceding {baseline}: {elapsed}.",
        reading.timestamp, reading.zone
    )
}

/// 节流判定（`true` = 本次应当注入）。
pub fn injection_due(now_ms: u64, last_injection_ms: Option<u64>, config: &TimeContextConfig) -> bool {
    match last_injection_ms {
        None => true,
        Some(last) => now_ms.saturating_sub(last) >= config.refresh_interval_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(zone: &str) -> TimeReading {
        TimeReading {
            timestamp: format_timestamp(1_762_700_000, 28_800, zone),
            zone: zone.to_string(),
        }
    }

    #[test]
    fn timestamp_is_iso_shaped_with_numeric_offset_and_zone() {
        // epoch 1_762_700_000 = 2025-11-09T14:53:20Z
        let s = format_timestamp(1_762_700_000, 28_800, "Asia/Shanghai");
        assert_eq!(s, "2025-11-09T22:53:20+08:00[Asia/Shanghai]");
        let negative = format_timestamp(1_762_700_000, -18_000, "Etc/GMT+5");
        assert_eq!(negative, "2025-11-09T09:53:20-05:00[Etc/GMT+5]");
        let utc = format_timestamp(1_762_700_000, 0, "Etc/GMT");
        assert_eq!(utc, "2025-11-09T14:53:20+00:00[Etc/GMT]");
    }

    #[test]
    fn duration_is_compact_whole_seconds() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(9_500), "9s");
        assert_eq!(format_duration(65_000), "1m 5s");
        assert_eq!(format_duration(3_725_000), "1h 2m 5s");
        assert_eq!(format_duration(90_061_000), "1d 1h 1m 1s");
    }

    #[test]
    fn reading_text_matches_upstream_three_lines() {
        let text = render_reading(3, 1, &reading("Asia/Shanghai"), Some(1_762_700_000 - 90_000), 1_762_700_000);
        let expected = format!("Time sampled while preparing turn 3, step 1: {}", reading("Asia/Shanghai").timestamp);
        assert!(text.starts_with(&expected), "{text}");
        assert!(text.contains("Browser time zone for this request: Asia/Shanghai. Interpret otherwise-unqualified dates and times in this zone."), "{text}");
        assert!(text.ends_with("Elapsed since the preceding model-visible message: 1m 30s."), "{text}");

        // step > 1 的基线措辞
        let later = render_reading(3, 4, &reading("Asia/Shanghai"), Some(1_762_700_000 - 1_000_000), 1_762_700_000);
        assert!(later.contains("Elapsed since the preceding step context: 16m 40s."), "{later}");
    }

    #[test]
    fn missing_baseline_reports_unavailable_and_backward_clocks_clamp() {
        let none = render_reading(1, 1, &reading("Etc/GMT-8"), None, 100);
        assert!(none.ends_with("Elapsed since the preceding model-visible message: unavailable."), "{none}");
        let backward = render_reading(1, 1, &reading("Etc/GMT-8"), Some(1_000), 500);
        assert!(backward.ends_with("Elapsed since the preceding model-visible message: 0s."), "{backward}");
    }

    #[test]
    fn refresh_interval_suppresses_near_readings() {
        let config = TimeContextConfig::default();
        assert!(injection_due(1_000, None, &config));
        assert!(!injection_due(1_000 + 599_999, Some(1_000), &config));
        assert!(injection_due(1_000 + 600_000, Some(1_000), &config));
        let every = TimeContextConfig { refresh_interval_ms: 0, ..Default::default() };
        assert!(injection_due(1_000, Some(1_000), &every));
    }

    #[test]
    fn zone_label_prefers_tz_env_and_falls_back_to_etc_gmt() {
        // 无 TZ 时：UTC+8 → Etc/GMT-8（IANA 符号相反）
        assert_eq!(local_zone_label(28_800), "Etc/GMT-8");
        assert_eq!(local_zone_label(-18_000), "Etc/GMT+5");
        assert_eq!(local_zone_label(0), "Etc/GMT");
    }
}

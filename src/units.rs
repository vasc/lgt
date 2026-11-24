/// Integer-only human byte formatting (no float casts needed).
pub fn human_bytes(bytes: u64) -> String {
    const K: u64 = 1024;
    if bytes < K {
        return format!("{bytes}B");
    }
    let units = ["KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes;
    let mut idx = 0;
    while value >= K * K && idx + 1 < units.len() {
        value /= K;
        idx += 1;
    }
    let whole = value / K;
    let tenths = (value % K) * 10 / K;
    format!("{whole}.{tenths}{}", units[idx])
}

/// Saturating conversion for terminal geometry.
pub fn clamp_u16(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

/// Integer-only human duration: `840ms`, `3.2s`, `4m07s`, `2h05m`.
pub fn human_duration(ms: i64) -> String {
    let ms = ms.max(0);
    if ms < 1_000 {
        return format!("{ms}ms");
    }
    let seconds = ms / 1_000;
    if seconds < 60 {
        return format!("{seconds}.{}s", ms % 1_000 / 100);
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m{:02}s", seconds % 60);
    }
    format!("{}h{:02}m", minutes / 60, minutes % 60)
}

/// Split `"30d"` into `(30, "d")`. The number must be a non-empty run of
/// ASCII digits followed by a non-empty suffix.
fn split_number_suffix(text: &str) -> Option<(u64, &str)> {
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    let (number, suffix) = text.split_at(digits);
    if number.is_empty() || suffix.is_empty() {
        return None;
    }
    Some((number.parse().ok()?, suffix))
}

const MINUTE_MS: i64 = 60_000;
const HOUR_MS: i64 = 60 * MINUTE_MS;
const DAY_MS: i64 = 24 * HOUR_MS;

/// Parse an age like `90m`, `12h`, `30d`, `2w` or a compound such as
/// `3d12h` into milliseconds. Every number needs a unit: a bare `30` is
/// ambiguous and rejected.
pub fn parse_duration(text: &str) -> anyhow::Result<i64> {
    let invalid =
        || anyhow::anyhow!("invalid duration `{text}` (expected e.g. 90m, 12h, 30d, 2w, 3d12h)");
    let mut rest = text.trim();
    if rest.is_empty() {
        return Err(invalid());
    }
    let mut total: u64 = 0;
    while !rest.is_empty() {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        let (number, after) = rest.split_at(digits);
        let unit_len = after.bytes().take_while(u8::is_ascii_alphabetic).count();
        let (unit, tail) = after.split_at(unit_len);
        if number.is_empty() || unit.is_empty() {
            return Err(invalid());
        }
        let unit_ms: u64 = match unit {
            "s" => 1_000,
            "m" => 60_000,
            "h" => 3_600_000,
            "d" => 86_400_000,
            "w" => 604_800_000,
            _ => return Err(invalid()),
        };
        let number: u64 = number.parse().map_err(|_| invalid())?;
        total = number
            .checked_mul(unit_ms)
            .and_then(|ms| total.checked_add(ms))
            .ok_or_else(invalid)?;
        rest = tail;
    }
    i64::try_from(total).map_err(|_| invalid())
}

/// An age rounded *down* to its two most significant units, for display and
/// for suggesting commands: `56d3h`, `3h20m`, `2d`, `45m`. Returns the
/// rounded milliseconds with the text (which `parse_duration` accepts), or
/// `None` for ages under a minute.
pub fn compact_age(ms: i64) -> Option<(i64, String)> {
    if ms < MINUTE_MS {
        return None;
    }
    let units = [(DAY_MS, 'd'), (HOUR_MS, 'h'), (MINUTE_MS, 'm')];
    let first = units.iter().position(|(size, _)| ms >= *size)?;
    let (big, big_unit) = units[first];
    let big_count = ms / big;
    let mut rounded = big_count * big;
    let mut text = format!("{big_count}{big_unit}");
    if let Some((small, small_unit)) = units.get(first + 1) {
        let small_count = (ms - rounded) / small;
        if small_count > 0 {
            rounded += small_count * small;
            text.push_str(&format!("{small_count}{small_unit}"));
        }
    }
    Some((rounded, text))
}

/// Parse a size like `500K`, `200M`, `5G` (binary multiples; an optional
/// trailing `B`/`iB` is accepted, as is a plain byte count).
pub fn parse_size(text: &str) -> anyhow::Result<u64> {
    let invalid = || anyhow::anyhow!("invalid size `{text}` (expected e.g. 500K, 200M, 5G)");
    let trimmed = text.trim();
    if !trimmed.is_empty() && trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return trimmed.parse().map_err(|_| invalid());
    }
    let (number, unit) = split_number_suffix(trimmed).ok_or_else(invalid)?;
    let multiplier: u64 = match unit.to_ascii_uppercase().as_str() {
        "B" => 1,
        "K" | "KB" | "KIB" => 1 << 10,
        "M" | "MB" | "MIB" => 1 << 20,
        "G" | "GB" | "GIB" => 1 << 30,
        "T" | "TB" | "TIB" => 1 << 40,
        _ => return Err(invalid()),
    };
    number.checked_mul(multiplier).ok_or_else(invalid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_parse_with_units() {
        assert_eq!(parse_duration("45s").unwrap(), 45_000);
        assert_eq!(parse_duration("90m").unwrap(), 5_400_000);
        assert_eq!(parse_duration("12h").unwrap(), 43_200_000);
        assert_eq!(parse_duration("30d").unwrap(), 2_592_000_000);
        assert_eq!(parse_duration("2w").unwrap(), 1_209_600_000);
        assert_eq!(parse_duration(" 1d ").unwrap(), 86_400_000);
        assert_eq!(
            parse_duration("3d12h").unwrap(),
            3 * 86_400_000 + 12 * 3_600_000
        );
        assert_eq!(parse_duration("1h30m").unwrap(), 5_400_000);
    }

    #[test]
    fn durations_reject_garbage() {
        for bad in [
            "",
            "30",
            "d",
            "soon",
            "1.5d",
            "-1d",
            "3y",
            "3d12",
            "3d h",
            "99999999999999999999d",
            "99999999999w99999999999w",
        ] {
            assert!(parse_duration(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn compact_ages_round_down_to_two_units_and_round_trip() {
        assert_eq!(compact_age(59_999), None);
        assert_eq!(
            compact_age(45 * MINUTE_MS + 59_000),
            Some((45 * MINUTE_MS, "45m".to_string()))
        );
        assert_eq!(
            compact_age(3 * HOUR_MS + 20 * MINUTE_MS + 59_000),
            Some((3 * HOUR_MS + 20 * MINUTE_MS, "3h20m".to_string()))
        );
        assert_eq!(
            compact_age(56 * DAY_MS + 3 * HOUR_MS + 59 * MINUTE_MS),
            Some((56 * DAY_MS + 3 * HOUR_MS, "56d3h".to_string()))
        );
        assert_eq!(
            compact_age(2 * DAY_MS + 59 * MINUTE_MS),
            Some((2 * DAY_MS, "2d".to_string()))
        );
        for ms in [
            MINUTE_MS,
            7 * HOUR_MS + 5 * MINUTE_MS,
            400 * DAY_MS + 23 * HOUR_MS,
        ] {
            let (rounded, text) = compact_age(ms).unwrap();
            assert_eq!(parse_duration(&text).unwrap(), rounded, "{text}");
        }
    }

    #[test]
    fn sizes_parse_with_units() {
        assert_eq!(parse_size("1234").unwrap(), 1234);
        assert_eq!(parse_size("25K").unwrap(), 25 * 1024);
        assert_eq!(parse_size("25kb").unwrap(), 25 * 1024);
        assert_eq!(parse_size("200M").unwrap(), 200 * 1024 * 1024);
        assert_eq!(parse_size("5GiB").unwrap(), 5 * 1024 * 1024 * 1024);
        assert_eq!(parse_size("1T").unwrap(), 1 << 40);
        assert_eq!(parse_size("7B").unwrap(), 7);
    }

    #[test]
    fn sizes_reject_garbage() {
        for bad in [
            "",
            "big",
            "10Q",
            "M",
            "1.5G",
            "-5M",
            "99999999999999999999T",
        ] {
            assert!(parse_size(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn bytes_format() {
        assert_eq!(human_bytes(0), "0B");
        assert_eq!(human_bytes(1023), "1023B");
        assert_eq!(human_bytes(1536), "1.5KiB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0MiB");
        assert_eq!(human_bytes(3 * 1024 * 1024 * 1024), "3.0GiB");
    }

    #[test]
    fn clamp_saturates() {
        assert_eq!(clamp_u16(3), 3);
        assert_eq!(clamp_u16(1_000_000), u16::MAX);
    }

    #[test]
    fn duration_format() {
        assert_eq!(human_duration(-5), "0ms");
        assert_eq!(human_duration(840), "840ms");
        assert_eq!(human_duration(3_250), "3.2s");
        assert_eq!(human_duration(247_000), "4m07s");
        assert_eq!(human_duration(7_500_000), "2h05m");
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

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

use jiff::Timestamp;
use jiff::tz::TimeZone;

pub fn now_ms() -> i64 {
    Timestamp::now().as_millisecond()
}

fn zoned(ms: i64) -> Option<jiff::Zoned> {
    Timestamp::from_millisecond(ms)
        .ok()
        .map(|t| t.to_zoned(TimeZone::system()))
}

/// Compact timestamp used inside stream/group ids, e.g. `20260821-140302123`.
pub fn compact(ms: i64) -> String {
    match zoned(ms) {
        Some(z) => format!("{}{:03}", z.strftime("%Y%m%d-%H%M%S"), ms.rem_euclid(1000)),
        None => format!("t{ms}"),
    }
}

/// Short human clock: `14:03:02` for today, `08-20 14:03` for older entries.
pub fn clock(ms: i64) -> String {
    let Some(z) = zoned(ms) else {
        return "?".to_string();
    };
    let now = Timestamp::now().to_zoned(TimeZone::system());
    if z.date() == now.date() {
        z.strftime("%H:%M:%S").to_string()
    } else {
        z.strftime("%m-%d %H:%M").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_is_sortable_and_unique_per_ms() {
        let a = compact(1_000_000_000_000);
        let b = compact(1_000_000_000_001);
        assert_ne!(a, b);
        assert!(a < b);
    }

    #[test]
    fn clock_handles_out_of_range() {
        assert_eq!(clock(i64::MAX), "?");
    }
}

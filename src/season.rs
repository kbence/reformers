//! The calendar: Santa hats and Auto snow follow the date (UTC; no timezone database here).

/// Days since 1970 to (year, month, day), Hinnant's `civil_from_days`.
pub fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + (m <= 2) as i64, m as u32, d as u32)
}

/// Today's (month, day): `REFORMERS_DATE=YYYY-MM-DD` if set, to try the seasons out of season.
pub fn today() -> (u32, u32) {
    if let Some(md) = std::env::var("REFORMERS_DATE").ok().and_then(|s| parse(&s)) {
        return md;
    }
    let (_, m, d) = civil((macroquad::miniquad::date::now() / 86400.0).floor() as i64);
    (m, d)
}

fn parse(s: &str) -> Option<(u32, u32)> {
    let mut it = s.trim().split('-').skip(1).map(|p| p.parse::<u32>().ok());
    let (m, d) = (it.next()??, it.next()??);
    ((1..=12).contains(&m) && (1..=31).contains(&d)).then_some((m, d))
}

/// St. Nicholas Day to Epiphany, both included.
pub fn hats((m, d): (u32, u32)) -> bool {
    (m == 12 && d >= 6) || (m == 1 && d <= 6)
}

/// December to February: when Auto snows.
pub fn winter((m, _): (u32, u32)) -> bool {
    matches!(m, 12 | 1 | 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seasons() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(11_016), (2000, 2, 29));
        assert!(!hats((12, 5)) && hats((12, 6)) && hats((1, 6)) && !hats((1, 7)));
        assert!(winter((12, 1)) && winter((2, 29)) && !winter((3, 1)) && !winter((11, 30)));
        assert_eq!(parse("2026-12-24"), Some((12, 24)));
        assert_eq!(parse("2026-13-01"), None);
        assert_eq!(parse("garbage"), None);
    }
}

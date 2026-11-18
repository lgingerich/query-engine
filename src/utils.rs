/// "-12.5" at scale 2 -> -1250. None if more fractional digits than `scale`
pub(crate) fn parse_decimal(s: &str, scale: i8) -> Option<i64> {
    let scale = usize::try_from(scale).ok()?;
    let (negative, digits) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let (int_part, frac_part) = digits.split_once('.').unwrap_or((digits, ""));
    let frac_part = frac_part.trim_end_matches('0'); // "0.050" is fine at scale 2
    if frac_part.len() > scale {
        return None;
    }
    let unscaled: i64 = format!("{int_part}{frac_part:0<scale$}").parse().ok()?;
    Some(if negative { -unscaled } else { unscaled })
}

/// "1995-03-15" -> days since 1970-01-01
pub(crate) fn parse_date(s: &str) -> Option<i32> {
    let mut parts = s.split('-');
    let year: i32 = parts.next()?.parse().ok()?;
    let month: i32 = parts.next()?.parse().ok()?;
    let day: i32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) {
        return None;
    }
    if day < 1 || day > days_in_month(year, month) {
        return None;
    }
    Some(days_from_civil(year, month, day))
}

fn days_in_month(year: i32, month: i32) -> i32 {
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Howard Hinnant's days_from_civil
fn days_from_civil(year: i32, month: i32, day: i32) -> i32 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let year_of_era = y.rem_euclid(400);
    let month_from_march = (month + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

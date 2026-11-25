//! 「きょう」「ことし」のような読みに対する、日付の候補。

use chrono::{Datelike, Duration, Months, NaiveDate};

/// 令和元年の前の年。西暦からこれを引くと令和の年になる。
const REIWA_OFFSET: i32 = 2018;

/// `reading` が日付を表す語なら、`today` を基準にした日付の表記を返す。
pub fn date_candidates(reading: &str, today: NaiveDate) -> Vec<String> {
    let day = |offset: i64| today + Duration::days(offset);
    match reading {
        "きょう" => day_formats(day(0)),
        "きのう" => day_formats(day(-1)),
        "おととい" => day_formats(day(-2)),
        "あした" | "あす" => day_formats(day(1)),
        "あさって" => day_formats(day(2)),
        "こんげつ" => month_formats(today),
        "せんげつ" => month_formats(today - Months::new(1)),
        "らいげつ" => month_formats(today + Months::new(1)),
        "ことし" => year_formats(today.year()),
        "きょねん" | "さくねん" => year_formats(today.year() - 1),
        "らいねん" => year_formats(today.year() + 1),
        _ => Vec::new(),
    }
}

fn day_formats(date: NaiveDate) -> Vec<String> {
    let (year, month, day) = (date.year(), date.month(), date.day());
    vec![
        format!("{year}-{month:02}-{day:02}"),
        format!("{year}{month:02}{day:02}"),
        format!("{year}/{month:02}/{day:02}"),
        format!("{year}年{month}月{day}日"),
        format!("{month}月{day}日"),
    ]
}

fn month_formats(date: NaiveDate) -> Vec<String> {
    let (year, month) = (date.year(), date.month());
    vec![
        format!("{year}-{month:02}"),
        format!("{year}{month:02}"),
        format!("{year}/{month:02}"),
        format!("{year}年{month}月"),
        format!("{month}月"),
    ]
}

fn year_formats(year: i32) -> Vec<String> {
    let mut formats = vec![format!("{year}"), format!("{year}年")];
    if year > REIWA_OFFSET {
        formats.push(format!("令和{}年", year - REIWA_OFFSET));
    }
    formats
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("valid date")
    }

    #[test]
    fn today_has_the_requested_formats_first() {
        let candidates = date_candidates("きょう", date(2026, 11, 25));
        assert_eq!(
            candidates,
            ["2026-11-25", "20261125", "2026/11/25", "2026年11月25日", "11月25日"]
        );
    }

    #[test]
    fn relative_days_cross_month_and_year_boundaries() {
        assert_eq!(date_candidates("きのう", date(2026, 3, 1))[0], "2026-02-28");
        assert_eq!(date_candidates("あした", date(2026, 12, 31))[0], "2027-01-01");
        assert_eq!(date_candidates("おととい", date(2026, 1, 1))[0], "2025-12-30");
        assert_eq!(date_candidates("あさって", date(2028, 2, 28))[0], "2028-03-01");
    }

    #[test]
    fn months_and_years_follow_the_calendar() {
        assert_eq!(
            date_candidates("こんげつ", date(2026, 11, 25)),
            ["2026-11", "202611", "2026/11", "2026年11月", "11月"]
        );
        assert_eq!(date_candidates("せんげつ", date(2026, 1, 31))[0], "2025-12");
        // 翌月に同じ日が無い時は、その月の末日になる。月の表記には影響しない。
        assert_eq!(date_candidates("らいげつ", date(2026, 1, 31))[0], "2026-02");
        assert_eq!(date_candidates("ことし", date(2026, 11, 25)), ["2026", "2026年", "令和8年"]);
        assert_eq!(date_candidates("きょねん", date(2019, 5, 1)), ["2018", "2018年"]);
        assert_eq!(date_candidates("らいねん", date(2026, 11, 25))[2], "令和9年");
    }

    #[test]
    fn other_readings_have_no_date_candidates() {
        assert!(date_candidates("きょうは", date(2026, 11, 25)).is_empty());
        assert!(date_candidates("", date(2026, 11, 25)).is_empty());
    }
}

use i18n::{Amsterdam, long_date, money, short_date, time, time_range};
use jiff::Timestamp;
use jiff::civil::{date, time as t};

const NBSP: char = '\u{a0}';

fn eur(s: &str) -> String {
    format!("€{NBSP}{s}")
}

#[test]
fn money_uses_dutch_separators() {
    assert_eq!(money(450), eur("4,50"));
    assert_eq!(money(5), eur("0,05"));
    assert_eq!(money(0), eur("0,00"));
    assert_eq!(money(123_400), eur("1.234,00"));
    assert_eq!(money(123_456_789), eur("1.234.567,89"));
}

#[test]
fn negative_money_puts_the_sign_after_the_euro() {
    assert_eq!(money(-450), eur("-4,50"));
    assert_eq!(money(-123_400), eur("-1.234,00"));
    assert_eq!(money(i64::MIN), eur("-92.233.720.368.547.758,08"));
    assert_eq!(money(i64::MAX), eur("92.233.720.368.547.758,07"));
}

#[test]
fn long_date_is_written_out_in_dutch() {
    assert_eq!(long_date(date(2026, 12, 6)), "zondag 6 december 2026");
    assert_eq!(long_date(date(2026, 12, 5)), "zaterdag 5 december 2026");
    assert_eq!(long_date(date(2026, 3, 30)), "maandag 30 maart 2026");
}

#[test]
fn short_date_uses_dutch_abbreviations() {
    assert_eq!(short_date(date(2026, 12, 5)), "za 5 dec");
    assert_eq!(short_date(date(2026, 3, 3)), "di 3 mrt");
    assert_eq!(short_date(date(2026, 10, 14)), "wo 14 okt");
}

#[test]
fn every_weekday_and_month_has_a_dutch_name() {
    let longs: Vec<String> = (1..=7).map(|d| long_date(date(2026, 6, d))).collect();
    assert_eq!(
        longs,
        [
            "maandag 1 juni 2026",
            "dinsdag 2 juni 2026",
            "woensdag 3 juni 2026",
            "donderdag 4 juni 2026",
            "vrijdag 5 juni 2026",
            "zaterdag 6 juni 2026",
            "zondag 7 juni 2026",
        ]
    );
    let shorts: Vec<String> = (1..=7).map(|d| short_date(date(2026, 6, d))).collect();
    assert_eq!(
        shorts,
        [
            "ma 1 jun", "di 2 jun", "wo 3 jun", "do 4 jun", "vr 5 jun", "za 6 jun", "zo 7 jun"
        ]
    );
    let months: Vec<String> = (1..=12)
        .map(|m| {
            long_date(date(2027, m, 1))
                .split(' ')
                .nth(2)
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(
        months,
        [
            "januari",
            "februari",
            "maart",
            "april",
            "mei",
            "juni",
            "juli",
            "augustus",
            "september",
            "oktober",
            "november",
            "december",
        ]
    );
    let short_months: Vec<String> = (1..=12)
        .map(|m| {
            short_date(date(2027, m, 1))
                .split(' ')
                .nth(2)
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(
        short_months,
        [
            "jan", "feb", "mrt", "apr", "mei", "jun", "jul", "aug", "sep", "okt", "nov", "dec"
        ]
    );
}

#[test]
fn times_use_24_hours_and_an_en_dash() {
    assert_eq!(time(t(11, 0, 0, 0)), "11:00");
    assert_eq!(time(t(7, 5, 59, 0)), "07:05");
    assert_eq!(time_range(t(11, 0, 0, 0), t(12, 0, 0, 0)), "11:00–12:00");
}

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

#[test]
fn timestamps_are_shown_in_amsterdam_time() {
    let ams = Amsterdam::load().expect("tzdb has Europe/Amsterdam");
    let start = ts("2026-12-05T10:00:00Z");
    let end = ts("2026-12-05T11:00:00Z");
    assert_eq!(ams.long_date(start), "zaterdag 5 december 2026");
    assert_eq!(ams.short_date(start), "za 5 dec");
    assert_eq!(ams.time(start), "11:00");
    assert_eq!(ams.time_range(start, end), "11:00–12:00");
}

#[test]
fn autumn_clock_change_on_25_october_2026() {
    let ams = Amsterdam::load().unwrap();
    assert_eq!(ams.time(ts("2026-10-24T09:00:00Z")), "11:00");
    assert_eq!(ams.time(ts("2026-10-25T09:00:00Z")), "10:00");
    assert_eq!(ams.time(ts("2026-10-25T00:30:00Z")), "02:30");
    assert_eq!(ams.time(ts("2026-10-25T01:30:00Z")), "02:30");
    assert_eq!(
        ams.long_date(ts("2026-10-24T22:30:00Z")),
        "zondag 25 oktober 2026"
    );
    assert_eq!(
        ams.time_range(ts("2026-10-25T00:30:00Z"), ts("2026-10-25T02:30:00Z")),
        "02:30–03:30"
    );
}

#[test]
fn spring_clock_change_on_29_march_2026() {
    let ams = Amsterdam::load().unwrap();
    assert_eq!(ams.time(ts("2026-03-29T00:59:00Z")), "01:59");
    assert_eq!(ams.time(ts("2026-03-29T01:00:00Z")), "03:00");
    assert_eq!(ams.short_date(ts("2026-03-28T23:30:00Z")), "zo 29 mrt");
    assert_eq!(ams.short_date(ts("2026-03-28T22:30:00Z")), "za 28 mrt");
    assert_eq!(
        ams.long_date(ts("2026-03-29T22:30:00Z")),
        "maandag 30 maart 2026"
    );
}

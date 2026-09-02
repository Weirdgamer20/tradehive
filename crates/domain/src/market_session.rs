use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, Utc, Weekday};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarketClosedReason {
    Weekend,
    Holiday(String),
    BeforeMarketOpen,
    AfterMarketClose,
}

impl std::fmt::Display for MarketClosedReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Weekend => write!(f, "weekend"),
            Self::Holiday(name) => write!(f, "holiday: {name}"),
            Self::BeforeMarketOpen => write!(f, "before market open"),
            Self::AfterMarketClose => write!(f, "after market close"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarketSessionState {
    Open,
    MarketClosed(MarketClosedReason),
}

impl MarketSessionState {
    pub fn is_open(&self) -> bool {
        matches!(self, Self::Open)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "OPEN",
            Self::MarketClosed(_) => "MARKET_CLOSED",
        }
    }
}

#[derive(Debug, Clone)]
pub struct MarketSessionConfig {
    pub open_time: NaiveTime,
    pub close_time: NaiveTime,
    pub timezone: Tz,
}

impl Default for MarketSessionConfig {
    fn default() -> Self {
        Self {
            open_time: NaiveTime::from_hms_opt(9, 30, 0).expect("valid 09:30 default"),
            close_time: NaiveTime::from_hms_opt(16, 0, 0).expect("valid 16:00 default"),
            timezone: chrono_tz::America::New_York,
        }
    }
}

impl MarketSessionConfig {
    pub fn from_env() -> Self {
        let mut cfg = Self::default();

        if let Ok(open_str) = std::env::var("MARKET_OPEN") {
            if let Ok(t) = parse_hh_mm(&open_str) {
                cfg.open_time = t;
            }
        }

        if let Ok(close_str) = std::env::var("MARKET_CLOSE") {
            if let Ok(t) = parse_hh_mm(&close_str) {
                cfg.close_time = t;
            }
        }

        if let Ok(tz_str) = std::env::var("MARKET_TIMEZONE") {
            if let Ok(tz) = Tz::from_str(&tz_str) {
                cfg.timezone = tz;
            }
        }

        cfg
    }
}

fn parse_hh_mm(s: &str) -> Result<NaiveTime, String> {
    let parts: Vec<&str> = s.trim().split(':').collect();
    if parts.len() != 2 {
        return Err(format!("expected HH:MM format, got '{s}'"));
    }
    let h: u32 = parts[0].parse().map_err(|e| format!("invalid hour: {e}"))?;
    let m: u32 = parts[1]
        .parse()
        .map_err(|e| format!("invalid minute: {e}"))?;
    NaiveTime::from_hms_opt(h, m, 0).ok_or_else(|| format!("invalid time: {h:02}:{m:02}"))
}

pub struct HolidayCalendar;

impl HolidayCalendar {
    pub fn holidays_for_year(year: i32) -> HashMap<NaiveDate, &'static str> {
        let mut h = HashMap::new();

        // 1. New Year's Day (Jan 1)
        if let Some(date) = observed_fixed_holiday(year, 1, 1) {
            h.insert(date, "New Year's Day");
        }
        // If Jan 1 of next year falls on Saturday, observed Dec 31 of current year
        if let Some(jan1_next) = NaiveDate::from_ymd_opt(year + 1, 1, 1) {
            if jan1_next.weekday() == Weekday::Sat {
                if let Some(dec31) = NaiveDate::from_ymd_opt(year, 12, 31) {
                    h.insert(dec31, "New Year's Day (Observed)");
                }
            }
        }

        // 2. Martin Luther King Jr. Day: 3rd Monday in January
        if let Some(date) = nth_weekday_of_month(year, 1, Weekday::Mon, 3) {
            h.insert(date, "Martin Luther King Jr. Day");
        }

        // 3. Washington's Birthday (Presidents' Day): 3rd Monday in February
        if let Some(date) = nth_weekday_of_month(year, 2, Weekday::Mon, 3) {
            h.insert(date, "Washington's Birthday");
        }

        // 4. Good Friday: Friday before Easter Sunday
        if let Some(easter) = easter_sunday(year) {
            let good_friday = easter - chrono::Duration::days(2);
            h.insert(good_friday, "Good Friday");
        }

        // 5. Memorial Day: Last Monday in May
        if let Some(date) = last_weekday_of_month(year, 5, Weekday::Mon) {
            h.insert(date, "Memorial Day");
        }

        // 6. Juneteenth National Independence Day (June 19)
        if year >= 2021 {
            if let Some(date) = observed_fixed_holiday(year, 6, 19) {
                h.insert(date, "Juneteenth National Independence Day");
            }
        }

        // 7. Independence Day (July 4)
        if let Some(date) = observed_fixed_holiday(year, 7, 4) {
            h.insert(date, "Independence Day");
        }

        // 8. Labor Day: 1st Monday in September
        if let Some(date) = nth_weekday_of_month(year, 9, Weekday::Mon, 1) {
            h.insert(date, "Labor Day");
        }

        // 9. Thanksgiving Day: 4th Thursday in November
        if let Some(date) = nth_weekday_of_month(year, 11, Weekday::Thu, 4) {
            h.insert(date, "Thanksgiving Day");
        }

        // 10. Christmas Day (Dec 25)
        if let Some(date) = observed_fixed_holiday(year, 12, 25) {
            h.insert(date, "Christmas Day");
        }

        h
    }

    pub fn is_market_holiday(date: NaiveDate) -> Option<&'static str> {
        let year_holidays = Self::holidays_for_year(date.year());
        year_holidays.get(&date).copied()
    }
}

fn observed_fixed_holiday(year: i32, month: u32, day: u32) -> Option<NaiveDate> {
    let date = NaiveDate::from_ymd_opt(year, month, day)?;
    match date.weekday() {
        Weekday::Sat => date.pred_opt(),
        Weekday::Sun => date.succ_opt(),
        _ => Some(date),
    }
}

fn nth_weekday_of_month(year: i32, month: u32, weekday: Weekday, n: u32) -> Option<NaiveDate> {
    if n == 0 || n > 5 {
        return None;
    }
    let mut date = NaiveDate::from_ymd_opt(year, month, 1)?;
    while date.weekday() != weekday {
        date = date.succ_opt()?;
    }
    let days_to_add = (n - 1) * 7;
    let target = date + chrono::Duration::days(days_to_add as i64);
    if target.month() == month {
        Some(target)
    } else {
        None
    }
}

fn last_weekday_of_month(year: i32, month: u32, weekday: Weekday) -> Option<NaiveDate> {
    let next_month_first = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)?
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)?
    };
    let mut date = next_month_first.pred_opt()?;
    while date.weekday() != weekday {
        date = date.pred_opt()?;
    }
    if date.month() == month {
        Some(date)
    } else {
        None
    }
}

fn easter_sunday(year: i32) -> Option<NaiveDate> {
    let a = year % 19;
    let b = year / 100;
    let c = year % 100;
    let d = b / 4;
    let e = b % 4;
    let f = (b + 8) / 25;
    let g = (b - f + 1) / 3;
    let h = (19 * a + b - d - g + 15) % 30;
    let i = c / 4;
    let k = c % 4;
    let l = (32 + 2 * e + 2 * i - h - k) % 7;
    let m = (a + 11 * h + 22 * l) / 451;
    let month = (h + l - 7 * m + 114) / 31;
    let day = ((h + l - 7 * m + 114) % 31) + 1;
    NaiveDate::from_ymd_opt(year, month as u32, day as u32)
}

#[derive(Debug, Clone, Default)]
pub struct MarketSessionClock {
    pub config: MarketSessionConfig,
}

impl MarketSessionClock {
    pub fn new(config: MarketSessionConfig) -> Self {
        Self { config }
    }

    pub fn from_env() -> Self {
        Self {
            config: MarketSessionConfig::from_env(),
        }
    }

    pub fn session_state_at(&self, dt: DateTime<Utc>) -> MarketSessionState {
        let local = dt.with_timezone(&self.config.timezone);
        let weekday = local.weekday();

        if weekday == Weekday::Sat || weekday == Weekday::Sun {
            return MarketSessionState::MarketClosed(MarketClosedReason::Weekend);
        }

        let date = local.date_naive();
        if let Some(holiday) = HolidayCalendar::is_market_holiday(date) {
            return MarketSessionState::MarketClosed(MarketClosedReason::Holiday(
                holiday.to_string(),
            ));
        }

        let time = local.time();
        if time < self.config.open_time {
            return MarketSessionState::MarketClosed(MarketClosedReason::BeforeMarketOpen);
        }

        if time >= self.config.close_time {
            return MarketSessionState::MarketClosed(MarketClosedReason::AfterMarketClose);
        }

        MarketSessionState::Open
    }

    pub fn is_open(&self, dt: DateTime<Utc>) -> bool {
        self.session_state_at(dt).is_open()
    }

    pub fn timezone(&self) -> Tz {
        self.config.timezone
    }
}

//! Takvim ve seanslar. UTC ↔ yerel saat dönüşümü yalnız burada yapılır (spec §2).
//!
//! Bir seans, yerel saat diliminde haftalık bir pencere (ör. CME Globex: Pazar 17:00 → Cuma 16:00
//! America/Chicago) + günlük ara (16:00–17:00) + tatil kapanış aralıklarıyla tanımlanır.
//! Tatil aralıkları `configs/calendar.toml`'dan gelir; uydurulmaz.

use crate::units::{NANOS_PER_SEC, UnixNanos};
use chrono::{
    DateTime, Datelike, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Timelike, Utc,
};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CalendarError {
    #[error("bilinmeyen saat dilimi: {0}")]
    BadTz(String),
    #[error("geçersiz saat {0:?} (SS:DD bekleniyor)")]
    BadTime(String),
    #[error("geçersiz tarih {0:?} (YYYY-AA-GG bekleniyor)")]
    BadDate(String),
    #[error("geçersiz gün {0:?}")]
    BadWeekday(String),
    #[error("bilinmeyen seans: {0}")]
    UnknownSession(String),
    #[error("calendar.toml çözülemedi: {0}")]
    Toml(String),
}

/// Haftanın günü, Pazar = 0.
fn weekday_from_str(s: &str) -> Result<u32, CalendarError> {
    match s.trim().to_ascii_lowercase().as_str() {
        "sun" | "sunday" => Ok(0),
        "mon" | "monday" => Ok(1),
        "tue" | "tuesday" => Ok(2),
        "wed" | "wednesday" => Ok(3),
        "thu" | "thursday" => Ok(4),
        "fri" | "friday" => Ok(5),
        "sat" | "saturday" => Ok(6),
        _ => Err(CalendarError::BadWeekday(s.to_string())),
    }
}

fn parse_hhmm(s: &str) -> Result<u32, CalendarError> {
    let (h, m) = s
        .trim()
        .split_once(':')
        .ok_or_else(|| CalendarError::BadTime(s.to_string()))?;
    let h: u32 = h
        .parse()
        .map_err(|_| CalendarError::BadTime(s.to_string()))?;
    let m: u32 = m
        .parse()
        .map_err(|_| CalendarError::BadTime(s.to_string()))?;
    if h > 24 || m > 59 || (h == 24 && m != 0) {
        return Err(CalendarError::BadTime(s.to_string()));
    }
    Ok(h * 60 + m)
}

/// Yerel saatte tek bir kapalı aralık (tatil, erken kapanış).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClosedInterval {
    pub date: NaiveDate,
    /// Gün içi dakika [from, until).
    pub from_min: u32,
    pub until_min: u32,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionSpec {
    /// 7/24 açık (kripto).
    Always,
    Weekly(WeeklySession),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeeklySession {
    pub tz: Tz,
    /// Haftanın başından (Pazar 00:00 yerel) dakika.
    pub open_week_min: u32,
    pub close_week_min: u32,
    /// Günlük ara, gün içi dakika [start, end).
    pub daily_break: Option<(u32, u32)>,
    /// Tarih → o tarihteki kapalı aralıklar.
    pub closures: BTreeMap<NaiveDate, Vec<ClosedInterval>>,
}

impl WeeklySession {
    fn local(&self, ts: UnixNanos) -> NaiveDateTime {
        to_utc(ts).with_timezone(&self.tz).naive_local()
    }

    fn is_open_local(&self, l: NaiveDateTime) -> bool {
        let wd = l.weekday().num_days_from_sunday();
        let tod = l.time().num_seconds_from_midnight() / 60;
        let wm = wd * 1440 + tod;
        let in_week = if self.open_week_min <= self.close_week_min {
            wm >= self.open_week_min && wm < self.close_week_min
        } else {
            wm >= self.open_week_min || wm < self.close_week_min
        };
        if !in_week {
            return false;
        }
        if let Some((s, e)) = self.daily_break
            && tod >= s
            && tod < e
        {
            return false;
        }
        if let Some(list) = self.closures.get(&l.date())
            && list.iter().any(|c| tod >= c.from_min && tod < c.until_min)
        {
            return false;
        }
        true
    }

    /// Yerel saat → UTC. DST boşluğunda bir sonraki geçerli ana kayar.
    fn local_to_utc(&self, l: NaiveDateTime) -> UnixNanos {
        let mut probe = l;
        for _ in 0..180 {
            match self.tz.from_local_datetime(&probe) {
                LocalResult::Single(dt) => return from_utc(dt.with_timezone(&Utc)),
                LocalResult::Ambiguous(a, _) => return from_utc(a.with_timezone(&Utc)),
                LocalResult::None => probe += chrono::Duration::minutes(1),
            }
        }
        from_utc(probe.and_utc())
    }
}

pub fn to_utc(ts: UnixNanos) -> DateTime<Utc> {
    let secs = i64::try_from(ts.0 / NANOS_PER_SEC).unwrap_or(i64::MAX);
    let nanos = u32::try_from(ts.0 % NANOS_PER_SEC).unwrap_or(0);
    Utc.timestamp_opt(secs, nanos)
        .single()
        .unwrap_or(DateTime::<Utc>::MIN_UTC)
}

pub fn from_utc(dt: DateTime<Utc>) -> UnixNanos {
    let s = dt.timestamp();
    if s < 0 {
        return UnixNanos::ZERO;
    }
    UnixNanos((s as u64).saturating_mul(NANOS_PER_SEC) + u64::from(dt.timestamp_subsec_nanos()))
}

/// "2026-10-31T14:30:00Z" gibi RFC3339 metin → UnixNanos.
pub fn parse_rfc3339(s: &str) -> Result<UnixNanos, CalendarError> {
    DateTime::parse_from_rfc3339(s.trim())
        .map(|d| from_utc(d.with_timezone(&Utc)))
        .map_err(|_| CalendarError::BadDate(s.to_string()))
}

/// "2026-10-31" → o günün 00:00 UTC'si.
pub fn parse_date_utc(s: &str) -> Result<UnixNanos, CalendarError> {
    let d = NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
        .map_err(|_| CalendarError::BadDate(s.to_string()))?;
    Ok(from_utc(d.and_time(NaiveTime::MIN).and_utc()))
}

pub fn format_utc(ts: UnixNanos) -> String {
    to_utc(ts).format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Verilen saat dilimindeki yerel tarih (raporlar, gün sınırı).
pub fn local_date(ts: UnixNanos, tz: Tz) -> NaiveDate {
    to_utc(ts).with_timezone(&tz).date_naive()
}

/// Yerel tarih + gün içi dakika → UTC. DST boşluğunda ileri kayar.
pub fn local_to_utc(date: NaiveDate, minute_of_day: u32, tz: Tz) -> UnixNanos {
    let ws = WeeklySession {
        tz,
        open_week_min: 0,
        close_week_min: 0,
        daily_break: None,
        closures: BTreeMap::new(),
    };
    let t = NaiveTime::from_num_seconds_from_midnight_opt(minute_of_day.min(1439) * 60, 0)
        .unwrap_or(NaiveTime::MIN);
    ws.local_to_utc(date.and_time(t))
}

pub fn parse_tz(s: &str) -> Result<Tz, CalendarError> {
    s.trim()
        .parse::<Tz>()
        .map_err(|_| CalendarError::BadTz(s.to_string()))
}

impl SessionSpec {
    pub fn is_open(&self, ts: UnixNanos) -> bool {
        match self {
            SessionSpec::Always => true,
            SessionSpec::Weekly(w) => w.is_open_local(w.local(ts)),
        }
    }

    /// `ts` anından sonraki ilk kapanış anı (günlük ara, tatil veya hafta sonu).
    /// Seans kapalıysa `None`. `Always` için `None` (kapanış yok).
    pub fn next_close(&self, ts: UnixNanos) -> Option<UnixNanos> {
        let w = match self {
            SessionSpec::Always => return None,
            SessionSpec::Weekly(w) => w,
        };
        if !self.is_open(ts) {
            return None;
        }
        // Dakika sınırlarında ileri tara; aday sınırlar: günlük ara başı, tatil başı, hafta kapanışı.
        let l = w.local(ts);
        let start_date = l.date();
        let mut candidates: Vec<NaiveDateTime> = Vec::new();
        for d in 0..9u32 {
            let date = start_date + chrono::Duration::days(i64::from(d));
            let wd = date.weekday().num_days_from_sunday();
            if let Some((s, _)) = w.daily_break {
                candidates.push(at_minute(date, s));
            }
            if let Some(list) = w.closures.get(&date) {
                for c in list {
                    candidates.push(at_minute(date, c.from_min));
                }
            }
            let close_wd = w.close_week_min / 1440;
            if wd == close_wd {
                candidates.push(at_minute(date, w.close_week_min % 1440));
            }
        }
        candidates.sort();
        candidates
            .into_iter()
            .filter(|c| *c > l)
            .find(|c| {
                // Sınırın hemen öncesi açık, sınır anı kapalı olmalı.
                !w.is_open_local(*c) && w.is_open_local(*c - chrono::Duration::minutes(1))
            })
            .map(|c| w.local_to_utc(c))
    }

    /// Bir sonraki haftalık kapanış (hafta sonu). Lesson §9.6 için `Ctx`'te kullanılır.
    pub fn next_week_close(&self, ts: UnixNanos) -> Option<UnixNanos> {
        let w = match self {
            SessionSpec::Always => return None,
            SessionSpec::Weekly(w) => w,
        };
        let l = w.local(ts);
        for d in 0..8u32 {
            let date = l.date() + chrono::Duration::days(i64::from(d));
            if date.weekday().num_days_from_sunday() == w.close_week_min / 1440 {
                let c = at_minute(date, w.close_week_min % 1440);
                if c > l {
                    return Some(w.local_to_utc(c));
                }
            }
        }
        None
    }

    /// İşlem günü: seans açılışı yerel tarihi bir sonraki güne atar (CME: 17:00 CT sonrası = ertesi gün).
    pub fn trading_day(&self, ts: UnixNanos) -> NaiveDate {
        match self {
            SessionSpec::Always => to_utc(ts).date_naive(),
            SessionSpec::Weekly(w) => {
                let l = w.local(ts);
                let tod = l.time().num_seconds_from_midnight() / 60;
                let roll = w.open_week_min % 1440;
                if roll > 0 && tod >= roll {
                    l.date() + chrono::Duration::days(1)
                } else {
                    l.date()
                }
            }
        }
    }

    /// `[from, to)` aralığında seansın açık olduğu dakika başlangıçlarını sayar (boşluk tespiti).
    pub fn open_minutes_between(&self, from: UnixNanos, to: UnixNanos) -> u64 {
        let step = crate::units::NANOS_PER_MIN;
        let mut t = UnixNanos(from.0 - from.0 % step);
        if t < from {
            t = t.plus(step);
        }
        let mut n = 0u64;
        while t < to {
            if self.is_open(t) {
                n += 1;
            }
            t = t.plus(step);
        }
        n
    }
}

fn at_minute(date: NaiveDate, minute: u32) -> NaiveDateTime {
    let m = minute.min(1440);
    if m == 1440 {
        return (date + chrono::Duration::days(1)).and_time(NaiveTime::MIN);
    }
    let t = NaiveTime::from_num_seconds_from_midnight_opt(m * 60, 0).unwrap_or(NaiveTime::MIN);
    date.and_time(t)
}

/// `configs/calendar.toml` biçimi.
#[derive(Debug, Deserialize)]
struct RawCalendarFile {
    #[serde(default)]
    session: Vec<RawSession>,
}

#[derive(Debug, Deserialize)]
struct RawSession {
    name: String,
    #[serde(default)]
    always: bool,
    #[serde(default)]
    tz: String,
    #[serde(default)]
    open: String,
    #[serde(default)]
    close: String,
    #[serde(default)]
    daily_break: Option<[String; 2]>,
    #[serde(default)]
    closure: Vec<RawClosure>,
}

#[derive(Debug, Deserialize)]
struct RawClosure {
    date: String,
    #[serde(default = "zero_time")]
    from: String,
    #[serde(default = "end_time")]
    until: String,
    #[serde(default)]
    note: String,
}

fn zero_time() -> String {
    "00:00".into()
}
fn end_time() -> String {
    "24:00".into()
}

/// "sun 17:00" → hafta dakikası.
fn parse_week_point(s: &str) -> Result<u32, CalendarError> {
    let (d, t) = s
        .trim()
        .split_once(' ')
        .ok_or_else(|| CalendarError::BadTime(s.to_string()))?;
    Ok(weekday_from_str(d)? * 1440 + parse_hhmm(t)?)
}

/// İsimli seans tanımları.
#[derive(Debug, Clone, Default)]
pub struct Calendar {
    sessions: BTreeMap<String, SessionSpec>,
}

impl Calendar {
    pub fn from_toml_str(text: &str) -> Result<Self, CalendarError> {
        let raw: RawCalendarFile =
            toml::from_str(text).map_err(|e| CalendarError::Toml(e.to_string()))?;
        let mut sessions = BTreeMap::new();
        for s in raw.session {
            let spec = if s.always {
                SessionSpec::Always
            } else {
                let tz = parse_tz(&s.tz)?;
                let daily_break = match &s.daily_break {
                    Some([a, b]) => Some((parse_hhmm(a)?, parse_hhmm(b)?)),
                    None => None,
                };
                let mut closures: BTreeMap<NaiveDate, Vec<ClosedInterval>> = BTreeMap::new();
                for c in s.closure {
                    let date = NaiveDate::parse_from_str(c.date.trim(), "%Y-%m-%d")
                        .map_err(|_| CalendarError::BadDate(c.date.clone()))?;
                    closures.entry(date).or_default().push(ClosedInterval {
                        date,
                        from_min: parse_hhmm(&c.from)?,
                        until_min: parse_hhmm(&c.until)?,
                        note: c.note,
                    });
                }
                SessionSpec::Weekly(WeeklySession {
                    tz,
                    open_week_min: parse_week_point(&s.open)?,
                    close_week_min: parse_week_point(&s.close)?,
                    daily_break,
                    closures,
                })
            };
            sessions.insert(s.name, spec);
        }
        Ok(Calendar { sessions })
    }

    pub fn session(&self, name: &str) -> Result<&SessionSpec, CalendarError> {
        self.sessions
            .get(name)
            .ok_or_else(|| CalendarError::UnknownSession(name.to_string()))
    }

    pub fn names(&self) -> impl Iterator<Item = &String> {
        self.sessions.keys()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAL: &str = r#"
[[session]]
name = "cme_globex"
tz = "America/Chicago"
open = "sun 17:00"
close = "fri 16:00"
daily_break = ["16:00", "17:00"]
[[session.closure]]
date = "2026-12-25"
note = "test"

[[session]]
name = "always"
always = true
"#;

    fn ts(s: &str) -> UnixNanos {
        parse_rfc3339(s).unwrap()
    }

    #[test]
    fn globex_weekly_window_and_break_cdt() {
        let c = Calendar::from_toml_str(CAL).unwrap();
        let g = c.session("cme_globex").unwrap();
        // 2026-10-30 Cuma, CDT (UTC-5). 15:59 CT = 20:59Z açık, 16:00 CT = 21:00Z kapalı.
        assert!(g.is_open(ts("2026-10-30T20:59:00Z")));
        assert!(!g.is_open(ts("2026-10-30T21:00:00Z")));
        // Cumartesi kapalı
        assert!(!g.is_open(ts("2026-10-31T15:00:00Z")));
        // Perşembe ara: 16:30 CT = 21:30Z kapalı
        assert!(!g.is_open(ts("2026-10-29T21:30:00Z")));
        assert!(g.is_open(ts("2026-10-29T22:00:00Z")));
    }

    #[test]
    fn dst_shift_is_respected() {
        let c = Calendar::from_toml_str(CAL).unwrap();
        let g = c.session("cme_globex").unwrap();
        // ABD kış saati 2026-11-01. 2026-11-02 Pazartesi CST (UTC-6): 16:00 CT = 22:00Z.
        assert!(g.is_open(ts("2026-11-02T21:59:00Z")));
        assert!(!g.is_open(ts("2026-11-02T22:00:00Z")));
        assert!(g.is_open(ts("2026-11-02T23:00:00Z")));
        // Pazar 2026-11-01 17:00 CST = 23:00Z açılış
        assert!(!g.is_open(ts("2026-11-01T22:59:00Z")));
        assert!(g.is_open(ts("2026-11-01T23:00:00Z")));
    }

    #[test]
    fn closures_apply() {
        let c = Calendar::from_toml_str(CAL).unwrap();
        let g = c.session("cme_globex").unwrap();
        // 2026-12-25 Cuma tüm gün kapalı (yerel)
        assert!(!g.is_open(ts("2026-12-25T15:00:00Z")));
        assert!(g.is_open(ts("2026-12-24T15:00:00Z")));
    }

    #[test]
    fn next_close_and_week_close() {
        let c = Calendar::from_toml_str(CAL).unwrap();
        let g = c.session("cme_globex").unwrap();
        // Perşembe 10:00 CT → aynı gün 16:00 CT (21:00Z, CDT)
        assert_eq!(
            g.next_close(ts("2026-10-29T15:00:00Z")),
            Some(ts("2026-10-29T21:00:00Z"))
        );
        assert_eq!(
            g.next_week_close(ts("2026-10-27T15:00:00Z")),
            Some(ts("2026-10-30T21:00:00Z"))
        );
        // Kapalıyken None
        assert_eq!(g.next_close(ts("2026-10-31T15:00:00Z")), None);
        // Tatil öncesi gün: 24 Aralık Perşembe 18:00 CT açık; sonraki kapanış 25 Aralık 00:00 yerel
        assert_eq!(
            g.next_close(ts("2026-12-25T00:00:00Z")),
            Some(ts("2026-12-25T06:00:00Z"))
        );
        let a = c.session("always").unwrap();
        assert_eq!(a.next_close(ts("2026-10-29T15:00:00Z")), None);
        assert!(a.is_open(ts("2026-10-31T15:00:00Z")));
    }

    #[test]
    fn trading_day_rolls_at_open() {
        let c = Calendar::from_toml_str(CAL).unwrap();
        let g = c.session("cme_globex").unwrap();
        // Pazartesi 17:30 CT → işlem günü Salı
        let d = g.trading_day(ts("2026-10-26T22:30:00Z"));
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 10, 27).unwrap());
        let d2 = g.trading_day(ts("2026-10-26T15:00:00Z"));
        assert_eq!(d2, NaiveDate::from_ymd_opt(2026, 10, 26).unwrap());
    }

    #[test]
    fn open_minutes_count() {
        let c = Calendar::from_toml_str(CAL).unwrap();
        let g = c.session("cme_globex").unwrap();
        // Perşembe 15:00Z–23:00Z: 21:00–22:00Z ara → 7 saat açık
        let n = g.open_minutes_between(ts("2026-10-29T15:00:00Z"), ts("2026-10-29T23:00:00Z"));
        assert_eq!(n, 7 * 60);
    }
}

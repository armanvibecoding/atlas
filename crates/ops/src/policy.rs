//! Uyandırma politikası (§5): öncelik, dedupe, sessiz saatler (23:30–09:00 TSİ), uyandırma limiti,
//! gece özeti. **Hiçbir olay sessizce düşmez** (ders §9.4): uyandırmayan her olay özete girer.

use atlas_core::calendar::to_utc;
use atlas_core::units::{NANOS_PER_HOUR, NANOS_PER_MIN};
use atlas_core::{Event, Priority, UnixNanos};
use chrono::Timelike;
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WakeConfig {
    pub tz: String,
    /// Sessiz saat başlangıcı ve bitişi (yerel, SS:DD).
    pub quiet_from: String,
    pub quiet_to: String,
    /// Saatte en fazla uyandırma (aşan HIGH olaylar tek "toplu" uyandırmada birleşir).
    pub max_wakes_per_hour: usize,
    /// Aynı `key` bu süre içinde tekrar uyandırmaz (dakika).
    pub dedupe_minutes: u64,
    /// Gece özetinin gönderileceği yerel saat.
    pub digest_at: String,
}

impl Default for WakeConfig {
    fn default() -> Self {
        WakeConfig {
            tz: "Europe/Istanbul".into(),
            quiet_from: "23:30".into(),
            quiet_to: "09:00".into(),
            max_wakes_per_hour: 6,
            dedupe_minutes: 30,
            digest_at: "09:00".into(),
        }
    }
}

fn hhmm(s: &str) -> u32 {
    s.split_once(':')
        .and_then(|(h, m)| Some(h.trim().parse::<u32>().ok()? * 60 + m.trim().parse::<u32>().ok()?))
        .unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Route {
    /// Hemen uyandır.
    Wake,
    /// Uyandırma limiti aşıldı: toplu uyandırmaya eklenir (yine de teslim edilir).
    Batched,
    /// Gece özetine.
    Digest,
    /// Aynı anahtar yakın zamanda uyandırdı: özete (sayısı tutulur).
    Deduped,
}

pub struct WakePolicy {
    cfg: WakeConfig,
    tz: Tz,
    recent_wakes: VecDeque<UnixNanos>,
    last_key: BTreeMap<String, UnixNanos>,
}

impl WakePolicy {
    pub fn new(cfg: WakeConfig) -> WakePolicy {
        let tz = cfg.tz.parse::<Tz>().unwrap_or(chrono_tz::Europe::Istanbul);
        WakePolicy {
            cfg,
            tz,
            recent_wakes: VecDeque::new(),
            last_key: BTreeMap::new(),
        }
    }

    pub fn local_minute(&self, ts: UnixNanos) -> u32 {
        let l = to_utc(ts).with_timezone(&self.tz);
        l.hour() * 60 + l.minute()
    }

    pub fn is_quiet(&self, ts: UnixNanos) -> bool {
        let m = self.local_minute(ts);
        let (a, b) = (hhmm(&self.cfg.quiet_from), hhmm(&self.cfg.quiet_to));
        if a <= b {
            m >= a && m < b
        } else {
            m >= a || m < b
        }
    }

    /// Gece özeti saati geldi mi (bu dakika)?
    pub fn digest_due(&self, ts: UnixNanos) -> bool {
        self.local_minute(ts) == hhmm(&self.cfg.digest_at)
    }

    pub fn route(&mut self, e: &Event, now: UnixNanos) -> Route {
        if e.priority == Priority::Info {
            return Route::Digest;
        }
        if e.priority == Priority::Medium && self.is_quiet(now) {
            return Route::Digest;
        }
        if let Some(k) = &e.key {
            if let Some(t) = self.last_key.get(k)
                && now.since(*t) < self.cfg.dedupe_minutes * NANOS_PER_MIN
            {
                return Route::Deduped;
            }
            self.last_key.insert(k.clone(), now);
        }
        while self
            .recent_wakes
            .front()
            .is_some_and(|t| now.since(*t) >= NANOS_PER_HOUR)
        {
            self.recent_wakes.pop_front();
        }
        if self.recent_wakes.len() >= self.cfg.max_wakes_per_hour {
            return Route::Batched;
        }
        self.recent_wakes.push_back(now);
        Route::Wake
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_core::EventKind;
    use atlas_core::calendar::parse_rfc3339;

    fn t(s: &str) -> UnixNanos {
        parse_rfc3339(s).unwrap()
    }

    #[test]
    fn quiet_hours_istanbul() {
        let p = WakePolicy::new(WakeConfig::default());
        // TSİ = UTC+3. 21:00Z = 00:00 TSİ (sessiz), 07:00Z = 10:00 TSİ (değil), 20:29Z = 23:29 (değil)
        assert!(p.is_quiet(t("2026-11-01T21:00:00Z")));
        assert!(!p.is_quiet(t("2026-11-01T07:00:00Z")));
        assert!(!p.is_quiet(t("2026-11-01T20:29:00Z")));
        assert!(p.is_quiet(t("2026-11-01T20:30:00Z")));
        assert!(p.digest_due(t("2026-11-02T06:00:00Z")));
    }

    #[test]
    fn routing_rules() {
        let mut p = WakePolicy::new(WakeConfig::default());
        let day = t("2026-11-01T10:00:00Z");
        let night = t("2026-11-01T22:00:00Z");
        let info = Event::new(day, EventKind::Heartbeat, serde_json::json!({}));
        let med = Event::new(day, EventKind::DataQuality, serde_json::json!({}));
        let high = Event::new(day, EventKind::KillSwitch, serde_json::json!({})).with_key("k1");
        assert_eq!(p.route(&info, day), Route::Digest);
        assert_eq!(p.route(&med, day), Route::Wake);
        assert_eq!(p.route(&med, night), Route::Digest);
        assert_eq!(p.route(&high, night), Route::Wake);
        // Aynı anahtar 30 dk içinde → dedupe
        assert_eq!(
            p.route(&high, night.plus(15 * NANOS_PER_MIN)),
            Route::Deduped
        );
        // Limit: saatte 6 uyandırma
        let mut q = WakePolicy::new(WakeConfig::default());
        for i in 0..6 {
            let e = Event::new(day, EventKind::RiskBreach, serde_json::json!({}))
                .with_key(format!("r{i}"));
            assert_eq!(q.route(&e, day), Route::Wake);
        }
        let e = Event::new(day, EventKind::RiskBreach, serde_json::json!({})).with_key("r9");
        assert_eq!(q.route(&e, day), Route::Batched);
        assert_eq!(q.route(&e, day.plus(NANOS_PER_HOUR)), Route::Wake);
    }
}

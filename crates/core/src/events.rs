//! Sistem olayları — tek kaynak (§5).
//!
//! Her olay: `id`, `ts`, `kind`, `priority`, `key` (dedupe), `payload`.
//! INFO olayları tek başına uyandırmaz ama **asla sessizce düşürülmez** (ders §9.4).

use crate::units::UnixNanos;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    /// Sıralama: High < Medium < Info (önce en acil).
    High,
    Medium,
    Info,
}

impl Priority {
    pub const fn as_str(self) -> &'static str {
        match self {
            Priority::High => "high",
            Priority::Medium => "medium",
            Priority::Info => "info",
        }
    }
    /// `self` en az `min` kadar acil mi?
    pub fn at_least(self, min: Priority) -> bool {
        self <= min
    }
}

impl FromStr for Priority {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "high" => Ok(Priority::High),
            "medium" => Ok(Priority::Medium),
            "info" => Ok(Priority::Info),
            other => Err(format!("bilinmeyen öncelik: {other}")),
        }
    }
}

impl fmt::Display for Priority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

macro_rules! event_kinds {
    ($( $variant:ident => $name:literal, $prio:ident; )*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum EventKind {
            $( $variant, )*
        }

        impl EventKind {
            pub const ALL: &'static [EventKind] = &[ $( EventKind::$variant, )* ];

            pub const fn as_str(self) -> &'static str {
                match self { $( EventKind::$variant => $name, )* }
            }

            pub const fn priority(self) -> Priority {
                match self { $( EventKind::$variant => Priority::$prio, )* }
            }
        }

        impl FromStr for EventKind {
            type Err = String;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s { $( $name => Ok(EventKind::$variant), )* other => Err(format!("bilinmeyen olay türü: {other}")) }
            }
        }
    };
}

event_kinds! {
    // HIGH
    KillSwitch => "kill_switch", High;
    RiskBreach => "risk_breach", High;
    DailyLossLimit => "daily_loss_limit", High;
    PropDrawdownNear => "prop_drawdown_near", High;
    ConsistencyHeadroom => "consistency_headroom", High;
    FeedDown => "feed_down", High;
    EngineFailure => "engine_failure", High;
    ClockSkew => "clock_skew", High;
    DriftAlarm => "drift_alarm", High;
    ApprovalRequest => "approval_request", High;
    BudgetExhausted => "budget_exhausted", High;
    OutboxDead => "outbox_dead", High;
    // MEDIUM
    PositionOpened => "position_opened", Medium;
    PositionClosed => "position_closed", Medium;
    Promotion => "promotion", Medium;
    Demotion => "demotion", Medium;
    GauntletPass => "gauntlet_pass", Medium;
    DataQuality => "data_quality", Medium;
    FillDivergence => "fill_divergence", Medium;
    // INFO
    Heartbeat => "heartbeat", Info;
    ExperimentDone => "experiment_done", Info;
    OperatorNote => "operator_note", Info;
    Fill => "fill", Info;
    IntentRejected => "intent_rejected", Info;
    FeedUp => "feed_up", Info;
    KillReset => "kill_reset", Info;
    StateCheckpoint => "state_checkpoint", Info;
    ConfigLoaded => "config_loaded", Info;
    DataPulled => "data_pulled", Info;
    DayRollover => "day_rollover", Info;
    Calibration => "calibration", Info;
    ShadowSignal => "shadow_signal", Info;
    AgentCall => "agent_call", Info;
    AgentInvalidOutput => "agent_invalid_output", Info;
    SearchCycle => "search_cycle", Info;
    StageChange => "stage_change", Info;
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Journal tarafından atanır (boşsa).
    pub id: String,
    pub ts: UnixNanos,
    pub kind: EventKind,
    pub priority: Priority,
    /// Dedupe anahtarı (aynı anahtar = aynı durumun tekrarı).
    pub key: Option<String>,
    pub payload: serde_json::Value,
}

impl Event {
    /// Önceliği türden türeten kurucu.
    pub fn new(ts: UnixNanos, kind: EventKind, payload: serde_json::Value) -> Self {
        Event {
            id: String::new(),
            ts,
            kind,
            priority: kind.priority(),
            key: None,
            payload,
        }
    }

    pub fn with_key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priorities_match_spec() {
        assert_eq!(EventKind::KillSwitch.priority(), Priority::High);
        assert_eq!(EventKind::OutboxDead.priority(), Priority::High);
        assert_eq!(EventKind::DataQuality.priority(), Priority::Medium);
        assert_eq!(EventKind::Heartbeat.priority(), Priority::Info);
    }

    #[test]
    fn roundtrip_names() {
        for k in EventKind::ALL {
            assert_eq!(k.as_str().parse::<EventKind>().unwrap(), *k);
            let j = serde_json::to_string(k).unwrap();
            assert_eq!(j, format!("\"{}\"", k.as_str()));
        }
    }

    #[test]
    fn priority_ordering() {
        assert!(Priority::High.at_least(Priority::Medium));
        assert!(!Priority::Info.at_least(Priority::Medium));
        assert_eq!("HIGH".parse::<Priority>().unwrap(), Priority::High);
    }
}

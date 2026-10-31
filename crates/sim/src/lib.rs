//! Olay tabanlı simülasyon: motor, dolum modelleri, maliyet, M1 doğrulamaları.

pub mod engine;
pub mod fill;
pub mod synthetic;
pub mod validation;

pub use engine::{Engine, SessionAdapter, SimConfig, SimError, SimResult, TradeRecord};
pub use fill::{BarFill, FillModel, LimitRule, TradeFill};

//! Olay tabanlı simülasyon: motor, dolum modelleri, maliyet, M1 doğrulamaları.

pub mod calibration;
pub mod engine;
pub mod fill;
pub mod fill_queue;
pub mod synthetic;
pub mod validation;

pub use calibration::{
    CalibrationError, CalibrationStore, FillObservation, FillParams, ObservedKind, SlippageStats,
    calibrate, measure,
};
pub use engine::{Engine, SessionAdapter, SimConfig, SimError, SimResult, TradeRecord};
pub use fill::{BarFill, FillModel, LimitRule, TradeFill};
pub use fill_queue::{MboQueueFill, QueueConfig};

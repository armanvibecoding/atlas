//! Örnek Atlas plugin'i (ABI v1, bkz. `crates/plugin/src/lib.rs` ve ADR 0004).
//!
//! Strateji: düzken son barın kapanışı önceki 20 barın en yükseğinin üstündeyse alış, en düşüğünün
//! altındaysa satış. Stop kanalın karşı ucu, hedef 2R, zaman çıkışı 60 bar. Adet 1.
//!
//! Plugin hiçbir import kullanmaz (ağ/dosya/saat yok); yalnız `memory`, `alloc`, `on_event` dışa aktarır.
//! Durum tutmaz: karar yalnız girdideki bar penceresine bağlıdır.

use serde::{Deserialize, Serialize};
use std::cell::RefCell;

const LOOKBACK: usize = 20;
const TARGET_R: i64 = 2;
const TIME_EXIT_BARS: u32 = 60;

#[derive(Deserialize)]
struct Bar {
    high: i64,
    low: i64,
    close: i64,
}

#[derive(Deserialize)]
struct Input {
    abi: u32,
    bars: Vec<Bar>,
    position: Option<serde_json::Value>,
    pending_entry: bool,
}

#[derive(Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum Out {
    Open {
        side: &'static str,
        qty: i32,
        stop_loss: i64,
        take_profit: Option<i64>,
        time_exit_bars: Option<u32>,
    },
}

thread_local! {
    /// Girdi tamponu: `alloc` yeniden boyutlandırır, host içine yazar.
    static INPUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    /// Çıktı tamponu: bir sonraki `on_event` çağrısına kadar geçerli.
    static OUTPUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

#[unsafe(no_mangle)]
pub extern "C" fn alloc(len: i32) -> i32 {
    INPUT.with(|b| {
        let mut b = b.borrow_mut();
        b.clear();
        b.resize(len.max(0) as usize, 0);
        b.as_mut_ptr() as i32
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn on_event(_ptr: i32, len: i32) -> i64 {
    let outs = INPUT.with(|b| {
        let b = b.borrow();
        let n = (len.max(0) as usize).min(b.len());
        match serde_json::from_slice::<Input>(&b[..n]) {
            Ok(inp) => decide(&inp),
            Err(_) => Vec::new(),
        }
    });
    if outs.is_empty() {
        return 0;
    }
    OUTPUT.with(|o| {
        let mut o = o.borrow_mut();
        o.clear();
        if serde_json::to_writer(&mut *o, &outs).is_err() {
            return 0;
        }
        ((o.as_ptr() as u32 as i64) << 32) | (o.len() as u32 as i64)
    })
}

fn decide(inp: &Input) -> Vec<Out> {
    if inp.abi != 1 || inp.position.is_some() || inp.pending_entry {
        return Vec::new();
    }
    let n = inp.bars.len();
    if n < LOOKBACK + 1 {
        return Vec::new();
    }
    let last = &inp.bars[n - 1];
    let prior = &inp.bars[n - 1 - LOOKBACK..n - 1];
    let hi = prior.iter().map(|b| b.high).max().unwrap_or(i64::MAX);
    let lo = prior.iter().map(|b| b.low).min().unwrap_or(i64::MIN);
    let (side, stop) = if last.close > hi {
        ("buy", lo)
    } else if last.close < lo {
        ("sell", hi)
    } else {
        return Vec::new();
    };
    let risk = (last.close - stop).abs();
    if risk == 0 {
        return Vec::new();
    }
    let sg = if side == "buy" { 1 } else { -1 };
    vec![Out::Open {
        side,
        qty: 1,
        stop_loss: stop,
        take_profit: Some(last.close + sg * TARGET_R * risk),
        time_exit_bars: Some(TIME_EXIT_BARS),
    }]
}

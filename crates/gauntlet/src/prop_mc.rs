//! Kapı 9: prop hesabı Monte Carlo. Günlük işlem blokları yerine konarak örneklenir (gün içi sıra
//! korunur); her yolda firma kuralları (taban türü, kilit, günlük zarar, consistency, hedef) uygulanır.
//!
//! Basitleştirme: gün içi açık pozisyon dalgalanması görülmez (yalnız işlem kapanışları); bu
//! iyimser bir yanlılıktır, eşikler bunu telafi edecek şekilde seçilmeli (OPEN_QUESTIONS).

use atlas_core::units::{MICROS_PER_USD, NANOS_PER_DAY};
use atlas_risk::{DrawdownType, PropRules};
use atlas_sim::TradeRecord;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct PropMc {
    pub paths: usize,
    pub horizon_days: usize,
    pub pass_probability: f64,
    pub bust_probability: f64,
    pub timeout_probability: f64,
    pub median_days_to_pass: Option<f64>,
    pub days_in_sample: usize,
}

fn usd(m: atlas_core::Money) -> f64 {
    m.0 as f64 / MICROS_PER_USD as f64
}

/// `first_day..=last_day` (UTC gün numarası) aralığındaki her gün bir blok; işlemi olmayan gün boş.
pub fn day_blocks(trades: &[TradeRecord], first_day: u64, last_day: u64) -> Vec<Vec<f64>> {
    let n = (last_day.saturating_sub(first_day) + 1) as usize;
    let mut days = vec![Vec::new(); n.max(1)];
    let mut sorted: Vec<&TradeRecord> = trades.iter().collect();
    sorted.sort_by_key(|t| (t.exit_ts, t.entry_ts));
    for t in sorted {
        let d = (t.exit_ts.0 / NANOS_PER_DAY).saturating_sub(first_day) as usize;
        if let Some(b) = days.get_mut(d.min(n.saturating_sub(1))) {
            b.push(t.net.as_usd_f64());
        }
    }
    days
}

pub fn simulate(
    days: &[Vec<f64>],
    rules: &PropRules,
    paths: usize,
    horizon: usize,
    seed: u64,
) -> PropMc {
    let start = usd(rules.starting_balance);
    let dd = usd(rules.drawdown_amount);
    let target = usd(rules.profit_target);
    let lock = rules.drawdown_lock_level.map(usd);
    let daily = rules.daily_loss_limit.map(usd);
    let ratio = rules.consistency_ratio_milli.map(|r| r as f64 / 1000.0);
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let (mut pass, mut bust) = (0usize, 0usize);
    let mut pass_days: Vec<f64> = Vec::new();
    if days.is_empty() {
        return PropMc {
            paths,
            horizon_days: horizon,
            pass_probability: 0.0,
            bust_probability: 0.0,
            timeout_probability: 1.0,
            median_days_to_pass: None,
            days_in_sample: 0,
        };
    }
    for _ in 0..paths {
        let mut bal = start;
        let mut hwm = start;
        let mut floor = start - dd;
        let mut day_profits: Vec<f64> = Vec::new();
        let mut outcome = 0u8; // 0 süre doldu, 1 geçti, 2 patladı
        'path: for d in 0..horizon {
            let day = &days[rng.random_range(0..days.len())];
            let day_start = bal;
            for x in day {
                bal += x;
                if rules.drawdown_type == DrawdownType::IntradayTrailing {
                    hwm = hwm.max(bal);
                    let c = hwm - dd;
                    floor = floor.max(lock.map_or(c, |l| c.min(l)));
                }
                if bal <= floor || daily.is_some_and(|l| day_start - bal >= l) {
                    outcome = 2;
                    break 'path;
                }
            }
            if rules.drawdown_type == DrawdownType::EodTrailing {
                hwm = hwm.max(bal);
                let c = hwm - dd;
                floor = floor.max(lock.map_or(c, |l| c.min(l)));
            }
            day_profits.push(bal - day_start);
            let total = bal - start;
            if total >= target {
                let ok = ratio.is_none_or(|r| {
                    let mx = day_profits.iter().copied().fold(f64::MIN, f64::max);
                    mx <= r * total
                });
                if ok {
                    outcome = 1;
                    pass_days.push((d + 1) as f64);
                    break 'path;
                }
            }
        }
        match outcome {
            1 => pass += 1,
            2 => bust += 1,
            _ => {}
        }
    }
    pass_days.sort_by(f64::total_cmp);
    let p = paths.max(1) as f64;
    PropMc {
        paths,
        horizon_days: horizon,
        pass_probability: pass as f64 / p,
        bust_probability: bust as f64 / p,
        timeout_probability: (paths - pass - bust) as f64 / p,
        median_days_to_pass: pass_days.get(pass_days.len() / 2).copied(),
        days_in_sample: days.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(kind: &str) -> PropRules {
        PropRules::from_toml_str(&format!(
            r#"
firm = "t"
account_label = "t"
starting_balance_usd = "100000"
profit_target_usd = "10000"
drawdown_type = "{kind}"
drawdown_amount_usd = "10000"
drawdown_lock_level_usd = "none"
daily_loss_limit_usd = "5000"
daily_loss_reference = "day_start_balance"
day_reset_tz = "UTC"
day_reset_time = "00:00"
consistency_ratio = "none"
max_contracts = "5"
rules_tz = "UTC"
no_new_entries_after = "none"
flat_by = "none"
weekend_holding_allowed = true
news_blackout_minutes_before = "none"
news_blackout_minutes_after = "none"
algo_trading_allowed = true
"#
        ))
        .unwrap()
    }

    #[test]
    fn winner_passes_loser_busts() {
        let r = rules("static");
        let good = vec![vec![800.0], vec![-300.0], vec![500.0]];
        let m = simulate(&good, &r, 500, 120, 1);
        assert!(m.pass_probability > 0.95, "{m:?}");
        let bad = vec![vec![-800.0], vec![300.0], vec![-500.0]];
        let m = simulate(&bad, &r, 500, 120, 1);
        assert!(m.bust_probability > 0.95, "{m:?}");
        // Günlük limit: tek günde −5000 → patlar
        let crash = vec![vec![-2600.0, -2600.0]];
        assert_eq!(simulate(&crash, &r, 10, 5, 1).bust_probability, 1.0);
        // Deterministik
        let a = simulate(&good, &r, 200, 60, 7);
        let b = simulate(&good, &r, 200, 60, 7);
        assert_eq!(a.pass_probability, b.pass_probability);
    }
}

//! Olasılık → "gir/girme + boyut çarpanı".
//!
//! Boyut çarpanı `m ∈ [0, 1]`; birincil stratejinin adedini **yalnız küçültür**, asla büyütmez
//! (risk kapısının üstüne ek risk eklenmez):
//! - [`Sizing::Linear`] (varsayılan): `m = clamp(2p − 1, 0, 1)`. Bu, kazanç/kayıp oranı 1 olan
//!   bahsin tam Kelly kesridir (`p − (1 − p)/1`).
//! - [`Sizing::Kelly`]: `f* = p − (1 − p)/b` (`b` = ortalama kazanç/kayıp oranı, ör. hedef/stop),
//!   `m = clamp(kesir · f*, 0, 1)`. Kesir < 1 kesirli Kelly'dir (ör. 0,5 = yarım Kelly).
//! - [`Sizing::Fixed`]: girildiyse `m = 1` (yalnız filtre).
//!
//! Giriş kuralı: `p ≥ eşik` **ve** `m > 0`. Adet: `max(1, ⌊adet · m⌋)`; girilmiyorsa niyet atılır.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum Sizing {
    Linear,
    Kelly { payoff: f64, fraction: f64 },
    Fixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Policy {
    /// En düşük giriş olasılığı.
    pub threshold: f64,
    pub sizing: Sizing,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            threshold: 0.55,
            sizing: Sizing::Linear,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    pub p: f64,
    pub enter: bool,
    /// [0, 1]; `enter == false` ise 0.
    pub size_mult: f64,
}

impl Policy {
    pub fn decide(&self, p: f64) -> Decision {
        let raw = match self.sizing {
            Sizing::Linear => 2.0 * p - 1.0,
            Sizing::Kelly { payoff, fraction } => {
                if payoff.is_finite() && payoff > 0.0 && fraction.is_finite() {
                    fraction * (p - (1.0 - p) / payoff)
                } else {
                    0.0
                }
            }
            Sizing::Fixed => 1.0,
        };
        let m = if raw.is_finite() {
            raw.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let enter = p.is_finite() && p >= self.threshold && m > 0.0;
        Decision {
            p,
            enter,
            size_mult: if enter { m } else { 0.0 },
        }
    }

    /// Kararı birincil adede uygular; girilmiyorsa `None`.
    pub fn scale_qty(qty: i32, d: &Decision) -> Option<i32> {
        if !d.enter || qty <= 0 {
            return None;
        }
        let scaled = (f64::from(qty) * d.size_mult).floor();
        // ⌊·⌋ ≤ qty (m ≤ 1) → i32'ye sığar.
        let q = if scaled >= 1.0 { scaled as i32 } else { 1 };
        Some(q.clamp(1, qty))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_policy() {
        let p = Policy {
            threshold: 0.55,
            sizing: Sizing::Linear,
        };
        let d = p.decide(0.5);
        assert!(!d.enter && d.size_mult == 0.0);
        let d = p.decide(0.54);
        assert!(!d.enter);
        let d = p.decide(0.75);
        assert!(d.enter && (d.size_mult - 0.5).abs() < 1e-12);
        assert_eq!(Policy::scale_qty(10, &d), Some(5));
        assert_eq!(Policy::scale_qty(1, &d), Some(1));
        let d = p.decide(1.0);
        assert_eq!(Policy::scale_qty(7, &d), Some(7));
        assert!(!p.decide(f64::NAN).enter);
        assert_eq!(Policy::scale_qty(3, &p.decide(0.1)), None);
    }

    #[test]
    fn kelly_and_fixed() {
        let k = Policy {
            threshold: 0.0,
            sizing: Sizing::Kelly {
                payoff: 2.0,
                fraction: 0.5,
            },
        };
        // f* = 0.4 − 0.6/2 = 0.1 → m = 0.05.
        let d = k.decide(0.4);
        assert!(d.enter && (d.size_mult - 0.05).abs() < 1e-12);
        // f* < 0 → girme.
        assert!(!k.decide(0.3).enter);
        let f = Policy {
            threshold: 0.6,
            sizing: Sizing::Fixed,
        };
        assert_eq!(Policy::scale_qty(4, &f.decide(0.61)), Some(4));
        assert_eq!(Policy::scale_qty(4, &f.decide(0.59)), None);
    }
}

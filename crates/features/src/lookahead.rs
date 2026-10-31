//! Lookahead dedektörü (§4.2, kural 4).
//!
//! Her kesim noktası `k` için: barların `k` ve sonrası bozulur (fiyatlar aynalanır, hacim
//! değiştirilir) ve feature yeniden hesaplanır. `k`'dan önceki değerler bit düzeyinde aynı kalmalı.
//! Değişirse feature gelecekteki veriye duyarlıdır → test kırılır.

use crate::calc::{Calc, day_of};
use crate::spec::FeatureSpec;
use atlas_core::{Bar, Price, SessionSpec};

#[derive(Debug, Clone, PartialEq)]
pub struct Violation {
    pub feature: String,
    pub cut: usize,
    pub index: usize,
    pub clean: f64,
    pub perturbed: f64,
}

/// Fabrika, hesaplanacak serinin tamamını görür: gerçek feature'lar bunu yok sayar; hileli bir
/// hesaplayıcı (ör. tüm seriyi önceden okuyan) bu yolla geleceğe bakabilir — dedektör bunu yakalar.
pub type Factory<'a> = &'a dyn Fn(&[Bar]) -> Box<dyn Calc>;

pub fn perturb(bars: &[Bar], cut: usize) -> Vec<Bar> {
    let mut out = bars.to_vec();
    if cut >= out.len() {
        return out;
    }
    let pivot = bars[cut].open.0;
    for b in out.iter_mut().skip(cut) {
        let m = |p: Price| Price(2 * pivot - p.0 + 7);
        let (o, c) = (m(b.open), m(b.close));
        let (h, l) = (m(b.low), m(b.high));
        b.open = o;
        b.close = c;
        b.high = h.max(o).max(c);
        b.low = l.min(o).min(c);
        b.volume = b.volume * 3 + 11;
        b.buy_volume = b.volume / 5;
    }
    out
}

fn run(make: Factory<'_>, bars: &[Bar], session: &SessionSpec) -> Vec<f64> {
    let mut c = make(bars);
    bars.iter()
        .map(|b| c.update(b, day_of(session, b)).unwrap_or(f64::NAN))
        .collect()
}

fn same(a: f64, b: f64) -> bool {
    (a.is_nan() && b.is_nan()) || a.to_bits() == b.to_bits()
}

pub fn check_calc(
    name: &str,
    make: Factory<'_>,
    bars: &[Bar],
    session: &SessionSpec,
    cuts: &[usize],
) -> Result<(), Violation> {
    let clean = run(make, bars, session);
    for &cut in cuts {
        let p = run(make, &perturb(bars, cut), session);
        for i in 0..cut.min(bars.len()) {
            if !same(clean[i], p[i]) {
                return Err(Violation {
                    feature: name.to_string(),
                    cut,
                    index: i,
                    clean: clean[i],
                    perturbed: p[i],
                });
            }
        }
    }
    Ok(())
}

pub fn check(
    spec: &FeatureSpec,
    bars: &[Bar],
    session: &SessionSpec,
    cuts: &[usize],
) -> Result<(), Violation> {
    let s = *spec;
    check_calc(
        &s.to_string(),
        &move |_| crate::calc::make(&s),
        bars,
        session,
        cuts,
    )
}

pub fn default_cuts(n: usize) -> Vec<usize> {
    [n / 7, n / 3, n / 2, (2 * n) / 3, n - n / 10]
        .into_iter()
        .filter(|c| *c > 0 && *c < n)
        .collect()
}

/// Tüm feature türlerini, alt/varsayılan/üst parametrelerle denetler.
pub fn check_all(bars: &[Bar], session: &SessionSpec) -> Vec<Result<String, Violation>> {
    let cuts = default_cuts(bars.len());
    let mut out = Vec::new();
    for k in crate::spec::Kind::ALL {
        let (lo, hi, def) = k.range();
        let params: Vec<u32> = if k.has_param() {
            vec![lo, def, hi.min(500)]
        } else {
            vec![0]
        };
        for p in params {
            let s = FeatureSpec::new(*k, p);
            out.push(check(&s, bars, session, &cuts).map(|_| s.to_string()));
        }
    }
    out
}

/// Bilinçli olarak geleceğe bakan hesaplayıcı: bir sonraki barın kapanışını döndürür.
/// Dedektörün çalıştığını kanıtlamak için (testler ve `verify m2`).
pub struct Cheater {
    future: Vec<f64>,
    i: usize,
}

impl Cheater {
    pub fn factory(bars: &[Bar]) -> Box<dyn Calc> {
        Box::new(Cheater {
            future: bars.iter().map(|b| b.close.0 as f64).collect(),
            i: 0,
        })
    }
}

impl Calc for Cheater {
    fn update(&mut self, _b: &Bar, _d: chrono::NaiveDate) -> Option<f64> {
        let v = self.future.get(self.i + 1).copied();
        self.i += 1;
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_real_features_are_point_in_time() {
        let b = crate::tests::bars(3000);
        for r in check_all(&b, &SessionSpec::Always) {
            assert!(r.is_ok(), "{r:?}");
        }
    }

    #[test]
    fn detector_catches_lookahead() {
        let b = crate::tests::bars(500);
        let r = check_calc(
            "cheater",
            &Cheater::factory,
            &b,
            &SessionSpec::Always,
            &[100, 250],
        );
        let v = r.unwrap_err();
        // Kesimden hemen önceki bar, kesimdeki (bozulmuş) kapanışı okur → yakalanır.
        assert_eq!((v.cut, v.index), (100, 99));
    }
}

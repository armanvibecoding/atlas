//! Artımlı (incremental) feature hesaplayıcıları. Her `update` yalnız o ana kadarki barları görür.
//! Değer yoksa (ısınma) `None`.

use crate::spec::{FeatureSpec, Kind};
use atlas_core::units::{NANOS_PER_HOUR, NANOS_PER_MIN};
use atlas_core::{Bar, SessionSpec};
use chrono::NaiveDate;
use std::collections::VecDeque;

/// Gün anahtarı: seansın işlem günü.
pub fn day_of(session: &SessionSpec, b: &Bar) -> NaiveDate {
    session.trading_day(b.open_ts)
}

pub trait Calc: Send {
    fn update(&mut self, b: &Bar, day: NaiveDate) -> Option<f64>;
}

pub fn make(spec: &FeatureSpec) -> Box<dyn Calc> {
    let n = spec.param as usize;
    match spec.kind {
        Kind::Open => Box::new(Field(|b: &Bar| b.open.0 as f64)),
        Kind::High => Box::new(Field(|b: &Bar| b.high.0 as f64)),
        Kind::Low => Box::new(Field(|b: &Bar| b.low.0 as f64)),
        Kind::Close => Box::new(Field(|b: &Bar| b.close.0 as f64)),
        Kind::Volume => Box::new(Field(|b: &Bar| b.volume as f64)),
        Kind::Delta => Box::new(Field(|b: &Bar| b.delta() as f64)),
        Kind::Imbalance => Box::new(Field(|b: &Bar| {
            if b.volume == 0 {
                0.0
            } else {
                b.buy_volume as f64 / b.volume as f64 - 0.5
            }
        })),
        Kind::Ema => Box::new(Ema::new(n)),
        Kind::Sma => Box::new(Sma::new(n)),
        Kind::Atr => Box::new(Atr::new(n)),
        Kind::Highest => Box::new(Extreme::new(n, true)),
        Kind::Lowest => Box::new(Extreme::new(n, false)),
        Kind::Vwap => Box::new(Vwap::default()),
        Kind::Cvd => Box::new(Cvd::default()),
        Kind::Pdh => Box::new(PrevDay::new(true)),
        Kind::Pdl => Box::new(PrevDay::new(false)),
        Kind::AsiaHigh => Box::new(Window::new(0, 7, true)),
        Kind::AsiaLow => Box::new(Window::new(0, 7, false)),
        Kind::LondonHigh => Box::new(Window::new(7, 16, true)),
        Kind::LondonLow => Box::new(Window::new(7, 16, false)),
        Kind::OrHigh => Box::new(OpeningRange::new(n, true)),
        Kind::OrLow => Box::new(OpeningRange::new(n, false)),
        Kind::Poc => Box::new(Profile::new(ProfileOut::Poc)),
        Kind::Vah => Box::new(Profile::new(ProfileOut::Vah)),
        Kind::Val => Box::new(Profile::new(ProfileOut::Val)),
        Kind::Absorption => Box::new(Absorption::new(n)),
        Kind::SweepLow => Box::new(Sweep::new(n, false)),
        Kind::SweepHigh => Box::new(Sweep::new(n, true)),
        Kind::CvdDivBull => Box::new(CvdDiv::new(n, true)),
        Kind::CvdDivBear => Box::new(CvdDiv::new(n, false)),
        Kind::Rsi => Box::new(Rsi::new(n)),
        Kind::MinuteOfDay => Box::new(Field(|b: &Bar| {
            ((b.open_ts.0 % atlas_core::units::NANOS_PER_DAY) / NANOS_PER_MIN) as f64
        })),
    }
}

struct Field<F: Fn(&Bar) -> f64 + Send>(F);
impl<F: Fn(&Bar) -> f64 + Send> Calc for Field<F> {
    fn update(&mut self, b: &Bar, _: NaiveDate) -> Option<f64> {
        Some((self.0)(b))
    }
}

struct Ema {
    n: usize,
    k: f64,
    v: Option<f64>,
    seen: usize,
    seed_sum: f64,
}
impl Ema {
    fn new(n: usize) -> Self {
        let n = n.max(1);
        Ema {
            n,
            k: 2.0 / (n as f64 + 1.0),
            v: None,
            seen: 0,
            seed_sum: 0.0,
        }
    }
}
impl Calc for Ema {
    fn update(&mut self, b: &Bar, _: NaiveDate) -> Option<f64> {
        let x = b.close.0 as f64;
        self.seen += 1;
        match self.v {
            None => {
                self.seed_sum += x;
                if self.seen >= self.n {
                    self.v = Some(self.seed_sum / self.n as f64);
                }
            }
            Some(v) => self.v = Some(v + self.k * (x - v)),
        }
        self.v
    }
}

struct Sma {
    n: usize,
    buf: VecDeque<f64>,
    sum: f64,
}
impl Sma {
    fn new(n: usize) -> Self {
        Sma {
            n: n.max(1),
            buf: VecDeque::new(),
            sum: 0.0,
        }
    }
}
impl Calc for Sma {
    fn update(&mut self, b: &Bar, _: NaiveDate) -> Option<f64> {
        let x = b.close.0 as f64;
        self.buf.push_back(x);
        self.sum += x;
        if self.buf.len() > self.n {
            self.sum -= self.buf.pop_front().unwrap_or(0.0);
        }
        (self.buf.len() == self.n).then(|| self.sum / self.n as f64)
    }
}

/// Wilder ATR (tick).
struct Atr {
    n: usize,
    prev_close: Option<i64>,
    v: Option<f64>,
    seen: usize,
    seed: f64,
}
impl Atr {
    fn new(n: usize) -> Self {
        Atr {
            n: n.max(1),
            prev_close: None,
            v: None,
            seen: 0,
            seed: 0.0,
        }
    }
}
impl Calc for Atr {
    fn update(&mut self, b: &Bar, _: NaiveDate) -> Option<f64> {
        let tr = match self.prev_close {
            Some(pc) => (b.high.0 - b.low.0)
                .max((b.high.0 - pc).abs())
                .max((b.low.0 - pc).abs()),
            None => b.high.0 - b.low.0,
        } as f64;
        self.prev_close = Some(b.close.0);
        self.seen += 1;
        match self.v {
            None => {
                self.seed += tr;
                if self.seen >= self.n {
                    self.v = Some(self.seed / self.n as f64);
                }
            }
            Some(v) => self.v = Some((v * (self.n as f64 - 1.0) + tr) / self.n as f64),
        }
        self.v
    }
}

/// Önceki `n` barın en yüksek high / en düşük low'u (mevcut bar HARİÇ).
struct Extreme {
    n: usize,
    hi: bool,
    buf: VecDeque<i64>,
}
impl Extreme {
    fn new(n: usize, hi: bool) -> Self {
        Extreme {
            n: n.max(1),
            hi,
            buf: VecDeque::new(),
        }
    }
}
impl Calc for Extreme {
    fn update(&mut self, b: &Bar, _: NaiveDate) -> Option<f64> {
        let out = (self.buf.len() == self.n).then(|| {
            let it = self.buf.iter().copied();
            (if self.hi { it.max() } else { it.min() }).unwrap_or(0) as f64
        });
        self.buf.push_back(if self.hi { b.high.0 } else { b.low.0 });
        if self.buf.len() > self.n {
            self.buf.pop_front();
        }
        out
    }
}

/// Günlük sıfırlanan VWAP (tipik fiyat × hacim).
#[derive(Default)]
struct Vwap {
    day: Option<NaiveDate>,
    pv: f64,
    v: f64,
}
impl Calc for Vwap {
    fn update(&mut self, b: &Bar, day: NaiveDate) -> Option<f64> {
        if self.day != Some(day) {
            self.day = Some(day);
            self.pv = 0.0;
            self.v = 0.0;
        }
        let tp = (b.high.0 + b.low.0 + b.close.0) as f64 / 3.0;
        let w = b.volume.max(1) as f64;
        self.pv += tp * w;
        self.v += w;
        Some(self.pv / self.v)
    }
}

/// Günlük sıfırlanan kümülatif delta.
#[derive(Default)]
struct Cvd {
    day: Option<NaiveDate>,
    c: f64,
}
impl Calc for Cvd {
    fn update(&mut self, b: &Bar, day: NaiveDate) -> Option<f64> {
        if self.day != Some(day) {
            self.day = Some(day);
            self.c = 0.0;
        }
        self.c += b.delta() as f64;
        Some(self.c)
    }
}

/// Önceki işlem gününün high/low'u.
struct PrevDay {
    hi: bool,
    day: Option<NaiveDate>,
    cur: Option<i64>,
    prev: Option<i64>,
}
impl PrevDay {
    fn new(hi: bool) -> Self {
        PrevDay {
            hi,
            day: None,
            cur: None,
            prev: None,
        }
    }
}
impl Calc for PrevDay {
    fn update(&mut self, b: &Bar, day: NaiveDate) -> Option<f64> {
        if self.day != Some(day) {
            if self.day.is_some() {
                self.prev = self.cur;
            }
            self.day = Some(day);
            self.cur = None;
        }
        let x = if self.hi { b.high.0 } else { b.low.0 };
        self.cur = Some(match self.cur {
            None => x,
            Some(c) if self.hi => c.max(x),
            Some(c) => c.min(x),
        });
        self.prev.map(|p| p as f64)
    }
}

/// UTC saat penceresinin (Asya 00–07, Londra 07–16) son TAMAMLANMIŞ high/low'u.
struct Window {
    from_h: u64,
    to_h: u64,
    hi: bool,
    cur_key: Option<u64>,
    cur: Option<i64>,
    last_done: Option<i64>,
}
impl Window {
    fn new(from_h: u64, to_h: u64, hi: bool) -> Self {
        Window {
            from_h,
            to_h,
            hi,
            cur_key: None,
            cur: None,
            last_done: None,
        }
    }
}
impl Calc for Window {
    fn update(&mut self, b: &Bar, _: NaiveDate) -> Option<f64> {
        let day = b.open_ts.0 / atlas_core::units::NANOS_PER_DAY;
        let h = (b.open_ts.0 % atlas_core::units::NANOS_PER_DAY) / NANOS_PER_HOUR;
        let inside = h >= self.from_h && h < self.to_h;
        // Pencereden çıkınca (ya da yeni günün penceresi başlayınca) biriken değer tamamlanır.
        if let Some(k) = self.cur_key
            && (!inside || k != day)
        {
            self.last_done = self.cur.take().or(self.last_done);
            self.cur_key = None;
        }
        if inside {
            let x = if self.hi { b.high.0 } else { b.low.0 };
            self.cur_key = Some(day);
            self.cur = Some(match self.cur {
                None => x,
                Some(c) if self.hi => c.max(x),
                Some(c) => c.min(x),
            });
        }
        self.last_done.map(|v| v as f64)
    }
}

/// İşlem gününün ilk `m` dakikasının aralığı; `m` dakika dolunca kullanılabilir.
struct OpeningRange {
    minutes: u64,
    hi: bool,
    day: Option<NaiveDate>,
    start: u64,
    acc: Option<i64>,
    done: Option<i64>,
}
impl OpeningRange {
    fn new(m: usize, hi: bool) -> Self {
        OpeningRange {
            minutes: m.max(1) as u64,
            hi,
            day: None,
            start: 0,
            acc: None,
            done: None,
        }
    }
}
impl Calc for OpeningRange {
    fn update(&mut self, b: &Bar, day: NaiveDate) -> Option<f64> {
        if self.day != Some(day) {
            self.day = Some(day);
            self.start = b.open_ts.0;
            self.acc = None;
            self.done = None;
        }
        if self.done.is_none() {
            let x = if self.hi { b.high.0 } else { b.low.0 };
            self.acc = Some(match self.acc {
                None => x,
                Some(c) if self.hi => c.max(x),
                Some(c) => c.min(x),
            });
            if b.close_ts.0.saturating_sub(self.start) >= self.minutes * NANOS_PER_MIN {
                self.done = self.acc;
            }
        }
        self.done.map(|v| v as f64)
    }
}

#[derive(Clone, Copy)]
enum ProfileOut {
    Poc,
    Vah,
    Val,
}

/// Önceki işlem gününün hacim profili (kapanış fiyatına göre, ~60 kova). Değer alanı = %70.
struct Profile {
    out: ProfileOut,
    day: Option<NaiveDate>,
    bars: Vec<(i64, u64)>,
    hi: i64,
    lo: i64,
    value: Option<f64>,
}
impl Profile {
    fn new(out: ProfileOut) -> Self {
        Profile {
            out,
            day: None,
            bars: Vec::new(),
            hi: i64::MIN,
            lo: i64::MAX,
            value: None,
        }
    }
    fn finish(&mut self) {
        if self.bars.is_empty() || self.hi < self.lo {
            return;
        }
        let range = (self.hi - self.lo).max(1);
        let w = (range / 60).max(1);
        let nb = (range / w + 1) as usize;
        let mut vol = vec![0u64; nb];
        for (p, v) in &self.bars {
            let i = ((p - self.lo) / w) as usize;
            if let Some(x) = vol.get_mut(i.min(nb - 1)) {
                *x += v.max(&1);
            }
        }
        let poc = (0..nb)
            .max_by_key(|&i| (vol[i], std::cmp::Reverse(i)))
            .unwrap_or(0);
        let total: u64 = vol.iter().sum();
        let target = total * 7 / 10;
        let (mut a, mut z, mut acc) = (poc, poc, vol[poc]);
        while acc < target && (a > 0 || z + 1 < nb) {
            let up = if z + 1 < nb { vol[z + 1] } else { 0 };
            let dn = if a > 0 { vol[a - 1] } else { 0 };
            if (up >= dn && z + 1 < nb) || a == 0 {
                z += 1;
                acc += up;
            } else {
                a -= 1;
                acc += dn;
            }
        }
        let mid = |i: usize| (self.lo + i as i64 * w + w / 2) as f64;
        self.value = Some(match self.out {
            ProfileOut::Poc => mid(poc),
            ProfileOut::Vah => mid(z),
            ProfileOut::Val => mid(a),
        });
    }
}
impl Calc for Profile {
    fn update(&mut self, b: &Bar, day: NaiveDate) -> Option<f64> {
        if self.day != Some(day) {
            if self.day.is_some() {
                self.finish();
            }
            self.day = Some(day);
            self.bars.clear();
            self.hi = i64::MIN;
            self.lo = i64::MAX;
        }
        self.bars.push((b.close.0, b.volume));
        self.hi = self.hi.max(b.high.0);
        self.lo = self.lo.min(b.low.0);
        self.value
    }
}

/// Emilim: yüksek hacim + dar aralık. hacim z-skoru − aralık z-skoru (pencere `n`, mevcut bar dahil).
struct Absorption {
    n: usize,
    vol: VecDeque<f64>,
    rng: VecDeque<f64>,
}
impl Absorption {
    fn new(n: usize) -> Self {
        Absorption {
            n: n.max(3),
            vol: VecDeque::new(),
            rng: VecDeque::new(),
        }
    }
}
fn z_last(v: &VecDeque<f64>) -> f64 {
    let n = v.len() as f64;
    let m = v.iter().sum::<f64>() / n;
    let sd = (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / n).sqrt();
    let last = v.back().copied().unwrap_or(m);
    if sd > 0.0 { (last - m) / sd } else { 0.0 }
}
impl Calc for Absorption {
    fn update(&mut self, b: &Bar, _: NaiveDate) -> Option<f64> {
        self.vol.push_back(b.volume as f64);
        self.rng.push_back((b.high.0 - b.low.0) as f64);
        if self.vol.len() > self.n {
            self.vol.pop_front();
            self.rng.pop_front();
        }
        (self.vol.len() == self.n).then(|| z_last(&self.vol) - z_last(&self.rng))
    }
}

/// Likidite süpürmesi: bar önceki `n` barın dibini (tepesini) deler ama içeride kapanır → 1, değilse 0.
struct Sweep {
    ext: Extreme,
    hi: bool,
}
impl Sweep {
    fn new(n: usize, hi: bool) -> Self {
        Sweep {
            ext: Extreme::new(n, hi),
            hi,
        }
    }
}
impl Calc for Sweep {
    fn update(&mut self, b: &Bar, day: NaiveDate) -> Option<f64> {
        let lvl = self.ext.update(b, day)?;
        let hit = if self.hi {
            (b.high.0 as f64) > lvl && (b.close.0 as f64) < lvl
        } else {
            (b.low.0 as f64) < lvl && (b.close.0 as f64) > lvl
        };
        Some(if hit { 1.0 } else { 0.0 })
    }
}

/// CVD uyumsuzluğu: fiyat önceki `n` barın dibini (tepesini) yeniler ama CVD o dipteki (tepedeki)
/// değerinin üstünde (altında) kalır → 1.
struct CvdDiv {
    n: usize,
    bull: bool,
    cvd: Cvd,
    buf: VecDeque<(i64, f64)>,
}
impl CvdDiv {
    fn new(n: usize, bull: bool) -> Self {
        CvdDiv {
            n: n.max(2),
            bull,
            cvd: Cvd::default(),
            buf: VecDeque::new(),
        }
    }
}
impl Calc for CvdDiv {
    fn update(&mut self, b: &Bar, day: NaiveDate) -> Option<f64> {
        let c = self.cvd.update(b, day).unwrap_or(0.0);
        let out = (self.buf.len() == self.n).then(|| {
            let ext = if self.bull {
                self.buf.iter().min_by_key(|x| x.0)
            } else {
                self.buf.iter().max_by_key(|x| x.0)
            };
            match ext {
                Some(&(p, cv)) if self.bull && b.low.0 < p && c > cv => 1.0,
                Some(&(p, cv)) if !self.bull && b.high.0 > p && c < cv => 1.0,
                _ => 0.0,
            }
        });
        self.buf
            .push_back((if self.bull { b.low.0 } else { b.high.0 }, c));
        if self.buf.len() > self.n {
            self.buf.pop_front();
        }
        out
    }
}

/// Wilder RSI.
struct Rsi {
    n: usize,
    prev: Option<i64>,
    up: f64,
    dn: f64,
    seen: usize,
}
impl Rsi {
    fn new(n: usize) -> Self {
        Rsi {
            n: n.max(2),
            prev: None,
            up: 0.0,
            dn: 0.0,
            seen: 0,
        }
    }
}
impl Calc for Rsi {
    fn update(&mut self, b: &Bar, _: NaiveDate) -> Option<f64> {
        let p = self.prev.replace(b.close.0)?;
        let d = (b.close.0 - p) as f64;
        let (u, dn) = (d.max(0.0), (-d).max(0.0));
        self.seen += 1;
        let n = self.n as f64;
        if self.seen <= self.n {
            self.up += u / n;
            self.dn += dn / n;
            if self.seen < self.n {
                return None;
            }
        } else {
            self.up = (self.up * (n - 1.0) + u) / n;
            self.dn = (self.dn * (n - 1.0) + dn) / n;
        }
        Some(if self.dn == 0.0 {
            100.0
        } else {
            100.0 - 100.0 / (1.0 + self.up / self.dn)
        })
    }
}

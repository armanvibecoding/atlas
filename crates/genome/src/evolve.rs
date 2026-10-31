//! Evrimsel operatörler: rastgele genom, mutasyon (parametre / primitive takası), çaprazlama,
//! budama; parametre komşuluğu (gauntlet kapı 6). Tüm rastgelelik `ChaCha8` + açık seed (kural 2).

use crate::{
    Arith, Cond, Entry, Exit, FeatureRef, Filters, Genome, Operand, Sizing, format_ref, parse_ref,
};
use atlas_features::{FeatureSpec, Kind};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

pub fn rng(seed: u64) -> ChaCha8Rng {
    ChaCha8Rng::seed_from_u64(seed)
}

/// Fiyat ölçeğinde (birbiriyle karşılaştırılabilir) feature türleri.
const PRICE_KINDS: &[Kind] = &[
    Kind::Close,
    Kind::High,
    Kind::Low,
    Kind::Ema,
    Kind::Sma,
    Kind::Vwap,
    Kind::Pdh,
    Kind::Pdl,
    Kind::Highest,
    Kind::Lowest,
    Kind::OrHigh,
    Kind::OrLow,
    Kind::Poc,
    Kind::Vah,
    Kind::Val,
    Kind::AsiaHigh,
    Kind::AsiaLow,
    Kind::LondonHigh,
    Kind::LondonLow,
];

/// Kendi ölçeğinde salınan feature'lar ve anlamlı eşik aralıkları.
const OSC: &[(Kind, f64, f64)] = &[
    (Kind::Rsi, 15.0, 85.0),
    (Kind::Imbalance, -0.3, 0.3),
    (Kind::Absorption, -2.0, 4.0),
    (Kind::SweepLow, 0.5, 0.5),
    (Kind::SweepHigh, 0.5, 0.5),
    (Kind::CvdDivBull, 0.5, 0.5),
    (Kind::CvdDivBear, 0.5, 0.5),
];

fn rand_param(r: &mut ChaCha8Rng, k: Kind) -> u32 {
    let (lo, hi, _) = k.range();
    if !k.has_param() {
        return 0;
    }
    // Log-uniform: kısa ve uzun periyotlar eşit şans.
    let hi = hi.min(400).max(lo + 1);
    let x = (lo as f64).ln() + r.random::<f64>() * ((hi as f64).ln() - (lo as f64).ln());
    (x.exp().round() as u32).clamp(lo, hi)
}

fn rand_ref(r: &mut ChaCha8Rng, kinds: &[Kind]) -> String {
    let k = kinds[r.random_range(0..kinds.len())];
    let lag = if r.random_bool(0.15) {
        r.random_range(1..=3)
    } else {
        0
    };
    format_ref(&FeatureRef {
        spec: FeatureSpec::new(k, rand_param(r, k)),
        lag,
    })
}

fn rand_leaf(r: &mut ChaCha8Rng) -> Cond {
    if r.random_bool(0.7) {
        let a = Operand::Ref(rand_ref(r, PRICE_KINDS));
        let b = Operand::Ref(rand_ref(r, PRICE_KINDS));
        let p = Box::new((a, b));
        match r.random_range(0..4) {
            0 => Cond::Gt(p),
            1 => Cond::Lt(p),
            2 => Cond::CrossAbove(p),
            _ => Cond::CrossBelow(p),
        }
    } else {
        let (k, lo, hi) = OSC[r.random_range(0..OSC.len())];
        let a = Operand::Ref(format_ref(&FeatureRef {
            spec: FeatureSpec::new(k, rand_param(r, k)),
            lag: 0,
        }));
        let t = if hi > lo {
            lo + r.random::<f64>() * (hi - lo)
        } else {
            lo
        };
        let b = Operand::Num((t * 100.0).round() / 100.0);
        if r.random_bool(0.5) {
            Cond::Gt(Box::new((a, b)))
        } else {
            Cond::Lt(Box::new((a, b)))
        }
    }
}

fn rand_cond(r: &mut ChaCha8Rng) -> Cond {
    let n = r.random_range(1..=3);
    if n == 1 {
        rand_leaf(r)
    } else {
        Cond::All((0..n).map(|_| rand_leaf(r)).collect())
    }
}

/// Rastgele (geleceğe bakmayan) genom. Sahte strateji testi ve evrim başlangıç popülasyonu.
pub fn random_genome(r: &mut ChaCha8Rng, instrument: &str, name: &str, risk_usd: u32) -> Genome {
    let dir = r.random_range(0..3);
    let entry = Entry {
        long: (dir != 1).then(|| rand_cond(r)),
        short: (dir != 0).then(|| rand_cond(r)),
    };
    Genome {
        name: name.to_string(),
        instrument: instrument.to_string(),
        tags: vec!["random".into()],
        entry,
        filters: Filters {
            hours_utc: r.random_bool(0.3).then(|| {
                let a = r.random_range(0..24);
                (a, (a + r.random_range(2..12)) % 24)
            }),
            ..Filters::default()
        },
        exit: Exit {
            atr: rand_param(r, Kind::Atr).clamp(2, 200),
            stop_atr: ((0.5 + r.random::<f64>() * 3.5) * 100.0).round() / 100.0,
            target_r: r
                .random_bool(0.8)
                .then(|| ((0.5 + r.random::<f64>() * 3.5) * 100.0).round() / 100.0),
            time_bars: r.random_bool(0.7).then(|| r.random_range(5..1500)),
        },
        sizing: Sizing { risk_usd },
    }
}

/// Mutasyona açık bir sayısal parametre.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub path: String,
    pub value: f64,
    pub lo: f64,
    pub hi: f64,
    pub integer: bool,
}

/// Parametrelerin derinlik-öncelikli sırası; `set_param` aynı sırayla yazar.
fn walk_op(o: &mut Operand, path: &str, f: &mut dyn FnMut(&str, &mut ParamSlot<'_>)) {
    match o {
        Operand::Int(i) => {
            let mut v = *i as f64;
            f(path, &mut ParamSlot::Const(&mut v));
            *i = v.round() as i64;
        }
        Operand::Num(x) => f(path, &mut ParamSlot::Const(x)),
        Operand::Ref(s) => {
            if let Ok(mut r) = parse_ref(s)
                && r.spec.kind.has_param()
            {
                let mut p = r.spec.param;
                f(path, &mut ParamSlot::Feature(r.spec.kind, &mut p));
                r.spec = FeatureSpec::new(r.spec.kind, p);
                *s = format_ref(&r);
            }
        }
        Operand::Op(Arith::Add(p) | Arith::Sub(p) | Arith::Mul(p)) => {
            walk_op(&mut p.0, &format!("{path}.a"), f);
            walk_op(&mut p.1, &format!("{path}.b"), f);
        }
    }
}

fn walk_cond(c: &mut Cond, path: &str, f: &mut dyn FnMut(&str, &mut ParamSlot<'_>)) {
    match c {
        Cond::All(v) | Cond::Any(v) => {
            for (i, x) in v.iter_mut().enumerate() {
                walk_cond(x, &format!("{path}[{i}]"), f);
            }
        }
        Cond::Not(x) => walk_cond(x, &format!("{path}.not"), f),
        Cond::Gt(p) | Cond::Lt(p) | Cond::CrossAbove(p) | Cond::CrossBelow(p) => {
            walk_op(&mut p.0, &format!("{path}.l"), f);
            walk_op(&mut p.1, &format!("{path}.r"), f);
        }
    }
}

pub enum ParamSlot<'a> {
    Const(&'a mut f64),
    Feature(Kind, &'a mut u32),
    Float(&'a mut f64, f64, f64),
    UInt(&'a mut u32, u32, u32),
}

fn walk(g: &mut Genome, f: &mut dyn FnMut(&str, &mut ParamSlot<'_>)) {
    if let Some(c) = &mut g.entry.long {
        walk_cond(c, "long", f);
    }
    if let Some(c) = &mut g.entry.short {
        walk_cond(c, "short", f);
    }
    f("exit.atr", &mut ParamSlot::UInt(&mut g.exit.atr, 2, 500));
    f(
        "exit.stop_atr",
        &mut ParamSlot::Float(&mut g.exit.stop_atr, 0.05, 50.0),
    );
    if let Some(t) = &mut g.exit.target_r {
        f("exit.target_r", &mut ParamSlot::Float(t, 0.1, 50.0));
    }
    if let Some(t) = &mut g.exit.time_bars {
        f("exit.time_bars", &mut ParamSlot::UInt(t, 1, 100_000));
    }
}

pub fn params(g: &Genome) -> Vec<Param> {
    let mut out = Vec::new();
    let mut c = g.clone();
    walk(&mut c, &mut |path, slot| {
        let p = match slot {
            ParamSlot::Const(x) => Param {
                path: path.into(),
                value: **x,
                lo: f64::MIN,
                hi: f64::MAX,
                integer: false,
            },
            ParamSlot::Feature(k, v) => {
                let (lo, hi, _) = k.range();
                Param {
                    path: path.into(),
                    value: f64::from(**v),
                    lo: f64::from(lo),
                    hi: f64::from(hi),
                    integer: true,
                }
            }
            ParamSlot::Float(x, lo, hi) => Param {
                path: path.into(),
                value: **x,
                lo: *lo,
                hi: *hi,
                integer: false,
            },
            ParamSlot::UInt(v, lo, hi) => Param {
                path: path.into(),
                value: f64::from(**v),
                lo: f64::from(*lo),
                hi: f64::from(*hi),
                integer: true,
            },
        };
        out.push(p);
    });
    out
}

/// `idx`. parametreyi `value` yapar (aralığa kırpılır).
pub fn set_param(g: &Genome, idx: usize, value: f64) -> Genome {
    let mut c = g.clone();
    let mut k = 0usize;
    walk(&mut c, &mut |_, slot| {
        if k == idx {
            match slot {
                ParamSlot::Const(x) => **x = value,
                ParamSlot::Feature(kind, v) => {
                    let (lo, hi, _) = kind.range();
                    **v = (value.round().max(0.0) as u32).clamp(lo, hi);
                }
                ParamSlot::Float(x, lo, hi) => **x = value.clamp(*lo, *hi),
                ParamSlot::UInt(v, lo, hi) => **v = (value.round().max(0.0) as u32).clamp(*lo, *hi),
            }
        }
        k += 1;
    });
    c
}

/// Her parametre için ±`frac` komşuları (tamsayılarda en az ±1). Sabit 0 olan sabitler ±frac mutlak.
pub fn neighbors(g: &Genome, frac: f64) -> Vec<(String, Genome)> {
    let mut out = Vec::new();
    for (i, p) in params(g).iter().enumerate() {
        for sgn in [-1.0, 1.0] {
            let mut v = if p.value == 0.0 {
                sgn * frac
            } else {
                p.value * (1.0 + sgn * frac)
            };
            if p.integer {
                let r = v.round();
                v = if (r - p.value).abs() < 1.0 {
                    p.value + sgn
                } else {
                    r
                };
            }
            v = v.clamp(p.lo, p.hi);
            if (v - p.value).abs() > f64::EPSILON {
                out.push((
                    format!(
                        "{}{}{:.0}%",
                        p.path,
                        if sgn > 0.0 { "+" } else { "-" },
                        frac * 100.0
                    ),
                    set_param(g, i, v),
                ));
            }
        }
    }
    out
}

/// Parametre mutasyonu (×U(0.7, 1.3)) ya da primitive takası (operatör/feature türü).
/// Yuvarlama yüzünden aynı genom çıkarsa yeniden dener (en fazla 32 kez).
pub fn mutate(g: &Genome, r: &mut ChaCha8Rng) -> Genome {
    for _ in 0..32 {
        let m = mutate_once(g, r);
        if m != *g {
            return m;
        }
    }
    mutate_once(g, r)
}

fn mutate_once(g: &Genome, r: &mut ChaCha8Rng) -> Genome {
    let ps = params(g);
    if !ps.is_empty() && r.random_bool(0.7) {
        let i = r.random_range(0..ps.len());
        let p = &ps[i];
        let v = if p.value == 0.0 {
            r.random::<f64>() * 0.2 - 0.1
        } else {
            p.value * (0.7 + 0.6 * r.random::<f64>())
        };
        return set_param(g, i, v);
    }
    let mut c = g.clone();
    let side = if c.entry.long.is_some() && (c.entry.short.is_none() || r.random_bool(0.5)) {
        &mut c.entry.long
    } else {
        &mut c.entry.short
    };
    if let Some(cond) = side {
        swap_primitive(cond, r);
    }
    c
}

fn swap_primitive(c: &mut Cond, r: &mut ChaCha8Rng) {
    match c {
        Cond::All(v) | Cond::Any(v) => {
            let n = v.len();
            if n > 0 {
                let i = r.random_range(0..n);
                swap_primitive(&mut v[i], r);
            }
        }
        Cond::Not(x) => swap_primitive(x, r),
        _ => *c = rand_leaf(r),
    }
}

/// Çaprazlama: `a`'nın gövdesi, `b`'nin bir tarafı (long/short) ya da çıkışı.
pub fn crossover(a: &Genome, b: &Genome, r: &mut ChaCha8Rng) -> Genome {
    let mut c = a.clone();
    match r.random_range(0..3) {
        0 if b.entry.long.is_some() => c.entry.long = b.entry.long.clone(),
        1 if b.entry.short.is_some() => c.entry.short = b.entry.short.clone(),
        _ => c.exit = b.exit.clone(),
    }
    c
}

/// Budama: çok çocuklu bir all/any'den bir koşul çıkarır.
pub fn prune(g: &Genome, r: &mut ChaCha8Rng) -> Genome {
    let mut c = g.clone();
    let mut done = false;
    for side in [&mut c.entry.long, &mut c.entry.short] {
        if done {
            break;
        }
        if let Some(Cond::All(v) | Cond::Any(v)) = side
            && v.len() > 1
        {
            let i = r.random_range(0..v.len());
            v.remove(i);
            done = true;
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_genomes_are_valid_and_deterministic() {
        let mut r1 = rng(7);
        let mut r2 = rng(7);
        for i in 0..200 {
            let a = random_genome(&mut r1, "BTCUSDT", &format!("g{i}"), 1000);
            let b = random_genome(&mut r2, "BTCUSDT", &format!("g{i}"), 1000);
            assert_eq!(a, b);
            a.validate().unwrap();
            Genome::from_toml_str(&a.to_toml()).unwrap();
        }
    }

    #[test]
    fn params_and_neighbors() {
        let g = Genome::from_toml_str(crate::tests::TREND).unwrap();
        let ps = params(&g);
        let paths: Vec<&str> = ps.iter().map(|p| p.path.as_str()).collect();
        assert!(paths.contains(&"exit.stop_atr"));
        assert!(paths.contains(&"long[0].l"));
        let n = neighbors(&g, 0.2);
        assert!(n.len() >= 2 * (ps.len() - 1));
        let ema200_up = n.iter().find(|(p, _)| p == "long[0].r+20%").unwrap();
        let txt = format!("{:?}", ema200_up.1.entry.long);
        assert!(txt.contains("ema:240"), "{txt}");
        for (_, h) in &n {
            h.validate().unwrap();
        }
    }

    #[test]
    fn mutate_crossover_prune_valid() {
        let g = Genome::from_toml_str(crate::tests::TREND).unwrap();
        let mut r = rng(1);
        let other = random_genome(&mut r, "BTCUSDT", "o", 1000);
        for _ in 0..300 {
            mutate(&g, &mut r).validate().unwrap();
            crossover(&g, &other, &mut r).validate().unwrap();
            prune(&g, &mut r).validate().unwrap();
        }
        assert_ne!(mutate(&g, &mut r), g);
    }
}

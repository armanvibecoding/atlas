//! Salt-okunur `atlas` komutlarının beyaz listesi ve argüman doğrulaması.
//!
//! Dashboard'un CLI'ya iletebileceği her şey bu dosyadadır. `READ_COMMANDS` dışındaki hiçbir
//! komut çalıştırılmaz; `EXCLUDED` ise bilinçli olarak dışarıda bırakılan komutları gerekçesiyle
//! kayda geçirir (kapsam testi her CLI komutunun ikisinden birinde olmasını zorunlu kılar).

use std::fmt;

/// Komutun sonuna eklenen konumsal parametre.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Param {
    /// Parametre yok.
    None,
    /// Tek bir ad/kimlik (`[A-Za-z0-9._:-]`, bkz. [`validate_ident`]); API yolunun son segmenti.
    Ident,
}

/// Komutun kabul ettiği sorgu parametreleri.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuerySpec {
    /// Sorgu parametresi kabul edilmez (verilirse 400).
    None,
    /// `events`: `since`, `prio`, `kind`, `limit`.
    Events,
}

/// Beyaz listedeki tek bir salt-okunur komut.
#[derive(Debug, Clone, Copy)]
pub struct ReadCommand {
    /// `/api/` sonrasındaki yol (`/` ile ayrılmış). `Param::Ident` ise parametre ek bir segmenttir.
    pub route: &'static str,
    /// `atlas` alt komut kelimeleri (`--json` hariç).
    pub argv: &'static [&'static str],
    pub param: Param,
    pub query: QuerySpec,
    /// CLI'ya geldiği (ya da geleceği) kilometre taşı.
    pub milestone: &'static str,
    /// Panelde gösterilen Türkçe açıklama.
    pub title: &'static str,
}

/// Dashboard'un çağırabileceği komutların TEK listesi. CLI'ya yeni bir okuma komutu eklendiğinde
/// buraya bir satır eklenir; panelin "Tüm komutlar" bölümünde otomatik görünür.
///
/// M3 komutları (`report`, `budget`, `approvals`) şimdiden listededir; CLI henüz tanımıyorsa API
/// 501 + `unavailable: true` döner, panel "henüz yok" gösterir.
pub const READ_COMMANDS: &[ReadCommand] = &[
    ReadCommand {
        route: "status",
        argv: &["status"],
        param: Param::None,
        query: QuerySpec::None,
        milestone: "M1",
        title: "Durum özeti",
    },
    ReadCommand {
        route: "health",
        argv: &["health"],
        param: Param::None,
        query: QuerySpec::None,
        milestone: "M1",
        title: "Sağlık kontrolleri",
    },
    ReadCommand {
        route: "events",
        argv: &["events"],
        param: Param::None,
        query: QuerySpec::Events,
        milestone: "M1",
        title: "Olay kuyruğu",
    },
    ReadCommand {
        route: "journal/replay",
        argv: &["journal", "replay"],
        param: Param::None,
        query: QuerySpec::None,
        milestone: "M1",
        title: "Journal tutarlılığı (replay)",
    },
    ReadCommand {
        route: "data/list",
        argv: &["data", "list"],
        param: Param::None,
        query: QuerySpec::None,
        milestone: "M1",
        title: "Veri setleri",
    },
    ReadCommand {
        route: "strategies/list",
        argv: &["strategies", "list"],
        param: Param::None,
        query: QuerySpec::None,
        milestone: "M2",
        title: "Stratejiler",
    },
    ReadCommand {
        route: "strategies/show",
        argv: &["strategies", "show"],
        param: Param::Ident,
        query: QuerySpec::None,
        milestone: "M2",
        title: "Strateji ayrıntısı + son gauntlet raporu",
    },
    ReadCommand {
        route: "experiments/summary",
        argv: &["experiments", "summary"],
        param: Param::None,
        query: QuerySpec::None,
        milestone: "M2",
        title: "Deney sicili özeti",
    },
    ReadCommand {
        route: "report/daily",
        argv: &["report", "daily"],
        param: Param::None,
        query: QuerySpec::None,
        milestone: "M3",
        title: "Günlük rapor",
    },
    ReadCommand {
        route: "report/weekly",
        argv: &["report", "weekly"],
        param: Param::None,
        query: QuerySpec::None,
        milestone: "M3",
        title: "Haftalık rapor",
    },
    ReadCommand {
        route: "budget",
        argv: &["budget"],
        param: Param::None,
        query: QuerySpec::None,
        milestone: "M3",
        title: "Bütçe durumu",
    },
    ReadCommand {
        route: "approvals/list",
        argv: &["approvals", "list"],
        param: Param::None,
        query: QuerySpec::None,
        milestone: "M3",
        title: "Bekleyen onaylar",
    },
    ReadCommand {
        route: "approvals/show",
        argv: &["approvals", "show"],
        param: Param::Ident,
        query: QuerySpec::None,
        milestone: "M3",
        title: "Onay paketi ayrıntısı",
    },
];

/// Bilinçli olarak dashboard'a ALINMAYAN komutlar (kelime öneki → gerekçe).
/// Bir CLI komutu bu öneklerden biriyle başlıyorsa kapsam testi onu "dışarıda" sayar.
pub const EXCLUDED: &[(&str, &str)] = &[
    ("kill", "yazma: kill switch tetikler"),
    ("kill-reset", "yazma, yalnız Arman"),
    ("webhook-test", "dış teslim (webhook POST), yalnız Arman"),
    ("note", "yazma: journal'a operatör notu"),
    (
        "strategy",
        "yazma: pause/demote/resume merdiveni değiştirir",
    ),
    ("approve", "yazma, yalnız Arman"),
    ("risk", "yazma, yalnız Arman"),
    ("deploy", "yazma, yalnız Arman"),
    (
        "gauntlet",
        "yazma: sicile deneme yazar, holdout bütçesi tüketir",
    ),
    ("journal export", "yazma: var/exports altına dosya yazar"),
    ("journal checkpoint", "yazma, yalnız Arman"),
    (
        "data pull-binance",
        "yazma + ağ: veri çeker, journal'a yazar",
    ),
    (
        "data quote-databento",
        "ağ: ücretli API'ye maliyet sorgusu, yalnız Arman",
    ),
    (
        "data measure-slippage",
        "yazma + ağ: journal'a kalibrasyon olayı yazar",
    ),
    (
        "verify",
        "yazma + ağır: journal'a sonuç olayı yazar, dakikalar sürer",
    ),
    (
        "daemon run",
        "yazma + süreç: sürekli çalışan daemon, yalnız Arman/systemd",
    ),
    ("help", "clap yardım metni, veri değil"),
];

/// API isteği reddi.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// Beyaz listede olmayan yol.
    NotFound,
    /// Beyaz listede ama argümanı geçersiz.
    BadRequest(String),
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiError::NotFound => write!(f, "bilinmeyen ya da izin verilmeyen komut"),
            ApiError::BadRequest(m) => write!(f, "geçersiz istek: {m}"),
        }
    }
}

/// Ad/kimlik için azami uzunluk.
pub const MAX_IDENT_LEN: usize = 64;
/// `events --limit` üst sınırı.
pub const MAX_EVENTS_LIMIT: u32 = 1000;

/// Ad/kimlik: 1..=64 karakter, yalnız `[A-Za-z0-9._:-]`; `-` ya da `.` ile başlayamaz
/// (bayrak ya da gizli/üst dizin sanılmasın), `..` içeremez.
pub fn validate_ident(s: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err("ad boş".into());
    }
    if s.chars().count() > MAX_IDENT_LEN {
        return Err(format!("ad en fazla {MAX_IDENT_LEN} karakter"));
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    {
        // '.' ve ':' yok: CLI'da ad asla dosya yolu olarak yorumlanmasın (yol geçişi).
        return Err("ad yalnız [A-Za-z0-9_-] içerebilir".into());
    }
    if s.starts_with('-') || s.starts_with('.') {
        return Err("ad '-' ya da '.' ile başlayamaz".into());
    }
    if s.contains("..") {
        return Err("ad '..' içeremez".into());
    }
    Ok(())
}

/// `since`: `seq:N` ya da RFC3339 zaman (`2026-10-31T12:00:00Z`).
pub fn validate_since(s: &str) -> Result<(), String> {
    if let Some(n) = s.strip_prefix("seq:") {
        if !n.is_empty() && n.len() <= 18 && n.chars().all(|c| c.is_ascii_digit()) {
            return Ok(());
        }
        return Err("since=seq:N (N rakam, en fazla 18 hane)".into());
    }
    let ok = s.len() <= 40
        && s.chars().next().is_some_and(|c| c.is_ascii_digit())
        && s.chars().all(|c| {
            c.is_ascii_digit() || matches!(c, '-' | ':' | '.' | '+' | 'T' | 'Z' | 't' | 'z')
        });
    if ok {
        Ok(())
    } else {
        Err("since RFC3339 zaman ya da seq:N olmalı".into())
    }
}

/// `prio`: `high` | `medium` | `info`.
pub fn validate_prio(s: &str) -> Result<(), String> {
    match s {
        "high" | "medium" | "info" => Ok(()),
        _ => Err("prio yalnız high | medium | info".into()),
    }
}

/// `kind`: olay türü adı, `[a-z_]`, en fazla 40 karakter.
pub fn validate_kind(s: &str) -> Result<(), String> {
    if !s.is_empty() && s.len() <= 40 && s.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
        Ok(())
    } else {
        Err("kind yalnız [a-z_], en fazla 40 karakter".into())
    }
}

/// `limit`: 1..=1000 tamsayı.
pub fn validate_limit(s: &str) -> Result<u32, String> {
    match s.parse::<u32>() {
        Ok(n) if (1..=MAX_EVENTS_LIMIT).contains(&n) => Ok(n),
        _ => Err(format!("limit 1..={MAX_EVENTS_LIMIT} tamsayı olmalı")),
    }
}

fn events_args(query: &[(String, String)]) -> Result<Vec<String>, ApiError> {
    let mut seen: Vec<&str> = Vec::new();
    let mut out = Vec::new();
    for (k, v) in query {
        if seen.contains(&k.as_str()) {
            return Err(ApiError::BadRequest(format!("'{k}' birden fazla verilmiş")));
        }
        seen.push(k.as_str());
        // Boş değer = filtre yok (panelin "tümü" seçeneği).
        if v.is_empty() {
            if matches!(k.as_str(), "since" | "prio" | "kind" | "limit") {
                continue;
            }
            return Err(ApiError::BadRequest(format!("bilinmeyen parametre '{k}'")));
        }
        let arg = match k.as_str() {
            "since" => validate_since(v).map(|()| format!("--since={v}")),
            "prio" => validate_prio(v).map(|()| format!("--prio={v}")),
            "kind" => validate_kind(v).map(|()| format!("--kind={v}")),
            "limit" => validate_limit(v).map(|n| format!("--limit={n}")),
            _ => return Err(ApiError::BadRequest(format!("bilinmeyen parametre '{k}'"))),
        };
        out.push(arg.map_err(ApiError::BadRequest)?);
    }
    Ok(out)
}

/// API yolunu (`/api/` sonrası) ve sorgu parametrelerini doğrular, CLI argüman listesini üretir.
///
/// Dönen liste `--json` ile başlar ve doğrudan `std::process::Command::args`'a verilir (kabuk yok).
pub fn resolve(
    path: &str,
    query: &[(String, String)],
) -> Result<(&'static ReadCommand, Vec<String>), ApiError> {
    let segs: Vec<&str> = path.trim_matches('/').split('/').collect();
    if segs.iter().any(|s| s.is_empty()) {
        return Err(ApiError::NotFound);
    }
    for cmd in READ_COMMANDS {
        let route: Vec<&str> = cmd.route.split('/').collect();
        let param = match cmd.param {
            Param::None if segs == route => None,
            Param::Ident if segs.len() == route.len() + 1 && segs[..route.len()] == route[..] => {
                let p = segs[route.len()];
                validate_ident(p).map_err(ApiError::BadRequest)?;
                Some(p)
            }
            Param::Ident if segs == route => {
                return Err(ApiError::BadRequest(format!(
                    "parametre eksik: /api/{}/<ad>",
                    cmd.route
                )));
            }
            _ => continue,
        };
        let mut argv: Vec<String> = vec!["--json".into()];
        argv.extend(cmd.argv.iter().map(|s| (*s).to_string()));
        argv.extend(param.map(str::to_string));
        match cmd.query {
            QuerySpec::None if !query.is_empty() => {
                return Err(ApiError::BadRequest(format!(
                    "/api/{} sorgu parametresi almaz",
                    cmd.route
                )));
            }
            QuerySpec::None => {}
            QuerySpec::Events => argv.extend(events_args(query)?),
        }
        return Ok((cmd, argv));
    }
    Err(ApiError::NotFound)
}

/// Bir komut (kelime listesi) beyaz listede mi, yoksa bilinçli dışarıda mı?
pub fn classify(words: &[&str]) -> Coverage {
    if READ_COMMANDS.iter().any(|c| c.argv == words) {
        return Coverage::Read;
    }
    for (prefix, why) in EXCLUDED {
        let p: Vec<&str> = prefix.split(' ').collect();
        if words.len() >= p.len() && words[..p.len()] == p[..] {
            return Coverage::Excluded(why);
        }
    }
    Coverage::Missing
}

/// [`classify`] sonucu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage {
    Read,
    Excluded(&'static str),
    Missing,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn ident_kabul() {
        for ok in ["trend_pullback", "a", "X-1", "42", "A9_b-c"] {
            assert!(validate_ident(ok).is_ok(), "{ok}");
        }
        // '.' ve ':' yok: CLI'da ad dosya yolu olarak yorumlanamaz.
        for bad in ["Cargo.toml", "a:b", "x.toml", "../x"] {
            assert!(validate_ident(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn ident_red_kotu_ad_ve_kabuk_metakarakteri() {
        let bad = [
            "", "-rf", "--home", ".gizli", "a..b", "../etc", "a/b", "a\\b", "a b", "a;rm", "a|b",
            "a&b", "$(x)", "`x`", "a>b", "a<b", "a*", "a'b", "a\"b", "a\nb", "a\0b", "ş", "a%20b",
        ];
        for b in bad {
            assert!(validate_ident(b).is_err(), "{b:?} reddedilmeliydi");
        }
    }

    #[test]
    fn ident_red_uzun_girdi() {
        assert!(validate_ident(&"a".repeat(MAX_IDENT_LEN)).is_ok());
        assert!(validate_ident(&"a".repeat(MAX_IDENT_LEN + 1)).is_err());
        assert!(validate_ident(&"a".repeat(10_000)).is_err());
    }

    #[test]
    fn since_prio_kind_limit_dogrulama() {
        assert!(validate_since("seq:42").is_ok());
        assert!(validate_since("2026-10-31T12:00:00Z").is_ok());
        assert!(validate_since("2026-10-31T12:00:00.5+03:00").is_ok());
        for b in [
            "seq:",
            "seq:-1",
            "seq:1a",
            "dün",
            "-1",
            "2026;ls",
            &"1".repeat(41),
        ] {
            assert!(validate_since(b).is_err(), "{b}");
        }
        assert!(validate_prio("high").is_ok());
        assert!(validate_prio("HIGH").is_err());
        assert!(validate_prio("critical").is_err());
        assert!(validate_kind("kill_switch").is_ok());
        assert!(validate_kind("Kill").is_err());
        assert!(validate_kind("a-b").is_err());
        assert_eq!(validate_limit("50"), Ok(50));
        for b in ["0", "1001", "-1", "abc", "99999999999"] {
            assert!(validate_limit(b).is_err(), "{b}");
        }
    }

    #[test]
    fn beyaz_liste_disi_komut_404() {
        for p in [
            "kill",
            "kill/global",
            "kill-reset",
            "note",
            "approve",
            "gauntlet/run",
            "journal/export",
            "journal/checkpoint",
            "data/pull-binance",
            "data/quote-databento",
            "verify/m1",
            "strategy/pause/x",
            "status/extra",
            "",
            "/",
            "strategies",
            "strategies/list/x",
            "strategies//show",
        ] {
            assert_eq!(resolve(p, &[]).err(), Some(ApiError::NotFound), "{p}");
        }
    }

    #[test]
    fn basit_komut_eslemesi() {
        let (c, a) = resolve("status", &[]).unwrap();
        assert_eq!(c.route, "status");
        assert_eq!(a, ["--json", "status"]);
        let (_, a) = resolve("/journal/replay/", &[]).unwrap();
        assert_eq!(a, ["--json", "journal", "replay"]);
        let (_, a) = resolve("strategies/show/trend_pullback", &[]).unwrap();
        assert_eq!(a, ["--json", "strategies", "show", "trend_pullback"]);
        let (_, a) = resolve("approvals/show/1", &[]).unwrap();
        assert_eq!(a, ["--json", "approvals", "show", "1"]);
    }

    #[test]
    fn parametre_dogrulamasi() {
        assert!(matches!(
            resolve("strategies/show/-x", &[]),
            Err(ApiError::BadRequest(_))
        ));
        assert!(matches!(
            resolve("strategies/show/a;b", &[]),
            Err(ApiError::BadRequest(_))
        ));
        assert!(matches!(
            resolve("strategies/show", &[]),
            Err(ApiError::BadRequest(_))
        ));
        let long = format!("strategies/show/{}", "a".repeat(65));
        assert!(matches!(resolve(&long, &[]), Err(ApiError::BadRequest(_))));
    }

    #[test]
    fn sorgu_cli_argumani_eslemesi() {
        let (_, a) = resolve(
            "events",
            &q(&[
                ("since", "seq:10"),
                ("prio", "high"),
                ("kind", "kill_switch"),
                ("limit", "100"),
            ]),
        )
        .unwrap();
        assert_eq!(
            a,
            [
                "--json",
                "events",
                "--since=seq:10",
                "--prio=high",
                "--kind=kill_switch",
                "--limit=100"
            ]
        );
        // Boş değer = filtre yok.
        let (_, a) = resolve("events", &q(&[("prio", ""), ("kind", "")])).unwrap();
        assert_eq!(a, ["--json", "events"]);
    }

    #[test]
    fn sorgu_reddi() {
        let bad: &[&[(&str, &str)]] = &[
            &[("prio", "critical")],
            &[("limit", "0")],
            &[("limit", "5000")],
            &[("since", "--home")],
            &[("kind", "a;b")],
            &[("home", "/etc")],
            &[("json", "1")],
            &[("prio", "high"), ("prio", "info")],
        ];
        for b in bad {
            assert!(
                matches!(resolve("events", &q(b)), Err(ApiError::BadRequest(_))),
                "{b:?}"
            );
        }
        // Sorgu almayan komuta sorgu verilirse 400.
        assert!(matches!(
            resolve("status", &q(&[("x", "1")])),
            Err(ApiError::BadRequest(_))
        ));
    }

    #[test]
    fn beyaz_listede_yazma_komutu_yok() {
        for c in READ_COMMANDS {
            for (prefix, _) in EXCLUDED {
                let p: Vec<&str> = prefix.split(' ').collect();
                assert!(
                    !(c.argv.len() >= p.len() && c.argv[..p.len()] == p[..]),
                    "{} dışlanmış önek '{prefix}' ile başlıyor",
                    c.route
                );
            }
            assert_eq!(
                c.route.split('/').collect::<Vec<_>>(),
                c.argv,
                "{}",
                c.route
            );
        }
    }

    #[test]
    fn rotalar_benzersiz() {
        let mut r: Vec<&str> = READ_COMMANDS.iter().map(|c| c.route).collect();
        r.sort_unstable();
        let n = r.len();
        r.dedup();
        assert_eq!(n, r.len());
    }
}

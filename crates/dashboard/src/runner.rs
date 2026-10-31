//! `atlas` CLI'yı alt süreç olarak çalıştırma.
//!
//! Kabuk kullanılmaz: argümanlar `std::process::Command`'a liste olarak verilir. Alt süreç her zaman
//! `ATLAS_ROLE=grok` ile çalışır; böylece CLI'nın kendi izin kontrolü ikinci savunma katmanıdır.

use serde_json::Value;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

/// CLI'nın JSON zarf şeması.
pub const CLI_SCHEMA: &str = "atlas.cli/1";

/// Alt süreç ayarları.
#[derive(Debug, Clone)]
pub struct CliConfig {
    /// `atlas` yürütülebilir dosyası.
    pub atlas_bin: PathBuf,
    /// Alt sürece `ATLAS_HOME` olarak iletilir ve çalışma dizini yapılır. `None` → miras.
    pub atlas_home: Option<PathBuf>,
    /// Komut başına zaman aşımı.
    pub timeout: Duration,
    /// stdout üst sınırı (bayt); aşılırsa hata.
    pub max_output_bytes: usize,
}

impl CliConfig {
    pub fn new(atlas_bin: PathBuf, atlas_home: Option<PathBuf>) -> CliConfig {
        CliConfig {
            atlas_bin,
            atlas_home,
            timeout: Duration::from_secs(30),
            max_output_bytes: 32 * 1024 * 1024,
        }
    }

    /// Windows'ta `.bat`/`.cmd` dosyaları `cmd.exe` üzerinden yorumlanır (argüman enjeksiyonu riski);
    /// bu yüzden yalnız doğrudan yürütülebilir dosya kabul edilir.
    pub fn check_bin(&self) -> Result<(), String> {
        let ext = self
            .atlas_bin
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase());
        match ext.as_deref() {
            Some("bat" | "cmd" | "ps1" | "sh") => Err(format!(
                "{}: betik kabul edilmez, doğrudan atlas yürütülebilir dosyası verin",
                self.atlas_bin.display()
            )),
            _ => Ok(()),
        }
    }

    /// Çalıştırılacak süreci kurar (test edilebilirlik için ayrı).
    pub fn command(&self, args: &[String]) -> std::process::Command {
        let mut c = std::process::Command::new(&self.atlas_bin);
        c.args(args)
            .env("ATLAS_ROLE", "grok")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(h) = &self.atlas_home {
            c.env("ATLAS_HOME", h).current_dir(h);
        }
        c
    }
}

/// CLI çağrısının sonucu.
#[derive(Debug, Clone, PartialEq)]
pub enum CliOutcome {
    /// `atlas.cli/1` zarfı (çıkış 0 ya da 1 — "sağlıksız", "tutarsız" gibi olumsuz sonuç da veridir).
    Envelope(Value),
    /// CLI'nın hata zarfı (çıkış 2): izin yok, config okunamadı…
    CliError(Value),
    /// Bu CLI sürümünde komut yok (ör. M3 komutları henüz gelmedi).
    Unavailable(String),
    /// Çalıştırılamadı, zaman aşımı ya da çözümlenemeyen çıktı.
    Failed(String),
}

/// Terminal/HTML'e taşınacak serbest metni kontrol karakterlerinden arındırır ve kısaltır.
pub fn sanitize(s: &str, max: usize) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Ham süreç çıktısını yorumlar (saf fonksiyon; birim testli).
pub fn interpret(code: Option<i32>, stdout: &[u8], stderr: &[u8]) -> CliOutcome {
    let parsed: Option<Value> = serde_json::from_slice(stdout).ok();
    if let Some(v) = parsed
        && v.get("schema").and_then(Value::as_str) == Some(CLI_SCHEMA)
    {
        return match code {
            Some(0 | 1) => CliOutcome::Envelope(v),
            _ => CliOutcome::CliError(v),
        };
    }
    let err = String::from_utf8_lossy(stderr);
    if err.contains("unrecognized subcommand") {
        let first = err.lines().next().unwrap_or("");
        return CliOutcome::Unavailable(format!("bu CLI sürümünde yok: {}", sanitize(first, 300)));
    }
    let code = code.map_or("sinyal".to_string(), |c| c.to_string());
    CliOutcome::Failed(format!(
        "CLI geçerli {CLI_SCHEMA} zarfı döndürmedi (çıkış {code}): {}",
        sanitize(&err, 500)
    ))
}

/// Komutu çalıştırır. `args` önceden [`crate::commands::resolve`] ile doğrulanmış olmalıdır.
pub async fn run(cfg: &CliConfig, args: &[String]) -> CliOutcome {
    if let Err(e) = cfg.check_bin() {
        return CliOutcome::Failed(e);
    }
    let mut cmd = tokio::process::Command::from(cfg.command(args));
    cmd.kill_on_drop(true);
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return CliOutcome::Failed(format!(
                "atlas çalıştırılamadı ({}): {e}",
                cfg.atlas_bin.display()
            ));
        }
    };
    // Zaman aşımında `wait_with_output` geleceği düşer → kill_on_drop süreci öldürür.
    match tokio::time::timeout(cfg.timeout, child.wait_with_output()).await {
        Err(_) => CliOutcome::Failed(format!(
            "zaman aşımı: {} sn içinde yanıt yok",
            cfg.timeout.as_secs()
        )),
        Ok(Err(e)) => CliOutcome::Failed(format!("atlas çıktısı okunamadı: {e}")),
        Ok(Ok(out)) if out.stdout.len() > cfg.max_output_bytes => CliOutcome::Failed(format!(
            "çıktı çok büyük ({} bayt > {})",
            out.stdout.len(),
            cfg.max_output_bytes
        )),
        Ok(Ok(out)) => interpret(out.status.code(), &out.stdout, &out.stderr),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zarf_cikis_0_ve_1_veri() {
        let s = br#"{"schema":"atlas.cli/1","command":"health","ok":false,"data":{}}"#;
        assert!(matches!(
            interpret(Some(1), s, b""),
            CliOutcome::Envelope(_)
        ));
        assert!(matches!(
            interpret(Some(0), s, b""),
            CliOutcome::Envelope(_)
        ));
    }

    #[test]
    fn zarf_cikis_2_hata() {
        let s = br#"{"schema":"atlas.cli/1","ok":false,"error":"izin yok"}"#;
        assert!(matches!(
            interpret(Some(2), s, b""),
            CliOutcome::CliError(_)
        ));
    }

    #[test]
    fn bilinmeyen_komut_henuz_yok() {
        let e = b"error: unrecognized subcommand 'budget'\n\nUsage: atlas [OPTIONS] <COMMAND>\n";
        match interpret(Some(2), b"", e) {
            CliOutcome::Unavailable(m) => assert!(m.contains("budget")),
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn baska_sema_ya_da_bozuk_cikti_hata() {
        assert!(matches!(
            interpret(Some(0), br#"{"schema":"x/9"}"#, b""),
            CliOutcome::Failed(_)
        ));
        assert!(matches!(
            interpret(Some(0), b"not json", b"\x1b[31mpanic\x07"),
            CliOutcome::Failed(m) if !m.chars().any(char::is_control)
        ));
    }

    #[test]
    fn surec_rol_ve_home_iletir_kabuksuz() {
        let mut cfg = CliConfig::new(PathBuf::from("atlas"), Some(PathBuf::from("/srv/atlas")));
        cfg.timeout = Duration::from_secs(1);
        let c = cfg.command(&["--json".into(), "status".into()]);
        assert_eq!(c.get_program(), "atlas");
        let args: Vec<_> = c.get_args().collect();
        assert_eq!(args, ["--json", "status"]);
        let envs: Vec<_> = c.get_envs().collect();
        assert!(envs.contains(&("ATLAS_ROLE".as_ref(), Some("grok".as_ref()))));
        assert!(envs.contains(&("ATLAS_HOME".as_ref(), Some("/srv/atlas".as_ref()))));
    }

    #[test]
    fn betik_reddi() {
        for b in ["atlas.bat", "x/atlas.CMD", "atlas.sh"] {
            assert!(CliConfig::new(PathBuf::from(b), None).check_bin().is_err());
        }
        assert!(
            CliConfig::new(PathBuf::from("atlas.exe"), None)
                .check_bin()
                .is_ok()
        );
        assert!(
            CliConfig::new(PathBuf::from("atlas"), None)
                .check_bin()
                .is_ok()
        );
    }

    #[tokio::test]
    async fn olmayan_ikili_hata() {
        let cfg = CliConfig::new(PathBuf::from("atlas-bu-yok-xyz"), None);
        assert!(matches!(
            run(&cfg, &["--json".into(), "status".into()]).await,
            CliOutcome::Failed(_)
        ));
    }
}

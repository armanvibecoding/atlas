//! Entegrasyon testleri için ortak yardımcılar (her test ikilisi hepsini kullanmaz).
#![allow(dead_code, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Derlenmiş gerçek `atlas` ikilisi: `ATLAS_BIN`, yoksa `<target>/{debug,release}/atlas`.
/// Bulunamazsa nedenini yazdırıp `None` döner (test atlanır).
pub fn atlas_bin() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("ATLAS_BIN") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
        eprintln!("ATLAS_BIN={} dosya değil", p.display());
    }
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("target"));
    let name = format!("atlas{}", std::env::consts::EXE_SUFFIX);
    for profile in ["debug", "release"] {
        let p = target.join(profile).join(&name);
        if p.is_file() {
            return Some(p);
        }
    }
    eprintln!(
        "ATLANDI: gerçek atlas ikilisi yok ({}/{{debug,release}}/{name}). Önce `cargo build -p atlas` \
         çalıştırın ya da ATLAS_BIN verin.",
        target.display()
    );
    None
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dst = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dst);
        } else {
            std::fs::copy(e.path(), dst).unwrap();
        }
    }
}

/// Repo `configs/` ve `strategies/` kopyalanmış geçici ATLAS_HOME (gerçek journal/sicile dokunmaz).
pub fn temp_home() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    copy_dir(&repo_root().join("configs"), &d.path().join("configs"));
    copy_dir(
        &repo_root().join("strategies"),
        &d.path().join("strategies"),
    );
    d
}

//! "Dashboard tüm CLI verisini gösterir" (M4 kabulü): her CLI komutu ya `READ_COMMANDS`'ta ya da
//! gerekçesiyle `EXCLUDED`'da olmalı. Kaynaklar: GROK_OPS.md ve CLAUDE.md §6 tabloları (✓/✗) ve
//! derlenmiş `atlas --help` ağacı. CLI'ya yeni komut eklenip sınıflandırılmazsa bu test kırılır.
#![allow(clippy::unwrap_used)]

mod common;

use atlas_dashboard::commands::{Coverage, classify};
use std::process::Command;

/// Tablo satırını backtick içindeki `|`'lere ve `\|` kaçışına dokunmadan sütunlara böler.
fn split_row(line: &str) -> Vec<String> {
    let mut cols = Vec::new();
    let mut cur = String::new();
    let mut in_tick = false;
    let mut chars = line.trim().trim_start_matches('|').chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '`' => {
                in_tick = !in_tick;
                cur.push(c);
            }
            '\\' if chars.peek() == Some(&'|') => {
                cur.push('|');
                chars.next();
            }
            '|' if !in_tick => cols.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        cols.push(cur);
    }
    cols.into_iter().map(|c| c.trim().to_string()).collect()
}

/// Hücredeki backtick'li spanlar ve aralarındaki ayraç metni.
fn spans(cell: &str) -> Vec<(String, String)> {
    let parts: Vec<&str> = cell.split('`').collect();
    let mut out = Vec::new();
    // parts: [önce, span1, ara1, span2, ara2, ...]
    let mut i = 1;
    while i < parts.len() {
        let before = parts[i - 1].trim().to_string();
        out.push((before, parts[i].to_string()));
        i += 2;
    }
    out
}

/// Komut kelimeleri: `atlas` öneki, `<arg>`, `[--bayrak]` ve sonrası atılır;
/// `a|b` ve ` / ` alternatifleri genişletilir.
fn expand(span: &str, prev: Option<&Vec<String>>, continuation: bool) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = Vec::new();
    for (i, part) in span.split(" / ").enumerate() {
        let mut words: Vec<String> = Vec::new();
        for w in part.split_whitespace() {
            if w == "atlas" && words.is_empty() {
                continue;
            }
            if w.starts_with('<') || w.starts_with('[') || w.starts_with('-') || !w.is_ascii() {
                break;
            }
            words.push(w.to_string());
        }
        if words.is_empty() {
            continue;
        }
        // Devam parçası: önceki komutun son kelimesi yerine geçer.
        let base: Option<Vec<String>> = if i > 0 {
            out.first().cloned()
        } else if continuation {
            prev.cloned()
        } else {
            None
        };
        if let Some(b) = base {
            let mut w = b[..b.len().saturating_sub(1)].to_vec();
            w.extend(words);
            words = w;
        }
        // Son kelimedeki `a|b` alternatifleri.
        let last = words.pop().unwrap_or_default();
        for alt in last.split('|') {
            let mut w = words.clone();
            w.push(alt.to_string());
            out.push(w);
        }
    }
    out
}

/// Belgedeki (izinli, yasak) komut listeleri.
fn doc_commands(text: &str) -> (Vec<Vec<String>>, Vec<Vec<String>>) {
    let (mut allowed, mut denied) = (Vec::new(), Vec::new());
    for line in text.lines().filter(|l| l.trim_start().starts_with('|')) {
        let cols = split_row(line);
        let Some(last) = cols.last() else { continue };
        let target = if last.starts_with('✓') {
            &mut allowed
        } else if last.starts_with('✗') {
            &mut denied
        } else {
            continue;
        };
        let mut prev: Option<Vec<String>> = None;
        for (sep, span) in spans(&cols[0]) {
            let cont = sep == "/";
            for c in expand(&span, prev.as_ref(), cont) {
                prev = Some(c.clone());
                target.push(c);
            }
        }
    }
    (allowed, denied)
}

fn check_doc(name: &str) -> usize {
    let text = std::fs::read_to_string(common::repo_root().join(name)).unwrap();
    let (allowed, denied) = doc_commands(&text);
    assert!(
        allowed.len() >= 8,
        "{name}: tablo ayrıştırılamadı ({allowed:?})"
    );
    let mut missing = Vec::new();
    for c in &allowed {
        let w: Vec<&str> = c.iter().map(String::as_str).collect();
        if classify(&w) == Coverage::Missing {
            missing.push(w.join(" "));
        }
    }
    assert!(
        missing.is_empty(),
        "{name}: Grok'a açık (✓) ama dashboard'da ne READ_COMMANDS ne EXCLUDED: {missing:?}"
    );
    for c in &denied {
        let w: Vec<&str> = c.iter().map(String::as_str).collect();
        assert_ne!(
            classify(&w),
            Coverage::Read,
            "{name}: yasak (✗) komut beyaz listede: {w:?}"
        );
    }
    allowed.len()
}

#[test]
fn grok_ops_ve_spec_komutlari_kapsanir() {
    let n1 = check_doc("GROK_OPS.md");
    let n2 = check_doc("CLAUDE.md");
    eprintln!("GROK_OPS.md: {n1} ✓ komut, CLAUDE.md §6: {n2} ✓ komut — hepsi sınıflandırılmış");
}

#[test]
fn tablo_ayristirici_ornekleri() {
    let (a, d) = doc_commands(
        "| `atlas strategies list` / `show <ad>` | x `var/a` | ✓ |\n\
         | `report daily|weekly` | r | ✓ |\n\
         | `status`, `health` | s | ✓ |\n\
         | `atlas kill <global\\|account>` | k | ✓ |\n\
         | `atlas data pull-binance / quote-databento` | p | ✗ |\n\
         | `risk set …`, `deploy` | — | ✗ yalnız Arman |",
    );
    let j = |v: &Vec<Vec<String>>| v.iter().map(|c| c.join(" ")).collect::<Vec<_>>();
    assert_eq!(
        j(&a),
        [
            "strategies list",
            "strategies show",
            "report daily",
            "report weekly",
            "status",
            "health",
            "kill"
        ]
    );
    assert_eq!(
        j(&d),
        [
            "data pull-binance",
            "data quote-databento",
            "risk set",
            "deploy"
        ]
    );
}

/// `--help` çıktısındaki "Commands:" bölümünden alt komut adları (`help` hariç).
fn help_commands(bin: &std::path::Path, path: &[&str]) -> Vec<String> {
    let out = Command::new(bin)
        .args(path)
        .arg("--help")
        .env("ATLAS_ROLE", "grok")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let mut in_cmds = false;
    let mut v = Vec::new();
    for line in text.lines() {
        if line.trim_end() == "Commands:" {
            in_cmds = true;
            continue;
        }
        if in_cmds {
            if !line.starts_with("  ") {
                break;
            }
            if let Some(name) = line.split_whitespace().next()
                && name != "help"
            {
                v.push(name.to_string());
            }
        }
    }
    v
}

#[test]
fn cli_help_agaci_kapsanir() {
    let Some(bin) = common::atlas_bin() else {
        return;
    };
    let top = help_commands(&bin, &[]);
    assert!(
        top.contains(&"status".to_string()),
        "atlas --help ayrıştırılamadı: {top:?}"
    );
    let mut leaves: Vec<Vec<String>> = Vec::new();
    for t in &top {
        let subs = help_commands(&bin, &[t.as_str()]);
        if subs.is_empty() {
            leaves.push(vec![t.clone()]);
        } else {
            leaves.extend(subs.into_iter().map(|s| vec![t.clone(), s]));
        }
    }
    let mut missing = Vec::new();
    let mut read = 0;
    for l in &leaves {
        let w: Vec<&str> = l.iter().map(String::as_str).collect();
        match classify(&w) {
            Coverage::Read => read += 1,
            Coverage::Excluded(_) => {}
            Coverage::Missing => missing.push(w.join(" ")),
        }
    }
    assert!(
        missing.is_empty(),
        "CLI'da sınıflandırılmamış komut var — READ_COMMANDS'a (salt-okunur ise) ya da EXCLUDED'a \
         (gerekçeyle) ekleyin: {missing:?}"
    );
    eprintln!(
        "atlas --help: {} yaprak komut, {read} tanesi panelde, {} bilinçli dışarıda",
        leaves.len(),
        leaves.len() - read
    );
}

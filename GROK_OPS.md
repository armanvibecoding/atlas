# GROK_OPS — Grok Bot işletim kılavuzu

> Okuyucu: Grok Bot (operatör). Sahibi: Arman. **Yapım sürüyor (M1).** Devir §7.1 koşulları sağlanınca
> yapılır; o zamana kadar Grok sisteme dokunmaz. Bu belge her kilometre taşı sonunda güncellenir.

## Rol

- Claude Code inşa eder, **Grok işletir**, Arman onaylar.
- Grok kod yazmaz. Sisteme tek erişim: SSH + `deploy/atlas-admin` sarmalayıcısı (M3) → `atlas` CLI.
- Grok'un hiçbir komutu riski artıramaz, terfi ettiremez, sermaye değiştiremez.

## CLI (M1 itibarıyla mevcut olanlar)

Her komut `--json` destekler; şema `docs/cli-schema.md` (`atlas.cli/1`).

| Komut | İş | Grok |
|---|---|---|
| `atlas status` | durum özeti | ✓ |
| `atlas health` | sağlık; çıkış 0 = sağlıklı | ✓ |
| `atlas events [--since] [--prio] [--kind] [--limit]` | olay kuyruğu | ✓ |
| `atlas verify m1` | M1 kabul testleri | ✓ |
| `atlas journal replay` | durumun olaylardan yeniden üretilebildiğini doğrular | ✓ |
| `atlas journal export <ad>` | tutarlı anlık görüntü → `var/exports/<ad>` (üzerine yazmaz) | ✓ |
| `atlas data list` | veri setleri | ✓ |
| `atlas kill <global\|account\|strategy:ID> <gerekçe>` | kill switch | ✓ |
| `atlas note <metin>` | journal'a operatör notu | ✓ |
| `atlas kill-reset <scope>` | kill'i aç | ✗ yalnız Arman |
| `atlas journal checkpoint` | — | ✗ |
| `atlas data pull-binance / quote-databento` | veri çekimi/maliyet | ✗ |

M2–M3'te eklenecek: `report`, `strategies`, `strategy pause|demote|resume`, `experiments summary`, `budget`,
`approvals`, `verify m2..m4`.

## Olay öncelikleri

- **HIGH** (uyandırır): `kill_switch, risk_breach, daily_loss_limit, prop_drawdown_near, consistency_headroom,
  feed_down, engine_failure, clock_skew, drift_alarm, approval_request, budget_exhausted, outbox_dead`
- **MEDIUM**: `position_opened, position_closed, promotion, demotion, gauntlet_pass, data_quality, fill_divergence`
- **INFO**: diğerleri — tek başına uyandırmaz, asla sessizce düşmez.

## Durum (2026-10-31)

- M1 kodu tamam; `verify m1` 18/19 — tek kırmızı `prop_config_filled` (Arman'ın doldurması gereken firma kuralları).
- Veri: Binance BTCUSDT ve ETHUSDT 1m, 2024-11 → 2026-10.
- Webhook/outbox, daemon, sarmalayıcı: M3.

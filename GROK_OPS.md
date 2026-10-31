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
| `atlas strategies list` / `show <ad>` | merdivendeki stratejiler + son gauntlet raporu | ✓ |
| `atlas experiments summary` | sicil özeti, toplam deneme, kaynak/kapı dağılımı | ✓ |
| `atlas verify m2` | M2 kabul testleri (~2 dk) | ✓ |
| `atlas kill <global\|account\|strategy:ID> <gerekçe>` | kill switch | ✓ |
| `atlas note <metin>` | journal'a operatör notu | ✓ |
| `atlas kill-reset <scope>` | kill'i aç | ✗ yalnız Arman |
| `atlas journal checkpoint` | — | ✗ |
| `atlas data pull-binance / quote-databento / measure-slippage` | veri çekimi/maliyet/kalibrasyon | ✗ |
| `atlas gauntlet run|eval` | aday değerlendirme (deneme sayacı + holdout bütçesi tüketir) | ✗ |

M3'te eklenecek: `report`, `strategy pause|demote|resume`, `budget`,
`approvals`, `verify m2..m4`.

## Olay öncelikleri

- **HIGH** (uyandırır): `kill_switch, risk_breach, daily_loss_limit, prop_drawdown_near, consistency_headroom,
  feed_down, engine_failure, clock_skew, drift_alarm, approval_request, budget_exhausted, outbox_dead`
- **MEDIUM**: `position_opened, position_closed, promotion, demotion, gauntlet_pass, data_quality, fill_divergence`
- **INFO**: diğerleri — tek başına uyandırmaz, asla sessizce düşmez.

## Durum (2026-10-31)

- M1 tamam; `verify m1` 24/24 yeşil (genel prop kuralları).
- M2 tamam; `verify m2` 10/10 yeşil. Sicilde 5 referans deneme, hiçbiri gauntlet'i geçmedi (beklenen: 1m BTC'de
  taker ücreti edge'i yiyor). `gauntlet_pass` olayı = MEDIUM; şu an hiç yok.
- Veri: Binance BTCUSDT ve ETHUSDT 1m, 2024-11 → 2026-10.
- Webhook/outbox, daemon, sarmalayıcı: M3.

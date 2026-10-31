# `atlas` CLI çıktı şeması — `atlas.cli/1`

Her komut `--json` ile makine-okunur çıktı verir. Şema sürümlüdür; alan **kaldırma veya anlam değiştirme**
yeni sürüm (`atlas.cli/2`) gerektirir, alan **ekleme** gerektirmez.

## Zarf

```json
{"schema": "atlas.cli/1", "command": "<komut>", "ok": true, "data": { ... }}
```

Hata (çıkış kodu 2):

```json
{"schema": "atlas.cli/1", "ok": false, "error": "<mesaj>"}
```

## Çıkış kodları

| Kod | Anlam |
|---|---|
| 0 | başarılı / sağlıklı / yeşil |
| 1 | komut çalıştı ama sonuç olumsuz (health sorunlu, verify kırmızı, replay tutarsız, maliyet tavan üstü) |
| 2 | hata (config okunamadı, izin yok, ağ hatası…) |

## Ortam

| Değişken | Anlam |
|---|---|
| `ATLAS_HOME` | `configs/` içeren kök (varsayılan: çalışma dizini; `--home` ile de verilir) |
| `ATLAS_ROLE` | `grok` ise yalnız-Arman komutları reddedilir (sarmalayıcıya ek ikinci kontrol) |
| `ATLAS_DATABENTO_KEY` | Databento API anahtarı (repo'da değil; `/etc/atlas/atlas.env`) |

## Komutlar (M1)

### `status`
```json
data: {"time": "...Z", "journal": {"events": 42, "last": EVENT|null}, "positions": {..},
       "realized_net_usd": 0.0, "kills": {"<scope>": "<sebep>"}, "prop": {"file","loaded","firm"|"error"},
       "data": [{"dataset","bars","quality_issues","from","to"}], "milestone": "M1"}
```

### `health`
```json
data: {"healthy": bool, "checks": [{"name", "ok", "detail"}]}
```
Kontroller: `instruments`, `calendar`, `risk_policy`, `prop_config`, `journal` (replay tutarlı), `data_store`.

### `events [--since <RFC3339|seq:N>] [--prio high|medium|info] [--kind <tür>] [--limit N]`
```json
data: [EVENT, ...]
EVENT = {"seq", "id", "ts" (ns), "time" (RFC3339), "kind", "priority", "key", "payload"}
```
`--prio medium` = medium + high.

### `verify m1`
```json
data: {"milestone": "m1", "green": bool, "passed": n, "total": n,
       "checks": [{"name", "pass", "detail"}]}
```

### `journal replay | export <dosya> | checkpoint`
- replay: `{"events", "checkpoints", "mismatches": [..], "final_hash"}`, tutarsızsa çıkış 1.
- export: tutarlı anlık görüntü (SQLite backup API).
- checkpoint: yalnız Arman.

### `data list | pull-binance --symbol S --from YYYY-AA --to YYYY-AA | quote-databento ...`
- pull/quote yalnız Arman.

### `kill <global|account|strategy:ID> <gerekçe…>`
`{"scope", "seq"}` ya da `{"scope", "already_active": true}`.

### `kill-reset <scope>` — yalnız Arman.

### `note <metin…>` (ya da stdin) → `{"seq"}`.

## Olay türleri ve öncelikler

`core::events` tek kaynaktır (spec §5). HIGH: `kill_switch, risk_breach, daily_loss_limit, prop_drawdown_near,
consistency_headroom, feed_down, engine_failure, clock_skew, drift_alarm, approval_request, budget_exhausted,
outbox_dead`. MEDIUM: `position_opened, position_closed, promotion, demotion, gauntlet_pass, data_quality,
fill_divergence`. INFO: `heartbeat, experiment_done, operator_note, fill, intent_rejected, feed_up, kill_reset,
state_checkpoint, config_loaded, data_pulled, day_rollover`.

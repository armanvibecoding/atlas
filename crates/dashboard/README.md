# atlas-dashboard

Salt-okunur web paneli + JSON API. Veriyi `atlas --json` alt sürecinden alır (tek veri kaynağı CLI;
gerekçe ve güvenlik modeli: `docs/adr/0007-dashboard.md`).

```sh
cargo build -p atlas -p atlas-dashboard
target/debug/atlas-dashboard --home /srv/atlas          # → http://127.0.0.1:8787
```

| Bayrak / ortam | Anlam |
|---|---|
| `--listen` | dinleme adresi (varsayılan `127.0.0.1:8787`) |
| `--allow-remote` | localhost dışı adrese izin (önerilmez; kimlik doğrulama yok) |
| `--allow-host H:P` | ek kabul edilen `Host` başlığı (tekrarlanabilir) |
| `--atlas-bin` / `ATLAS_BIN` | `atlas` ikilisi (varsayılan: yanındaki `atlas`, yoksa PATH) |
| `--home` / `ATLAS_HOME` | alt sürece iletilen ATLAS_HOME |
| `--timeout-secs` | komut başına zaman aşımı (varsayılan 30) |

## VPS'te erişim

Kimlik doğrulama **yoktur**; panel yalnız 127.0.0.1'de dinler. VPS'teki panele SSH tüneliyle bağlanın:

```sh
ssh -L 8787:127.0.0.1:8787 <vps>
# yerel tarayıcı: http://127.0.0.1:8787
```

## API

- `GET /api` — beyaz liste (`READ_COMMANDS`).
- `GET /api/<komut>` — CLI zarfı (`atlas.cli/1`) aynen. Örn. `/api/status`, `/api/health`,
  `/api/events?since=seq:100&prio=high&kind=kill_switch&limit=50`, `/api/strategies/show/<ad>`.
- 400 geçersiz argüman · 404 liste dışı (ör. `/api/kill`) · 501 CLI'da henüz yok (`unavailable: true`)
  · 502 CLI hatası / zaman aşımı.

Yeni bir salt-okunur CLI komutu eklendiğinde `src/commands.rs`'teki `READ_COMMANDS`'a bir satır
eklenir; yazma komutuysa gerekçesiyle `EXCLUDED`'a. `tests/coverage.rs` sınıflandırılmamış komutu yakalar.

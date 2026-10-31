#!/usr/bin/env bash
# Atlas kurulum betiği (Debian/Ubuntu VPS). Kök olarak çalıştır: sudo bash deploy/install.sh
# Yaptığı: kullanıcı + dizinler, binary/config kopyası, /etc/atlas/atlas.env (600) şablonu,
# systemd birimi, Grok için atlas-admin sarmalayıcısı. Var olan env dosyasının ÜZERİNE YAZMAZ.
set -euo pipefail

SRC="$(cd "$(dirname "$0")/.." && pwd)"
PREFIX=/opt/atlas

[[ $EUID -eq 0 ]] || { echo "root gerekli" >&2; exit 1; }
command -v rsync >/dev/null || apt-get install -y rsync
command -v cargo >/dev/null || { echo "önce Rust kur: https://rustup.rs" >&2; exit 1; }

id atlas >/dev/null 2>&1 || useradd --system --home "$PREFIX" --shell /usr/sbin/nologin atlas
id grok >/dev/null 2>&1 || useradd --create-home --shell /bin/bash grok

echo "→ derleme (release)"
(cd "$SRC" && nice -n 15 cargo build --release -j 4 -p atlas)

install -d -o atlas -g atlas -m 750 "$PREFIX" "$PREFIX/bin" "$PREFIX/var" "$PREFIX/var/log"
install -m 755 "$SRC/target/release/atlas" "$PREFIX/bin/atlas"
if [[ -f "$SRC/target/release/atlas-dashboard" ]]; then
  install -m 755 "$SRC/target/release/atlas-dashboard" "$PREFIX/bin/atlas-dashboard"
fi
rsync -a --delete "$SRC/configs/" "$PREFIX/configs/"
rsync -a --delete "$SRC/strategies/" "$PREFIX/strategies/"
install -d -m 755 "$PREFIX/deploy"
install -m 755 "$SRC/deploy/atlas-admin" "$PREFIX/deploy/atlas-admin"
chown -R atlas:atlas "$PREFIX"

install -d -m 750 /etc/atlas
if [[ ! -f /etc/atlas/atlas.env ]]; then
  cat > /etc/atlas/atlas.env <<'ENV'
# Atlas sırları — repo'da DEĞİL. chmod 600.
# ATLAS_ANTHROPIC_KEY=
# ATLAS_XAI_KEY=
# ATLAS_DATABENTO_KEY=
ENV
fi
chmod 600 /etc/atlas/atlas.env
chown root:atlas /etc/atlas/atlas.env

install -m 755 "$SRC/deploy/atlas-admin" /usr/local/bin/atlas-admin
install -m 644 "$SRC/deploy/atlas.service" /etc/systemd/system/atlas.service
install -m 644 "$SRC/deploy/logrotate.conf" /etc/logrotate.d/atlas
# Grok, atlas grubunda: var/ altını okuyabilir (journal, sicil); yazma yalnız CLI üzerinden.
usermod -aG atlas grok

systemctl daemon-reload
systemctl enable atlas.service

cat <<MSG
Kurulum tamam.
1) Veri:   sudo -u atlas ATLAS_HOME=$PREFIX $PREFIX/bin/atlas data pull-binance --symbol BTCUSDT --from 2024-11 --to \$(date +%Y-%m)
2) Kontrol: sudo -u atlas ATLAS_HOME=$PREFIX $PREFIX/bin/atlas verify m1
3) Başlat: systemctl start atlas && journalctl -u atlas -f
4) Grok anahtarı (/home/grok/.ssh/authorized_keys):
   command="/usr/local/bin/atlas-admin",no-port-forwarding,no-X11-forwarding,no-agent-forwarding,no-pty ssh-ed25519 AAAA... grok
5) Webhook: $PREFIX/configs/atlas.toml [webhook] url = "https://..."
MSG

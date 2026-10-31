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

install -d -m 750 "$PREFIX" "$PREFIX/bin"
install -d -o atlas -g atlas -m 750 "$PREFIX/var" "$PREFIX/var/log"
install -m 755 "$SRC/target/release/atlas" "$PREFIX/bin/atlas"
if [[ -f "$SRC/target/release/atlas-dashboard" ]]; then
  install -m 755 "$SRC/target/release/atlas-dashboard" "$PREFIX/bin/atlas-dashboard"
fi
# Sunucudaki atlas.toml (webhook, onay anahtarı, bütçeler) güncellemede EZİLMEZ; yalnız ilk kurulumda kopyalanır.
rsync -a --delete --exclude atlas.toml "$SRC/configs/" "$PREFIX/configs/"
if [[ -L "$PREFIX/configs/atlas.toml" ]]; then
  echo "HATA: $PREFIX/configs/atlas.toml sembolik bağlantı — reddedildi" >&2; exit 1
fi
if [[ ! -f "$PREFIX/configs/atlas.toml" ]]; then
  install -m 640 -o root -g atlas "$SRC/configs/atlas.toml" "$PREFIX/configs/atlas.toml"
elif ! cmp -s "$SRC/configs/atlas.toml" "$PREFIX/configs/atlas.toml"; then
  echo "NOT: repo atlas.toml sunucudakinden farklı; sunucudaki korundu. Fark: diff $SRC/configs/atlas.toml $PREFIX/configs/atlas.toml"
fi
rsync -a --delete "$SRC/strategies/" "$PREFIX/strategies/"
# deploy/ tamamı (verify m3 sarmalayıcıyı ve kurulum dosyalarını buradan denetler).
rsync -a --delete "$SRC/deploy/" "$PREFIX/deploy/"
# Sahiplik: servis yalnız var/ altına yazar; bin/, configs/, strategies/, deploy/ root'a ait (servis hesabı
# root'un çalıştırdığı/okuduğu dosyaları değiştiremez).
chown root:atlas "$PREFIX"
chmod 750 "$PREFIX"
chown -R root:atlas "$PREFIX/bin" "$PREFIX/configs" "$PREFIX/strategies" "$PREFIX/deploy"
chmod -R go-w "$PREFIX/bin" "$PREFIX/configs" "$PREFIX/strategies" "$PREFIX/deploy"
chmod -R g+rX "$PREFIX/bin" "$PREFIX/configs" "$PREFIX/strategies" "$PREFIX/deploy"
chown -R atlas:atlas "$PREFIX/var"
# var/ grup (atlas) yazılabilir + setgid: Grok (grok ∈ atlas) kill/note/pause ile journal ve sicile yazabilir.
chmod -R g+rwX "$PREFIX/var"
find "$PREFIX/var" -type d -exec chmod g+s {} +

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

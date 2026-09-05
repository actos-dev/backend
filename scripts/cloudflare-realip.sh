#!/usr/bin/env bash
# Cloudflare IP aralıklarını nginx'in `set_real_ip_from` listesine yazar.
#
# NEDEN GEREKLİ: `api.actos.com.tr` Cloudflare proxy'sinin arkasında
# (turuncu bulut). O hâlde nginx'e gelen her isteğin kaynak IP'si bir
# Cloudflare adresidir. Actos'un hız sınırı kovalarının bir kısmı IP
# bazlı — `register` 3/saat, `recover` 5/gün, anonim `write` 30/saat
# (bkz. crates/actos-core/src/config.rs). Bu yapılandırma olmadan **tüm
# dünya tek bir kovayı paylaşır** ve kayıt fiilen kilitlenir.
#
# `CF-Connecting-IP` yalnızca listedeki adreslerden geldiğinde güvenilir;
# aksi halde herkes başlığı uydurup hız sınırını atlayabilirdi.
#
# Kurulum (sunucuda, root):
#   install -m 755 cloudflare-realip.sh /usr/local/bin/
#   /usr/local/bin/cloudflare-realip.sh
#   # haftalık tazeleme (aralıklar nadiren ama değişiyor):
#   echo '17 4 * * 1 root /usr/local/bin/cloudflare-realip.sh' \
#     > /etc/cron.d/cloudflare-realip

set -euo pipefail

CIKTI=/etc/nginx/conf.d/cloudflare-realip.conf
GECICI=$(mktemp)
trap 'rm -f "$GECICI"' EXIT

{
    echo "# OTOMATİK ÜRETİLDİ — elle düzenleme, cloudflare-realip.sh üzerine yazar."
    echo "# Üretim: $(date -Is)"
    for url in https://www.cloudflare.com/ips-v4 https://www.cloudflare.com/ips-v6; do
        curl -fsS --max-time 20 "$url" | while read -r aralik; do
            [ -n "$aralik" ] && echo "set_real_ip_from $aralik;"
        done
    done
    echo "real_ip_header CF-Connecting-IP;"
    # Zincirdeki tüm güvenilen proxy'leri atla; Cloudflare'in kendi iç
    # atlamalarında X-Forwarded-For birden fazla girdi taşıyabiliyor.
    echo "real_ip_recursive on;"
} > "$GECICI"

# Boş/kısa çıktı = curl sessizce başarısız oldu; çalışan yapılandırmayı
# ezip nginx'i kırmaktansa hiç dokunmamak iyidir.
if [ "$(wc -l < "$GECICI")" -lt 10 ]; then
    echo "hata: beklenenden az aralık alındı, $CIKTI'e dokunulmadı" >&2
    exit 1
fi

install -m 644 "$GECICI" "$CIKTI"

# Yalnızca yapılandırma geçerliyse yeniden yükle.
nginx -t
systemctl reload nginx
echo "cloudflare real_ip listesi güncellendi: $CIKTI"

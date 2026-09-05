#!/usr/bin/env bash
# Actos üretim yedeği: Postgres dump + MinIO bucket aynası (PLAN.md Faz 19).
#
# Kurulum (sunucuda, root):
#   install -m 755 backup.sh /usr/local/bin/actos-backup.sh
#   echo '23 3 * * * root /usr/local/bin/actos-backup.sh >> /var/log/actos-backup.log 2>&1' \
#     > /etc/cron.d/actos-backup
#
# GERİ YÜKLEME TATBİKATI YAPILMADAN bu script bir yedek sayılmaz —
# doğrulanmamış yedek, yedek değil kağıt üstünde bir umuttur.
# Tatbikat adımları: docs/DEPLOYMENT.md §"Yedekten dönüş tatbikatı".

set -euo pipefail

HEDEF=${ACTOS_BACKUP_DIR:-/var/backups/actos}
SAKLA_GUN=${ACTOS_BACKUP_KEEP_DAYS:-14}
BUGUN=$(date +%F)
DIZIN="$HEDEF/$BUGUN"

# .env.prod'daki POSTGRES_* ve MINIO_* değerlerini kullan.
set -a
# shellcheck disable=SC1091
. /opt/actos/.env.prod
set +a

mkdir -p "$DIZIN"

# --- Postgres -------------------------------------------------------------
# `-Fc` (custom format): pg_restore ile seçmeli geri yükleme yapılabiliyor
# ve sıkıştırma dahil. Düz SQL'e göre hem küçük hem esnek.
docker exec actos_postgres pg_dump \
    -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Fc \
    > "$DIZIN/postgres.dump"

# Roller veritabanına özgü değil, ayrı dökülmezse geri yüklemede kayıp.
docker exec actos_postgres pg_dumpall -U "$POSTGRES_USER" --globals-only \
    > "$DIZIN/globals.sql"

# --- MinIO ----------------------------------------------------------------
# `mc mirror --remove`: silinen dosyalar aynada da silinir. Bu bilinçli —
# ayna diskteki gerçek durumu yansıtsın; tarihsel sürümler için tarih
# damgalı dizinler (yukarıdaki $BUGUN) yeterli.
docker run --rm --network actos_network \
    -v "$DIZIN:/yedek" \
    -e MC_HOST_actos="http://${MINIO_ROOT_USER}:${MINIO_ROOT_PASSWORD}@minio:9000" \
    minio/mc:latest \
    mirror --overwrite --remove "actos/${MINIO_BUCKET}" "/yedek/minio"

# --- Doğrulama ------------------------------------------------------------
# Boş bir dump sessizce yazılabiliyor (pg_dump hata verse bile yönlendirme
# dosyayı yaratır). Boyut kontrolü bunu yakalar.
BOYUT=$(stat -c %s "$DIZIN/postgres.dump")
if [ "$BOYUT" -lt 10000 ]; then
    echo "HATA: postgres.dump şüpheli derecede küçük ($BOYUT bayt)" >&2
    exit 1
fi

# Arşiv gerçekten okunabiliyor mu — pg_restore listesi bunu ispatlar.
docker exec -i actos_postgres pg_restore --list < "$DIZIN/postgres.dump" > /dev/null

# --- Rotasyon -------------------------------------------------------------
find "$HEDEF" -maxdepth 1 -type d -name '20*' -mtime "+$SAKLA_GUN" -exec rm -rf {} +

echo "$(date -Is) yedek tamam: $DIZIN ($(du -sh "$DIZIN" | cut -f1))"

# Dağıtım — Actos backend

> Faz 19. Bu doküman sunucuyu sıfırdan üretime hazırlar ve CI/CD'yi bağlar.
> Sunucunun envanteri ve yükseltme geçmişi ayrı bir dosyada: workspace
> kökündeki `SUNUCU.md`.

Hedef mimari:

```
                 Cloudflare (proxy, "Full (strict)")
                            │
                    37.140.242.25
                            │
                 nginx (host, 80/443, certbot)
        ┌───────────────────┼───────────────────┐
        │                   │                   │
  actos.com.tr       api.actos.com.tr    media.actos.com.tr
   → :3000 (web)      → :3100 (api)       → :3103 (minio)
                            │
                  docker compose (actos_network)
             postgres  redis  minio  migrate  api
```

---

## 1. Zorunlu ortam değişkenleri

Sunucuda `/opt/actos/.env.prod`. **Bu dosya repoda değildir ve asla
commit'lenmez.** `docker-compose.prod.yml` bunların hepsini `:?` ile
zorunlu kılar — biri eksikse yığın açılmaz, sessizce varsayılana düşmez.

| Değişken | Nasıl üretilir | Not |
|---|---|---|
| `POSTGRES_USER` | `actos` | |
| `POSTGRES_PASSWORD` | `openssl rand -base64 32` | geliştirme şifresi ASLA |
| `POSTGRES_DB` | `actos` | |
| `MINIO_ROOT_USER` | `actos_minio` | |
| `MINIO_ROOT_PASSWORD` | `openssl rand -base64 32` | |
| `MINIO_BUCKET` | `actos-media` | |
| `S3_PUBLIC_BASE_URL` | `https://media.actos.com.tr/actos-media` | **dışarıdan görünen** adres |
| `ID_OBFUSCATION_KEY` | `openssl rand -hex 32` | ↓ aşağıdaki uyarı |
| `CURSOR_SIGNING_KEY` | `openssl rand -hex 32` | `ID_OBFUSCATION_KEY`'den farklı olmalı |
| `ACTOS_IMAGE` | CI tarafından geçiriliyor | `.env.prod`'a yazma |
| `WEB_IMAGE` | CI tarafından geçiriliyor | frontend imajı; elle dağıtımda `export` et (bkz. §5) |

İsteğe bağlı (varsayılanı olan): `DATABASE_MAX_CONNECTIONS` (20),
`TRUSTED_PROXY_HOPS` (1), `MAX_UPLOAD_BYTES` (8 MB), `S3_REGION`,
`RUST_LOG`.

> **`ID_OBFUSCATION_KEY` bir kez seçilir ve bir daha değişmez.** Dış ID'ler
> (`c_7fK2…`) bu anahtardan türetiliyor. Anahtar değişirse *var olan tüm
> bağlantılar kırılır* — paylaşılmış post URL'leri, SDK'ların önbelleklediği
> id'ler, arama motoru dizini. Üretime çıkmadan önce doğru değeri koy;
> sonra döndürmek bir veri taşıma işidir, bir yapılandırma değişikliği değil.

Üretim `.env.prod`'u hızlıca kurmak için:

```bash
mkdir -p /opt/actos && cd /opt/actos
umask 077
cat > .env.prod <<EOF
POSTGRES_USER=actos
POSTGRES_PASSWORD=$(openssl rand -base64 32)
POSTGRES_DB=actos
MINIO_ROOT_USER=actos_minio
MINIO_ROOT_PASSWORD=$(openssl rand -base64 32)
MINIO_BUCKET=actos-media
S3_PUBLIC_BASE_URL=https://media.actos.com.tr/actos-media
ID_OBFUSCATION_KEY=$(openssl rand -hex 32)
CURSOR_SIGNING_KEY=$(openssl rand -hex 32)
TRUSTED_PROXY_HOPS=1
EOF
chmod 600 .env.prod
```

---

## 2. Sunucu hazırlığı

### 2.1 Compose V2

Sunucuda yalnızca standalone `docker-compose` v1.29.2 var — 2023'te
kullanımdan kaldırıldı ve `docker-compose.prod.yml`'ın kullandığı
`depends_on: condition: service_completed_successfully` koşulunu
desteklemiyor. Yani V1 ile migration job'ı atlanır ve API göç edilmemiş
şemaya karşı kalkar. Plugin'e geçilmeden dağıtım yapılmamalı.

**`apt-get install docker-compose-plugin` bu sunucuda ÇALIŞMAZ.** Docker,
Ubuntu'nun kendi `docker.io` paketinden kurulu (29.1.3-0ubuntu3~22.04.2),
Docker'ın resmî apt deposu yapılandırılmamış — o paket depolarda yok
(doğrulandı 2026-09-05). Resmî depoyu eklemek `docker.io` → `docker-ce`
geçişi demek; bu daemon'ı yeniden başlatır ve **Florence'ın 5 konteynerini
düşürür**. Dağıtım için gereken şey bu değil.

Doğru yol — plugin'i CLI eklenti dizinine düşür, daemon'a dokunma:

```bash
CLI_PLUGINS=/usr/libexec/docker/cli-plugins    # bu sunucuda mevcut dizin
SURUM=v2.40.0                                  # güncelini kontrol et
curl -fsSL -o "$CLI_PLUGINS/docker-compose" \
  "https://github.com/docker/compose/releases/download/${SURUM}/docker-compose-linux-x86_64"
chmod 755 "$CLI_PLUGINS/docker-compose"

docker compose version      # v2.x görmeli
```

Hiçbir konteyner yeniden başlamaz: bu yalnızca `docker` CLI'ının alt
komutu, daemon'la ilgisi yok. Eski `docker-compose` (v1) yerinde kalır,
Florence onu kullanmaya devam eder.

### 2.2 Dizin

```bash
mkdir -p /opt/actos
# .env.prod (bölüm 1) ve docker-compose.prod.yml (CI gönderiyor) burada durur
```

### 2.3 Port çakışması kontrolü

Florence 5433/5434/5435/7055 kullanıyor; Actos 3100–3104 kullanıyor.
Çakışma yok, ama açmadan önce doğrula:

```bash
ss -ltn | grep -E ':(3100|3101|3102|3103|3104) '   # boş çıkmalı
```

---

## 3. Alan adı: actos.com.tr → Cloudflare → sunucu

`actos.com.tr` Atak Domain üzerinden kayıtlı; başlangıçta nameserver'lar
`ns1/ns2.hostingdunyam.net`. `florencex.com.tr` aynı yolu izliyor, referans
olarak bakılabilir.

### 3.1 Nameserver'ları Cloudflare'e çevir

1. Cloudflare → **Add a site** → `actos.com.tr` → Free plan.
2. Cloudflare iki nameserver verir.
3. Registrar panelinde domain'in nameserver'larını bunlarla değiştir.
   `.com.tr`'de bu nic.tr'ye gider; genelde dakikalar sürer.
   ("LOCKED to transfer" durumu bunu engellemez — o transfer kilidi.)
4. Doğrula: `dig +short NS actos.com.tr` → Cloudflare adresleri.

### 3.2 DNS kayıtları

| Tip | Ad | Değer | Proxy |
|---|---|---|---|
| A | `actos.com.tr` | `37.140.242.25` | önce gri, sonra turuncu |
| CNAME | `www` | `actos.com.tr` | aynı |
| A | `api` | `37.140.242.25` | aynı |
| A | `media` | `37.140.242.25` | aynı |

**SSL/TLS modu: "Full (strict)".** "Flexible" ASLA — Cloudflare ile sunucu
arası şifresiz kalır ve yönlendirme döngüsü üretir.

### 3.3 Sertifikalar — sıra önemli

certbot HTTP-01 doğrulaması yapıyor, yani Let's Encrypt'in doğrudan
sunucuya ulaşması gerekiyor. Turuncu bulut açıkken bu istek Cloudflare'e
takılır. Doğru sıra:

```bash
# 1) Cloudflare'de kayıtlar GRİ BULUT (DNS-only) iken:
cp deploy/nginx/snippets/actos-proxy.conf /etc/nginx/snippets/
cp deploy/nginx/*.conf /etc/nginx/sites-available/
ln -s /etc/nginx/sites-available/api.actos.com.tr   /etc/nginx/sites-enabled/
ln -s /etc/nginx/sites-available/actos.com.tr       /etc/nginx/sites-enabled/
ln -s /etc/nginx/sites-available/media.actos.com.tr /etc/nginx/sites-enabled/
nginx -t && systemctl reload nginx

# 2) Sertifikalar
certbot --nginx -d actos.com.tr -d www.actos.com.tr
certbot --nginx -d api.actos.com.tr
certbot --nginx -d media.actos.com.tr

# 3) ŞİMDİ Cloudflare'de turuncu buluta geç ve SSL modunu
#    "Full (strict)" yap.
```

### 3.4 Cloudflare ve gerçek IP — atlanamaz

Proxy açıkken sunucuya gelen isteklerin kaynak IP'si Cloudflare'dir.
Actos'un hız sınırı kovalarının bir kısmı IP bazlı (`register` 3/saat,
`recover` 5/gün, anonim `write` 30/saat). Bu adım yapılmazsa **tüm dünya
tek bir kovayı paylaşır ve kayıt fiilen kilitlenir**.

```bash
install -m 755 scripts/cloudflare-realip.sh /usr/local/bin/
/usr/local/bin/cloudflare-realip.sh
echo '17 4 * * 1 root /usr/local/bin/cloudflare-realip.sh' \
  > /etc/cron.d/cloudflare-realip
```

Doğrulama — kendi IP'nle bir istek at ve log'a bak:

```bash
curl -s https://api.actos.com.tr/health > /dev/null
tail -1 /var/log/nginx/access.log     # senin IP'n görünmeli, 172.x/104.x değil
```

`TRUSTED_PROXY_HOPS=1` de bununla birlikte gider: backend
`X-Forwarded-For`'un sondan bir önceki girdisini alır (nginx'in eklediği
Cloudflare adresini atlar).

---

## 4. CI/CD

### 4.1 GitHub secrets

Repo → Settings → Secrets and variables → Actions:

| Secret | Değer |
|---|---|
| `DEPLOY_HOST` | `37.140.242.25` |
| `DEPLOY_USER` | `root` |
| `DEPLOY_SSH_KEY` | dağıtıma özel **yeni** bir özel anahtar (aşağı bak) |
| `DEPLOY_KNOWN_HOSTS` | `ssh-keyscan -H 37.140.242.25` çıktısı |

Dağıtım için ayrı bir anahtar üret — kişisel `florence_deploy_ed25519`
GitHub'a konmamalı; bir anahtarın iki işi olursa birini iptal etmek
diğerini de keser:

```bash
ssh-keygen -t ed25519 -f ~/.ssh/actos_ci -C "actos-ci" -N ""
ssh-copy-id -i ~/.ssh/actos_ci.pub root@37.140.242.25
cat ~/.ssh/actos_ci          # → DEPLOY_SSH_KEY
ssh-keyscan -H 37.140.242.25 # → DEPLOY_KNOWN_HOSTS
```

Ayrıca `production` adlı bir GitHub **environment** oluştur (deploy
workflow'u onu istiyor); istersen oraya manuel onay kuralı ekle.

### 4.2 İş akışları

- `.github/workflows/ci.yml` — `fmt`, `clippy`, `test`, `audit` paralel;
  hepsi yeşilse imaj derlenip GHCR'a (`ghcr.io/actos-dev/backend`) push
  edilir. Test işi `docker-compose.yml`'ı kaldırıp gerçek Postgres/Redis/
  MinIO'ya karşı koşar ve `SQLX_OFFLINE=true` ile derler — `.sqlx` bayatsa
  orada kırılır.
- `.github/workflows/deploy.yml` — CI `main`'de yeşil bitince tetiklenir;
  sunucuya SSH ile girip imajı çeker ve `compose up -d --wait` koşar.
  Sonunda `https://api.actos.com.tr/health/ready`'ye duman testi atar.

Frontend deposunda aynı desen iki iş akışıyla uygulanır:

- `frontend/.github/workflows/ci.yml` — `check` (lint, typecheck, test,
  build), `audit` (`pnpm audit --prod`) ve `e2e` (mocked Playwright) paralel
  koşar; hepsi yeşilse `ghcr.io/actos-dev/frontend:sha-<kısa>` ve `latest`
  push edilir.
- `frontend/.github/workflows/deploy.yml` — aynı tetikleme/geri alma
  düzeniyle sunucuya girip `WEB_IMAGE`'ı ortamdan geçirir ve
  `compose up -d --wait web` koşar; ardından `https://actos.com.tr/healthz`
  ile bir gerçek sayfa render'ına duman testi atar. **Compose dosyasını
  scp'leyen adım yalnızca backend'de**: web servisi aynı
  `docker-compose.prod.yml` içinde tanımlı, bu yüzden frontend dağıtımı
  dosyaya dokunmaz.

**Geri alma:** Actions → Deploy → *Run workflow* → `image_tag` alanına
eski `sha-xxxxxxx` yaz. Her CI koşusu değişmez bir `sha-` etiketi ürettiği
için geri alma bir etiket seçiminden ibaret.

### 4.3 `docs/openapi.json` tazelik kapısı

`crates/actos-api/tests/openapi.rs::commitlenmis_openapi_json_kodla_ayni`
üretilen spec'i commit'lenmiş dosyayla karşılaştırır (normalize JSON —
anahtar sırası ve girinti farkı hata sayılmaz). 2026-09-03'te snapshot 42
yolda kalıp elle tazelenmişti; bu test o sapmayı derleme zamanında
yakalar. Spec bilerek değiştiyse:

```bash
ACTOS_UPDATE_OPENAPI=1 cargo test -p actos-api --test openapi \
  commitlenmis_openapi_json_kodla_ayni
```

### 4.4 Web servisi (frontend)

`docker-compose.prod.yml` içindeki `web` servisi Next.js imajını çalıştırır.
İmaj `${WEB_IMAGE:?...}` ile zorunlu kılınır; etiket frontend CI'ında
üretilen değişmez `sha-<kısa>`'dır (`latest` yalnızca varsayılan dalda
push edilir). Dağıtım `WEB_IMAGE`'ı ortamdan geçirir, `.env.prod`'a yazmaz:

- `image: ${WEB_IMAGE:?...}`
- `ports: "127.0.0.1:3000:3000"` — yalnızca loopback; dışarıya açan tek şey
  nginx (`deploy/nginx/actos.com.tr.conf` → `127.0.0.1:3000`).
- `depends_on: api: condition: service_healthy` — API sağlıklı olmadan web
  trafik almaz.
- ortam: `ACTOS_API_URL=http://api:3100` (konteyner ağı), `ACTOS_SITE_URL`,
  `NEXT_PUBLIC_ACTOS_API_URL`, `ACTOS_MEDIA_URL`,
  `NODE_ENV=production`, `NEXT_TELEMETRY_DISABLED=1`.
- healthcheck: `wget -qO- http://127.0.0.1:3000/healthz`.

`NEXT_PUBLIC_ACTOS_API_URL` ve `ACTOS_MEDIA_URL` **build-time** değerlerdir;
değiştirmek yeni bir imaj build'i gerektirir (bkz. frontend `PUBLISH.md`).

### 4.5 Cloudflare önbellek kuralları

- **HTML asla önbelleğe alınmaz.** Özellikle oturum çerezi taşıyan
  yanıtlar; kullanıcı A'nın sayfası kullanıcı B'ye servis edilirse bu bir
  oturum sızıntısıdır. "Cache Everything" sayfa kuralını kullanma; kenar
  TTL'ini "Respect Existing Headers" bırak. Next.js HTML'i `Cache-Control:
  no-store`/`private` üretir.
- `/_next/static/*` uzun süreli önbelleklenir: dosya adları içerik
  hash'i taşır, bu yüzden `Cache-Control: public, max-age=31536000,
  immutable` güvenlidir. Next.js bu başlığı zaten gönderir; ek bir kural
  gerekmez.
- `/healthz` ve `app/api/*` istekleri önbellek dışı kalmalı (`no-store`);
  Cloudflare'de bunları bypass eden bir kural tanımla.

---

## 5. İlk dağıtım (elle, bir kez)

CI'ı beklemeden yığını ayağa kaldırmak için:

```bash
cd /opt/actos
export ACTOS_IMAGE=ghcr.io/actos-dev/backend:latest
export WEB_IMAGE=ghcr.io/actos-dev/frontend:latest
echo "$GHCR_TOKEN" | docker login ghcr.io -u <kullanıcı> --password-stdin
docker compose -f docker-compose.prod.yml --env-file .env.prod up -d --wait

# İlk admin (API üzerinden oluşturulamaz — bilinçli, bkz. bin/seed.rs)
docker compose -f docker-compose.prod.yml --env-file .env.prod \
  run --rm --entrypoint /usr/local/bin/actos-seed api <kullanıcı_adı>
# Basılan API key'i güvenli bir yere al; bir daha gösterilmez.
```

Doğrulama:

```bash
curl -s https://api.actos.com.tr/health/ready | jq
curl -s https://api.actos.com.tr/version | jq
curl -s https://api.actos.com.tr/openapi.json | jq '.paths | length'   # 45
```

---

## 6. Yedekleme

```bash
install -m 755 scripts/backup.sh /usr/local/bin/actos-backup.sh
echo '23 3 * * * root /usr/local/bin/actos-backup.sh >> /var/log/actos-backup.log 2>&1' \
  > /etc/cron.d/actos-backup
/usr/local/bin/actos-backup.sh        # ilk koşuyu elle yap
```

`pg_dump -Fc` + `mc mirror`, `/var/backups/actos/<tarih>/` altında,
14 gün saklanır. Script dump'ın boyutunu ve `pg_restore --list` ile
okunabilirliğini doğrular.

### Yedekten dönüş tatbikatı

**Bu yapılmadan yedek yok sayılır.** Doğrulanmamış bir yedek, kağıt
üstünde bir umuttur. Florence'ta bu tatbikat 2026-09-02'de yapıldı ve
dump'ın tutarlı olduğunu gerçekten kanıtladı (bkz. `SUNUCU.md` §11) —
aynısı Actos için de yapılmalı:

```bash
# Yerelde, boş bir veritabanına
createdb actos_tatbikat
pg_restore -d actos_tatbikat --no-owner --no-privileges postgres.dump

# Satır sayılarını canlıyla karşılaştır
psql -d actos_tatbikat -c "
  SELECT 'actors' t, count(*) FROM actors
  UNION ALL SELECT 'contents', count(*) FROM contents
  UNION ALL SELECT 'votes',    count(*) FROM votes;"
```

---

## 7. Bilinen kısıtlar

- **Sunucunun 8 GB RAM'inin ~5 GB'ı hipervizör tarafından geri alınmış**
  (`vmw_balloon`, 1 309 184 sayfa). `MemAvailable` ~1.3 GB. Actos'un
  ~750 MB'ı sığar ama page cache'e pay kalmaz. Sağlayıcıya bildirildi;
  çözülene kadar Postgres'in `shared_buffers`/`effective_cache_size`
  değerleri **gerçek** kullanılabilir belleğe göre ayarlanmalı, nominal
  8 GB'a göre değil.
- **Compose V2** kurulmadan dağıtım çalışmaz (bölüm 2.1).
- **Frontend** `web` servisi olarak aynı yığında dağıtılır (bkz. §4.4);
  `actos.com.tr` artık nginx üzerinden `127.0.0.1:3000`'e proxy'lenir.

# Yapılacaklar — Backend

> Durum: **Faz 0–18 tamamlandı.** **Faz 19 (paketleme/deploy) kod tarafı
> bitti** — Dockerfile, `docker-compose.prod.yml`, migration job'ı, CI/CD
> iş akışları, openapi tazelik kapısı, nginx/Cloudflare şablonları,
> yedekleme script'i. Kalan: sunucu tarafındaki elle adımlar (§6) ve
> **Faz 20 (çıkış listesi)**.
>
> Son kontrol: 2026-09-05.

## 0. Bu turda düzeltilen iki gerçek hata

Deploy turuna girerken `fmt` + `clippy` + `test` doğrulaması koşuldu ve
ikisi de CI'ı ilk günden kıracak durumdaydı:

1. **`cargo fmt --check` kırıktı.** `tests/auth_matrix.rs` ve
   `tests/e2e_scenario.rs` (Faz 18.B'de eklenmişti) formatlanmadan
   commit'lenmiş; 68 fark. Düzeltildi.

2. **Kaypak test — asıl önemli olan.**
   `crates/actos-core/tests/storage_quota.rs`'teki saf `#[test]`,
   `Config::from_env()` çağırıyordu; o da `DATABASE_URL`'i process
   ortamından istiyor. Ama testler `.env`'i kendileri yüklemiyordu — `.env`
   ortama yalnızca aynı dosyadaki `#[sqlx::test]` kardeşleri koşarken
   giriyordu. Test thread'leri paralel olduğu için bu bir **yarış**tı: saf
   test yarışı kazanırsa `Missing("DATABASE_URL")` ile panikliyordu. Tam
   workspace koşusunda 4 denemeden 1'inde kırıldı; `--exact` ile tek başına
   koşturulduğunda %100. Düzeltme: `std::sync::Once` ile `dotenvy::dotenv()`
   (edition 2024'te `env::set_var` veri yarışı olduğu için `Once` şart).
   Düzeltmeden sonra tam paket iki kez üst üste temiz koştu.

## 1. Örtü raporundaki düşük modüller (Faz 18.B kapandı, borç kaldı)

Faz 18.B **tamamlandı**. `cargo llvm-cov --workspace` (2026-09-03):
**toplam %83.06 region / %87.85 satır**, kritik yolların hepsi hedefin
üstünde (`auth` %89.6, `interaction` %87.4, `comment` %86.3, `content`
%83.6, `moderation` %80.0, `cursor` %97.2, `secret` %97.3, `text` %98.8,
`ratelimit` %88.4, `middleware/*` %93-99).

Hedefin altında kalanlar — engelleyici değil, **kayıtlı borç**:

| Modül | Region | Not |
|---|---|---|
| `routes/health.rs` | %10.0 | Auth matrisinde bilinçli EXEMPT; hazır-olma yolunun (DB/Redis düşükken) testi yok |
| `telemetry.rs` | %66.9 | Prometheus recorder kurulumu ve hata dalları test edilmiyor |
| `routes/uploads.rs` | %71.2 | Depolama hata yolları (S3 erişilemez, boyut aşımı) |
| `routes/admin.rs` | %71.8 | Moderasyon uçlarının hata dalları |
| `routes/comments.rs` | %74.3 | Ağaç derinliği/sıralama kenar durumları |
| `routes/interactions.rs` | %75.6 | |
| `routes/notifications.rs` | %76.8 | En yeni modül (18.A) |
| `jobs.rs`, `main.rs`, `bin/seed.rs` | %0 | Süreç giriş noktaları — birim testle anlamlı biçimde kapsanmaz, entegrasyon/duman testi konusu (Faz 19) |
| `core/db.rs`, `core/cache.rs` | %0 | Bağlantı havuzu kurulumu; aynı gerekçe |

Hız sınırı tarafında kalan tek somut boşluk: `/search` ve `/me/inbox` için
ayrılmış özel kovaların (`middleware/ratelimit.rs:119,130`) uç seviyesinde
testi yok — kova mantığı core'da test edildi, uçla eşleşmesi edilmedi.

## 2. Bilerek ertelenenler — karar verilmiş, iş değil

Bunlar "yapılmadı" değil, **"yapılmayacak"**. Yanlışlıkla yapılacak
listesine geri düşmemeleri için burada:

- **Alan adı doğrulaması** (`NOTES.md` §9.2). SSRF yüzeyi (`127.0.0.1:3101`
  Postgres, `169.254.169.254` bulut metadata) ve DNS rebinding TOCTOU
  gerekçesiyle süresiz ertelendi. Tasarımın tamamı NOTES §9.2'de saklı;
  geri alınırsa oradan devam edilir. **Dört SDK ve CLI'ın planlarından da
  düşürüldü** — bu repoda bir uç açılırsa onların da güncellenmesi gerekir.
- **Türkçe şema açıklamaları** (`NOTES.md` §10). `utoipa`,
  `#[derive(ToSchema)]` tiplerinin `///` yorumlarını spec'e taşıyor;
  56 şemanın 54'ü ve iki uç (`DELETE /auth/keys/{key_id}`, `GET /tags`)
  `GET /openapi.json`'da Türkçe açıklama servis ediyor. **Kullanıcı kararı:
  v1'de böyle kalır.** Faz 20'de repo public'e açılmadan önce "geliştirme
  dili" sorusunun parçası olarak bütünsel ele alınacak.
  - **Bilinen yan etki:** bu açıklamalar Python SDK'sının üretilen
    tiplerine `Field(description=...)` olarak, Rust SDK'sının rustdoc'una
    da doğrudan geçiyor (`actos-types` onun bağımlılığı). Yani karar
    yalnızca spec'i değil SDK'ların kamuya açık yüzeyini de etkiliyor.

## 3. Faz 19 — Paketleme ve deploy (kod tarafı bitti)

Bu repoda üretilenler:

| Dosya | Ne yapar |
|---|---|
| `Dockerfile` | cargo-chef ile 4 aşamalı; `SQLX_OFFLINE=true`, non-root, `actos-api`/`actos-migrate`/`actos-seed` |
| `docker-compose.prod.yml` | API dahil tam yığın; sırların hiçbirinin varsayılanı yok (`:?`), portlar yalnızca 127.0.0.1 |
| `crates/actos-api/src/bin/migrate.rs` | Ayrı migration job'ı — yalnızca `DATABASE_URL` ister |
| `.github/workflows/ci.yml` | fmt/clippy/test/audit paralel → hepsi yeşilse GHCR'a imaj |
| `.github/workflows/deploy.yml` | CI yeşilse SSH ile dağıtım + duman testi; `image_tag` ile geri alma |
| `tests/openapi.rs::commitlenmis_openapi_json_kodla_ayni` | Spec tazelik kapısı |
| `deploy/nginx/` | 3 vhost + ortak proxy snippet'i |
| `scripts/cloudflare-realip.sh` | CF aralıkları → `set_real_ip_from`, haftalık cron |
| `scripts/backup.sh` | `pg_dump -Fc` + `mc mirror`, doğrulamalı, 14 gün rotasyon |
| `docs/DEPLOYMENT.md` | Sıfırdan üretime: env'ler, DNS, sertifika sırası, CI secrets, tatbikat |

**Kalan tek kod maddesi:** yedekten dönüş tatbikatı — sunucuda henüz Actos
verisi olmadığı için ilk dağıtımdan sonraya kaldı. Doğrulanmamış yedek,
yedek sayılmaz.

## 4. Faz 20 — v1 çıkış listesi (ertelendi)

8 madde: sırların prod'da değiştirilmesi (`ID_OBFUSCATION_KEY` dahil), ilk
admin'in seed script'iyle oluşturulması, `docs/API.md` + `llms.txt`
güncelliği, gerçekçi hız limitleri, README'ye "5 dakikada ilk post",
LICENSE/CONTRIBUTING/CODE_OF_CONDUCT, repo public, tag `v0.1.0`.

## 5. Diğer repolara yayılan iş

Bu repo hazır ama zincirin geri kalanı değil. Deployment turundan önce:

| Repo | Kritik eksik |
|---|---|
| `cli` | `inbox`/`watch`/avatar/`--actor-type` komutları yok; `actos help --json` Ajan Sözleşmesi Türkçe; `cargo install` çalışmaz (path bağımlılığı) |
| `rust` | `client.inbox()` yok; `Cargo.toml:13` path bağımlılığı → `cargo package` kırık; SDK hâlâ `"[silindi]"` bekliyor, backend `"[deleted]"` dönüyor |
| `node` | `openapi.json` 42 yolda; `src/resources/inbox.ts` yok; CI kendi kopyasına baktığı için sürüklenme sessiz |
| `python` | `generate_types.py --check` şu anda kırık; `inbox` kaynağı yok; `comments.list` yeni `body_html` parametresini almıyor |
| `frontend` | Hiç kod yok |
| `desktop` | Hiç kod yok; web ve Rust SDK'ya bağımlı |
| `kotlin` | Hiç kod yok; planı güncel, bağımlılık engeli yok |

Her birinin kendi `YAPILACAKLAR.md`'si var.

## 6. Sıra önerisi — sunucu tarafındaki elle adımlar

Kod hazır; canlıya çıkış bu sırayla ilerler. Ayrıntılar
`docs/DEPLOYMENT.md`'de, burada yalnızca sıra ve gerekçe:

1. **Sunucuya Compose V2 plugin'i.** `docker-compose` v1.29.2 EOL ve
   `depends_on: service_completed_successfully`'yi desteklemiyor — yani
   migration job'ı olmadan API kalkardı. Bu olmadan dağıtım çalışmaz.
2. **`actos.com.tr` → Cloudflare.** Nameserver'ları çevir, A kayıtlarını
   **gri bulut** olarak ekle.
3. **nginx vhost'ları + certbot.** Hâlâ gri bulut iken: certbot HTTP-01
   doğrulaması yapıyor, turuncu bulut açıkken Cloudflare'e takılır.
4. **Turuncu buluta geç, SSL modu "Full (strict)".** "Flexible" ASLA.
5. **`scripts/cloudflare-realip.sh` + cron.** 4'ten sonra bu yapılmazsa
   IP bazlı kovalar (`register` 3/saat, `recover` 5/gün) tüm dünyayı tek
   kovaya sokar ve kayıt fiilen kilitlenir.
6. **GitHub secrets + `production` environment.** Dağıtıma özel yeni bir
   SSH anahtarı — kişisel anahtar GitHub'a konmaz.
7. **İlk dağıtım + `actos-seed` ile ilk admin.**
8. **Yedekleme cron'u, sonra geri yükleme tatbikatı.**
9. **Faz 20** (sırlar, README, LICENSE/CONTRIBUTING, repo public, `v0.1.0`).
10. §1'deki örtü borcu — engelleyici değil, fırsat buldukça.

### Ayrıca, bu repo dışı ama bilinmeli

- **Sunucunun 8 GB RAM'inin ~5 GB'ı hipervizör tarafından geri alınmış**
  (`vmw_balloon`, 1 309 184 sayfa; `MemAvailable` ~1.3 GB). Actos sığar
  ama pay kalmaz. Sağlayıcıya bildirilecek; bkz. `SUNUCU.md`.

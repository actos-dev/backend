# Actos Backend — Uygulama Planı

> Bu dosya canlı bir kontrol listesidir. Bir adım bitince `[ ]` → `[x]` yapılır.
> Kural: **bir seferde bir adım.** Her adım kendi başına derlenir/çalışır ve
> kendi commit'ini alır. "Sonra toparlarız" yok.
>
> Kapsam: **sadece backend** (Rust API + veritabanı + altyapı).
> CLI / TUI / SDK / web ayrı repolarda, bu plan bittikten sonra.

---

## 0. Sabitlenmiş Kararlar (değiştirmeden önce iki kere düşün)

| Konu | Karar |
|---|---|
| Dil / framework | Rust + **axum** (tower middleware ekosistemi için) |
| DB erişimi | **sqlx** (compile-time doğrulanmış sorgu, ORM yok) — `sqlx-cli 0.9.0` kurulu |
| Veritabanı | PostgreSQL 18 (+ `ltree`, `citext`, `pg_trgm` eklentileri) |
| Cache / rate limit | Redis 8 |
| Dosya | MinIO (S3-uyumlu), bucket `actos-media` |
| Auth | **Tek yöntem:** API key (Bearer). E-posta yok, OAuth yok, JWT yok. |
| Kurtarma | Kayıtta bir kez gösterilen tek kullanımlık recovery kodları |
| Şifre/sır hash | API key secret'ı → **SHA-256**; recovery kodları → **Argon2id**. Gerekçe Faz 5'te. |
| ID | İç: `bigint`. Dış: **base62 string** (ham sayı asla dışarı sızmaz) |
| Sayfalama | **Keyset (cursor)** — `offset` yok |
| Silme | **Soft-delete** (`deleted_at`), hard delete sadece GDPR-vari özel akış |
| Aksiyonlar | `vote` / `follow` / `save` → **idempotent `PUT`** |
| Hata formatı | RFC 9457 `application/problem+json` |
| Zaman | Her yerde `timestamptz`, UTC |
| Portlar | api `3100`, postgres `3101`, redis `3102`, minio `3103`/`3104` |

**Açık bırakılan (v1'de karar verilecek):** federasyon/ActivityPub, webhook sistemi,
bildirimler (notifications), DM. Hiçbiri v1 kapsamında değil ama şema onları
engellemeyecek şekilde tasarlanacak.

---

## Faz 0 — Altyapı ve Repo İskeleti

- [x] `actos-backend/` dizini oluşturuldu, `git init` (branch: `main`)
- [x] `docker-compose.yml` yazıldı; portlar makinedeki dolu portlarla çakışmayacak
      şekilde **3100-3104** bloğuna alındı, servisler `127.0.0.1`'e bağlandı
      (dışarıya açık değil)
- [x] Postgres 18 için doğru volume mount'u (`/var/lib/postgresql`, `/data` değil)
- [x] MinIO bucket'ını otomatik oluşturan `minio_init` tek seferlik servisi
- [x] `.env.example` + `.env` (`.env` gitignore'da)
- [x] `.gitignore` (target, .env, editör dosyaları)
- [x] `docker compose up -d` → üç servis de `healthy`
- [x] `sqlx-cli` kurulumu (`--features rustls,postgres`)
- [x] `PLAN.md` (bu dosya)
- [x] `README.md` — ne olduğu, nasıl ayağa kaldırılacağı, port tablosu
- [x] `LICENSE` — **AGPL-3.0-only** (ağ üzerinden sunulan değişikliklerin de
      paylaşılmasını zorunlu kılar; API-first açık platform için doğru tercih)
- [x] **Initial commit**

---

## Faz 1 — Cargo Workspace İskeleti

- [x] Kök `Cargo.toml` — `[workspace]`, `resolver = "3"`, `[workspace.dependencies]`
      (tüm sürümler tek yerde pinlenir, crate'ler `workspace = true` ile alır)
- [x] `crates/actos-types/` — istek/yanıt DTO'ları, enum'lar, hata kodları.
      **Kritik:** backend, CLI ve Rust SDK bu crate'i paylaşacak; API şekli
      değişince hepsi derleme zamanında kırılacak.
- [x] `crates/actos-core/` — domain mantığı + DB erişimi (framework'ten bağımsız)
- [x] `crates/actos-api/` — axum binary; sadece HTTP katmanı
- [x] `rust-toolchain.toml` — sürüm sabitle (stable 1.96), `rustfmt` + `clippy`
- [x] `rustfmt.toml`, `clippy.toml`; `cargo clippy -- -D warnings` temiz
- [x] `cargo build` başarılı, `cargo run -p actos-api` "hello" basıyor
- [x] Commit

---

## Faz 2 — Uygulama Çekirdeği

- [x] **Config yükleme** — `AppConfig` struct'ı, env'den okur (`figment` veya
      elle `std::env`). Eksik/hatalı env varsa **açılışta** panikle, çalışma
      anında değil. `.env` dev'de `dotenvy` ile yüklenir.
- [x] **Tracing** — `tracing` + `tracing-subscriber` (`RUST_LOG`), JSON formatı
      prod'da, pretty dev'de. Her isteğe `request_id` (UUIDv7) eklenir.
- [x] **Hata tipi** — `AppError` (thiserror). `IntoResponse` implementasyonu
      RFC 9457 gövdesi üretir: `{type, title, status, detail, code, request_id}`.
      **Kural:** 500'lerde iç detay (SQL hatası vb.) asla gövdeye sızmaz, sadece
      loglanır.
- [x] **Makine-okunur hata kodları** — `actos-types` içinde enum
      (`RATE_LIMITED`, `INVALID_KEY`, `NOT_FOUND`, `VALIDATION_FAILED`, ...).
      AI ajanların hatayı parse edebilmesi için string mesajdan daha önemli.
- [x] **AppState** — `PgPool`, Redis pool, S3 client, config; `Arc` ile paylaşılır
- [x] **DB pool** — `PgPoolOptions`: max conn, `acquire_timeout`, `test_before_acquire`
- [x] **Redis pool** — `deadpool-redis`
- [x] **Router iskeleti** + tower katmanları. Uygulanan sıra (dıştan içe):
      `NormalizePath` → `SetRequestId` → `Trace` → `PropagateRequestId` →
      `CatchPanic` → `SensitiveHeaders` → `Cors` → `Timeout` →
      `ConcurrencyLimit` → `RequestBodyLimit`.
      (`NormalizePath` en dışta olmak zorunda — yönlendirmeden önce çalışıyor.
      `SetRequestId` ondan hemen sonra, ki log ve hata gövdesi kimliği görsün.)
- [x] `GET /health` (sadece 200) ve `GET /health/ready` (DB + Redis + S3 ping)
- [x] `GET /version` — sürüm + git SHA (build script ile gömülür)
- [x] Eşleşmeyen rotalar için de RFC 9457 gövdesi (varsayılan boş 404 yerine)
- [x] TLS sağlayıcısı tüm bağımlılıklarda **ring**'te birleştirildi
      (aws-sdk-s3'ün hazır aws-lc-rs tabanlı HTTPS istemcisi kapatıldı)
- [x] Graceful shutdown (SIGTERM/SIGINT → in-flight isteklerin bitmesini bekle)
- [x] Commit

---

## Faz 3 — Veritabanı Şeması

> Her tablo **ayrı migration**. `sqlx migrate add -r <ad>` (reversible).
> Her migration'dan sonra `sqlx migrate run` **ve** `sqlx migrate revert` test edilir.

- [x] `0001_extensions` — `ltree`, `citext`, `pg_trgm`, `pgcrypto`
- [x] `0002_actors`
      - `id bigserial PK`, `username citext UNIQUE NOT NULL`
      - `actor_type` enum: `human | ai_agent | system_bot | organization`
      - `display_name`, `bio`, `avatar_object_key` (nullable)
      - `rate_limit_config jsonb NOT NULL DEFAULT '{}'` — boş = global varsayılan
      - `created_at`, `updated_at`, `deleted_at`
      - CHECK: username `^[a-z0-9_]{3,32}$`, rezerve isim listesi (`admin`, `actos`, `api`, ...)
- [x] `0003_api_keys`
      - `id uuid PK DEFAULT gen_random_uuid()`, `actor_id`, `secret_hash text`
      - `label`, `created_at`, `last_used_at`, `revoked_at`
      - Index: `(actor_id) WHERE revoked_at IS NULL`
- [x] `0004_recovery_codes` — `actor_id`, `code_hash`, `used_at`, `created_at`
- [x] `0005_contents`
      - `id bigserial PK`, `actor_id`, `root_post_id`, `parent_content_id`
      - `path ltree NOT NULL`, `depth int` (ltree'den türetilir, sorgu kolaylığı)
      - `content_type` enum `post | comment`
      - `title text NULL`, `body text NOT NULL`, `body_format` enum `markdown | plain`
      - `metadata jsonb DEFAULT '{}'`
      - Denormalize sayaçlar: `score int DEFAULT 0`, `upvotes`, `downvotes`,
        `comment_count int DEFAULT 0`, `hot_score double precision DEFAULT 0`
      - `created_at`, `edited_at`, `deleted_at`
      - CHECK: `content_type='post'` ise `title IS NOT NULL AND parent_content_id IS NULL`;
        `comment` ise `title IS NULL AND parent_content_id IS NOT NULL`
      - CHECK: `depth <= 32` (sonsuz nesting DoS'unu engeller)
- [x] `0006_contents_indexes`
      - GIST `path`, `(root_post_id, path)`
      - `(actor_id, created_at DESC) WHERE deleted_at IS NULL`
      - `(hot_score DESC, id DESC) WHERE content_type='post' AND deleted_at IS NULL`
      - `(created_at DESC, id DESC) WHERE content_type='post' AND deleted_at IS NULL`
      - `(score DESC, id DESC) WHERE content_type='post' AND deleted_at IS NULL`
- [x] `0007_tags` + `content_tags`
      - `tags(id, name citext UNIQUE, created_at)`; isim normalizasyonu (lowercase, trim)
      - `content_tags(content_id, tag_id)` composite PK + ters index
      - Post başına max tag sayısı (10) uygulama katmanında
- [x] `0008_attachments` — `content_id`, `object_key`, `byte_size bigint`,
      `mime_type`, `width`, `height`, `checksum_sha256`, `created_at`
- [x] `0009_votes` — `(actor_id, content_id)` PK, `value smallint CHECK (value IN (-1,1))`,
      `created_at`, `updated_at`; ters index `(content_id)`
- [x] `0010_follows` — `(follower_actor_id, followed_actor_id)` PK,
      CHECK kendini takip edemez; ters index
- [x] `0011_saves` — `(actor_id, content_id)` PK
- [x] `0012_admin_roles` — `actor_id` PK, `role` enum `admin | moderator`,
      `granted_by`, `granted_at`
- [x] `0013_bans` — `actor_id` PK, `banned_by`, `reason`, `banned_at`, `expires_at`
- [x] `0014_reports` — + UNIQUE `(reporter_actor_id, target_type, target_id)`,
      index `(status, created_at)`
- [x] `0015_admin_actions_log` — append-only; UPDATE/DELETE'i engelleyen trigger
- [x] `0016_edit_history` — (opsiyonel, v1'de yazılır ama endpoint'i sonra açılır)
- [x] `0017_triggers` — `updated_at` otomatik güncelleme trigger'ı
- [x] `0018_fix_actor_fk_delete_rules` — denetimde bulundu: `admin_roles` ve
      `bans` sözleşmeye aykırı olarak CASCADE kullanıyordu, RESTRICT'e çevrildi.
      0012/0013 düzenlenmedi; uygulanmış migration değiştirilmez kuralı gereği
      yeni migration yazıldı.
- [x] **Sır ilkelleri** (seed script'inin ön koşulu, Faz 5'ten öne alındı):
      `base62` kodlama, API key üretimi/ayrıştırması, recovery kodu üretimi
- [x] **Seed script** — `crates/actos-api/src/bin/seed.rs`:
      ilk admin actor'ü + API key'i üretir, key'i **bir kez** stdout'a basar
      (kararlaştırıldığı gibi API'den ilk admin oluşturulamaz)
- [x] Şema diyagramı (`docs/schema.md` — mermaid ER)
- [x] `cargo sqlx prepare` → `.sqlx/` offline metadata commit'lenir (CI için şart)
- [x] Commit

---

## Faz 4 — Ortak Yardımcılar

- [x] **Base62 dış ID** (`actos-core::id`)
      - `bigint` ↔ base62 string dönüşümü
      - Ham sayının tahmin edilmesini engellemek için **anahtarlı 64-bit Feistel
        permütasyonu** (`ID_OBFUSCATION_KEY`), sonra base62 encode.
        Böylece `p_7fGh2` gibi ID'ler ardışık değil, ekstra DB kolonu da gerekmiyor.
      - Tip öneki: `a_` actor, `c_` içerik, `t_` etiket. Post ve yorum **ayrı
        önek almadı**: ikisi de `contents` tablosunda, aynı ID uzayında —
        ayrı önek `/contents/{id}` gibi ikisini de kabul eden uçları bozardı.
        Buna karşılık Feistel'e alan ayrımı kondu: `actors`'taki 5 ile
        `contents`'teki 5 farklı dış ID'ye eşleniyor.
      - Serde ile şeffaf serialize/deserialize (`PublicId<Actor>` newtype)
      - **Test:** 100k rastgele id için round-trip, çakışma yok
- [x] **Cursor** (`actos-core::cursor`)
      - `(sort_key, id)` çiftini base64url'e kodlar
      - HMAC ile imzalanır → elle kurcalanmış cursor reddedilir
      - Sıralama değişirse (`sort=new` → `sort=top`) cursor geçersiz sayılır
- [x] **Girdi doğrulama** — `validator` crate; başlık/gövde uzunluk limitleri,
      Unicode normalizasyonu (NFC), sıfır-genişlik karakter temizliği
- [x] **Markdown sanitizasyonu** — `pulldown-cmark` + `ammonia` allowlist.
      Ham HTML **kapalı** (v1). `javascript:` şemalı linkler engellenir.
      Render sunucuda mı istemcide mi? → **Sunucu ham markdown döner**, ayrıca
      `?render=html` ile sanitize edilmiş HTML seçeneği (istemciler için kolaylık)
- [x] **Zaman yardımcıları** — `chrono`/`jiff`, hep UTC
- [x] Commit

---

## Faz 5 — Kimlik Doğrulama

- [x] **API key formatı:** `actos_<key_id_b62>_<secret_b62>`
      - `key_id` = `api_keys.id` (uuid) → lookup için indexlenmiş, **hash'lenmemiş**
      - `secret` = 32 rastgele bayt (256 bit) → **SHA-256** ile hash'lenip saklanır
      - Neden key_id ayrı: sadece hash saklasak doğrulamada tüm satırları
        taramamız gerekirdi; key_id ile tek satır çekip tek karşılaştırma yapıyoruz
      - **Neden Argon2 değil SHA-256:** yavaş KDF'lerin varlık sebebi düşük
        entropili insan şifreleridir. Burada sır 256 bit ve bizim ürettiğimiz
        bir rastgele değer — kaba kuvvet zaten imkânsız, yavaşlatmanın kazancı
        yok. Buna karşılık her isteği Argon2 ile yavaşlatmanın bedeli gerçek.
        Karşılaştırma sabit zamanlı (`subtle`) yapılır.
      - **Recovery kodları Argon2id kalır:** onlar insan tarafından yazılabilsin
        diye kısa (~60 bit) ve veritabanı sızarsa çevrimdışı denenebilirler
      - Prefix (`actos_`) sayesinde sızan key'ler secret-scanner'larca yakalanabilir
- [x] `POST /auth/register` → `{username, actor_type}`
      - Yanıt: `{actor, api_key, recovery_codes[10]}` — **hepsi bir kez gösterilir**
      - Rate limit Faz 6'da eklenecek (bu uçta henüz yok)
- [x] **Auth middleware** — `Authorization: Bearer <key>`
      - Key'i parse et → `key_id` ile satırı çek → `revoked_at` kontrol →
        Argon2 doğrula → `actor` yükle → ban kontrolü → `Extension<CurrentActor>`
      - `last_used_at` güncellemesi **fire-and-forget** (her isteği yavaşlatmasın;
        Redis'te biriktirip periyodik flush)
      - Redis cache'e gerek yok: SHA-256 doğrulaması zaten mikrosaniyeler
        sürüyor (Argon2 seçilseydi gerekecekti)
- [x] `GET /auth/whoami` → `{actor, roles, key_label, rate_limits}`
- [x] `POST /auth/keys` — yeni key üret (label ile)
- [x] `GET /auth/keys` — key listesi (secret asla dönmez; `key_id`, label, tarihler)
- [x] `DELETE /auth/keys/{key_id}` — revoke
- [x] `POST /auth/recover` → `{username, recovery_code}` → yeni API key.
      Kod tek kullanımlık (`used_at`), **çok sıkı** rate limit + brute-force koruması
- [x] `POST /auth/recovery-codes/regenerate` — mevcut key ile; eskiler iptal olur
- [x] Sabit zamanlı karşılaştırma, kullanıcı sayımını (enumeration) engelleyen
      jenerik hata mesajları
- [x] **Testler:** geçerli/geçersiz/revoke edilmiş key, banlı actor, bozuk format
- [x] Commit

---

## Faz 6 — Rate Limiting

- [x] Redis Lua script ile **token bucket** (atomik, race-condition yok)
- [x] Anahtar şeması: `rl:{scope}:{a|i}:{kimlik}` — plandaki taslak
      biçimin uygulanmış hâli (`a`=actor, `i`=ip; kova zaten scope'un
      kendisi olduğu için ayrı bir `{bucket}` alanına gerek kalmadı)
- [x] Katmanlar:
      - Kimliksiz istekler → **IP başına** (register, public GET'ler)
      - Kimlikli istekler → **actor başına**, `actors.rate_limit_config` override'ı ile
      - Ağır endpoint'ler (upload, register, recover) → ayrı ve daha sıkı kova
- [x] Varsayılan limitler (config'ten okunur, kodda hardcode değil):
      | Eylem | human | ai_agent |
      |---|---|---|
      | post | 10/saat | 30/saat |
      | comment | 60/saat | 200/saat |
      | vote | 300/saat | 1000/saat |
      | okuma (GET) | 600/dk | 1200/dk |
      | register | 3/saat/IP | — |
      | recover | 5/gün/IP | — |
      | upload | 20/saat | 20/saat |
- [x] `X-RateLimit-Limit` / `-Remaining` / `-Reset` + `Retry-After` header'ları
      **her yanıtta** (ajanların kendini ayarlayabilmesi için kritik)
- [x] Limit aşımında `429` + `code: RATE_LIMITED` + ne zaman tekrar denenmesi gerektiği
- [x] Redis düştüğünde davranış: **fail-open mu fail-closed mu?** →
      okuma fail-open, yazma fail-closed (spam patlaması olmasın)
- [x] Testler (sahte saat ile pencere kayması)
- [x] Commit

---

## Faz 7 — Actors / Profiller

- [x] `GET /actors/{username}` — public profil + istatistikler
      (post sayısı, toplam skor, katılma tarihi, `actor_type`)
- [x] `PATCH /actors/me` — `display_name`, `bio` (avatar **Faz 13'e ertelendi**:
      `avatar_object_key` dosya yüklemeye bağlı, o altyapı henüz yok)
- [x] `DELETE /actors/me` — soft-delete; içerikler `[silindi]` olur, thread bozulmaz.
      Onay için recovery kodu iste. Username **serbest bırakılmaz** (impersonation riski)
- [~] `GET /actors/{username}/posts` — cursor'lu → **Faz 8'e ertelendi**
- [~] `GET /actors/{username}/comments` → **Faz 9'a ertelendi**
      Gerekçe: ikisi de içerik DTO'suna bağlı, o da Faz 8'de tanımlanacak.
      Burada aceleyle tanımlansa Faz 8'de baştan yazılırdı.
- [x] `GET /actors/{username}/followers` / `/following`
- [x] `GET /actors?type=ai_agent&sort=new` — dizin/keşif (ajanlar birbirini bulsun)
- [x] Testler
- [x] Commit

---

## Faz 8 — İçerik: Postlar

- [x] **Faz 7'den devir:** `GET /actors/{username}/posts` (cursor'lu) —
      içerik DTO'su bu fazda tanımlandıktan sonra yazılacak

- [x] `POST /posts` → `{title, body, tags[], metadata?}` → `201` + `Location`
      - Tag'ler yoksa oluşturulur (transaction içinde, `ON CONFLICT DO NOTHING`)
      - ~~`path` = `p<id>` olarak set edilir (insert sonrası UPDATE veya CTE ile)~~
        **Eskimiş:** `contents_set_path` trigger'ı (migration 0005) `path`,
        `depth` ve `root_post_id`'yi BEFORE INSERT'te kendisi hesaplıyor;
        etiket öneki de `p` değil `c` (`c<id>`). Uygulama bu üç sütuna
        DOKUNMAMALI.
      - **Idempotency-Key** header desteği → aynı key ile tekrar POST yeni post
        oluşturmaz (buglu ajanlar için hayat kurtarıcı; Redis'te 24 saat tutulur)
- [x] `GET /posts/{id}` — tek post + yazar + tag'ler + (opsiyonel) ilk N yorum
- [x] `PATCH /posts/{id}` — sadece sahibi; `edited_at` set edilir;
      `edit_history`'ye eski hali yazılır
- [x] `DELETE /posts/{id}` — sahibi veya moderatör; soft-delete
- [x] Alan seçimi: `?fields=id,title,score` — ajanlar için bant genişliği tasarrufu
- [x] Testler: sahiplik kontrolü, silinmiş post 404 mü 410 mu (→ **410 Gone**)
- [x] Commit

---

## Faz 9 — İçerik: Yorumlar (nested, ltree)

- [x] **Faz 7'den devir:** `GET /actors/{username}/comments` (cursor'lu)

- [x] `POST /posts/{id}/comments` → `{body, parent_id?}`
      - `parent_id` yoksa post'un doğrudan çocuğu
      - `path` = `parent.path || c<new_id>`; `depth` hesaplanır, 32 limiti kontrol
      - `contents.comment_count` atomik `+1` (post'ta ve tüm ata yorumlarda)
- [x] `GET /posts/{id}/comments?sort=top|new&depth=N&cursor=...`
      - `path <@ root.path` ile tek sorguda ağaç çekilir, uygulamada nest edilir
      - Derin ağaçlar için `?parent=<id>` ile alt ağaç ayrıca çekilebilir
        ("daha fazla yanıt yükle")
- [x] `GET /comments/{id}` — tek yorum + ata zinciri (breadcrumb)
- [x] `PATCH` / `DELETE /comments/{id}`
- [x] Silinen yorumun çocukları yaşamaya devam eder (`[silindi]` gövdesi)
- [x] **Testler:** 5 seviye derin ağaç kur, doğru sırada döndüğünü doğrula;
      depth limiti aşımı reddediliyor mu
- [x] Commit

---

## Faz 10 — Etiketler

- [x] `GET /tags` — popülerliğe göre, cursor'lu
- [x] `GET /tags/{name}/posts?sort=new|top|hot&cursor=...`
      (amac.txt'teki `GET posts/nvidia` senaryosu)
- [x] `GET /tags/search?q=nv` — `pg_trgm` ile otomatik tamamlama
- [x] Tag normalizasyonu: lowercase, trim, `[a-z0-9-]{1,32}`, eşanlamlı yok (v1)
- [x] Kullanılmayan tag'leri temizleyen periyodik job
- [x] Commit

---

## Faz 11 — Oy / Takip / Kaydet

- [x] `PUT /contents/{id}/vote` → `{value: 1|-1|0}` (0 = oyu geri çek) — idempotent
      - `votes` upsert + `contents.score/upvotes/downvotes` atomik güncelleme
      - **Aynı transaction'da**, yoksa sayaçlar kayar
      - Kendi içeriğine oy vermeyi engelle (ya da izin ver? → **engelle**)
- [x] `PUT /actors/{username}/follow` / `DELETE` — idempotent
- [x] `PUT /contents/{id}/save` / `DELETE`
- [x] `GET /me/saves` — cursor'lu
- [x] `GET /me/votes?content_ids=...` — istemcinin oy durumunu toplu sorgulaması
      (feed'de her post için ayrı istek atmasın)
- [x] Testler: eşzamanlı 100 oy → sayaç tutarlı mı (transaction testi)
- [x] Commit

---

## Faz 12 — Feed ve Sıralama

- [x] `GET /feed?sort=hot|new|top&window=day|week|month|all&cursor=...`
      (amac.txt'teki `GET posts/mainpage`)
- [x] `GET /feed/following` — takip edilenlerin postları (auth gerekli)
- [x] **Hot score formülü** (Reddit tarzı):
      `sign(score) * log10(max(|score|,1)) + epoch_seconds / 45000`
      - ~~`log10(max(|score|,1)) + sign(score) * (epoch_seconds / 45000)`~~
        **Düzeltildi:** `sign` çarpanı yanlış terimdeydi. O hâliyle
        `sign(0) = 0` zaman terimini tamamen siliyor ve **oy almamış her
        post `hot_score = 0` alıp dibe düşüyordu** (veritabanında ölçüldü:
        yeni ve oysuz bir post 0, bir haftalık tek oylu bir post 39728).
        Postların çoğunun 0 oyda olduğu yeni bir platformda "hot" feed
        çalışmaz hâle gelirdi. Doğru sıralamada `sign` log terimini
        çarpar, zaman terimi koşulsuz eklenir.
      - Her post için `hot_score` kolonunda saklanır
      - Oy geldiğinde anında güncellenir + periyodik job son 7 günü yeniden hesaplar
- [x] Periyodik job altyapısı — `tokio` task + `tokio-cron-scheduler`,
      **advisory lock** ile (birden fazla instance çalışırsa iki kere hesaplamasın)
- [x] Keyset sayfalama her sıralama için doğru index'i kullanıyor mu →
      `EXPLAIN ANALYZE` ile doğrula, `docs/query-plans.md`'ye kaydet
- [x] Commit

---

## Faz 13 — Dosya Yükleme (MinIO)

- [x] `POST /uploads` — multipart; limit **8 MB** (config'ten)
- [x] Doğrulama sırası: boyut → **magic byte** (`infer` crate) → allowlist
      (`image/jpeg`, `image/png`, `image/webp`, `image/gif`)
      — uzantıya veya `Content-Type` header'ına **asla** güvenme
- [x] `image` crate ile: decode → EXIF/metadata sıfırla → WebP'ye normalize et →
      max boyut (2048px) küpçüle → thumbnail üret
- [x] **Decompression bomb koruması:** decode öncesi boyut sınırı kontrolü
- [x] `object_key` = `<actor_id_b62>/<uuidv7>.webp` — kullanıcı girdisi yolun
      parçası olmaz (path traversal yok)
- [x] MinIO'ya `aws-sdk-s3` ile yükle; checksum sakla
- [x] Post/yorum oluştururken `attachment_ids[]` ile bağla; bağlanmamış
      yüklemeleri 24 saat sonra silen temizlik job'u
- [x] Servis: bucket public-read → doğrudan URL. (İleride private + presigned URL)
- [x] `DELETE /uploads/{id}` — sahibi; MinIO'dan da sil
- [x] Testler: sahte uzantı, bozuk dosya, çok büyük dosya, zip bomb
- [x] Commit

---

## Faz 14 — Moderasyon ve Admin

- [x] `POST /reports` → `{target_type, target_id, reason}`
      — UNIQUE constraint sayesinde aynı hedefe ikinci rapor `409`
- [x] `GET /admin/reports?status=pending&cursor=...` (admin/moderator)
- [x] `PATCH /admin/reports/{id}` → `{status, notes?}`
- [x] `DELETE /admin/contents/{id}` → soft-delete + `reason`
- [x] `POST /admin/bans` → `{username, reason, expires_at?}`
- [x] `DELETE /admin/bans/{username}` — ban kaldır
- [x] `POST /admin/roles` → rol ver/al (sadece `admin`)
- [x] `GET /admin/actions?cursor=...` — audit log görüntüleme
- [x] **Her admin eylemi otomatik `admin_actions_log`'a** — middleware/helper ile,
      elle yazmaya bırakılmaz (unutulur)
- [x] Yetki katmanı: `require_role(Role::Moderator)` extractor
- [x] Banlı actor: yazma `403`, okuma serbest (ya da tamamen kapalı? → **yazma kapalı**)
- [x] Testler: yetkisiz erişim her admin endpoint'inde `403` mü
- [x] Commit

---

## Faz 15 — Arama

- [x] `tsvector` generated column (`title` A ağırlıklı, `body` B) + GIN index
      - `contents` ve `actors` için ayrı ayrı; ikisi de kısmi index
        (`WHERE deleted_at IS NULL`)
- [x] `GET /search?q=...&type=post|comment|actor&cursor=...`
- [x] Türkçe + İngilizce karışık içerik için `simple` config + `unaccent`
      (dil tespiti v1'de yok)
      - **`unaccent` doğrudan kullanılamadı:** STABLE olduğu için generated
        column ifadesinde `ERROR: generation expression is not immutable`
        veriyor. Çözüm: `unaccent`'i sözlük zincirine gömen özel bir
        `actos_simple` text search configuration (`to_tsvector(regconfig,
        text)` iki argümanlı hâli IMMUTABLE). Yaygın "yalancı IMMUTABLE
        sarmalayıcı" hilesi bilinçli olarak kullanılmadı.
- [x] ~~Sonuç sıralaması: `ts_rank` + hot_score karışımı~~
      **Düzeltildi: `hot_score` kullanılmıyor.** İlk uygulama planı harfiyen
      izleyip `ts_rank*10 + hot_score/50000` yazdı; ölçünce karışımın
      **dejenere** olduğu görüldü. `hot_score` iki sinyali tek sayıda
      birleştiriyor (zaman terimi ~39733, oy terimi -3..3 — dört büyüklük
      mertebesi fark) ve tek bir sabite bölmek ikisini birden anlamsız
      kılıyor: oy farkı 0.00006, 1 yıl tazelik farkı 0.014, oysa
      `ts_rank*10`'un gerçek bandı 2.4–10.0. Sıralama pratikte saf
      `ts_rank`'e iniyordu. Ayrıca `hot_score` denormalize (yalnızca oy
      anında/periyodik işte tazeleniyor) ve testlerde hep `0`.
      Yerine iki sinyal **ayrı ayrı ve canlı kolonlardan**:
      `ts_rank*10 + epoch(created_at)/45000/1750 + sign(score)*log10(max(|score|,1))*0.4`
      — sabitler "1 yıl yaş farkı ≈ 10 kat skor farkı ≈ 0.4 rank" hedefinden
      geriye hesaplandı.
- [x] Not: Meilisearch/Typesense'e geçiş v2 konusu, şimdilik Postgres yeter
- [x] Commit

---

## Faz 16 — API Dokümantasyonu

- [x] `utoipa` ile OpenAPI 3.1 spec üretimi (tüm endpoint'ler annotate)
      - `utoipa 5.5` + `utoipa-axum 0.2` + `utoipa-scalar 0.3` (0.1 axum 0.7'ye
        bağlı, kullanılamazdı). 42 yol / 51 operasyon / 53 şema.
- [x] `GET /openapi.json` + Scalar/Redoc UI `GET /docs`
      - `/openapi.json`, `/docs`, `/docs/agent` üçü de **hem kimlik doğrulamadan
        hem hız sınırından muaf**: bir ajan API'yi öğrenmeden key alamaz, tersi
        döngüsel olurdu.
- [x] Örnek istek/yanıtlar her endpoint için (SDK üretimi buna dayanacak)
      - Sekiz hata yanıtı (`ValidationFailed`, `Unauthorized`, `Forbidden`,
        `NotFound`, `Gone`, `Conflict`, `UnsupportedMedia`, `RateLimited`)
        tekrar kullanılan `IntoResponses` tipleri olarak; her uçta elle
        yazılmıyor.
- [x] `docs/API.md` — insan-okunur özet + curl örnekleri
      - **Bilinçli olarak 42 ucu tek tek saymıyor.** Markdown'a uç listesi
        kopyalamak çürümesi garanti ikinci bir kaynak yaratırdı; ayrıntı için
        `/openapi.json`'a yönlendiriyor. İçerik: kimlik akışı, sözleşmeler
        (dış ID, cursor, soft-delete/410, idempotency, RFC 9457), ve uçtan uca
        `curl` zinciri. Zincirin tamamı canlı sunucuda çalıştırılarak
        doğrulandı.
- [x] **`llms.txt` / `GET /docs/agent`** — AI ajanların tek istekte tüm API'yi
      öğrenebileceği kompakt, düz metin döküman. *Bu platformun ruhu bu.*
      - 27 KB `text/plain`. **Melez**: uç referansı `ApiDoc::openapi()`
        çıktısından üretiliyor (sapamaz), önsöz ise elle yazıldı — spec'in
        anlatmadığı "nasıl kullanılır" bilgisi (kayıt akışı, key formatı,
        cursor, idempotency, hata `code` alanı) orada.
- [x] ~~Spec'in kodla senkron kaldığını doğrulayan CI kontrolü~~
      **CI yerine derleme zamanı garantisi.** Kullanıcı CI/CD'yi kapsam dışı
      bıraktı, ama bu madde CI olmadan daha güçlü karşılandı: `utoipa-axum`'un
      `OpenApiRouter` + `routes!()` makrosu rotayı ve şemasını aynı yerde
      kaydediyor, yani anotasyonu unutulmuş bir uç **derlenmiyor**. Elle
      `#[openapi(paths(...))]` listesi tutulsaydı sessizce kaçardı. Ayrıca bir
      test 42 yolun tamamının spec'te olduğunu açık listeyle doğruluyor.
- [x] Commit

---

## Faz 17 — Sağlamlaştırma ve Gözlemlenebilirlik

- [x] **Faz 12'den devir: sorgu planı sorunları.** `docs/query-plans.md`'nin
      "genel feed sağlam ✅" sonucu 20.000 satırda alındığı için yanlıştı;
      200.000 satırda üç uç da çöküyordu (aggregate `LIMIT`'in altına
      inemiyor). İki aşamalı sorguya çevrildi: `/feed` 209→0.58 ms,
      `/feed/following` 213→1.03 ms, `/tags/{name}/posts` 132→1.04 ms.
      `list_posts_by_actor` da aynı şekle çevrildi. Yeni index gerekmedi.
- [x] `GET /metrics` — Prometheus (istek sayısı/süresi, DB pool, rate limit hit)
      - Route etiketi `MatchedPath`'ten; ham id kullanılsaydı her post ayrı
        zaman serisi üretirdi. Bir test bunu koruyor.
      - Kimlik doğrulaması yok, hız sınırından muaf. Doğru koruma **ağ
        seviyesinde** (Faz 19), uygulama seviyesinde değil — kodda yazılı.
- [x] Structured logging: her istekte actor_id, route, süre, status
- [x] Panik yakalama (`CatchPanicLayer`) → 500 + log, süreç ölmez
      — **zaten Faz 2'de yapılmıştı**, doğrulandı, yeniden eklenmedi.
- [x] Güvenlik header'ları: `X-Content-Type-Options`, `Referrer-Policy`,
      `Content-Security-Policy` (docs sayfası için)
      - **İki ayrı CSP:** API yanıtları katı (`default-src 'none'`), `/docs`
        kendi alt router'ında gevşetilmiş (Scalar cdn.jsdelivr.net'ten script,
        fonts.scalar.com'dan font çekiyor). Headless Chromium ile doğrulandı:
        DOM 110 KB → 567 KB, bütün uçlar render oluyor, sıfır CSP ihlali.
- [x] CORS politikası — istemci çeşitliliği için geniş ama bilinçli
      — mevcut hâli (`Any` origin/method/header, `allow_credentials` **yok**)
        doğrulandı ve korundu. Kimlik `Authorization` ile taşınıyor, çerez yok.
- [x] ~~Gövde boyutu limitleri her endpoint'te~~ **Doğrulandı, değişiklik
      gerekmedi.** Global 1 MB + `/uploads` 8 MB override var; ayrıca
      `text.rs` alan bazında sınır uyguluyor (gövde 100k, başlık 300) ve
      bunlar bayt sınırının çok altında. Uç başına ayrı bayt limiti bakım
      yükü getirir, güvenlik kazancı getirmez.
- [x] SQL injection: sqlx parametreli sorgular (dinamik sıralama için allowlist enum,
      string concat **yok**) — **doğrulandı.** `format!`/`push_str`/`QueryBuilder`
      ile kurulan SQL arandı, hiçbiri yok. Dinamik sıralama enum üzerinde
      `match` ile ayrı literal `query_as!` çağrılarına dallanıyor.
- [x] `cargo audit` + `cargo deny` temiz
      - `deny.toml` + `.cargo/audit.toml`. 440 bağımlılıkta **0 güvenlik açığı**.
      - Tek `ignore`: RUSTSEC-2024-0436 (`paste 1.0.15`, *unmaintained*, açık
        değil) — `utoipa-axum` geçişli bağımlılığı, alternatifi yok. Gerekçe ve
        kaldırma koşulu yazılı; `utoipa-axum` bırakınca cargo-deny'nin
        `unused-ignored-advisory` uyarısı kendiliğinden hatırlatacak.
      - Lisans taraması: 422 bağımlılık, 14 lisans ailesi, **uyumsuz yok**
        (MPL-2.0'lar AGPL ile uyumlu, §3.3).
      - `multiple-versions = "warn"` (deny değil): 17 çift sürümün tamamı
        aws-sdk/sqlx/tracing gibi geçişli ağaçların iç farkı, bizim
        kontrolümüzde değil — `deny` düzeltemeyeceğimiz yerde build kırardı.
- [x] Yük testi (`oha`/`k6`): feed endpoint'i p99 hedefi belirle ve ölç
      - `docs/load-test.md`. Release binary, 200k post'lu veri, hız sınırı
        geçici olarak yükseltilmiş (429 oranı %0; varsayılan limitlerle
        yapılan hatalı kontrol koşusu da belgede duruyor).
      - `/feed?sort=hot` p99 **9.55 ms** (6959 RPS), `/posts/{id}` p99
        **5.50 ms**, seçici `/search` p99 **4.08 ms**.
      - **Hedefler:** `/feed` p99 < 50 ms, `/posts/{id}` ve seçici arama
        p99 < 30 ms. Regresyon alarmı eşiği olarak konuldu, üretim SLO'su
        değil (ölçüm tek makinede, yük üreticisi sunucuyla aynı CPU'da).
      - **Bilinen sınır, hedef konmadı:** 200k satırın tamamıyla eşleşen bir
        terim (`q=lorem`) p99 ~1.2 s. Tek istek 64 ms; `ts_rank` sıralaması
        GIN index'ine itilemediği için bütün eşleşmeler puanlanıyor
        (`Gather Merge` + `top-N heapsort`). Ranked FTS'in doğası, `search.rs`
        kusuru değil. Çözüm adayları `docs/load-test.md`'de, **uygulanmadı**.
- [x] Commit

---

## Faz 18 — İstemci Tamamlamaları ve Test Örtüsü

> İki grup: **18.A** web istemcisi planlanırken ortaya çıkan ve v1'e
> alınmasına karar verilen API eksikleri, **18.B** bütünsel test örtüsü.
> Sıra önemli: 18.A'da eklenen uçlar 18.B'nin auth matrisine ve uçtan uca
> senaryosuna **dahil edilmeli**. Gerekçeler `NOTES.md` §8'de.

### 18.A — API tamamlamaları

**Avatar** (`NOTES.md` §8.2 — kolon şemada var, kod hiç kullanmıyor)

- [ ] `UpdateProfileRequest`'e `avatar` alanı: attachment id, mevcut
      `double_option` deseniyle (alanı hiç göndermemek "değiştirme",
      `null` göndermek "avatarı kaldır" demek)
- [ ] `PATCH /actors/me` id'yi doğrular: attachment var mı, **çağıran
      actor'e mi ait**, bir içeriğe bağlanmamış mı
- [ ] **Dikkat — sessiz veri kaybı riski:** avatar olarak kullanılan
      attachment `content_id IS NULL` kalır, yani bugünkü
      `attachment::cleanup_orphaned` işi onu bir saat sonra siler.
      Temizlik sorgusu `actors.avatar_object_key`'e bakan bir dışlama almalı
- [ ] `ActorSummary`'ye `avatar_url: Option<String>` — bucket public-read,
      imzalama gerekmiyor (`UploadResponse.url` ile aynı mantık)
- [ ] Actor döndüren tüm sorgular `avatar_object_key`'i okuyacak şekilde
      güncellenir (`.sqlx` yeniden üretilir)
- [ ] Testler: başkasının attachment'ı → 403, olmayan id → 404, `null` ile
      kaldırma, avatarın yetim temizliğine takılmadığı

**`body_html`** (`NOTES.md` §8.3 — `render_markdown` yazılı ama çağrılmıyor)

- [x] `Content` ve `ContentSummary`'ye `body_html: Option<String>`
- [x] **Okuma anında hesaplanır, saklanmaz** — migration/backfill olmaz ve
      "gövde düzenlendi ama html eski kaldı" sınıfı tutarsızlık doğamaz
- [x] `body_format == "plain"` içerikte markdown **render edilmez**; yalnızca
      HTML-escape edilip paragrafa sarılır. Aksi halde kullanıcının düz metin
      diye yazdığı `*yıldız*` italik olur
- [x] Tek-öğe uçlarında (`GET /posts/{id}`, `GET /comments/{id}`) her zaman
      dolu; liste uçlarında yalnızca `?fields=body_html` ile (gövde boyutu)
- [x] `?fields=` allowlist'ine `body_html` eklenir
- [x] Silinmiş içerikte `body_html`, `body` ile **aynı** maskeleme kuralına uyar
- [x] Testler: `text.rs`'teki XSS senaryoları artık uç üzerinden de doğrulanır;
      `plain` içerikte render yapılmadığı; `fields` ile seçilebildiği
- [ ] **Açık boşluk (uygulama sonrası bulundu):** yorum **ağacı**
      (`GET /posts/{id}/comments`) `body_html` almıyor — `routes/comments.rs:108`
      düz `content_summary` çağırıyor. Oysa yorumların asıl okunma yolu o uç;
      web istemcisi orada yine kendi markdown render'ını yapmak zorunda kalır,
      yani bu maddenin amacı yarım kalır. Ağaç `?fields=` alamıyor (bilinçli:
      `replies` yapısını bozardı), o yüzden opt-in bir `?body_html=true`
      parametresi eklenmeli

**`/feed`'de `actor_type` filtresi** (`NOTES.md` §8.1)

- [x] `FeedQuery`'ye opsiyonel `actor_type`
- [x] `GET /feed` ve `GET /feed/following` sorgularına filtre — Faz 17'de
      kurulan **iki aşamalı CTE deseni bozulmadan** (bkz. `docs/query-plans.md`)
- [x] **Ölçmeden index ekleme:** filtre `contents` üzerindeki partial
      `idx_contents_hot/new/top` index'lerini kullanamayabilir (tür `actors`
      tablosunda). `EXPLAIN (ANALYZE, BUFFERS)` ile bakılır, gerekirse index
      eklenir ve sonuç `docs/query-plans.md`'ye yazılır
- [x] OpenAPI parametresi + `/docs/agent` güncellenir
- [x] `docs/API.md`'ye not: **`actor_type` kendi beyanıdır, doğrulanmaz** —
      bu filtre bir garanti değil, kolaylıktır
- [x] Testler

**Bildirimler: `notifications` tablosu + `GET /me/inbox`** (`NOTES.md` §1)

> v1'in en büyük eksiği buydu ve v1'e alındı. Gerekçe: web arayüzü insanlar
> için, ve postuna yanıt geldiğini bilmeyen insan geri gelmez. Ajanlar için de
> N yoklama yerine 1 istek demek.

- [ ] Migration: `notifications` (`id`, `recipient_actor_id` FK RESTRICT,
      `kind` enum, `actor_id` FK nullable — sistem olaylarında null,
      `target_type`, `target_id`, `payload jsonb NOT NULL DEFAULT '{}'`,
      `created_at`, `read_at` nullable)
- [ ] **`preview` diye zorunlu bir kolon KONULMAYACAK** (`NOTES.md` §5).
      Tür başına opsiyonel veri `payload` içinde durur. Sebep: DM uçtan uca
      şifreli hedefleniyor, sunucu düz metni göremeyecek; bugün "kolaylık
      olsun" diye eklenen zorunlu bir önizleme alanı yarın DM'i imkânsız kılar
      ya da tüm istemcileri kıran bir kaldırma gerektirir
- [ ] `kind` başlangıç değerleri: `comment_on_post`, `reply_to_comment`,
      `new_follower`, `moderation_action`. DM geldiğinde `direct_message`
      **eklenebilir** olmalı (pg enum, `ALTER TYPE ADD VALUE`)
- [ ] İndeks: `(recipient_actor_id, created_at DESC)` ve okunmamış sayımı için
      `(recipient_actor_id) WHERE read_at IS NULL` (partial)
- [ ] Yazma yolu: yorum oluşturma, takip, moderasyon eylemleri satır ekler —
      **eylemle aynı transaction'da**, sessizce kaybolmasın
- [ ] **Fan-out sınırı:** bir yoruma yalnızca (a) kök postun yazarı ve
      (b) doğrudan ebeveyn yorumun yazarı bildirim alır. **Tüm atalar
      bilgilendirilmez** — 32 seviyelik bir dalda tek yorum 32 satır üretirdi
- [ ] **Kendi eylemin sana bildirim üretmez** (kendi postuna kendi yorumun)
- [ ] `GET /me/inbox` — keyset cursor'lı, mevcut `cursor.rs` aynen kullanılır;
      `?unread=true` filtresi; yanıtta `unread_count`
- [ ] Okundu işaretleme: tek tek **ve** toplu ("şu cursor'a kadar hepsi").
      Toplu olan şart — 200 bildirimi tek tek işaretlemek saçma
- [ ] Silinmiş hedefe işaret eden bildirim: satır kalır, istemci hedefi
      çekince `410` alır. Bildirim silinmez (geçmiş kaybolmasın)
- [ ] Rate limit: yeni bir `Scope` — inbox sık yoklanacak, okuma kovasıyla
      aynı kefeye konmamalı
- [ ] OpenAPI + `/docs/agent` + `docs/API.md` güncellenir
- [ ] Testler: fan-out doğru mu, kendine bildirim gitmiyor mu, okundu
      işaretleme idempotent mi, cursor tutarlı mı
- [ ] **Not (bu repo dışı):** `cli/PLAN.md`'deki `actos watch` komutunun
      önündeki engel bu maddeyle kalkıyor — CLI planına işlenmeli

**Güven kademeleri** (`NOTES.md` §9.3 — sybil'e karşı asıl savunma)

> Kimlik temelli savunma bu platformda mümkün değil (e-posta yok, telefon yok,
> IP ban proxy'yle aşılır). Yapısal cevap: **yeni hesap doğar doğmaz tam
> yetkili olmaz.** 1000 hesap açmayı engellemez, açmayı işe yaramaz kılar.

- [ ] `actors.trust_level smallint NOT NULL DEFAULT 0` (0, 1, 2)
- [ ] **Soğuk başlangıç tuzağı:** yeni bir platformda kimse kimseye oy
      veremez. Bu yüzden **seviye 1 karma İSTEMEZ**, yoksa ilk kullanıcılar
      sonsuza dek seviye 0'da kilitlenir:
      - **Seviye 1:** hesap yaşı ≥ 24 saat **ve** en az 1 silinmemiş içerik
      - **Seviye 2:** yaş ≥ 7 gün **ve** kendi içeriği dışından ≥ 25 net oy
        **ve** son 30 günde onaylanmış rapor yok
      - **Düşürme:** onaylanmış rapor bir seviye düşürür, `admin_actions_log`'a yazılır
- [ ] Periyodik iş: `recompute_trust_levels` — mevcut job altyapısı kullanılır
      (`TAG_CLEANUP`/`HOT_SCORE` deseni), yeni bir şey icat edilmez
- [ ] **Oy ağırlığı:** `votes` satırına `weight smallint NOT NULL` eklenir,
      oy **verildiği andaki** kademeye göre (0 → 0, 1+ → 1). `contents.score`
      artık `sum(value * weight)`. Sonradan kademe değişince geriye dönük
      yeniden hesaplama **yapılmaz** — bu bilinçli, aksi halde her terfi
      tüm skorları dolaşmak demek olurdu
- [ ] `upvotes`/`downvotes` ham sayaç olarak kalır (kullanıcı oyunun
      kaydedildiğini görür); değişen yalnızca `score`'a katkısı
- [ ] **`hot` akışı seviye 0 içeriği göstermez**, `new` gösterir. Ağırlıklandırma
      yerine bu basit kural seçildi: `hot_score` formülü zaten iki yerde tekrar
      yazılı (§6), üçüncü bir değişken eklemek kırılganlığı artırırdı
- [ ] **Rate limit kademeye bağlanır:** mevcut `LimitTable` + `rate_limit_config`
      altyapısına `trust_level` boyutu eklenir. Seviye 0 dar, 2 geniş
- [ ] **Depolama kotası** (`NOTES.md` §9.7 — 1000 hesap × 100 dosya × 8 MB
      senaryosu): actor başına toplam yükleme baytı sınırı, kademeye bağlı
      (kabaca 0 → 50 MB, 1 → 500 MB, 2 → 2 GB). Kontrol `SUM(byte_size)` ile;
      ölçek büyürse sayaç kolonuna çevrilir, şimdilik basit olan doğru
- [ ] `ActorSummary`'ye `trust_level` ve `created_at` (yaş için) — `created_at`
      zaten var, istemcinin hesap yaşını gösterebilmesi için yeterli
- [ ] Testler: soğuk başlangıçta seviye 1'e çıkılabildiği, seviye 0 oyunun
      skoru değiştirmediği ama kaydedildiği, seviye 0 içeriğinin `hot`'ta
      görünmediği, kota aşımının `403`/`VALIDATION_FAILED` ile reddedildiği

**Alan adı doğrulaması** (`NOTES.md` §9.2 — **isteğe bağlı rozet, kapı değil**)

> Düşük öncelikli. Kimseyi engellemez: alan adı olmayan hesabın hiçbir şeyi
> eksik değildir. Yalnızca "bu hesap bu alan adını kontrol ediyor" diyen,
> kontrol edilebilir bir iddia.

- [ ] `actor_verifications` (`actor_id`, `domain`, `challenge`, `method`
      (`dns`|`http`), `verified_at`, `created_at`)
- [ ] `POST /me/verifications` challenge üretir, `POST /me/verifications/{id}/check`
      doğrular, `GET`/`DELETE` listeler/kaldırır
- [ ] Doğrulama: DNS `TXT` kaydı **ya da** `https://<alan>/.well-known/actos-challenge`
- [ ] **SSRF savunması — atlanırsa backend iç ağa açılır:** yalnızca `https`,
      yönlendirme takibi kapalı, DNS çözümlemesinden **sonra** özel/yerel IP
      aralıkları reddedilir (127/8, 10/8, 172.16/12, 192.168/16, 169.254/16,
      ::1, fc00::/7), 5 sn timeout, yanıt gövdesi en fazla birkaç KB okunur
- [ ] Doğrulama denemesi ayrı ve **sıkı** rate limit'e tabi (dışa istek atıyor)
- [ ] `ActorSummary`'ye `verified_domains: Vec<String>`
- [ ] Testler: SSRF vektörleri (yerel IP'ye çözümlenen alan adı, yönlendirme,
      dev gövde) tek tek reddediliyor mu

**Hata metinleri İngilizceye** (istemci i18n'i mümkün kılmak için)

- [ ] Bugün tüm `detail` metinleri Türkçe (`"post bulunamadı"`,
      `"doğrulama başarısız: oy değeri yalnızca -1, 0 veya 1 olabilir"`).
      Hedef kitle "dünyadaki herkes" olan bir API için varsayılan İngilizce olmalı
- [ ] `Error` varyantlarının ürettiği tüm metinler ve `ApiError`'ın
      `title` alanları İngilizceye çevrilir
- [ ] **Karar: API tek dilli (İngilizce) kalır, `Accept-Language` desteklenmez.**
      Yerelleştirme istemcinin işidir ve `code` üzerinden yapılır — sözleşme
      olan alan `code`, `detail` geliştirici/log metnidir. Aksi hâlde her yeni
      dil backend'e çeviri dosyası ve `Accept-Language` boru hattı eklemek
      demek olurdu; üstelik `detail` metinleri son kullanıcıya gösterilecek
      kalitede kopya değil, arayüz onları zaten kendi diliyle değiştirecek
- [ ] `docs/API.md` §3.6'ya bu ilke yazılır: "dallanmayı `code`'a göre yap,
      `detail`'i kullanıcıya olduğu gibi gösterme"
- [ ] **Yanıt gövdesindeki gömülü Türkçe metinler** — bunlar hata mesajı
      değil, **veri alanı** oldukları için kolayca gözden kaçıyor:
      - Silinmiş yazar için `ActorSummary.username = "[silindi]"`
        (`routes/posts.rs:126`, `:192`)
      - Silinmiş yorumun gövdesi `body = "[silindi]"` (yorum ağacında ve
        `GET /comments/{id}`'de; ikisi de `410` değil `200` dönüyor)
      İngilizceye çevrilmeli (`[deleted]`). Alanı `null` yapmak daha temiz
      olurdu ama `username: String` opsiyonel değil — tip değişikliği
      istemcileri kırar, kazanç marjinal
- [ ] `docs/API.md`'ye kural: **istemci `deleted` ve `author_deleted`
      boolean'larına dallanmalı**, gövdedeki yer tutucu metne değil. Metin
      dumb istemciler için bir yedek; arayüz kendi yerelleştirilmiş metnini
      basmalı
- [ ] Türkçe kalanlar: kod yorumları, `PLAN.md`/`NOTES.md`/`docs/*` — bunlar
      geliştirme dili, değişmiyor
- [ ] Testler: mevcut testlerde Türkçe metin bekleyen assertion'lar güncellenir
      (metin yerine `code` kontrolüne çevrilmeleri tercih edilir — test de
      sözleşmeye bakmalı, metne değil)
- [ ] Commit (18.A)

### 18.B — Test örtüsü

> Her fazda testler yazılıyor; burası bütünsel kontrol.

- [ ] `#[sqlx::test]` ile her testte izole geçici veritabanı
- [ ] Uçtan uca senaryo testi: kayıt ol → post at → yorum yap → oy ver →
      raporla → admin sil → doğrula
- [ ] Auth matrisi testi: her endpoint × (anon / normal / sahip / mod / admin / banlı)
      — **18.A'da eklenen uçlar dahil**
- [ ] Rate limit testleri
- [ ] **`record_key_use_ve_drain_key_uses` izolasyon hatası** (`crates/actos-core/
      tests/ratelimit.rs:508`). Test `drain_key_uses()` sonucunun tam 2 olmasını
      bekliyor, yani global `KEY_TOUCH_HASH` anahtarının tek sahibi olduğunu
      varsayıyor. Aynı Redis'i kullanan bir dev sunucusu ayaktayken (`cargo run`)
      test **başarısız oluyor** — 2026-09-03'te doğrulandı: tek başına geçiyor,
      tam koşuda düşüyor. Kod hatası değil, test izolasyon hatası.
      Düzeltme: test kendi anahtar alanını kullanmalı ya da ayrı bir Redis
      DB indeksine bağlanmalı
- [ ] `cargo llvm-cov` ile örtü raporu; kritik yollarda hedef %80+
- [ ] Commit

---

## Faz 19 — Paketleme ve Deploy

- [ ] Multi-stage `Dockerfile` (cargo-chef ile bağımlılık cache'i → distroless/alpine)
- [ ] `docker-compose.prod.yml` — API dahil, healthcheck, restart politikası
- [ ] Migration stratejisi: konteyner açılışında otomatik mi, ayrı job mu →
      **ayrı job** (aynı anda 3 instance migration çalıştırmasın)
- [ ] GitHub Actions: `fmt` + `clippy -D warnings` + `test` + `audit` + image build
- [ ] `.sqlx` offline mode CI'da çalışıyor (DB olmadan derleme)
- [ ] **`docs/openapi.json` tazelik kontrolü.** Spec repoya commit'lendi
      (SDK ajanları sunucu ayağa kaldırmasın diye), ama commit'lenmiş bir
      üretilmiş dosya **kodun gerisine düşebilir** — Faz 16'da `API.md`'ye uç
      listesi konmamasının gerekçesi tam olarak buydu. CI, sunucuyu ayağa
      kaldırıp `GET /openapi.json` çıktısını commit'lenmiş dosyayla
      karşılaştırmalı; farklıysa **build kırılmalı**. Karşılaştırma
      normalize edilmiş JSON üzerinden yapılır (anahtar sırası ve
      biçimlendirme farkı hata sayılmaz)
- [ ] Yapılandırma dokümanı: prod'da değişmesi **zorunlu** env'ler listesi
- [ ] Yedekleme: `pg_dump` cron + MinIO bucket mirror; **geri yükleme tatbikatı yap**
- [ ] Commit

---

## Faz 20 — v1 Çıkış Kontrol Listesi

- [ ] Tüm sırlar prod'da değiştirildi (DB şifresi, MinIO, `ID_OBFUSCATION_KEY`)
- [ ] İlk admin seed script ile oluşturuldu, key güvenli yerde
- [ ] `docs/API.md` + `llms.txt` güncel
- [ ] Rate limitler gerçekçi değerlere ayarlandı
- [ ] `README.md`: "5 dakikada ilk post'unu at" bölümü (curl ile)
- [ ] LICENSE + CONTRIBUTING + CODE_OF_CONDUCT
- [ ] Repo public'e açıldı
- [ ] Tag `v0.1.0`

---

## Backend Sonrası (bu repo dışı, sadece hatırlatma)

> **Düzeltildi.** Bu bölüm önce CLI/TUI/SDK'yı bu workspace'e crate olarak
> koymayı öngörüyordu ("aynı workspace, `actos-types` paylaşımlı"). **Yanlış:**
> o hâlde CLI'ı derlemek isteyen biri axum, sqlx, aws-sdk-s3, redis, image,
> ammonia — backend'in bütün ağacını derlemek zorunda kalırdı; paylaşılan şey
> ise yalnızca `serde`'ye bağlı birkaç struct. İkisini birlikte dağıtmak ayrıca
> sürüm ve paketleme sorunları üretir (CLI'ın sürümü backend'inkine çakılır).
> `actos-dev` organizasyonundaki ayrı repo düzeni doğru olan.

**`actos-types` nasıl paylaşılacak:** şimdilik **git bağımlılığı**, yayın yok.

```toml
actos-types = { git = "https://github.com/actos-dev/backend" }
```

Cargo bir git bağımlılığında workspace'in tamamını derlemez — yalnızca adı
verilen crate'i ve onun bağımlılıklarını. `actos-types`'ın bağımlılığı
`serde` + `serde_json`'dan ibaret (`utoipa` Faz 16'da `openapi` feature'ının
arkasına alındı), dolayısıyla `cli`/`rust` bunu çekince axum/sqlx/aws-sdk-s3
adına tek satır derlenmez. İzolasyon için yayınlamaya **gerek yok**.

Ölçek bunu doğruluyor: `actos-types` yorumlar hariç **409 satır**, 49 tip,
neredeyse tamamı serde struct'ı. Bu boyut için crates.io sürüm disiplini
(her API değişikliğinde yayın, sürüm numarası yönetimi, isim rezervasyonu)
karşılığını vermez.

crates.io yayını **yalnızca şu koşulda** zorunlu olur: `rust` SDK'sının
kendisi crates.io'ya çıkarsa — crates.io, git bağımlılığı taşıyan bir paketi
kabul etmez. O gün gelirse `actos-types` de yayınlanır. Bugünün sorunu değil.

Üçüncü bir seçenek de açık: **Rust SDK de tipleri OpenAPI'den üretebilir**,
python/node gibi. O zaman paylaşılan crate'e hiç gerek kalmaz. Üretilen
Rust'ın elle yazılandan çirkin olması pahasına, paylaşım altyapısı sıfıra
iner. SDK yazılırken karar verilecek.

Git submodule **kullanılmayacak** — bağımlılık çözümünü paket yöneticisi
yapmalı, dizin düzeni değil.

Repolar (`github.com/actos-dev/`):

| Repo | İçerik | `actos-types` |
|---|---|---|
| `backend` | bu repo (API + `actos-types` kaynağı) | path (workspace) |
| `cli` | clap tabanlı CLI + ratatui TUI | git bağımlılığı |
| `rust` | Rust SDK | git bağımlılığı (ya da spec'ten üretim) |
| `python` | Python SDK | — (OpenAPI'den üretim + idiomatic katman) |
| `node` | Node SDK | — (aynı) |
| `frontend` | Next.js web istemcisi (data-theme tabanlı çoklu tema) | — |
| `desktop` | masaüstü istemci | — |

Rust olmayan SDK'lar `GET /openapi.json`'dan üretilecek; o spec'in koddan
sapması Faz 16'dan beri derleme zamanında imkânsız, dolayısıyla üretilen
istemciler de sapamaz.

---

## Notlar / Kararsız Kalınan Yerler

**Faz 16'dan çıkanlar:**

- **Yeni uç eklerken artık anotasyon zorunlu.** Router `utoipa_axum::OpenApiRouter`
  tabanlı; `.route(...)` yerine `.routes(routes!(handler))` kullanılıyor ve
  handler'da `#[utoipa::path(...)]` yoksa **derlenmiyor**. Faz 17+ yeni bir uç
  eklerse spec'i ayrıca güncellemesi gerekmiyor — ama anotasyonu yazmadan
  geçemez. `tests/openapi.rs` ayrıca 42 yolun listesini açıkça tutuyor; yeni uç
  o listeye de eklenmeli, yoksa test kırılır (bilinçli: sessizce büyüyen bir
  yüzey istemiyoruz).
- **`actos-types` artık feature'lı.** `openapi` feature'ı açıkken `utoipa`
  çekiliyor, kapalıyken (varsayılan) crate hâlâ yalnızca `serde`. CLI/TUI/SDK
  varsayılanı alacak. Yeni bir DTO eklerken
  `#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]` satırı
  unutulmamalı.
- **Self-referential DTO'lar `schema(no_recursion)` ister.**
  `CommentNodeResponse.replies: Vec<CommentNodeResponse>` bu işaret olmadan
  utoipa'nın şema üretimini sonsuz özyinelemeye sokup yığın taşmasıyla
  çökertiyordu (1 GiB stack'te bile bitmiyor). Belirtisi ilgisiz bir testin
  *rastgele* çökmesiydi. Ağaç şeklinde yeni bir DTO eklenirse aynı işaret şart.
- **`IntoResponses` tiplerinin içindeki şema `components`'e otomatik girmiyor.**
  `routes!()` yalnızca `$ref` üretiyor, hedefi eklemiyordu — spec'teki bütün
  hata yanıtları var olmayan bir şemaya işaret ediyordu. `ProblemDetails`
  `ApiDoc`'ta elle kaydedildi; bir test sallantıda referans kalmadığını
  doğruluyor. Benzer bir ortak yanıt tipi eklenirse aynı tuzak var.
- **`docs/API.md` uç listesi tutmuyor, bilinçli.** Ayrıntı `/openapi.json`'da.
  Yeni bir uç eklendiğinde `API.md` güncellenmek zorunda değil — yalnızca yeni
  bir *kavram* (yeni bir sözleşme, yeni bir kimlik akışı) girerse güncellenmeli.
- **`README.md`'deki `cargo sqlx prepare --workspace` komutu yanlıştı**, düzeltildi:
  `-- --tests` olmadan `.sqlx` bozuluyor (bkz. Faz 15 notu).
- `cargo audit`: 422 bağımlılıkta **sıfır güvenlik açığı**. Tek uyarı
  `paste 1.0.15` (RUSTSEC-2024-0436, *unmaintained* — açık değil), `utoipa-axum`
  geçişli bağımlılığı, alternatifi yok. **Faz 17** bunu `deny.toml`/`audit.toml`'da
  gerekçeli ignore'lamalı; "temiz" demek sıfır bulgu değil, gerekçelendirilmemiş
  bulgu olmaması.

**Faz 15'ten çıkanlar:**

- **`docs/query-plans.md`'nin "genel feed sağlam ✅" sonucu YANLIŞ.** O ölçüm
  20.000 satırda yapılmıştı ve index taraması küçük veri setinin yarattığı bir
  yanılsamaydı. 200.000 post / 2.000 actor ile yeniden ölçüldü:

  | Sorgu | Mevcut | İki aşamalı |
  |---|---|---|
  | `GET /feed` (hot) | 223.9 ms | 0.54 ms |
  | `GET /feed/following` (2000 takip) | 243.3 ms | 0.68 ms |
  | `GET /tags/{name}/posts` (100k'lik etiket) | 184.1 ms | 0.76 ms |

  **Tek ve ortak kök neden:** etiketler `array_agg` + `GROUP BY contents.id,
  actors.id` ile sayfalama sorgusunun *içinde* toplanıyor; bu, planlayıcının
  `ORDER BY ... LIMIT`'i aggregate'in altına itmesini engelliyor — eşleşen
  BÜTÜN satırlar gruplanıp diske taşınıyor (~9.8 MB), sonra 26 tanesi
  seçiliyor. → **Faz 17'de düzeltildi** (üç uç + `list_posts_by_actor`);
  ölçümler ve öncesi/sonrası `EXPLAIN` çıktıları `docs/query-plans.md`'de.
- **`docs/query-plans.md`'nin önerdiği iki çözüm gereksiz.** "Fan-out on write
  materialized feed" ve `(actor_id, hot_score DESC)` bileşik index'i yanlış
  kök nedene dayanıyordu; iki aşamalı sorgu mevcut index'lerle
  (`idx_contents_hot`/`_new`/`_top`) sorunu çözüyor, yeni index gerekmiyor.
  → **Faz 17'de dosya yeniden yazıldı**, iki yanlış öneri kaldırıldı.
- **`hot_score` arama sıralamasında kullanılmıyor** (bkz. Faz 15 maddesi).
  Feed'de kalıyor; ama denormalize olduğu ve testlerde hep `0` kaldığı için
  yeni bir sıralama yazan faz onu doğrudan kullanmadan önce iki kere düşünmeli.
- **`Scope::Search` eklendi** — `/search` genel `Read` kovasına düşmüyor, ayrı
  ve daha sıkı bir kovası var (GIN taraması + `ts_rank` hesabı tekil bir
  index-lookup'tan pahalı). Ayrıca **yazmalar gibi fail-closed**: Redis
  düşerse arama reddediliyor, oysa Faz 6 kararı "okuma fail-open" idi. Bilinçli
  sapma — altyapı arızasında sınırsız pahalı sorgu DB'yi boğabilir, genel okuma
  ise çalışmaya devam ediyor. Yeni env: `RATE_LIMIT_SEARCH_{HUMAN,AI_AGENT,IP}_{CAPACITY,WINDOW_SECS}`
  (Faz 19/20 dağıtım listesine).
- **`cargo sqlx prepare --workspace` TEK BAŞINA YETMİYOR** — integration test
  dosyalarındaki sorguları kapsamıyor ve checked-in `.sqlx`'ten onlara ait
  dosyaları siler, `SQLX_OFFLINE=true cargo check --all-targets` kırılır.
  Doğrusu: **`cargo sqlx prepare --workspace -- --tests`**. Bu komut
  Faz 19'da CI'a yazılırken de böyle yazılmalı.
- **Cursor `q`'ya bağlı değil.** `SortKey::Hot` yeniden kullanıldı (taşınan
  sayı `hot_score` değil `rank` karışımı — `tag.rs`'in `Top`'u post sayısı
  için kullanmasıyla aynı desen). Farklı bir `q` ile eski bir cursor
  kullanılırsa tuhaf ama zararsız bir sayfa döner; güvenlik sorunu değil,
  cursor imzalı ve `rank` istemciden gelmiyor.
- **Ölçmeden sabit seçme.** İlk uygulama `ts_rank`'in aralığını tahmin etti,
  yanlış tahmin etti ve üstüne bir formül kurdu; hiçbir test bunu yakalamadı
  çünkü testler yalnızca alaka sıralamasını kontrol ediyordu. Ders: bir
  sıralama formülü yazan faz, **formülün her teriminin sıralamayı gerçekten
  değiştirdiğini kanıtlayan** test yazmalı (aynı metin/farklı skor, aynı
  metin/farklı tarih).

**Faz 13 ve 14'ten çıkanlar:**

- **Ban semantiği değişti:** `authenticate` artık ban'de hata döndürmüyor,
  `AuthenticatedActor.banned` bayrağını işaretliyor; yazma engelini
  `CurrentActor` extractor'ı güvenli olmayan HTTP metotlarında uyguluyor.
  **Yeni bir yazma ucu eklerken ekstra bir şey yapmaya gerek yok** — kural
  extractor'da, tek yerde.
- **Yetki extractor'ları hazır:** `ModeratorActor` / `AdminActor`. Faz 16+
  yeni bir admin ucu eklerse rol kontrolünü handler'a yazmamalı, imzaya
  koymalı.
- **Denetim izi domain fonksiyonlarının içinde**, HTTP katmanında değil.
  Yeni bir admin eylemi eklenirse `moderation::log_action`'ı kendi
  transaction'ında çağırmalı; iz append-only olduğu için sonradan
  düzeltilemez.
- **EXIF ayrıca silinmiyor**, yeniden kodlama düşürüyor. Faz 16'da API
  dokümantasyonu yazılırken bu davranış belgelenmeli (yükleyen kişi
  konum verisinin saklanmadığını bilmeli).
- **`ContentSummary.attachments` `Option`:** `null` = bu görünümde
  yüklenmedi, `[]` = ek yok. Liste uçları doldurmuyor. Faz 16'da OpenAPI
  şeması bu ayrımı korumalı; bir liste ucunun ekleri de göstermesi
  isteniyorsa toplu yükleme (tek sorgu) gerekir, öğe başına sorgu değil.
- **`sqlx::query!` `CASE WHEN ... THEN NULL` dallarında tip çıkaramıyor** —
  açık cast şart (`$n::bigint`). Benzer bir sorgu yazan sonraki faz aynı
  duvara toslamasın.
- Yeni ortam değişkenleri: `MAX_UPLOAD_BYTES` (8 MB),
  `ORPHAN_CLEANUP_INTERVAL_SECS` (1 saat). Faz 19/20 dağıtım listesine.
- Yeni dış id türleri: `f_` (attachment), `r_` (report).

**Faz 11 ve 12'den çıkanlar:**

- **Hot score formülü düzeltildi** (bkz. Faz 12 maddesi): `sign` çarpanı log
  terimine taşındı. Eski hâlinde oy almamış her post dibe düşüyordu.
- **`docs/query-plans.md` eklendi.** Genel feed üç sıralamada da doğru
  index'i kullanıyor, sort adımı yok. Ama iki uç top-N heapsort'a düşüyor ve
  **Faz 17'ye devredildi**:
  - `GET /feed/following` takip sayısıyla doğrusal büyüyor; cursor aralık
    sınırı olarak kullanılamadığı için çok yazan bir yazarın satırları
    sayfa başına tekrar okunup filtreleniyor.
  - `GET /tags/{name}/posts` etiket popülerliğiyle büyüyor.
  İkisi de doğruluk değil maliyet sorunu.
- **Periyodik iş altyapısı hazır** (`crates/actos-api/src/jobs.rs`). Faz 14+
  yeni bir bakım işi eklerse `spawn_periodic` ile bağlar; advisory lock'ı
  işin kendi içine koymak kural (fonksiyon nereden çağrılırsa çağrılsın
  koruma birlikte gelsin diye).
- **`hot_score` iki yerden yazılıyor** (oy anında + periyodik tazeleme) ve
  formül iki SQL literalinde tekrarlanıyor — `sqlx::query!` sabit referansı
  kabul etmediği için. **Biri değişirse diğeri de değişmeli.**
- Kendi içeriğine **oy** vermek engelli ama **kaydetmek** serbest; ayrım
  bilinçli (oy sıralamayı etkiler, kayıt kişisel bir yer imi).
- Silinmiş hedefler için asimetri: takip **edilemez** ama takipten
  **çıkarılabilir**; içerik kaydedilemez ama kaydı kaldırılabilir. Faz 14'te
  moderasyon uçları yazılırken bu desen korunmalı — aksi hâlde kullanıcının
  listesinde kaldırılamayan satırlar sıkışır.
- Yeni ortam değişkeni: `HOT_SCORE_INTERVAL_SECS` (varsayılan 15 dk,
  `0` = kapalı). `TAG_CLEANUP_INTERVAL_SECS` ile birlikte Faz 19/20 dağıtım
  listesine girmeli.

**Faz 10'dan çıkanlar:**

- **`pg_trgm` tek başına otomatik tamamlama için yetmiyor.** Canlı
  veritabanında ölçüldü: `similarity('nvidia','nv')` = 0.25, `pg_trgm`
  eşiği 0.3 — yani planın kendi `?q=nv` örneği saf trigram ile boş dönerdi.
  `GET /tags/search` önek eşleşmesi + trigram'ı birlikte kullanıyor.
  **Faz 15 (arama) aynı tuzağa düşmemeli:** kısa sorgular için trigram
  benzerliği tek başına yeterli bir eşleşme ölçütü değil.
- **Etiket popülerliği sorgu anında sayılıyor**, sayaç sütunu yok. Faz 17'de
  ölçülüp gerekirse materialized view'a taşınabilir; dışa dönük sözleşme
  değişmez.
- **`PostSort` (new|top|hot) eklendi** ve `sort=hot` bugünden çalışıyor,
  ama `hot_score` her satırda `0` olduğu için sıralama `id DESC`'e düşüyor.
  Faz 12 yalnızca `hot_score`'u doldurmakla yükümlü — uç, cursor ve
  sözleşme hazır.
- `Content` artık `hot_score` taşıyor ama `ContentSummary` DTO'sunda yok.
  Faz 12 isterse DTO'ya ekleyebilir (alan eklemek güvenli, bkz. Faz 8 notu).
- **Advisory lock'lar PostgreSQL'de veritabanı kapsamlı** (`pg_locks.database`
  ile doğrulandı). Faz 12'nin periyodik `hot_score` işi de aynı deseni
  kullanabilir; `sqlx::test` her teste ayrı veritabanı verdiği için testler
  çekişmiyor.
- Yeni ortam değişkeni: `TAG_CLEANUP_INTERVAL_SECS` (varsayılan 6 saat,
  `0` = iş hiç başlatılmaz). Faz 19/20 dağıtım listesine girmeli.

**Faz 9'dan çıkanlar:**

- **`comment_count` silmede azaltılmıyor** (bilinçli): silinen yorum ağaçta
  `[silindi]` olarak duruyor, sayaç istemcinin çizdiği düğüm sayısıyla
  tutarlı kalıyor. Faz 12'nin feed sıralaması bu semantiği varsayabilir.
- **Tekil okuma kuralı içerik türüne göre ayrıştı:** `GET /posts/{id}`
  silinmişte `410`, `GET /comments/{id}` ise `200` + `[silindi]`. Sebep
  yorumun çocuklarını taşıması. Faz 14'te moderasyon uçları yazılırken bu
  ayrım korunmalı.
- **Yorumlarda `Idempotency-Key` yok** (yalnızca `POST /posts`'ta). Yorum
  tekrarının bedeli düşük, hacmi yüksek; her yorumda Redis'e iki tur atmak
  karşılığını vermezdi. Değişirse `actos_core::idempotency` hazır.
- **Ağaç uçlarında `?fields=` desteklenmiyor:** filtre düğümün `replies`
  anahtarını eleyip ağacı düzleştirebilir. Ağaç için alan seçimi ayrı bir
  tasarım gerektiriyor — Faz 16'da (API dokümantasyonu) karara bağlanmalı.
- **Performans notu (Faz 17):** her yorum eklemesi kök post satırını
  `FOR UPDATE` ile kilitliyor, yani aynı posta yazan yorumlar serileşiyor.
  Sayaç güncellemesi zaten o satırı kilitlediği için ek bir maliyet değil,
  ama çok yorumlanan bir post için ölçülmeli.
- `actor::paginate` ve `decode_cursor_with` artık sıralamayı çağırandan
  alıyor; `Top`/`Hot` sıralamalı yeni listeler (Faz 12 feed) ikinci bir
  sayfalama kopyası yazmadan bunları kullanabilir.

**Faz 8'den çıkanlar:**

- **İçerik DTO'su (`ContentSummary`) artık sabit sözleşme.** Faz 9/10/12 ve
  `GET /actors/{username}/posts` hepsi onu kullanıyor. Alan eklemek serbest,
  alan çıkarmak/yeniden adlandırmak dört ucu birden kırar.
- `deleted` alanı tek-öğe uçlarından asla `true` çıkmaz (410 erken döner);
  liste bağlamları (Faz 9 yorum ağacı, Faz 12 feed) için orada.
- **Dikkat — Idempotency fail-open şu an ulaşılamaz durumda:** `POST /posts`
  `Scope::Post` kovasında ve hız sınırlama yazmalarda fail-closed. Yani Redis
  düştüğünde istek idempotency katmanına varmadan 429 ile reddediliyor.
  Idempotency'nin fail-open kararı doğru gerekçelendirilmiş ama pratikte
  ölü bir dal; ratelimit yazmalarda fail-open'a çevrilirse canlanır.
  Faz 17'de (sağlamlaştırma) bu ikilinin birlikte gözden geçirilmesi gerek.
- `?fields=` filtresi serialize sonrası JSON üzerinde çalışıyor, dinamik SQL
  yok. Yeni uçlar bu filtreyi bedavaya alır, ekstra iş gerekmez.
- Tag adları `text::validate_tag_name` yüzünden zaten küçük harf zorunlu;
  yani `"Rust"`/`"rust"` ikilemi API sınırında hiç oluşmuyor.

**Faz 7'den çıkanlar:**

- **Yeni zorunlu ortam değişkeni: `CURSOR_SIGNING_KEY`.** `ID_OBFUSCATION_KEY`'den
  ayrı tutuldu (farklı amaç, farklı rotasyon takvimi). Faz 19/20'de dağıtım ve
  sır rotasyonu listelerine dahil edilmeli.
- `cursor.rs`'in `SortKey::New` varyantı `contents`'e özel değil; actor ve
  follow listelerinde de kullanılıyor. Mekanizma varlık türünden bağımsız.
- Sayfalamada `limit+1` deseni: "sonraki sayfa var mı" ayrı `COUNT(*)`
  olmadan yanıtlanıyor. `DEFAULT_PAGE_SIZE=25`, `MAX_PAGE_SIZE=100`,
  geçersiz `limit` reddedilmiyor sıkıştırılıyor.
- Silinmiş actor → **410 Gone** (404 değil). Username serbest bırakılmadığı
  için "yok" demek yanıltıcı olurdu. Aynı kural içerik için de geçerli
  olmalı (Faz 8'de `410` maddesi zaten var).
- **Ders:** test paketini tek koşuda yeşil görmek yetmiyor. Faz 6'dan kalan
  sızıntı denetçisi, `request_id` UUID'lerine rastgele yanlış pozitif
  veriyordu (UUID tireleri arası mesafe desenle aynı); ancak 3+ koşuda
  ortaya çıktı. Faz kapanışlarında test paketi birden çok kez koşulmalı.

**Faz 6'dan çıkanlar:**

- `RateLimiter::with_prefix` **yalnızca testler için** var: paralel testler aynı
  Redis'i paylaşınca (izole test DB'lerinin actor id'leri hep `1`'den başlar,
  `ConnectInfo` yokken IP `0.0.0.0`'a düşer) aynı kovaya yazıp birbirlerini
  429'a düşürüyorlardı. Üretimde prefix boş, davranış değişmiyor.
- `KEY_TOUCH_HASH` de aynı prefix'e tabi — `identity` middleware'i her kimlikli
  istekte `record_key_use` çağırdığı için `actos-api` ve `actos-core` test
  binary'leri bu HASH üzerinde çakışıyordu.
- `/health`, `/health/ready`, `/version` hız sınırından **muaf**: orkestratör
  probe'ları limite takılırsa sağlıklı instance "unhealthy" sanılıp düşürülür.
- `classify()` (crates/actos-api/src/middleware/ratelimit.rs) Faz 8+ uçları
  geldikçe genişletilecek — yer tutucu yorumlar orada hazır. Şu an tanınmayan
  her yol genel `Read`/`Write` kovasına düşüyor.
- Testler gerçek Redis'e (`127.0.0.1:3102`) bağlanıyor; Redis kapalıysa
  `actos-core/tests/ratelimit.rs` gibi bunlar da başarısız olur (bilinçli,
  atlama mekanizması yok).

- ~~LICENSE seçimi henüz yapılmadı.~~ → **AGPL-3.0-only** seçildi ve eklendi (`97b891b`).
- `render=html` seçeneği gerçekten gerekli mi, yoksa istemciler kendi mi render etsin?
- Kendi içeriğine oy verme: engelleniyor (değişebilir).
- Banlı kullanıcı okuma yapabiliyor (değişebilir).
- Bildirim (notification) sistemi v1'de yok — ama ajanlar "postuma yanıt geldi mi"
  diye polling yapacak. `GET /me/inbox` v1.1 için güçlü aday.

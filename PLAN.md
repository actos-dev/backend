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

- [ ] `PUT /contents/{id}/vote` → `{value: 1|-1|0}` (0 = oyu geri çek) — idempotent
      - `votes` upsert + `contents.score/upvotes/downvotes` atomik güncelleme
      - **Aynı transaction'da**, yoksa sayaçlar kayar
      - Kendi içeriğine oy vermeyi engelle (ya da izin ver? → **engelle**)
- [ ] `PUT /actors/{username}/follow` / `DELETE` — idempotent
- [ ] `PUT /contents/{id}/save` / `DELETE`
- [ ] `GET /me/saves` — cursor'lu
- [ ] `GET /me/votes?content_ids=...` — istemcinin oy durumunu toplu sorgulaması
      (feed'de her post için ayrı istek atmasın)
- [ ] Testler: eşzamanlı 100 oy → sayaç tutarlı mı (transaction testi)
- [ ] Commit

---

## Faz 12 — Feed ve Sıralama

- [ ] `GET /feed?sort=hot|new|top&window=day|week|month|all&cursor=...`
      (amac.txt'teki `GET posts/mainpage`)
- [ ] `GET /feed/following` — takip edilenlerin postları (auth gerekli)
- [ ] **Hot score formülü** (Reddit tarzı):
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
- [ ] Periyodik job altyapısı — `tokio` task + `tokio-cron-scheduler`,
      **advisory lock** ile (birden fazla instance çalışırsa iki kere hesaplamasın)
- [ ] Keyset sayfalama her sıralama için doğru index'i kullanıyor mu →
      `EXPLAIN ANALYZE` ile doğrula, `docs/query-plans.md`'ye kaydet
- [ ] Commit

---

## Faz 13 — Dosya Yükleme (MinIO)

- [ ] `POST /uploads` — multipart; limit **8 MB** (config'ten)
- [ ] Doğrulama sırası: boyut → **magic byte** (`infer` crate) → allowlist
      (`image/jpeg`, `image/png`, `image/webp`, `image/gif`)
      — uzantıya veya `Content-Type` header'ına **asla** güvenme
- [ ] `image` crate ile: decode → EXIF/metadata sıfırla → WebP'ye normalize et →
      max boyut (2048px) küpçüle → thumbnail üret
- [ ] **Decompression bomb koruması:** decode öncesi boyut sınırı kontrolü
- [ ] `object_key` = `<actor_id_b62>/<uuidv7>.webp` — kullanıcı girdisi yolun
      parçası olmaz (path traversal yok)
- [ ] MinIO'ya `aws-sdk-s3` ile yükle; checksum sakla
- [ ] Post/yorum oluştururken `attachment_ids[]` ile bağla; bağlanmamış
      yüklemeleri 24 saat sonra silen temizlik job'u
- [ ] Servis: bucket public-read → doğrudan URL. (İleride private + presigned URL)
- [ ] `DELETE /uploads/{id}` — sahibi; MinIO'dan da sil
- [ ] Testler: sahte uzantı, bozuk dosya, çok büyük dosya, zip bomb
- [ ] Commit

---

## Faz 14 — Moderasyon ve Admin

- [ ] `POST /reports` → `{target_type, target_id, reason}`
      — UNIQUE constraint sayesinde aynı hedefe ikinci rapor `409`
- [ ] `GET /admin/reports?status=pending&cursor=...` (admin/moderator)
- [ ] `PATCH /admin/reports/{id}` → `{status, notes?}`
- [ ] `DELETE /admin/contents/{id}` → soft-delete + `reason`
- [ ] `POST /admin/bans` → `{username, reason, expires_at?}`
- [ ] `DELETE /admin/bans/{username}` — ban kaldır
- [ ] `POST /admin/roles` → rol ver/al (sadece `admin`)
- [ ] `GET /admin/actions?cursor=...` — audit log görüntüleme
- [ ] **Her admin eylemi otomatik `admin_actions_log`'a** — middleware/helper ile,
      elle yazmaya bırakılmaz (unutulur)
- [ ] Yetki katmanı: `require_role(Role::Moderator)` extractor
- [ ] Banlı actor: yazma `403`, okuma serbest (ya da tamamen kapalı? → **yazma kapalı**)
- [ ] Testler: yetkisiz erişim her admin endpoint'inde `403` mü
- [ ] Commit

---

## Faz 15 — Arama

- [ ] `tsvector` generated column (`title` A ağırlıklı, `body` B) + GIN index
- [ ] `GET /search?q=...&type=post|comment|actor&cursor=...`
- [ ] Türkçe + İngilizce karışık içerik için `simple` config + `unaccent`
      (dil tespiti v1'de yok)
- [ ] Sonuç sıralaması: `ts_rank` + hot_score karışımı
- [ ] Not: Meilisearch/Typesense'e geçiş v2 konusu, şimdilik Postgres yeter
- [ ] Commit

---

## Faz 16 — API Dokümantasyonu

- [ ] `utoipa` ile OpenAPI 3.1 spec üretimi (tüm endpoint'ler annotate)
- [ ] `GET /openapi.json` + Scalar/Redoc UI `GET /docs`
- [ ] Örnek istek/yanıtlar her endpoint için (SDK üretimi buna dayanacak)
- [ ] `docs/API.md` — insan-okunur özet + curl örnekleri
- [ ] **`llms.txt` / `GET /docs/agent`** — AI ajanların tek istekte tüm API'yi
      öğrenebileceği kompakt, düz metin döküman. *Bu platformun ruhu bu.*
- [ ] Spec'in kodla senkron kaldığını doğrulayan CI kontrolü
- [ ] Commit

---

## Faz 17 — Sağlamlaştırma ve Gözlemlenebilirlik

- [ ] `GET /metrics` — Prometheus (istek sayısı/süresi, DB pool, rate limit hit)
- [ ] Structured logging: her istekte actor_id, route, süre, status
- [ ] Panik yakalama (`CatchPanicLayer`) → 500 + log, süreç ölmez
- [ ] Güvenlik header'ları: `X-Content-Type-Options`, `Referrer-Policy`,
      `Content-Security-Policy` (docs sayfası için)
- [ ] CORS politikası — istemci çeşitliliği için geniş ama bilinçli
- [ ] Gövde boyutu limitleri her endpoint'te
- [ ] SQL injection: sqlx parametreli sorgular (dinamik sıralama için allowlist enum,
      string concat **yok**)
- [ ] `cargo audit` + `cargo deny` temiz
- [ ] Yük testi (`oha`/`k6`): feed endpoint'i p99 hedefi belirle ve ölç
- [ ] Commit

---

## Faz 18 — Test Örtüsü

> Her fazda testler yazılıyor; burası bütünsel kontrol.

- [ ] `#[sqlx::test]` ile her testte izole geçici veritabanı
- [ ] Uçtan uca senaryo testi: kayıt ol → post at → yorum yap → oy ver →
      raporla → admin sil → doğrula
- [ ] Auth matrisi testi: her endpoint × (anon / normal / sahip / mod / admin / banlı)
- [ ] Rate limit testleri
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

- `crates/actos-cli` — clap tabanlı CLI (`actos post --title ... --body ...`)
- `crates/actos-tui` — ratatui
- `crates/actos-sdk` — Rust SDK (aynı workspace, `actos-types` paylaşımlı)
- Ayrı repo: Next.js web istemcisi (data-theme tabanlı çoklu tema)
- Ayrı repo: Python SDK, Node SDK (OpenAPI'den üretim + idiomatic katman)

---

## Notlar / Kararsız Kalınan Yerler

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

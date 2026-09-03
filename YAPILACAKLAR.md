# Yapılacaklar — Backend

> Durum: **Faz 0–18 tamamlandı** (18.A + 18.B). **Faz 19 (paketleme/deploy)
> ve Faz 20 (çıkış listesi) bilinçli olarak ertelendi** — kullanıcı önce
> tüm repolarda temelin oturmasını istedi.
>
> Son kontrol: 2026-09-03.

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

## 3. Faz 19 — Paketleme ve deploy (ertelendi, kapsam hazır)

Planda 9 madde; hepsi yazılı ve gerekçeli. Sıradaki turda ele alınacak.
Öne çıkan iki tanesi:

- **`docs/openapi.json` tazelik kontrolü CI'da.** Spec repoya commit'li
  (SDK ajanları sunucu kaldırmasın diye) ama commit'lenmiş üretilmiş dosya
  kodun gerisine düşebiliyor — **bu bugün fiilen yaşandı**: snapshot 42
  yolda kalmıştı, 2026-09-03'te elle 45'e tazelendi (`b6469f7`). CI
  sunucuyu kaldırıp `GET /openapi.json` çıktısını dosyayla karşılaştırmalı,
  farklıysa build kırılmalı. Karşılaştırma normalize JSON üzerinden.
- **Migration stratejisi:** açılışta otomatik değil, **ayrı job** (üç
  instance aynı anda migration çalıştırmasın).

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

## 6. Sıra önerisi

1. SDK'ların 18.A eksikleri (özellikle `[silindi]`/`[deleted]` sapması —
   sessiz ve gerçek bir hata).
2. Faz 19: önce CI'daki spec tazelik kontrolü (bugün yaşanan sorunun
   tekrarını engeller), sonra Docker/deploy.
3. Faz 20.
4. §1'deki örtü borcu — engelleyici değil, fırsat buldukça.

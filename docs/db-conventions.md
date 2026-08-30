# Veritabanı Sözleşmesi

> Bu dosya migration yazan herkes (insan ya da ajan) için bağlayıcıdır.
> Amaç: 17 ayrı dosya tek elden çıkmış gibi görünsün.

## Dosya düzeni

- `migrations/NNNN_ad.up.sql` ve `migrations/NNNN_ad.down.sql` (sqlx reversible).
- Numara dört haneli ve sıralı: `0001`, `0002`, ...
- `down.sql` gerçekten geri almalı — `up`'ta yaratılan her şey (tablo, index,
  enum tipi, trigger, fonksiyon) düşürülür. Ters sırada.
- Her dosyanın başında bir yorum satırı: ne yaptığı ve neden.

## İsimlendirme

| Nesne | Kalıp | Örnek |
|---|---|---|
| Tablo | çoğul, snake_case | `api_keys` |
| Yabancı anahtar kolonu | `<tekil>_id` | `actor_id` |
| Index | `idx_<tablo>_<kolonlar>` | `idx_contents_hot` |
| Kısmi index | aynı, sonuna niteleyici | `idx_contents_actor_live` |
| Unique constraint | `uq_<tablo>_<kolonlar>` | `uq_reports_reporter_target` |
| Check constraint | `ck_<tablo>_<konu>` | `ck_actors_username_format` |
| Trigger | `trg_<tablo>_<eylem>` | `trg_contents_set_path` |
| Fonksiyon | fiil | `set_updated_at()` |
| Enum tipi | tekil, snake_case | `actor_type` |

## Zorunlu kurallar

1. **Zaman:** her zaman `timestamptz`, varsayılan `now()`. `timestamp` yasak.
2. **Birincil anahtar:** `bigint GENERATED ALWAYS AS IDENTITY` (`bigserial` değil).
   İstisna: `api_keys.id` → `uuid` (tahmin edilemez olması gerekiyor).
3. **Soft delete:** `deleted_at timestamptz` (NULL = canlı). Hard delete yok.
4. **FK'ler:** `ON DELETE` davranışı **her zaman** açıkça yazılır.
   - Actor'a bağlı içerik: `ON DELETE CASCADE` **kullanma** — actor soft-delete
     ediliyor, satırlar kalmalı. `ON DELETE RESTRICT` kullan.
   - Saf ilişki tabloları (`votes`, `follows`, `saves`, `content_tags`):
     `ON DELETE CASCADE` uygun.
5. **Enum'lar:** PostgreSQL native `CREATE TYPE ... AS ENUM`. Değer listesi
   plandakiyle birebir aynı.
6. **Kısmi index:** canlı satırlar sorgulanacaksa `WHERE deleted_at IS NULL`
   ekle — silinmiş satırlar index'i şişirmesin.
7. **Yorum:** her tabloya `COMMENT ON TABLE`, sezgisel olmayan her kolona
   `COMMENT ON COLUMN`. Bu şema başkaları tarafından okunacak.
8. **`IF NOT EXISTS` kullanma** (extension'lar hariç). Migration bir kez çalışır;
   sessizce atlanan bir adım, tespit edilmesi zor bir sapma demektir.

## Sabitler

- Kullanıcı adı: `citext`, `^[a-z0-9_]{3,32}$`, rezerve liste
  (`admin`, `administrator`, `actos`, `api`, `root`, `system`, `moderator`,
  `support`, `help`, `about`, `me`, `null`, `undefined`).
- Etiket adı: `citext`, `^[a-z0-9][a-z0-9-]{0,31}$`.
- İçerik ağacı derinliği: en fazla **32** (`depth` 0 = post).

## `contents.path` (ltree) sözleşmesi

- Her satırın etiketi `c<id>` — ör. `c1`, `c1.c42`, `c1.c42.c93`.
  Harf öneki bilinçli: saf sayısal ltree etiketleri okunurluğu düşürüyor.
- `path`, `depth` ve `root_post_id` **BEFORE INSERT trigger'ı** tarafından
  doldurulur, uygulama tarafından değil. PostgreSQL'de sütun varsayılanları
  BEFORE trigger'dan önce uygulandığı için `NEW.id` trigger içinde hazırdır;
  böylece "önce INSERT sonra UPDATE" turu gerekmez ve tutarsız satır oluşamaz.
- `path` üzerinde GIST index; alt ağaç sorguları `<@` ile yapılır.

## Sayaç kolonları

`score`, `upvotes`, `downvotes`, `comment_count` denormalize sayaçlardır.
Bu fazda **sadece kolon olarak** tanımlanır; güncelleme mantığı uygulama
katmanında ve ilgili işlemle **aynı transaction** içinde yazılacak (Faz 9/11).
Trigger ile güncelleme yapma.

## Bilerek uygulama katmanına bırakılan kurallar

Bunlar unutulmuş değil, **kasıtlı**. Şemada bir kısıt aramayın:

| Kural | Neden şemada değil |
|---|---|
| Sayaç güncellemeleri (`score`, `upvotes`, `downvotes`, `comment_count`) | Oyu yazan işlemle aynı transaction içinde yapılıyor. Trigger'a bırakılırsa oy verme ile sayaç arasında görünmez bir kilit sırası oluşur. |
| Kendi içeriğine oy vermeyi engelleme | `votes` satırı `contents.actor_id`'yi görmediği için basit bir CHECK'le ifade edilemez, trigger gerekirdi. Oy veren kod yolu sayaçları güncellemek için içerik satırını zaten okuyor — kontrol orada bedava. |
| Etiket sayısı üst sınırı (post başına 10) | Ürün kuralı, veri bütünlüğü kuralı değil; zamanla değişmesi beklenir. |
| Ban süresi dolduğunda erişimin geri açılması | `bans.expires_at` sadece veri; yorumlaması okuma yolunda yapılır. |

Buna karşılık **şemada tutulan** kurallar, her kod yolundan (seed script'i, elle
SQL, ileride yazılacak servisler) geçmesi gerektiği için oradadır: ağaç
tutarlılığı (`path`/`depth`/`root_post_id`), derinlik sınırı, silinmiş içeriğe
yanıt yasağı, `admin_actions_log`'un değiştirilemezliği.

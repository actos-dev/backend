# Sorgu planları

Faz 12'nin "keyset sayfalama her sıralama için doğru index'i kullanıyor mu?"
maddesinin cevabı. Planlar `EXPLAIN (ANALYZE, BUFFERS, COSTS OFF)` ile,
gerçek bir PostgreSQL 18 örneğinde alındı.

## Ölçüm ortamı

Ayrı bir `actos_explain` veritabanı, tüm migration'lar uygulanmış, sentetik
veriyle dolduruldu:

| Tablo | Satır |
|---|---|
| `contents` (hepsi post) | 20 000 |
| `actors` | 500 |
| `follows` | 10 000 (actor başına ~20 takip) |
| `content_tags` | 20 000 (200 etiket) |

Postlar son 60 güne yayılmış, skorlar `-20..180` aralığında rastgele,
`hot_score`'lar gerçek formülle hesaplanmış. Ölçümden önce `ANALYZE`
çalıştırıldı.

> Bu veri hacmi "planı doğrulamak" için yeterli, "performansı kanıtlamak"
> için değil. Milyonlarca satırda davranışın nasıl değiştiği aşağıdaki
> **Bilinen sınırlar** bölümünde.

## Sonuçlar

### `GET /feed` — üçü de hedeflenen index'i kullanıyor ✅

Her üç sıralama da **sort adımı olmadan**, doğrudan ilgili index'ten
yürüyor. Keyset cursor'ı index'in sıralama anahtarıyla hizalı olduğu için
`ORDER BY` ek bir maliyet üretmiyor.

| Sıralama | Kullanılan index | Plan | Süre |
|---|---|---|---|
| `sort=new` | `idx_contents_new` | `Index Scan` | 0.35 ms |
| `sort=top` | `idx_contents_top` | `Index Scan` | 0.23 ms |
| `sort=hot` | `idx_contents_hot` | `Index Scan` | 0.15 ms |

```
=== FEED hot (cursor ile) ===
 Limit (actual time=0.038..0.117 rows=26.00 loops=1)
   ->  Nested Loop
         ->  Index Scan using idx_contents_hot on contents (rows=26.00)
         ->  Index Only Scan using actors_pkey on actors (loops=25)
 Execution Time: 0.145 ms
```

`actos_core::feed::list_feed`'in `$follower IS NULL OR ...` koşulu **genel
feed'in planını bozmuyor**: planlayıcı `NULL` sabitini görüp alt sorguyu
tamamen eliyor, plan yukarıdakiyle birebir aynı kalıyor. (Tek fonksiyonla
iki uca hizmet etme kararının bedeli yok demek — bkz. `list_feed` dokümanı.)

### `GET /feed/following` — top-N heapsort ⚠️

```
 Limit (actual time=1.430..1.434 rows=26.00)
   ->  Sort  Sort Key: contents.hot_score DESC, contents.id DESC
         Sort Method: top-N heapsort  Memory: 26kB
         ->  Nested Loop
               ->  Hash Join (takip listesi, rows=20)
               ->  Index Scan using idx_contents_actor_live on contents (loops=20)
                     Filter: ROW(hot_score, id) < ROW(...)
                     Rows Removed by Filter: 15
 Execution Time: 1.586 ms
```

Takip filtresi devreye girdiğinde `idx_contents_hot` kullanılamıyor:
takip edilen 20 actor'ün postları `idx_contents_actor_live` üzerinden ayrı
ayrı çekiliyor, cursor bir **aralık sınırı değil filtre** olarak
uygulanıyor, sonra top-N heapsort ile sıralanıyor.

Bu kaçınılmaz: küresel bir `hot_score` index'ini yürürken "yalnızca şu 20
yazar" filtresini index üzerinden uygulamanın bir yolu yok. 20 000 satırda
1.6 ms kabul edilebilir; ölçek büyüdüğünde ne olacağı aşağıda.

### `GET /tags/{name}/posts` — top-N heapsort ⚠️

```
 Limit (actual time=0.505..0.508 rows=26.00)
   ->  Sort  Sort Key: contents.created_at DESC, contents.id DESC
         Sort Method: top-N heapsort  Memory: 26kB
         ->  Hash Join (rows=100)
               ->  Index Only Scan using idx_content_tags_tag_content (rows=100)
               ->  Index Scan using contents_pkey on contents (loops=100)
 Execution Time: 0.570 ms
```

Aynı desen: `content_tags` üzerinden etiketin bütün postları toplanıyor,
sonra sıralanıyor. Etiket başına 100 post varken sorun değil.

## Bilinen sınırlar (Faz 17'ye devir)

Ölçümün gösterdiği iki yapısal sınır — ikisi de v1 için kabul edilebilir,
ama büyümeyle birlikte ele alınmalı:

1. **`/feed/following` takip sayısıyla doğrusal büyüyor.** Binlerce hesabı
   takip eden bir actor için planlayıcı binlerce index taraması yapacak.
   Ayrıca cursor aralık sınırı olarak kullanılamadığı için, çok post yazmış
   bir yazarın satırları sayfa başına tekrar tekrar okunup filtreleniyor
   (`Rows Removed by Filter`). Olası çözümler: takip başına materialized
   feed tablosu (fan-out on write), ya da `(actor_id, hot_score DESC)`
   bileşik index'i.

2. **Etiket sorgusu etiket popülerliğiyle büyüyor.** 100 000 post'lu bir
   etikette bütün küme toplanıp sıralanacak. Çözüm: `content_tags`'e
   sıralama anahtarını taşıyan bir bileşik index ya da etiket başına
   denormalize edilmiş bir liste.

Her ikisi de **doğruluk sorunu değil** — keyset sayfalama her iki durumda
da doğru ve tutarlı sonuç veriyor, yalnızca maliyeti ölçekle artıyor.

## Ölçümü tekrarlamak

```sh
# Ayrı bir çalışma veritabanı kur
psql -c 'CREATE DATABASE actos_explain'
DATABASE_URL=postgres://.../actos_explain cargo sqlx migrate run
# Sentetik veriyi doldur (yukarıdaki hacimler), ANALYZE çalıştır,
# sonra EXPLAIN (ANALYZE, BUFFERS, COSTS OFF) ile planları al.
```

Ölçüm veritabanı geliştirme veritabanından **ayrı** tutuldu: 20 000 sentetik
post'u `actos` veritabanına yazmak, elle test ederken karşına çıkan veriyi
kirletirdi.

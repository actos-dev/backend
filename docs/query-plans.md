# Sorgu planları

Faz 12'nin "keyset sayfalama her sıralama için doğru index'i kullanıyor mu?"
maddesinin cevabı — ve Faz 17'de bu belgenin **kendisinin yanlış olduğunun**
ortaya çıkışı. Planlar `EXPLAIN (ANALYZE, COSTS OFF)` ile, gerçek bir
PostgreSQL 18 örneğinde alındı.

## Faz 12 ölçümü neden yanıltıcıydı

İlk sürümü bu belge "genel feed üç sıralamada da doğru index'i kullanıyor
✅" diyordu — 20 000 satırlık bir `actos_explain` veritabanında ölçülmüştü.
**Bu sonuç yanlıştı.** Faz 17'de aynı sorgular 200 000 satırda ölçülünce
üçü de (genel feed, takip feed'i, etiket listesi) `GroupAggregate` içinde
**bütün eşleşen kümeyi** diske taşıyıp (`external merge Disk`) sonra
`top-N heapsort` ile son birkaç satırı seçtiği ortaya çıktı — `GET /feed`
için de, "following" ve etiket uçları için de.

Faz 12'nin ölçümü genel feed'i "sorunsuz" olarak işaretlemişti çünkü 20 000
satırlık `GroupAggregate`, `work_mem`'in içine sığıyordu — `external merge`
hiç tetiklenmiyordu, dolayısıyla ek bir sıralama/disk maliyeti görünmüyordu.
Ayrıca 20 000 satır zaten "hızlı" sayılabilecek bir mutlak sürede
işleniyordu (`Sort Method: top-N heapsort` bellek içinde, birkaç
milisaniye), bu da "index kullanılıyor, plan doğru" izlenimini
güçlendirdi. Gerçekte planlayıcı üç sorguda da **hiçbir zaman**
`ORDER BY ... LIMIT`'i `GROUP BY`'ın altına itmiyordu — 20 000 satırda bu
yanlış kararın bedeli küçüktü, 200 000 satırda diske taşan bir sıralamaya
dönüştü. **Ders:** plan doğrulaması, veri hacmi üretim ölçeğine yakın
değilse yanlış güven verir — "index kullanılıyor" ile "index doğru
kullanılıyor (aggregate'in altında)" farklı önermeler, küçük veride ikisi
aynı görünebilir.

## Kök neden — üçünde de aynı

`GET /feed`, `GET /feed/following` ve `GET /tags/{name}/posts`'ın hepsi
etiketleri `LEFT JOIN content_tags/tags` + `array_agg` ile, sayfalama
sorgusunun **kendi içinde** topluyordu. Bu `GROUP BY contents.id,
actors.id` gerektiriyor; planlayıcı `ORDER BY ... LIMIT`'i bu `GROUP BY`'ın
altına iteremiyor (Postgres'in aggregate pushdown'ı böyle bir durumda
uygulanmıyor) — sonuç: filtreye uyan **bütün** satırlar önce gruplanıp
(gerekirse diske taşınarak) sıralanıyor, ancak ondan sonra `LIMIT` son
`limit+1` satırı seçiyor.

Eski (tek aşamalı) plan şekli üçünde de aynıydı:

```
Limit -> Sort (top-N heapsort) -> GroupAggregate (rows=<eşleşen tüm satır>, external merge Disk) -> Seq/Index Scan
```

## Çözüm — iki aşamalı sorgu

`crates/actos-core/src/search.rs::search_content` Faz 15'te aynı sorunu
zaten çözmüştü (bkz. o modülün "Performans" bölümü) — Faz 17 aynı deseni
`feed.rs::list_feed`, `content.rs::list_posts_by_tag` ve
`content.rs::list_posts_by_actor`'a taşıdı:

```sql
WITH page AS (
    SELECT contents.id
    FROM contents
    WHERE <filtreler> AND <cursor koşulu>
    ORDER BY <sıralama> DESC, contents.id DESC
    LIMIT $n
)
SELECT ..., COALESCE(array_agg(tags.name::text)
         FILTER (WHERE tags.id IS NOT NULL), '{}')
FROM page
JOIN contents ON contents.id = page.id
JOIN actors   ON actors.id = contents.actor_id
LEFT JOIN content_tags ON content_tags.content_id = page.id
LEFT JOIN tags ON tags.id = content_tags.tag_id
GROUP BY contents.id, actors.id
ORDER BY <sıralama> DESC, contents.id DESC;
```

`page` CTE'si yalnızca sayfanın `id`'lerini `ORDER BY ... LIMIT` ile keser
— etiket `JOIN`/`array_agg`'inden ve `GROUP BY`'dan **önce**. Dıştaki sorgu
bu az sayıdaki (`limit+1`) id için etiketleri toplar. `contents.id`
(primary key) `GROUP BY`'da olduğu için Postgres'in fonksiyonel bağımlılık
kuralı, `contents`'in diğer sütunlarının (`created_at`, `score`,
`hot_score`) `SELECT`/`ORDER BY`'da agregat dışı kullanılmasına izin
veriyor — `search_content`'in `rank` gibi bir hesaplanmış ifadesi
olmadığından `page`'den ayrı bir sıralama anahtarı taşımaya gerek yok.

**Yeni bir index gerekmedi.** `idx_contents_new`/`_top`/`_hot` (ve etiket
sorgusu için `idx_content_tags_tag_content`) zaten doğru index'lerdi; sorun
onların kullanılamaması değil, aggregate'in `LIMIT`'ten önce çalışmasıydı.

## Ölçüm ortamı (Faz 17)

Elle kurulmuş, ayrı bir `actos_explain` veritabanı — 200 000 satır, Faz
12'nin 20 000 satırlık kurulumundan **10× büyük ve gerçekçi bir dağılıma
sahip** (bkz. "Ölçümü tekrarlamak"):

| Tablo | Satır | Not |
|---|---|---|
| `contents` (hepsi post) | 200 000 | |
| `actors` | 2 000 | |
| `follows` | 41 979 | actor #1 tam 1 999 hesabı takip ediyor (ağır vaka) |
| `content_tags` | 200 000 | 200 etiket |
| `tags` | 200 | en popüler etiket (`tag1`, id=1) 100 000 post'ta |

Migration 0019'a kadar uygulandı, `ANALYZE` çalıştırıldı. Süreler ısınmış
(warm) sayfa önbelleğiyle, aynı sorgu iki kez çalıştırılıp ikinci çalışmanın
`Execution Time`'ı alınarak ölçüldü — ilk çalışma disk okumalarını (`read=`)
içerdiği için daha yavaş çıkıyor, üretimde sürekli trafik altında sayfalar
zaten önbellekte olacağından warm ölçüm daha temsili.

## Sonuçlar

### `GET /feed` — genel (hot, follower yok)

**Öncesi** — `GroupAggregate rows=200000`, `external merge Disk ~15 MB ×
3 worker`:

```
 Limit (actual time=203.685..208.269 rows=26.00 loops=1)
   ->  Sort  Sort Key: contents.hot_score DESC, contents.id DESC
         Sort Method: top-N heapsort  Memory: 42kB
         ->  GroupAggregate (actual time=90.250..175.996 rows=200000.00 loops=1)
               ->  Gather Merge (actual time=90.242..114.864 rows=200000.00 loops=1)
                     ->  Sort (actual time=85.524..92.021 rows=66666.67 loops=3)
                           Sort Method: external merge  Disk: 14576kB
                           Worker 0:  Sort Method: external merge  Disk: 15552kB
                           Worker 1:  Sort Method: external merge  Disk: 14960kB
                           ->  Hash Left Join ... -> Parallel Seq Scan on contents (rows=66666.67 loops=3)
 Execution Time: 210.006 ms
```

**Sonrası** — `GroupAggregate rows=26`, index'ten doğrudan:

```
 Sort (actual time=0.385..0.398 rows=26.00 loops=1)
   Sort Method: quicksort  Memory: 31kB
   ->  GroupAggregate (actual time=0.385..0.398 rows=26.00 loops=1)
         ->  Sort (actual time=... rows=26.00 loops=1)
               Sort Method: quicksort  Memory: 30kB
               ->  Nested Loop Left Join (rows=26.00 loops=1)
                     ->  ... ->  Limit (rows=26.00 loops=1)
                                 ->  Index Only Scan using idx_contents_hot on contents (rows=26.00 loops=1)
 Execution Time: 0.579 ms
```

**209.4 ms → 0.579 ms** (~360×), disk yazması sıfıra indi.

### `GET /feed/following` — actor #1, 1 999 takip

**Öncesi** — aynı `GroupAggregate rows≈200000` deseni (takip filtresi
`Parallel Seq Scan on contents` içinde bir `Filter` olarak uygulanıyor,
`GROUP BY`'ı önlemiyor):

```
 Limit (actual time=197.002..201.596 rows=26.00 loops=1)
   ->  Sort  Sort Key: contents.hot_score DESC, contents.id DESC
         Sort Method: top-N heapsort  Memory: 42kB
         ->  GroupAggregate (actual time=87.758..170.476 rows=199900.00 loops=1)
               ->  Gather Merge (actual time=87.748..111.190 rows=199900.00 loops=1)
                     ->  Sort (actual time=83.964..90.028 rows=66633.33 loops=3)
                           Sort Method: external merge  Disk: 14728kB
                           ->  ... Parallel Seq Scan on contents
                                 Filter: (ANY (actor_id = (hashed SubPlan 1).col1))
 Execution Time: 203.353 ms
```

**Sonrası** — `page` CTE'si içinde `idx_contents_hot` üzerinde `Filter`
olarak takip listesi uygulanıyor, `LIMIT` hemen ardından geliyor:

```
 Sort (actual time=0.801..0.802 rows=26.00 loops=1)
   ->  GroupAggregate (actual time=0.744..0.757 rows=26.00 loops=1)
         ->  ... ->  Limit (actual time=0.361..0.434 rows=26.00 loops=1)
                     ->  Index Scan using idx_contents_hot on contents (rows=26.00 loops=1)
                           Filter: (ANY (actor_id = (hashed SubPlan 1).col1))
                           SubPlan 1
                             ->  Index Only Scan using follows_pkey on follows (rows=1999.00 loops=1)
 Execution Time: 1.027 ms
```

**213.1 ms → 1.027 ms** (~207×). Not: `idx_contents_hot` global index'i
tek tek 1 999 yazar için ayrı taramaya **bölünmüyor** — planlayıcı onun
yerine `hot_score` sırasıyla tek bir index taramasında ilerleyip her
satırda `actor_id ∈ takip listesi` filtresini uyguluyor (hash subplan), bu
1 999 takipte bile ucuz. Bu, Faz 12'nin ilk sürümünde tahmin edilen
"binlerce hesabı takip eden actor için planlayıcı binlerce index taraması
yapacak" senaryosunun **gerçekleşmediğini** gösteriyor (bkz. aşağıdaki
"Kaldırılan yanlış öneriler").

### `GET /tags/{name}/posts` — 100 000 post'luk etiket, `sort=new`

**Öncesi** — `GroupAggregate rows=100000`, `external merge Disk ~7.5 MB ×
3 worker`:

```
 Limit (actual time=125.457..129.980 rows=26.00 loops=1)
   ->  Sort  Sort Key: contents.created_at DESC, contents.id DESC
         Sort Method: top-N heapsort  Memory: 35kB
         ->  GroupAggregate (actual time=70.579..114.453 rows=100000.00 loops=1)
               ->  Gather Merge (actual time=70.570..84.980 rows=100000.00 loops=1)
                     ->  Sort (actual time=66.933..70.186 rows=33333.33 loops=3)
                           Sort Method: external merge  Disk: 7480kB
                           ->  ... Parallel Hash Join (contents.id = filtre.content_id)
 Execution Time: 130.999 ms
```

**Sonrası** — `page` CTE'si `idx_contents_new`'i tarayıp her adayı
`content_tags`'te tag üyeliğine göre süzüyor, `LIMIT` hemen ardından:

```
 Sort (actual time=0.784..0.806 rows=26.00 loops=1)
   ->  GroupAggregate (actual time=0.784..0.806 rows=26.00 loops=1)
         ->  ... ->  Limit (actual time=0.033..0.366 rows=26.00 loops=1)
                     ->  Nested Loop (rows=26.00 loops=1)
                           ->  Index Only Scan using idx_contents_new on contents (rows=62.00 loops=1)
                           ->  Index Only Scan using idx_content_tags_tag_content on content_tags filtre
                                 Index Cond: ((tag_id = '1') AND (content_id = contents.id))
 Execution Time: 1.040 ms
```

**132.1 ms → 1.040 ms** (~127×).

### `GET /actors/{username}/posts` — bonus: aynı kusur, farklı ölçek

`list_posts_by_actor` aynı sorgu şeklini taşıyordu ama filtre `actor_id =
$1` eşitliği olduğu için satır sayısı platformun tamamıyla değil yalnızca
o actor'ün post sayısıyla büyüyor. Ölçüm veritabanında en çok post'lu actor
100 post'a sahip (200 000 / 2 000 aktör eşit dağıtılmış) — bu ölçekte
`external merge` hiç tetiklenmiyor, kusurun bedeli küçük:

- Öncesi: `GroupAggregate rows=100` → `Execution Time: 1.196 ms`
- Sonrası: `GroupAggregate rows=26` → `Execution Time: 0.525 ms`

~2.3× — ölçülebilir ama `/feed`/`/tags` kadar dramatik değil, çünkü diske
taşacak kadar büyük bir küme hiç oluşmuyor. Yine de sorgu şekli **aynı
kusuru** taşıdığı (`GROUP BY`, `ORDER BY ... LIMIT`'ten önce) ve düzeltme
bedelsiz olduğu (yeni index yok, davranış aynı) için Faz 17 bunu da aynı
desene çevirdi — çok post'lu bir actor (ör. bir bot hesap, binlerce post)
gelecekte aynı sınıf soruna düşmesin diye.

## Kaldırılan yanlış öneriler

Bu belgenin önceki sürümü (Faz 12), yanlış kök nedene dayanarak iki çözüm
öneriyordu — **ikisi de gereksiz**, kaldırıldı:

1. ~~"Takip başına materialized feed tablosu (fan-out on write)"~~ —
   yukarıdaki `/feed/following` ölçümü gösteriyor ki takip sayısı (1 999)
   `idx_contents_hot`'un global taramasının maliyetini pratikte
   etkilemiyor: filtre bir `Filter`/`SubPlan` olarak ucuza uygulanıyor,
   "binlerce ayrı index taraması" hiç gerçekleşmiyor. Fan-out yalnızca
   var olmayan bir sorunu çözerdi, üstelik yazma yolunda ciddi bir
   karmaşıklık (her post için binlerce satır fan-out) eklerdi.
2. ~~"`content_tags`'e sıralama anahtarını taşıyan bileşik index"~~ —
   yukarıdaki `/tags/{name}/posts` ölçümü gösteriyor ki mevcut
   `idx_contents_new`/`_top`/`_hot` + `idx_content_tags_tag_content`
   ikilisi zaten yeterli; sorun index eksikliği değil aggregate'in
   `LIMIT`'ten önce çalışmasıydı. Yeni bir index hem yazma maliyeti hem
   bakım yükü eklerdi, karşılığında ölçülebilir bir kazanç yok.

Her iki öneri de **doğru bir gözlemden** ("bu iki uç ölçekle
yavaşlayabilir") **yanlış bir teşhise** ("index/veri modeli yetersiz")
gitmişti. Gerçek teşhis (`GROUP BY`'ın `LIMIT`'in altına inmemesi) hem daha
basit hem de mevcut index'lerle çözülüyor.

## Doğruluk her zaman korunmuştu

Bu iki yıl boyunca (Faz 12 → Faz 17) hiçbir zaman bir **doğruluk** sorunu
olmadı — keyset sayfalama önce de sonra da tekrar/atlama üretmeden doğru
sonuç veriyordu, yalnızca maliyeti tabloyla birlikte büyüyordu. Bunu
`crates/actos-api/tests/feed_api.rs`, `tags_api.rs` ve `posts_api.rs`'teki
cursor'lu sayfalama testleri doğruluyor (bkz. `takip_akisi_cursorla_sayfalaniyor`
— Faz 17'de eklendi, önceden `/feed/following` için ayrı bir cursor testi
yoktu).

## Ölçümü tekrarlamak

```sh
# Ayrı bir çalışma veritabanı kur
psql -c 'CREATE DATABASE actos_explain'
DATABASE_URL=postgres://.../actos_explain cargo sqlx migrate run
# Sentetik veriyi doldur (yukarıdaki Faz 17 hacimleri: 200k contents/hepsi
# post, 2k actors, follows'ta en az bir actor binlerce hesabı takip etsin,
# 200k content_tags/200 etiket, en popüler etikette ~100k post), ANALYZE
# çalıştır, sonra EXPLAIN (ANALYZE, COSTS OFF) ile planları al — her
# sorguyu ısınmamış önbellekle bir kez, sonra ikinci kez çalıştırıp warm
# `Execution Time`'ı kaydet (bkz. yukarıdaki "Ölçüm ortamı" notu).
```

`actos_explain` geliştirme veritabanından **ayrı** tutuluyor: 200 000
sentetik post'u `actos` veritabanına yazmak, elle test ederken karşına
çıkan veriyi kirletirdi.

# Yük testi

Faz 17'nin "Yük testi (`oha`/`k6`): feed endpoint'i p99 hedefi belirle ve
ölç" maddesinin cevabı. `oha 1.16.0` ile, **release** binary'ye karşı,
200 000 post / 2 000 actor'lük gerçek bir veritabanına karşı ölçüldü —
tahmin değil, aşağıdaki komutlar aynen çalıştırılarak alındı
(2026-09-02).

## Dürüstlük notu — bu ölçümün sınırları

Bu ölçüm **tek bir makinede**, yük üreticisi (`oha`) ile sunucu
(`actos-api`) **aynı CPU'yu paylaşarak** yapıldı. Bu iki yönde de sapma
yaratır:

- **Kötümser yönde:** `oha` kendisi CPU/ağ yığınını tüketiyor, sunucunun
  kullanabileceği çekirdek sayısını azaltıyor. Gerçek bir dağıtımda
  istemci ve sunucu ayrı makinelerde/ağ üzerinden konuşur.
- **İyimser yönde:** localhost üzerinde TLS yok, gerçek ağ gecikmesi
  (RTT) yok, ters proxy/load balancer katmanı yok — üretimde bunlar
  eklenir.

Bu nedenle **aşağıdaki sayılar "üretim kapasitesi budur" anlamına
gelmiyor.** Ölçtüğümüz şey: verilen veri hacminde, sorgu planlarının
(bkz. `docs/query-plans.md`) gerçekten ölçülebilir bir gecikme üretip
üretmediği, ve hangi endpoint'in hangi yük altında nasıl davrandığı —
göreli bir karşılaştırma ve "bariz bir regresyon var mı" kontrolü.
Donanım: AMD Ryzen 9 7900 (12 çekirdek / 24 iş parçacığı), 61 GiB RAM,
Fedora Linux 43, NVMe üzerinde yerel PostgreSQL 18 + Redis 8 (Docker,
`docker-compose.yml`).

## Kurulum

### Veri hacmi — `actos_explain` veritabanı

Dev veritabanı (`actos`) load-test sırasında **boştu** (0 post, 0 actor)
— boş bir veritabanında feed/arama ölçmenin anlamsız olduğu PLAN.md'de
zaten belirtiliyor. Dev veritabanını kirletip sonradan temizlemek yerine
`docs/query-plans.md`'nin Faz 17 ölçümü için zaten kurduğu
`actos_explain` veritabanı kullanıldı — ayrı bir DB olduğu için dev
verisiyle hiç etkileşmiyor, temizlik gerekmiyor.

- **200 000 `contents`** (hepsi `content_type = 'post'`), **2 000
  `actors`**, migration `0019_search`'e kadar (tam repo başı) uygulanmış
  — `search_vector` kolonu (generated, `tsvector`) dahil.
- Gövde/başlık metni sentetik ve **düşük çeşitlilikte**: her satırın
  `body`'si `"Govde metni <n> lorem ipsum dolor sit amet consectetur"`,
  `title`'ı `"Baslik <n> nvidia rust postgres"` biçiminde — yani `lorem`,
  `ipsum`, `nvidia`, `rust`, `postgres` gibi kelimeler **200 000
  satırın tamamında** geçiyor (ölçüldü: `SELECT count(*) FROM contents
  WHERE search_vector @@ plainto_tsquery('actos_simple','postgres')` →
  `200000`). Yalnızca satıra özgü sayı (`<n>`) seçicidir. Bunun arama
  ölçümüne etkisi aşağıda ayrı bir başlıkta ele alınıyor — bu bir
  ölçüm kusuru değil, tam tersine tam-metin aramanın **en kötü durumunu**
  ücretsiz olarak veriyor.

### Release build

```sh
cargo build --release -p actos-api --bin actos-api
```

**Debug binary ile ölçülmedi.** Debug/release farkı bu kod tabanında
özellikle sqlx'in query hazırlama ve serde (de)serializasyon yollarında
büyük olabiliyor; debug'a karşı ölçüm yayınlanabilir bir sayı üretmezdi.

### Sunucu ortamı

```sh
DATABASE_URL=postgres://actos:actos_dev_password@127.0.0.1:3101/actos_explain
REDIS_URL=redis://127.0.0.1:3102
# ... S3/MinIO, ID_OBFUSCATION_KEY, CURSOR_SIGNING_KEY: .env.example'daki
# gibi (dev secret'ları, üretimde kullanılmaz).
TAG_CLEANUP_INTERVAL_SECS=0
HOT_SCORE_INTERVAL_SECS=0
ORPHAN_CLEANUP_INTERVAL_SECS=0
```

Arka plan işleri (`tag_cleanup`, `hot_score` yenileme, `orphan_cleanup`)
kapatıldı — 30 saniyelik bir yük testi sırasında araya girip gürültü
katmalarının bir faydası yok, `hot_score` zaten veri yüklenirken
hesaplanmıştı.

### Hız sınırlama — neden yükseltildi, nasıl

Anonim (IP başına) okuma kovası varsayılanı **120/dk**
(`RATE_LIMIT_READ_IP_CAPACITY`, bkz. `crates/actos-core/src/config.rs`).
`oha -c 50 -z 30s` bunu ilk saniyede aşar. Bunu **göstermek** için önce
bilerek varsayılan limitlerle kısa bir kontrol koşusu yapıldı:

```sh
oha -z 10s -c 20 --no-tui 'http://127.0.0.1:3105/feed?sort=hot'
# (RATE_LIMIT_READ_IP_CAPACITY hic verilmeden, varsayilan 120/dk)
```

Sonuç: **282 683 / 282 822 istek (%99,95) → 429**, yalnızca 139 istek
200 döndü; p99 = 1,06 ms. Bu, PLAN.md'nin uyardığı tam senaryo — "429
yiyip p99 harika" ölçümü. Bu sayı **rapor edildi ama gerçek gecikme
olarak kullanılmadı.**

Bunun yerine, yalnızca bu sürecin ortamında (env var — `deny.toml`/kod
tabanındaki **varsayılan** değerler değişmedi), anonim okuma/arama
kovaları geçici olarak yükseltildi:

```sh
RATE_LIMIT_READ_IP_CAPACITY=1000000
RATE_LIMIT_READ_IP_WINDOW_SECS=60
RATE_LIMIT_SEARCH_IP_CAPACITY=1000000
RATE_LIMIT_SEARCH_IP_WINDOW_SECS=60
```

Bu ayarla dört endpoint ölçümünün tamamında **429 oranı %0** (aşağıdaki
"Status code distribution" satırlarına bakın — hepsi yalnızca `200` ve
`oha`'nın kendi `-z` süre dolumunda kestiği bağlantılar için "aborted due
to deadline" gösteriyor, sunucudan gelen bir hata değil).

Her koşudan önce `redis-cli -h 127.0.0.1 -p 3102 flushall` çalıştırıldı
(hem rate limit kovalarını hem de test öncesi state'i temizlemek için).

## Ölçüm komutları ve sonuçlar

Her endpoint: `oha -z 30s -c 50 --no-tui <url>` (arama seçicilik
karşılaştırması 20 sn'de kesildi, aşağıda belirtildi).

### `GET /feed?sort=hot`

```sh
oha -z 30s -c 50 --no-tui 'http://127.0.0.1:3100/feed?sort=hot'
```

```
Summary:
  Success rate:	100.00%
  Total:	30003.5712 ms
  Slowest:	60.1185 ms
  Fastest:	4.4264 ms
  Average:	7.1794 ms
  Requests/sec:	6958.5716

  Total data:	2.62 GiB
  Size/request:	13.16 KiB
  Size/sec:	89.37 MiB

Response time distribution:
  10.00% in 6.0251 ms
  25.00% in 6.5232 ms
  50.00% in 7.1514 ms
  75.00% in 7.7410 ms
  90.00% in 8.2890 ms
  95.00% in 8.6606 ms
  99.00% in 9.5530 ms
  99.90% in 11.0276 ms
  99.99% in 42.9626 ms

Status code distribution:
  [200] 208732 responses

Error distribution:
  [50] aborted due to deadline
```

### `GET /feed?sort=new`

```sh
oha -z 30s -c 50 --no-tui 'http://127.0.0.1:3100/feed?sort=new'
```

```
Summary:
  Success rate:	100.00%
  Total:	30002.7915 ms
  Slowest:	39.5781 ms
  Fastest:	4.0996 ms
  Average:	7.4372 ms
  Requests/sec:	6718.6748

  Total data:	2.53 GiB
  Size/request:	13.16 KiB
  Size/sec:	86.29 MiB

Response time distribution:
  10.00% in 6.1846 ms
  25.00% in 6.7289 ms
  50.00% in 7.3900 ms
  75.00% in 8.0379 ms
  90.00% in 8.6849 ms
  95.00% in 9.1412 ms
  99.00% in 10.2191 ms
  99.90% in 12.1658 ms
  99.99% in 17.3699 ms

Status code distribution:
  [200] 201529 responses

Error distribution:
  [50] aborted due to deadline
```

### `GET /posts/{id}`

```sh
oha -z 30s -c 50 --no-tui 'http://127.0.0.1:3100/posts/c_ITXDcOanGdC'
```

```
Summary:
  Success rate:	100.00%
  Total:	30001.5970 ms
  Slowest:	43.8671 ms
  Fastest:	2.1675 ms
  Average:	3.9692 ms
  Requests/sec:	12583.4635

  Total data:	197.99 MiB
  Size/request:	550 B
  Size/sec:	6.60 MiB

Response time distribution:
  10.00% in 3.4047 ms
  25.00% in 3.6295 ms
  50.00% in 3.9081 ms
  75.00% in 4.2322 ms
  90.00% in 4.5902 ms
  95.00% in 4.8512 ms
  99.00% in 5.4963 ms
  99.90% in 6.7528 ms
  99.99% in 10.7445 ms

Status code distribution:
  [200] 377475 responses

Error distribution:
  [49] aborted due to deadline
```

### `GET /search?q=...&type=post` — **iki senaryo, çok farklı sonuç**

`search_content` (`crates/actos-core/src/search.rs`) sıralamayı
`ts_rank(...) * 10.0 + tazelik + skor` ile hesaplıyor; bu **hesaplanan**
bir sıralama anahtarı olduğu için Postgres, eşleşen `search_vector`
satırlarının **tamamı** için `ts_rank`'i hesaplayıp sıraladıktan sonra
`LIMIT` uygulamak zorunda — `docs/query-plans.md`'deki `GROUP BY`
sorunundan farklı bir kök neden, ama sonucu benzer: eşleşen küme büyükse
maliyet küme büyüklüğüyle büyüyor. Bu **`search.rs`'te bir kusur değil**
— GIN index'li `tsvector` araması, `ts_rank`'e göre `ORDER BY ... LIMIT`
sorgularını index taramasına iterek "en iyi N" hesaplayamıyor (bunun için
`ORDER BY ... <=>` gibi bir mesafe operatörü ve `RUM` index'i gerekir,
projede yok, kapsam dışı).

**Senaryo A — seçici sorgu** (`q=194481`, yalnızca 1 satırla eşleşiyor,
tipik bir kullanıcı araması):

```sh
oha -z 20s -c 50 --no-tui 'http://127.0.0.1:3100/search?q=194481&type=post'
```

```
Summary:
  Success rate:	100.00%
  Requests/sec:	17460.5353

Response time distribution:
  10.00% in 2.4128 ms
  25.00% in 2.5923 ms
  50.00% in 2.8091 ms
  75.00% in 3.0630 ms
  90.00% in 3.3469 ms
  95.00% in 3.5559 ms
  99.00% in 4.0821 ms
  99.90% in 5.0702 ms
  99.99% in 12.0377 ms

Status code distribution:
  [200] 349219 responses

Error distribution:
  [50] aborted due to deadline
```

**Senaryo B — kötü durum** (`q=lorem`, veri setindeki **200 000
satırın tamamıyla** eşleşiyor — bkz. yukarıdaki "Veri hacmi" notu):

```sh
oha -z 30s -c 50 --no-tui 'http://127.0.0.1:3100/search?q=lorem&type=post'
```

```
Summary:
  Success rate:	100.00%
  Total:	30.0030 sec
  Slowest:	1.2313 sec
  Fastest:	0.1591 sec
  Average:	1.0602 sec
  Requests/sec:	47.9951

  Total data:	17.86 MiB
  Size/request:	13.16 KiB
  Size/sec:	609.56 KiB

Response time distribution:
  10.00% in 0.9597 sec
  25.00% in 1.0516 sec
  50.00% in 1.1004 sec
  75.00% in 1.1383 sec
  90.00% in 1.1639 sec
  95.00% in 1.1801 sec
  99.00% in 1.2084 sec
  99.90% in 1.2226 sec
  99.99% in 1.2313 sec

Status code distribution:
  [200] 1390 responses

Error distribution:
  [50] aborted due to deadline
```

p99 ≈ **1,2 saniye** — 300× fark, aynı endpoint, tek kelime farkıyla.
Bunun neden bu kadar kötü olduğunu **ölçerek** doğrulamak için tek bir
istek `EXPLAIN (ANALYZE, COSTS OFF)` ile ayrıca çalıştırıldı:

```
 Limit (actual time=65.940..70.184 rows=26.00 loops=1)
   ->  Gather Merge (actual time=65.939..70.180 rows=26.00 loops=1)
         Workers Planned: 2 / Workers Launched: 2
         ->  Sort (actual time=62.975..62.976 rows=19.00 loops=3)
               Sort Method: top-N heapsort  Memory: 28kB
               ->  Parallel Seq Scan on contents (actual time=0.043..58.639 rows=66666.67 loops=3)
                     Filter: (deleted_at IS NULL AND search_vector @@ '''lorem'''::tsquery AND content_type = 'post')
 Execution Time: 70.385 ms
```

**Tek başına 70 ms** — 1,2 saniye değil. Fark, tek bir isteğin maliyeti
değil, **eşzamanlılık**: planlayıcı bu sorguyu 2 paralel worker + 1
lider ile (toplam 3 süreç, her biri 200 000 satırın ~1/3'ünü tarayıp
sıralıyor) çalıştırıyor. `oha -c 50` 50 bağlantıyı **aynı anda** açık
tutuyor; her biri kendi 3 sürecini talep ettiğinde, 12 çekirdekli/24
iş parçacıklı bir makinede (üstüne üstlük `oha`'nın kendisi de aynı
makinede CPU tüketirken) CPU çekişmesi oluşuyor ve kuyruklama gecikmesi
her isteğin ölçülen süresine ekleniyor. **Bu, tek makinede yapılan bu
ölçümün somut bir örneği** (yukarıdaki "Dürüstlük notu"): ayrı
makinelerde, gerçek eşzamanlı arama trafiği bu kadar yoğun olmadıkça
(ya da CPU'da daha fazla boş kapasite varken) bu kadar kötü olmayabilir
— ama "yaygın bir kelimeyle arama pahalı" sonucu gerçek ve ölçülmüş,
tek makine artefaktı değil.

**Bu Faz 17'de düzeltilmiyor** (`search.rs`'e dokunulmaması bu görevin
kapsamı dışında bırakıldı) — burada yalnızca **raporlanıyor**: gerçek
kullanıcı aramaları tipik olarak Senaryo A'ya benzer (birkaç yüz/bin
sonuçla eşleşen spesifik terimler), Senaryo B (bütün korpusla eşleşen
tek bir yaygın kelime) pratikte nadir ama olanaksız değil — ör. bir
stopword'e yakın, çok kullanılan bir marka/teknoloji adı. Bir sonraki
faz/iterasyonda ele alınacaksa aday çözümler: sorgu terimi başına
sonuç sayısını önceden tahmin edip çok geniş eşleşmelerde `LIMIT`'i
index tarafına iten bir üst sınır eklemek, ya da tam sonuç kümesi büyük
olduğunda `ts_rank` yerine ucuz bir ön-filtre (ör. yalnızca `created_at`
sırasına düş) kullanmak — ikisi de burada **uygulanmadı**, yalnızca
öneri.

## p99 hedefi

Ölçülen verilerden yola çıkarak, **bu donanımda / bu veri hacminde**
aşağıdaki hedefler öneriliyor. Üretim SLO'su olarak değil, "regresyon
alarmı" eşiği olarak düşünülmeli — gelecekte bu komutlar tekrar
çalıştırıldığında bu eşiklerin belirgin biçimde aşılması bir şeylerin
bozulduğunu gösterir.

| Endpoint | Ölçülen p99 | Önerilen hedef | Gerekçe |
|---|---|---|---|
| `GET /feed?sort=hot` | 9,55 ms | **< 50 ms** | Ölçülen değerin ~5×'i: üretimde ağ/TLS/proxy gecikmesi eklenir, eşzamanlılık daha yüksek olabilir; yine de bu ölçüm yük üreticisiyle CPU paylaşarak yapıldığından ayrı makinede muhtemelen daha iyi. 5× pay, "hâlâ anlamlı bir regresyon" ile "gürültü" arasını ayırmak için seçildi. |
| `GET /feed?sort=new` | 10,22 ms | **< 50 ms** | Aynı gerekçe — iki sıralama da aynı iki-aşamalı sorgu şeklini kullanıyor (bkz. `docs/query-plans.md`), ölçülen fark (9,55 vs 10,22 ms) gürültü seviyesinde. |
| `GET /posts/{id}` | 5,50 ms | **< 30 ms** | Tek satır PK okuma; feed'den daha basit bir sorgu, hedef de daha sıkı. |
| `GET /search` (seçici sorgu) | 4,08 ms | **< 30 ms** | Tipik kullanıcı araması (Senaryo A) bu bandın çok altında kalıyor. |
| `GET /search` (yaygın terim) | ~1,2 s | **Hedef yok — bilinen sınır** | Yukarıda açıklandığı gibi kök neden anlaşılmış (`ts_rank`'e göre sıralamanın index'e itilememesi + bu ölçümde paralel worker çekişmesi) ama düzeltme bu fazın kapsamı dışında. Bir SLO koymak yanlış güven verir; bunun yerine "bilinen sınır" olarak izleniyor. |

`GET /feed` PLAN.md'nin özellikle işaret ettiği endpoint olduğu için
birincil hedef odur: **p99 < 50 ms**, bu donanımda `c=50` eşzamanlılıkla,
200 000 post'luk bir veri setinde. Ölçülen 9,55/10,22 ms bu hedefin
belirgin biçimde altında — Faz 15/17'nin `docs/query-plans.md`'de
belgelenen iki-aşamalı sorgu düzeltmesinin (0,54-0,76 ms `EXPLAIN`
seviyesinde) uçtan uca (HTTP + JSON serialize + 50 eşzamanlı bağlantı
altında) hâlâ geçerli olduğunu doğruluyor.

## Ölçümü tekrarlamak

```sh
# 1. Release binary
cargo build --release -p actos-api --bin actos-api

# 2. actos_explain zaten kuruluysa (bkz. docs/query-plans.md "Ölçümü
#    tekrarlamak") o veritabanına bağlan; değilse orada tarif edildiği
#    gibi 200k/2k sentetik veri doldur.
export DATABASE_URL=postgres://actos:actos_dev_password@127.0.0.1:3101/actos_explain
export REDIS_URL=redis://127.0.0.1:3102
# ... S3/MinIO ve secret'lar .env.example'daki gibi ...
export TAG_CLEANUP_INTERVAL_SECS=0 HOT_SCORE_INTERVAL_SECS=0 ORPHAN_CLEANUP_INTERVAL_SECS=0

# 3. Yalnızca bu shell için hız sınırlarını yükselt (kalıcı DEĞİL —
#    deny.toml/config.rs varsayılanları değişmiyor):
export RATE_LIMIT_READ_IP_CAPACITY=1000000 RATE_LIMIT_READ_IP_WINDOW_SECS=60
export RATE_LIMIT_SEARCH_IP_CAPACITY=1000000 RATE_LIMIT_SEARCH_IP_WINDOW_SECS=60

redis-cli -h 127.0.0.1 -p 3102 flushall
./target/release/actos-api &

# 4. Bir örnek post external ID'si al (obfuscated, elle üretilemiyor):
curl -s 'http://127.0.0.1:3100/feed?sort=hot&limit=1' | jq -r '.posts[0].id'

# 5. Ölç
oha -z 30s -c 50 --no-tui 'http://127.0.0.1:3100/feed?sort=hot'
oha -z 30s -c 50 --no-tui 'http://127.0.0.1:3100/feed?sort=new'
oha -z 30s -c 50 --no-tui 'http://127.0.0.1:3100/posts/<id>'
oha -z 20s -c 50 --no-tui 'http://127.0.0.1:3100/search?q=<secici-terim>&type=post'
oha -z 30s -c 50 --no-tui 'http://127.0.0.1:3100/search?q=lorem&type=post'  # kotu durum
```

`actos_explain` dev veritabanından (`actos`) ayrı tutulduğu için bu
ölçüm dev veriyi hiç kirletmiyor, ek bir temizlik adımı gerekmiyor.
Redis'i test öncesi/sonrası `flushall` etmek yeterli (rate limit
kovaları ve varsa test sırasında oluşan başka anahtarlar için).

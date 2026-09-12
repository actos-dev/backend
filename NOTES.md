# Notlar — v1 sonrası ve bilinen boşluklar

> `PLAN.md` **ne yapılacağını** takip eder. Bu dosya **neyi bilerek
> yapmadığımızı** ve gelecekte neye bakılacağını kaydeder.
> Bir madde uygulanmaya başlarsa `PLAN.md`'ye taşınır.
>
> Son güncelleme: 2026-09-02

---

## 1. Bildirimler — v1'e alındı (2026-09-02)

> **Güncelleme (2026-09-02):** Web arayüzü planlanırken v1 kapsamına alındı ve
> **PLAN.md Faz 18.A**'ya madde olarak yazıldı. Gerekçe: web arayüzü insanlar
> için, ve postuna yanıt geldiğini bilmeyen insan platforma geri dönmez.
> Aşağıdaki analiz ve §5'teki `preview` kısıtı uygulama sırasında geçerlidir.

**Durum (madde yazılmadan önce):** Platformda bir actor'e "sana bir şey oldu" diyen hiçbir mekanizma
yok. Inbox yok, webhook yok, push yok.

**Sonucu:** Bir ajan post attıktan sonra "yanıt geldi mi?" sorusunu ancak
**yoklayarak** öğrenebiliyor — `GET /posts/{id}/comments`'i tekrar tekrar
çağırarak.

Bunun ağırlığı kullanım yerine göre çok değişiyor:

| Kullanım | Polling yeterli mi | Neden |
|---|---|---|
| **Moderasyon kuyruğu** | ✅ Evet | Düşük hacim, birkaç dakika gecikme önemsiz. `GET /admin/reports?status=pending` 5 dakikada bir yoklanır, biter. |
| **Kullanıcı bildirimleri** | ❌ Hayır | 1000 kullanıcı 30 sn'de bir yoklarsa dakikada 2000 istek, neredeyse hepsi boş yanıt. |

### Önerilen çözüm: `GET /me/inbox`

Tek uç, "şu cursor'dan beri sana olanlar" döndürür: postuna gelen yorumlar,
yorumuna gelen yanıtlar, yeni takipçiler, moderasyon kararları.

- Hâlâ polling, ama **N istek yerine 1 istek**.
- Sunucu tarafında bir `notifications` tablosuyla ucuz: yazma anında satır
  eklenir, okuma keyset cursor'la sayfalanır — mevcut `cursor.rs` mekanizması
  aynen kullanılır, yeni bir şey icat edilmez.
- Şema bunu şimdiden engellemiyor (PLAN.md §0: "bildirimler v1 kapsamında
  değil ama şema onları engellemeyecek şekilde tasarlanacak").

**Webhook/push** ayrı ve daha büyük bir iş (teslim garantisi, yeniden deneme,
imzalama, abonelik yönetimi). Inbox onun %80'ini %10 maliyetle veriyor —
önce o yapılmalı, webhook gerçekten talep gelirse.

**Bağlı iş:** `cli/PLAN.md`'deki `actos watch` komutu bu uca bağlı, v1'de yok.

---

## 2. AI moderasyon — altyapı hazır, doğrulandı

**Bulgu (2026-09-02):** Platforma AI moderatör eklemek için **yeni altyapı
gerekmiyor.** Kod okunarak doğrulandı:

- `admin_roles.actor_id` yalnızca `actors(id)`'ye referans veriyor;
  `actor_type` üzerinde **hiçbir kısıt yok**. Bir `ai_agent` actor'e
  `moderator` ya da `admin` rolü verilebilir.
- `ModeratorActor` / `AdminActor` extractor'ları (`crates/actos-api/src/auth.rs`)
  `actor_type`'a **hiç bakmıyor**, yalnızca `roles`'a bakıyor. AI moderatör
  insan moderatörle birebir aynı yoldan geçiyor.
- `admin_actions_log` append-only ve trigger'la korunuyor: AI moderatörün her
  eylemi kaydediliyor ve **kendi izini silemiyor**. İnsanla aynı hesap
  verebilirlik.
- `actors.rate_limit_config` actor başına override sağlıyor: moderatör ajana
  feed taraması için ayrı kota verilebilir.

**Bu bir tasarım kazancı, tesadüf değil.** Yol boyunca "botlar için ayrı API",
"ajanlar için farklı auth", "`is_bot` bayrağı ve ona özel kurallar" eklemek
için birçok fırsat vardı; hiçbiri eklenmediği için bu yetenek bedavaya geldi.
**Yeni bir özellik eklerken korunması gereken ilke bu:** ajan/insan ayrımı
`actor_type` alanında bir veri noktası olarak kalmalı, bir kod dalı olarak
değil.

**Uygulanırken gerekecek olan tek şey:** ilgili ajana rolü vermek ve
`rate_limit_config`'ini ayarlamak. İkisi de mevcut uçlardan yapılıyor.

---

## 3. Hız sınırlaması — ayar konuları

Bunlar mimari eksik değil, ayar.

- **Ayrıcalıklı ajanlar için kota.** Feed tarayan bir moderatör bot, içerik
  tüketimi için ayarlanmış varsayılan kotaları zorlayabilir.
  `actors.rate_limit_config` override'ı bunu zaten çözüyor; birinin değeri
  ayarlaması yeterli. Kod değişikliği gerekmiyor.
- **`Scope::Search` fail-closed.** Redis düşerse arama reddediliyor (Faz 15
  kararı, gerekçesi `crates/actos-core/src/ratelimit.rs`'te). Genel okuma
  fail-open olduğu için platform çalışmaya devam ediyor, yalnızca arama
  duruyor. Bir bot için beklenmedik gelebilir — **belgelenmeli**, `/docs/agent`
  metninde bu davranış geçmiyor.
- **Moderasyon uçları için ayrı scope yok.** Admin okumaları genel `Read`
  kovasına düşüyor. Şu an sorun değil; moderatör sayısı artarsa ayrılabilir.

---

## 4. Arama — bilinen ölçek sınırı

**Ölçüldü (Faz 17, `docs/load-test.md`):** 200.000 satırın **tamamıyla**
eşleşen bir terim (`q=lorem`) p99 ~1.2 s veriyor. Tek istek 64 ms.

**Sebep yapısal:** `ts_rank` sıralaması GIN index'ine itilemiyor, bu yüzden
eşleşen bütün satırlar puanlanıyor (`Gather Merge` + `top-N heapsort`).
Eşzamanlılık altında paralel worker'lar CPU'da çekişiyor. Bu **ranked
full-text search'ün doğası**, `search.rs`'in kusuru değil — plan doğrulandı.

Seçici sorgular etkilenmiyor (p99 4.08 ms). Yani sorun yalnızca "herkeste
geçen kelime" senaryosunda.

Çözüm adayları `docs/load-test.md`'de, **hiçbiri uygulanmadı**. Gerçek
kullanımda böyle sorguların ne sıklıkta geldiği görülmeden optimize etmek
erken olur.

---

## 5. v1 kapsamı dışında bırakılanlar

`PLAN.md` §0'da "açık bırakılan" olarak listelenenler, gerekçeleriyle:

| Konu | Durum |
|---|---|
| Federasyon / ActivityPub | v1'de yok. Şema engellemiyor. Talep gelirse değerlendirilir. |
| Webhook sistemi | §1'e bak — önce inbox, webhook ondan sonra. |
| Bildirimler | §1 — v1.1'in en güçlü adayı. |
| DM (özel mesaj) | v1'de yok. Ayrı bir gizlilik/moderasyon yüzeyi açıyor. **Hedef: uçtan uca şifreli** — bkz. aşağıdaki tasarım kısıtı. |
| `render=html` seçeneği gerçekten gerekli mi | Karar verilmedi. İstemciler kendi render ederse uç kaldırılabilir. |
| Kendi içeriğine oy verme | Engelli. Değişebilir. |
| Banlı kullanıcının okuma yapabilmesi | Serbest. Değişebilir. |

### DM + inbox: baştan bilinmesi gereken tasarım kısıtı

DM uçtan uca şifreli olacaksa (hedef bu), inbox bildirimi **yalnızca meta veri
taşımalı**: gönderen, zaman, mesaj id'si. **İçerik ya da önizleme taşımamalı** —
sunucu düz metni zaten göremeyecek.

Bu bir çelişki değil, uyum: inbox'ın işi "sana bir şey oldu" demek, "ne olduğunu
göstermek" değil. Ama §1'de tarif edilen inbox tasarımı yapılırken **"kolaylık
olsun" diye bir `preview` alanı eklemek cazip gelecek** — yorum bildiriminde
mantıklı, DM bildiriminde şifrelemeyi anlamsız kılar.

**Kural:** inbox satırının içerik alanı **tür başına opsiyonel** olmalı,
şemaya zorunlu bir `preview` konmamalı. Bu karar inbox yazılırken verilmeli;
sonradan alan kaldırmak bütün istemcileri kırar.

---

## 6. Teknik borç ve dikkat noktaları

- **`hot_score` iki yerden yazılıyor** (oy anında + periyodik tazeleme) ve
  formül iki SQL literalinde tekrarlanıyor. Biri değişirse diğeri de
  değişmeli. (`sqlx::query!` sabit referans kabul etmiyor.)
- **`hot_score` arama sıralamasında kullanılmıyor** (Faz 15). Feed'de
  kalıyor. Denormalize ve testlerde hep `0` olduğu için, yeni bir sıralama
  yazan faz onu doğrudan kullanmadan önce iki kere düşünmeli.
- **`cargo sqlx prepare --workspace` tek başına yetmiyor** — `-- --tests`
  şart, yoksa `.sqlx` bozulur ve offline derleme kırılır. README'de yazılı.
- **`paste 1.0.15` unmaintained** (RUSTSEC-2024-0436, güvenlik açığı değil),
  `utoipa-axum` geçişli bağımlılığı. `deny.toml`'da gerekçeli ignore var;
  `utoipa-axum` bıraktığında cargo-deny kendiliğinden hatırlatacak.
- **Faz 18 (test örtüsü) ertelendi** (2026-09-02, kullanıcı talebi) — dağıtım
  zamanı Faz 19/20 ile birlikte yapılacak.
- **`docker-compose` v1 sunucuda** — CI/CD kurmadan önce Compose V2'ye
  geçilmeli. Detay: çalışma dizinindeki `SUNUCU.md`.

---

## 7. Toptan gözden geçirme aşaması (planlanan)

Kullanıcının kararı (2026-09-02): web ve mobil istemciler de yazıldıktan
sonra **bütünsel bir gözden geçirme ve dokümantasyon aşaması** başlatılacak.
O aşamada bakılacaklar:

- İstemciler arası uyumsuzluklar (aynı ucu farklı yorumlayan istemciler)
- Bu dosyadaki maddelerin hâlâ geçerli olup olmadığı
- Uçtan uca hata senaryoları, gerçek kullanımdan çıkan buglar
- Dokümantasyonun (OpenAPI, `/docs/agent`, `API.md`) gerçeği yansıtıp
  yansıtmadığı

Bu dosya o aşamanın girdilerinden biri olacak — **maddeler silinmeden, durumu
güncellenerek** tutulmalı.

---

## 8. Web istemcisi tasarlanırken çıkanlar (2026-09-02)

Frontend planlanırken backend'e bakılarak bulunan üç madde. Üçü de v1
kapsamına alınmadı, ama ilk ikisi **prod'a çıkmadan** karara bağlanmalı.

### 8.1. `/feed`'de `actor_type` filtresi — Faz 18.A'ya alındı

`GET /feed` bugün yalnızca `sort`, `window`, `cursor`, `limit`, `fields`
alıyor. "Sadece insanların postlarını göster" / "sadece ajanlarınkini
göster" gibi bir arayüz fikri bu parametreyi gerektirirdi.

**Karar (2026-09-02, güncellendi): ekleniyor, Faz 18.A.** Önce "gerek yok"
denmişti (karmaşıklık, gerçek talep yok); sonra "madem şimdi konuşuyoruz,
unutmadan yapalım" diye plana alındı. Yine de tek bir uyarı kayda geçmeli:
`actor_type` **kendi beyanı, doğrulanmıyor** (bir insan `ai_agent` diye
kaydolabilir, tersi de). Doğrulanmamış bir alan üzerine kurulan filtre
kullanıcıya tutamayacağı bir söz verir — "insan içeriği görüyorum"
garantisi aslında yok.

İstemci tarafında filtrelemek de **çözüm değil**: feed cursor'lı
sayfalanıyor, sayfayı istemcide süzmek düzensiz sayfa boyutları üretir
(20 istenip 11 gösterilir). Yapılacaksa sunucu tarafında yapılmalı.

Bu yüzden `docs/API.md`'de filtrenin bir **garanti değil kolaylık** olduğu
açıkça yazılmalı. Performans tarafı da ölçülmeden geçilmemeli: filtre
`actors` tablosunda, feed sıralaması `contents` üzerindeki partial
index'lerde — ikisinin birlikte nasıl planlandığı `EXPLAIN` ile görülmeli.

### 8.2. Avatar — şemada var, API'de **yok**

`migrations/0002_actors.up.sql` `actors.avatar_object_key text` kolonunu
tanımlıyor, ama kod tabanında `avatar` geçen **tek bir satır yok**:

- `UpdateProfileRequest` yalnızca `display_name` + `bio` alıyor
  (`crates/actos-types/src/actor.rs:49`)
- `ActorSummary` avatar döndürmüyor (`actor.rs:31`)
- Hiçbir sorgu bu kolonu okumuyor/yazmıyor

Yani kolon ölü. Bir web arayüzü avatarsız da çalışır ama bir sosyal
platformda bu göze batar. Eklenecekse iş küçük ve mevcut parçalarla
oturuyor: `POST /uploads` zaten görsel alıp WebP'ye normalize ediyor,
tek gereken `PATCH /actors/me`'nin `avatar` (attachment id) kabul etmesi
ve `ActorSummary`'nin `avatar_url` döndürmesi.

**Karar (2026-09-02): v1'e giriyor, Faz 18.A.** Avatarsız bir sosyal
platform arayüzü eksik görünüyor ve iş küçük.

Uygulanırken atlanmaması gereken bir tuzak var: avatar olarak kullanılan
attachment `content_id IS NULL` kalır, yani bugünkü
`attachment::cleanup_orphaned` işi onu yetim sanıp **bir saat sonra siler**.
Temizlik sorgusu `actors.avatar_object_key`'e bakan bir dışlama almalı.

### 8.3. `render_markdown` yazıldı, test edildi, **hiç çağrılmıyor**

`crates/actos-core/src/text.rs:433` `render_markdown` — `pulldown-cmark` ile
markdown'ı HTML'e çevirip `ammonia` ile katı bir allowlist'ten geçiriyor.
10'dan fazla XSS testi var (`<script>`, `javascript:`, `data:`, `onerror`,
`<iframe>`, `vbscript:` hepsi kapsanmış).

Ama üretim yolunda **hiçbir yerden çağrılmıyor** — tek çağıranlar kendi
testleri. API `body`'yi ham markdown olarak saklıyor ve ham markdown olarak
döndürüyor (`Content.body: String` + `body_format: "markdown"|"plain"`).

Sonucu: **markdown render + sanitize işi her istemciye ayrı ayrı düşüyor.**
Web arayüzü kendi sanitizasyonunu yazacak, masaüstü istemci kendininkini,
üçüncü taraf bir istemci de kendininkini — ve içlerinden biri bunu yanlış
yaparsa XSS alır. Oysa doğru yapılmış bir uygulama zaten burada duruyor.

**Karar (2026-09-02): `body_html` ekleniyor, Faz 18.A.** Gerekçe
kullanıcıya ait: sanitizasyon backend'in işi, her istemcinin tek tek
uğraşacağı bir şey değil.

Biçim: `body` (ham markdown) her zaman dönmeye devam eder — ajanlar kaynağı
ister, doğru olan bu. Yanına `body_html` gelir: tek-öğe uçlarında her zaman,
liste uçlarında `?fields=` ile. Sanitizasyon tek yerde kalır, her istemci
(web, masaüstü, üçüncü taraf) bedava güvenli HTML alır.

**Saklanmaz, okuma anında hesaplanır.** Saklamak migration + backfill
isterdi ve "gövde düzenlendi ama html eski kaldı" sınıfı bir tutarsızlık
kapısı açardı; hesaplamak `ammonia` ile ucuz.

İki incelik: `body_format == "plain"` içerikte markdown render **edilmemeli**
(kullanıcının düz metin diye yazdığı `*yıldız*` italik olmamalı), ve silinmiş
içerikte `body_html` `body` ile aynı maskeleme kuralına uymalı.

---

## 9. Güven, doğrulama ve kötüye kullanım (2026-09-02) — KARAR BEKLİYOR

Kullanıcı v1'e "onay/doğrulama sistemi" istedi. Aşağıdaki ayrım yapılmadan
madde yazılmamalı: **birbirine karıştırılan iki ayrı problem var.**

### 9.1. İki ayrı problem

| | Soru | Yanlış cevap verirse |
|---|---|---|
| **Doğrulama** (verification) | "Bu hesap kim?" | Rozet bir statü sembolüne döner, kimlik hakkında hiçbir şey söylemez (Twitter mavi tık) |
| **Güven** (trust) | "Bu hesabın eylemleri ne kadar ağırlık taşımalı?" | Sybil saldırısı bedava olur |

Bunları tek bir "onaylı kullanıcı" bayrağında birleştirmek klasik hatadır.
Biri **kimlik**, diğeri **yetki kademesi**.

### 9.2. Alan adı doğrulaması — ERTELENDİ (2026-09-03), tasarım burada saklı

**Karar: v1'e girmiyor.** Gerekçeler §9.2.5'te. Aşağıdaki tasarım, gün geldiğinde
sıfırdan düşünmek gerekmesin diye eksiksiz kaydedildi.

#### 9.2.1. Hangi problemi çözer: kimlik taklidi

Actos'ta e-posta yok, telefon yok, kimlik kontrolü yok ve kullanıcı adları
**ilk gelen alır**. Yani biri `nvidia`, `anthropic` ya da `openai` adıyla
kaydolup o kurum adına post atabilir. Bugün okuyan bir insanın gerçekle
sahteyi ayırt etmesinin **hiçbir yolu yok** — elimizdeki tek kimlik sinyali
kullanıcı adı metni, o da kime ait olduğunu söylemiyor.

Bunun Actos'ta diğer platformlardan daha kritik olmasının sebebi: Twitter'da
rozet satın alınabilir ya da desteğe yazılabilir; burada **başka hiçbir
mekanizma yok**. Elle inceleme de istenmiyor (moderatör darboğazı).

**Kimin işine yarar:** kurumlar, `organization` tipindeki hesaplar, sitesi
olan projeler, bir şirketin çalıştırdığı AI ajanı (hangi şirket olduğunu
kanıtlar). **Bireylerin ihtiyacı yok** ve rozet isteğe bağlı — olmayan
hesabın hiçbir şeyi eksik olmaz. ("Herkesin alan adı yok" itirazı doğru ama
konu dışı: bu bir kapı değil, bir işaret.)

#### 9.2.2. Mekanizma

Sunucu rastgele bir sır üretir, kullanıcı onu **ancak alan adının sahibinin
koyabileceği bir yere** koyar:

1. Sunucu üretir: `actos-verify=8f3c1a…` (hesaba + alan adına özel)
2. Kullanıcı ya DNS bölgesine bir `TXT` kaydı ekler, ya da
   `https://<alan>/.well-known/actos-challenge` yoluna koyar
3. Sunucu bakar; bulursa "bu hesap bu alan adını kontrol ediyor" sonucuna varır

Kilit nokta: DNS bölgesine kayıt eklemek ya da o alan adının web sunucusuna
dosya koymak **yalnızca alan adını yönetenin** yapabileceği bir şey.

Bu, **Let's Encrypt'in HTTPS sertifikası verirken kullandığı yöntemin
aynısıdır** (ACME `dns-01` / `http-01`). Google Search Console, Mastodon'un
`rel=me`'si, Bluesky'ın alan adı handle'ları da aynı fikir.

#### 9.2.3. Neyi kanıtlar, neyi kanıtlamaz

**Alan adı kontrolünü** kanıtlar; "bu hesap hukuken NVIDIA Corp'tur" demez.
Aradaki bağ dolaylı: `nvidia.com`'un NVIDIA'ya ait olduğunu alan adı kayıt
sistemi kuruyor ve dünya zaten alan adını o kurumun kimliği sayıyor. Yani
"alan adı kontrolü ≈ kurumsal kimlik" pratikte geçerli, teoride değil.

Zayıf yerleri, uygulanacağı gün karşılanması gerekenler:
- **Alan adı el değiştirir** (süre dolar, satılır) → rozet eski sahipte
  kalmamalı, **periyodik yeniden doğrulama** şart
- **Alt alan adı ele geçirme** (dangling CNAME) → alt alan adı kabul edilecekse
  ayrı düşünülmeli
- DNS'e erişimi olan bir çalışan hukuken şirket değildir — rozet "bu alan adına
  teknik erişim" demektir, yetkili temsilcilik değil
- İptal akışı: kullanıcı rozeti kaldırabilmeli, moderatör de kaldırabilmeli

#### 9.2.4. SSRF — HTTPS yöntemi seçilirse asıl risk

**SSRF (Server-Side Request Forgery):** normalde saldırgan kendi makinesinden
istek atar ve yalnızca internete açık olana erişir. "Kullanıcının verdiği
adresi **sunucu** çeksin" diyen bir özellik yazarsan, saldırgan istekleri
**senin sunucunun içinden** attırır — senin ağ konumunla.

Actos'ta somut hâli: saldırgan doğrulama için şu adresleri dener —

```
http://127.0.0.1:3101    → Postgres
http://127.0.0.1:3103    → MinIO
http://169.254.169.254/  → bulut metadata servisi (kimlik bilgisi dağıtır)
```

Bunların hiçbirine internetten erişilemez ama **sunucu erişir**. Doğrulama
sonucu (eşleşti / hata metni / süre) bile bir sızıntı kanalı olur. Sonuncusu
en tehlikelisi: Capital One'ın 2019 sızıntısı tam olarak bulut metadata
endpoint'ine yapılan bir SSRF'ti.

Uygulanırsa zorunlu savunmalar:
- Yalnızca `https`
- **Yönlendirme takip etme** — naif bir IP kontrolü `127.0.0.1`'e yönlendirmeyle atlatılır
- DNS çözümlemesinden **sonra** özel/yerel aralıkları reddet
  (127/8, 10/8, 172.16/12, 192.168/16, 169.254/16, ::1, fc00::/7)
- Kısa timeout, yanıt gövdesi birkaç KB ile sınırlı
- **DNS rebinding'e karşı çözümlenen IP'yi bağlantıya sabitle** — kontrol ile
  bağlantı arasındaki boşlukta alan adı `127.0.0.1`'e çözümlenebilir (TOCTOU)
- Doğrulama denemesi ayrı ve sıkı rate limit

**Bu yüzden yalnızca DNS-TXT yöntemi cazip:** kullanıcının verdiği bir adrese
hiç istek atılmaz, sadece bir alan adının TXT kaydı çözümlenir — yukarıdaki
sınıfın tamamı ortadan kalkar. Bedeli: kullanıcının DNS kaydı düzenleyebilmesi
gerekir (dosya koymaktan biraz yüksek eşik) ve yayılma gecikmesi vardır.

#### 9.2.5. Neden şimdi yapılmıyor

1. **Henüz olmayan bir problemi çözüyor.** Platformda hiç kurum yok, kimse
   kimseyi taklit etmiyor. Bu özellik ancak platform birinin adını kapmaya
   değecek kadar önemli olduğunda karşılığını verir.
2. **Bedeli bugün somut, faydası bugün sıfır.** Backend'in şu an **hiç dışa
   giden isteği yok** — ne HTTP istemcisi ne DNS resolver bağımlılığı var. Bu
   özellik ona ağ erişimi, yeni bağımlılıklar (ve `deny.toml` lisans listesi
   genişletmesi), deploy'da egress gereksinimi ekler.
3. **Asıl işi yapan parça zaten bitti.** Sybil savunması güven kademeleriydi
   (§9.3) ve v1'e girdi. Doğrulama ondan bağımsız bir rozet.
4. **Sonradan eklemek tamamen additive** — hiçbir şemayı ya da kararı
   kilitlemiyor.

**Yeniden değerlendirme tetikleyicisi:** biri kimlik taklidinden şikâyet
ettiğinde, ya da bir kurum "kimliğimi nasıl kanıtlarım" diye sorduğunda.
O gün tasarım hazır; uygulaması (DNS-TXT yolu seçilirse) yarım gün.

### 9.3. Güven kademeleri — sybil'e karşı asıl savunma

Kullanıcının senaryosu: *"biri gece boyunca limitlere takılmadan 100 hesap
açıp 100 hesapla kendine upvote atabilir."* Bu senaryo bugün **tamamen
mümkün** ve sistemin en zayıf yeri.

Kimlik temelli savunma burada işe yaramaz (e-posta yok, telefon yok, IP ban
proxy ile aşılır). **Yapısal cevap kimlik değil kademe olmalı:**

- Yeni hesap doğar doğmaz tam yetkili olmaz
- Oy **ağırlığı** hesabın kademesine bağlıdır; taze hesabın oyu sıralamayı
  kıpırdatmaz (silinmez, sayılır, ama ağırlığı düşüktür)
- Kademe zamanla ve davranışla yükselir: hesap yaşı, **kendi içeriği dışından**
  aldığı oy, onaylanmış rapor almamış olmak, doğrulanmış alan adı
- Yüksek kademe daha geniş rate limit demek (mevcut `rate_limit_config` jsonb
  bunu zaten taşıyabilir — yeni altyapı gerekmiyor)

Bu, 100 hesap açmayı engellemez; **açmayı işe yaramaz kılar.** Doğru hedef bu.

### 9.4. Ban'ın gerçek sınırı

Bugünkü ban `actors.id` üzerinde. E-posta, telefon ya da IP bağı olmadığı için
banlanan kişi yeni hesap açıp devam edebilir. Bu bir uygulama hatası değil,
e-postasız tasarımın doğal sonucu ve **kabul edilmiş bir sınır olarak
belgelenmeli** — "ban ediyoruz, sorun çözüldü" yanılgısı üretmemeli.

Ban'ın gerçekten işe yaradığı yer: kademesini emekle yükseltmiş bir hesabı
kaybettirmek. Yani §9.3 olmadan ban da anlamsız — ikisi aynı sistemin
parçası.

### 9.5. Tespit (v1 değil, sonraya not)

- **Oy halkası tespiti:** neredeyse yalnızca tek bir yazara oy veren hesap
  kümeleri. Gerçek zamanlı değil, periyodik bir sorgu işi.
- **Kayıt hızı:** aynı `/24` bloğundan kısa sürede çok kayıt — sinyal, kanıt
  değil; otomatik ban değil moderatör kuyruğuna düşmeli.
- **Davet ağacı (lobste.rs deseni):** kayıt davetle olursa sybil kümesi
  köküne kadar izlenebilir ve toplu iptal edilebilir. Güçlü ama **açık kayıt
  felsefesiyle çelişiyor** — Actos'ta muhtemelen istenmez, opsiyonel bir
  "davetli hesaplar bir kademe yukarıdan başlar" biçimi düşünülebilir.

### 9.6. Sıralama sinyali — ayrı ama bağlantılı

`hot` bugün oy skoruna dayanıyor, yani "insanların beğendiği". Bilgi paylaşımı
hedefleyen bir platformda popülerlik yanlış sinyal olabilir. Veritabanında
zaten duran ama kullanılmayan daha dürüst sinyaller var:

- **`saves` sayısı** — beğenmekten daha maliyetli bir eylem, "buna geri
  döneceğim" demek
- **Yorum derinliği/çeşitliliği** — tartışma üretmiş içerik
- **Düzeltilmemiş olmak** — `edit_history` boş kalmış eski içerik

v1 kapsamında değil ama `hot_score` formülü değiştirilirken (iki yerde tekrar
yazılı, bkz. §6) akılda tutulmalı.

### 9.7. Kararlar (2026-09-02)

- **Proof-of-work: reddedildi.** Önce önerildi, sonra kullanıcı tarafından
  gereksiz bulundu. Kayıt anında bir bedel, kararlı saldırganı durdurmuyor
  (zorluk bir hesap için 3 sn olacak şekilde ayarlansa 100 hesap tek
  çekirdekte 5 dakika eder) ve gerçek kullanıcıya sürtünme ekliyor.
- **Matematik/metin sorusu (CAPTCHA benzeri): reddedildi.** Bu platformda
  ters teper: saldırgan zaten bir dil modeli çalıştırıyor, ona böyle bir soru
  bedel değil. Zorlanan taraf insanlar olur. "Botu insandan ayır" mantığı,
  ajanları birinci sınıf vatandaş sayan bir platformla temelden çelişiyor.
- **Güven kademeleri: kabul edildi, Faz 18.A.**
- **Alan adı doğrulaması: kabul edildi ama isteğe bağlı rozet olarak**,
  düşük öncelikli. Kapı değil — alan adı olmayan hesabın hiçbir şeyi eksik olmaz.
- **Moderatör eliyle verilen rozet: yok.** Darboğaz ve statü hiyerarşisi üretir.

### 9.8. Depolama kötüye kullanımı

Kullanıcının endişesi: *"biri 1000 hesap açıp her biriyle 100 tane 8 MB'lık
görsel yükleyip diski doldurabilir"* — 800 GB, bugünkü altyapıda öldürücü.

Bugün hiçbir kota yok: tek dosya `MAX_UPLOAD_BYTES` (8 MB) ile sınırlı ama
**toplam** yükleme sınırsız. Rate limit hızı kısar, toplamı değil — sabırlı
bir saldırgan zamanla aynı yere varır.

Çözüm §9.3'ün parçası: **actor başına toplam depolama kotası, kademeye bağlı.**
Seviye 0 dar (~50 MB), yükseldikçe genişler. Aynı sistem hem oy manipülasyonunu
hem disk doldurmayı karşılıyor, ayrı bir mekanizma gerekmiyor.

Not: kullanıcı platformun küçük ölçekte kalacağını öngörüyor (hobi ölçeği),
ama işin kalitesi "yarın gerçek sunuculara koyacakmışız gibi" tutulacak.
Kota bu yüzden "ölçek gelirse eklenir" listesine değil v1'e yazıldı.

### 9.9. Seviye 0 rate limit'i pratikte dar çıkabilir (2026-09-03)

Kademe çarpanları `[0.5, 1.0, 2.0]` uygulandıktan sonra ortaya çıktı: seviye
0'ın yorum kotası yarıya iniyor (60 → 30). Uygulama sırasında
`comments_api::derinlik_limiti_asimi_400_ile_reddediliyor` testi 32 yorum
attığı için `400` yerine `429` almaya başladı ve test aktörü seviye 1'e
çekilerek düzeltildi.

Bu bir test kusuru değil, **gerçek bir ürün sinyali**: yeni kaydolmuş bir
insan hareketli bir tartışmada 30 yorumu bir saatte rahatlıkla geçebilir ve
platformdaki ilk deneyimi bir `429` olur — tam da tutmak istediğimiz
kullanıcıyı iten şey.

Çarpan bilinçli seçildi (sybil savunması) ama **gerçek kullanım görülünce
gözden geçirilmeli**. Olası ayarlar: yorum kovasını okuma/oy kovalarından
ayrı tutup seviye 0 için daha cömert yapmak, ya da çarpanı yalnızca yazma
hacmi yüksek kovalarda (post, upload) uygulamak. Şu an ölçüm yok, karar
verisiz alınmamalı.

---

## 10. Spec'te kalan Türkçe şema açıklamaları — **ÇÖZÜLDÜ (2026-09-05)**

> **Bu bölüm artık tarihsel kayıt.** Aşağıdaki bulgu doğruydu ve o gün
> "v1'de kalıyor" kararı verilmişti; karar Faz 20'de gözden geçirildi ve
> **değiştirildi**. Projenin ana dili İngilizce olarak sabitlendi ve çeviri
> yapıldı:
>
> | Ölçüm | Önce | Sonra |
> |---|---|---|
> | `docs/openapi.json`'daki Türkçe açıklama | 116 | **0** |
> | Public spec'e sızmış iç Rust tip yolu | 24 | **0** |
> | Yol / şema sayısı | 45 / 56 | 45 / 56 (değişmedi) |
>
> Aşağıda anlatılan yayılım (Python SDK'sının `Field(description=...)`'ı,
> Rust SDK'sının rustdoc'u) tam da bu yüzden çözüldü: `actos-types`
> crates.io'ya çıktığında docs.rs'te Türkçe rustdoc olarak donacaktı.
> Ayrıntı ve gerekçe: PLAN.md Faz 20 → "Dil kararı".

### Özgün bulgu (2026-09-03)

**Bulgu.** Faz 18.A'nın "dışa dönük metinler İngilizce" hedefi utoipa
makrolarındaki `summary`/`description` literalleriyle sınırlı kaldı. Oysa
`utoipa`, `#[derive(ToSchema)]` taşıyan bir tipin **`///` doküman
yorumunu** spec'e `description` olarak taşıyor — aynı şey açık bir
`description =` verilmemiş handler'ların `///` yorumu için de geçerli.

Ölçüm (`docs/openapi.json`, 45 yol / 56 şema):

- **54/56 şemanın** açıklamasında Türkçe var. Örnek — `ContentSummary.body_html`:
  `"body'nin sanitize edilmiş HTML'i (Faz 18.A, bkz. NOTES.md §8.3)..."`
- **2 yol** handler'ının `///` yorumunu yayınlıyor: `DELETE /auth/keys/{key_id}`
  ve `GET /tags`

Yani `GET /openapi.json`'ı tek kaynak olarak okuyan bir ajan, uç
açıklamalarını ve hata metinlerini İngilizce, **şema alanlarının
açıklamalarını Türkçe** görüyor.

**Karar (kullanıcı, 2026-09-03): v1'de böyle kalır.** Gerekçe: uçların
`summary`/`description`'ları, tüm hata metinleri (`code`, `title`, `detail`)
ve `/docs/agent` önsözü zaten İngilizce; API bu hâliyle kullanılabilir.
Şema açıklamaları alanın *ne olduğunu* değil *neden öyle tasarlandığını*
anlatan uzun prose — bir istemci yazmak için gerekli olan bilgi tip ve
`required` listesinde zaten var.

**Değerlendirilen ve seçilmeyen alternatif:** rustdoc'u Türkçe bırakıp
tiplere kısa İngilizce `#[schema(description = "...")]` override'ı eklemek.
Reddedildi çünkü aynı bilgi iki yerde durur ve ayrışır — bu proje spec'i
bilinçli olarak koddan üretiyor (bkz. Faz 16'da `API.md`'ye uç listesi
konmama gerekçesi) ve elle tutulan ikinci bir metin tam olarak o kalıba
aykırı.

**Yeniden ele alma tetiği.** Bu iş ertelenmiş bir çeviri değil, bir
**dil kararı**: `actos-types` aynı zamanda Rust SDK'sının git bağımlılığı,
yani bu `///` yorumları SDK kullanıcısının rustdoc'unda da görünüyor. Repo
public'e açılmadan (Faz 20) önce "geliştirme dili Türkçe kalsın mı"
sorusunun bütünsel olarak yanıtlanması gerekiyor; şema açıklamaları o
kararın parçası olarak ele alınmalı, tek tek değil.

---

## 11. Attachments cannot be edited after creation (2026-09-12)

Opened by the single-step upload rework (REFACTOR.md §4). Images are now
attached inside the same transaction as the post or comment that carries
them, and nothing changes them afterwards.

That leaves an asymmetry worth fixing: the body of a post is editable and
carries an edit history, while its images are frozen. Picking the wrong
image is at least as common as a typo, and the only remedy today is deleting
the post and writing it again, which throws away its votes, its comment
tree, its edit history and its permalink. That is a heavy penalty for a
mistake the text path forgives.

### The question that has to be answered first

Edit history exists so that what people voted on cannot change silently. The
same risk is sharper for images: post something harmless, collect votes,
swap it. So attachment edits have to be recorded the way body edits are.

That runs into a problem with no obvious answer. If the history is to show
the previous state, the replaced object has to survive, which costs storage
and needs a rule for how it counts against the quota. If it does not
survive, the history can only record that the images changed, without
showing what they were. Both are defensible. Neither should be chosen by
accident while implementing something else.

### Shape, when it happens

A narrow endpoint scoped to attachments rather than folding this into the
general content edit path. Three reasons: the body type differs, JSON versus
multipart; the size limits differ; and once community moderation exists the
permission surface will differ too.

The quota also needs both directions. Removing an image should return its
bytes, adding one should be checked before the write. Today both only happen
on the creation path.

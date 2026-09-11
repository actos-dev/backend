//! Hız sınırlama (rate limiting) çekirdeği: Redis üzerinde atomik bir
//! **token bucket**.
//!
//! HTTP'yi bilmez — `429` üretmek, `X-RateLimit-*`/`Retry-After` header'larını
//! yazmak taşıma katmanının (`actos-api`, ayrı bir görev) işi. Burada sadece
//! [`RateLimiter::check`] bir karar döner.
//!
//! ## Neden token bucket, neden sabit pencere sayacı değil
//!
//! Sabit pencere sayacı (`INCR` + `EXPIRE`) basittir ama pencere sınırında
//! **iki katı** isteğe izin verir: kapasite 10/saat ise, bir istemci 00:59'da
//! 10 istek ve 01:00'de 10 istek daha atarak bir dakika içinde 20 istek
//! yapabilir. Token bucket bu sızıntıyı kapatır: kova sürekli, orantılı
//! olarak dolar; sert bir pencere sınırı yoktur.
//!
//! ## Neden Redis Lua ve neden `TIME`
//!
//! Oku-hesapla-yaz turu Redis'e giden ayrı komutlarla yapılsaydı (`HMGET`
//! sonra `HMSET`), iki eşzamanlı istek arasına bir yarış durumu girebilirdi
//! (ikisi de aynı "eski" durumu okuyup ikisi de izin verebilirdi — bkz.
//! `eşzamanlı_elli_istekten_tam_on_tanesi_izinli` testi, bu tam olarak bunu
//! kanıtlıyor). Tek bir Lua script'i Redis'te **tek komut** olarak çalışır;
//! Redis tek iş parçacıklı olduğu için script'in ortasına başka bir istemci
//! giremez.
//!
//! Saat kaynağı olarak uygulamanın kendi `SystemTime::now()`'ı **değil**,
//! Redis'in `TIME` komutu kullanılır: API'nin birden fazla instance'ı olduğunda
//! (bu projenin hedeflediği dağıtım biçimi) instance'lar arası saat kayması
//! (clock skew), aynı kovanın instance'a göre farklı hızlarda dolmasına yol
//! açar — bir instance'ta izinli olan istek, saati birazcık ileride olan
//! diğerinde reddedilebilir. Redis, kovanın da yaşadığı yer olduğu için tek
//! doğruluk kaynağı olmaya zaten aday.
//!
//! `TIME` normalde "nondeterministic" işaretli bir komuttur (`COMMAND INFO
//! TIME` → `nondeterministic_output`), ama script içinde çağrılması için
//! `noscript` bayrağı **taşımaması** gerekir — taşımıyor. Redis 7'den beri
//! script'ler varsayılan olarak "effects replication" kullanıyor (script'in
//! kendisi değil, ürettiği yazma komutları replikasyona gönderiliyor), bu da
//! `TIME` gibi nondeterministic komutların script içinde serbestçe
//! çağrılabilmesini sağlıyor. Bu, geliştirme sırasında bu makinedeki
//! `redis:8-alpine` (sunucu 8.10.1) örneğine karşı hem `COMMAND INFO TIME`
//! (bayraklarda `noscript` yok) hem de doğrudan `EVAL "redis.call('SET', ...)
//! redis.call('TIME')"` çağrısıyla (hatasız döndü) doğrulandı.
//!
//! ## Durum biçimi: `HASH`, tek string değil
//!
//! Her kova, `tokens` (kesirli — sonraki dolumlarda hassasiyet kaybetmemek
//! için) ve `last_refill_ms` alanlarını taşıyan bir Redis `HASH`'te tutulur.
//! Tek bir ayraçlı string (`"3.5:1699999999000"`) yerine `HASH` tercih
//! edildi çünkü: (1) `HMGET`/`HMSET` iki adlandırılmış alanı okumak/yazmak
//! için ayrıştırma/birleştirme kodu gerektirmez — script daha kısa ve daha az
//! hataya açık; (2) üretimde teşhis kolaylaşır (`redis-cli HGETALL rl:...`
//! okunabilir çıktı verir, ayraçlı bir string'i elle bölmek gerekmez); (3)
//! ileride üçüncü bir alan eklemek (ör. son reddedilen istek zamanı) geriye
//! uyumlu kalır. Bedeli: `HASH`'in `HGETALL`/`HMSET`'i, tek bir `GET`/`SET`'e
//! göre biraz daha fazla bayt taşır — bu ölçekte önemsiz.
//!
//! ## Redis erişilemezse: fail-open mu fail-closed mu
//!
//! [`RateLimiter::check`] **hiçbir zaman hata döndürmez** — her zaman bir
//! [`RateLimitDecision`] üretir, çağıran tarafın ayrıca bir Redis hatası
//! ele alma zorunluluğu olmasın diye. Redis'e erişilemediğinde iki farklı
//! politika uygulanır:
//!
//! - [`Scope::Read`] → **fail-open**: okuma engellenirse platform tamamen
//!   kullanılamaz hâle gelir (her sayfa yüklemesi 429 döner). Bir Redis
//!   arızası bunu hak etmiyor; okuma zaten görece zararsız.
//! - Diğer tüm scope'lar (yazma) → **fail-closed**: Redis yokken sınırsız
//!   yazmaya izin vermek, bir altyapı arızasını spam/DoS penceresine
//!   çevirmek demek. Yazma hacmi düşük ve gecikmeye toleranslı olduğu için
//!   (kullanıcı "şu an gönderilemiyor, tekrar dene" görür) bu kabul edilebilir.
//!
//! Her iki durum da `tracing::warn!` ile loglanır (bkz. [`fallback_decision`]).

use std::{net::IpAddr, sync::LazyLock, time::Duration};

use chrono::{DateTime, Utc};
use deadpool_redis::Pool;
use redis::{AsyncTypedCommands, RedisResult, Script};
use uuid::Uuid;

use crate::config::LimitTable;

// --- Veri tipleri ---------------------------------------------------------

/// Bir kova için kapasite ve pencere. `RateLimiter::check`'e `override_cfg`
/// olarak geçirilirse, [`Subject`]'ten otomatik seçilen kademenin (bkz.
/// [`crate::config::LimitTable::resolve`]) yerini tamamen alır (kısmi
/// birleştirme yok) — `actors.rate_limit_config` jsonb'sinden gelen kişiye
/// özel bir sınır için kullanılır, kademe seçmek için değil.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitConfig {
    pub capacity: u32,
    pub window: Duration,
}

/// Hangi eylem için sınırlama uygulandığı. Redis anahtarının ve
/// (config.rs'teki) env değişkeni öneklerinin bir parçası olur.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    Read,
    Post,
    Comment,
    Vote,
    Register,
    Recover,
    Write,
    /// `GET /search`. Genel `Read`'den **ayrı** bir kova — bkz.
    /// `crates/actos-api/src/middleware/ratelimit.rs::classify` üzerindeki
    /// gerekçe: arama tek bir satır çekmek yerine GIN index taraması +
    /// `ts_rank` hesaplaması + (actor aramasında) trigram benzerliği
    /// yapıyor, `GET /posts/{id}` gibi bir tekil okumadan belirgin ölçüde
    /// daha pahalı.
    Search,
    /// `GET /me/inbox` (Faz 18.A, bildirimler). Genel `Read`'den **ayrı**
    /// bir kova — gerekçe `Search`'ünkinin tam tersi yönde ama aynı ilke:
    /// inbox ucu, "postuma yanıt geldi mi" sorusunu yanıtlamak için **sık
    /// sık yoklanması** teşvik edilen bir uç (bkz. NOTES.md §1 — bu uç,
    /// N ayrı okuma isteğinin yerine geçiyor). Genel `Read` kovasına
    /// koyulsaydı, bir istemcinin inbox'ı sık yoklaması onun post/profil
    /// okuma kotasını da tüketirdi (ve tersi: yoğun içerik okuyan bir
    /// istemci inbox'ını kontrol edemez hâle gelirdi) — ikisinin kullanım
    /// desenleri birbirinden bağımsız, kotaları da öyle olmalı.
    Inbox,
}

impl Scope {
    /// Redis anahtarında kullanılan kısa, sabit ad (ör. `"post"`).
    #[must_use]
    pub const fn as_key_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Post => "post",
            Self::Comment => "comment",
            Self::Vote => "vote",
            Self::Register => "register",
            Self::Recover => "recover",
            Self::Write => "write",
            Self::Search => "search",
            Self::Inbox => "inbox",
        }
    }
}

/// Kovanın kime ait olduğu: kimlikli bir actor mı, yoksa kimliksiz bir IP mi.
///
/// `Actor` carries only `id`. It used to also carry `actor_type` and
/// `trust_level` — so that `RateLimiter::check` could pick the right tier
/// (human/ai_agent × trust level 0-2) — but rate limiting is now a single
/// table for every authenticated actor (see REFACTOR.md §1 and §3,
/// `crate::config::LimitTable`): with no tier left to select, both of
/// those fields are gone. `id` is already enough to build the bucket's key.
#[derive(Debug, Clone, Copy)]
pub enum Subject {
    Actor { id: i64 },
    Ip(IpAddr),
}

/// [`RateLimiter::check`]'in her zaman döndürdüğü karar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitDecision {
    pub allowed: bool,
    pub limit: u32,
    pub remaining: u32,
    /// Kovanın **tamamen** dolmasına kalan süre (izin verilsin verilmesin).
    pub reset_after: Duration,
    /// Yalnızca reddedildiyse `Some`: bir sonraki token'ın ne zaman hazır
    /// olacağı (kapasitenin tamamı değil, sadece 1 token).
    pub retry_after: Option<Duration>,
}

// --- Redis anahtar şeması --------------------------------------------------
//
// `{prefix}rl:{scope}:{a|i}:{kimlik}` — ör. `rl:post:a:42`,
// `rl:register:i:203.0.113.7`. IPv6 adresleri (`2001:db8::1` gibi) `:`
// içerir; bu bilinçli olarak sorun değil çünkü anahtar geri ayrıştırılmıyor,
// sadece Redis'in opak bir string olarak sakladığı bir tanımlayıcı. Biçim
// yine de tutarlı kalsın diye her zaman `<prefix>rl:<scope>:<a|i>:<kimlik>`
// şeklinde kurulur.
//
// `prefix` üretimde her zaman boştur ([`RateLimiter::new`]) — yalnızca
// [`RateLimiter::with_prefix`] ile testler tarafından doldurulur. Amacı: bir
// aktör ID'si (izole test veritabanlarının her biri kendi otoincrement
// sırasını `1`'den başlattığı için) ya da bir istemci IP'si (test harness'i
// `ConnectInfo` sağlamadığında hepsi `0.0.0.0`'a düşer) paralel çalışan
// testler arasında çakışabiliyor; aynı Redis'i paylaşan iki test aynı kovayı
// paylaşırsa birbirinin token'ını tüketip rastgele 429 alır (flaky test).
// Her test kendi benzersiz önekiyle kurulursa, üretim anahtar şemasına hiç
// dokunmadan tamamen izole bir anahtar uzayı alır.

fn bucket_key(prefix: &str, scope: Scope, subject: &Subject) -> String {
    match subject {
        Subject::Actor { id } => format!("{prefix}rl:{}:a:{id}", scope.as_key_str()),
        Subject::Ip(ip) => format!("{prefix}rl:{}:i:{ip}", scope.as_key_str()),
    }
}

// --- Lua script'leri --------------------------------------------------------

/// `KEYS[1]` kova anahtarı.
/// `ARGV[1]` kapasite (tamsayı).
/// `ARGV[2]` pencere, milisaniye.
/// `ARGV[3]` **isteğe bağlı**: test saati (ms). Verilirse Redis `TIME`
/// yerine bu kullanılır — üretim kodu bu argümanı asla göndermez (bkz.
/// [`RateLimiter::check_at`]).
///
/// Döner: `{allowed(0|1), remaining, reset_after_ms, retry_after_ms}`.
const BUCKET_SCRIPT_SRC: &str = r"
local key = KEYS[1]
local capacity = tonumber(ARGV[1])
local window_ms = tonumber(ARGV[2])

local now_ms
if ARGV[3] ~= nil and ARGV[3] ~= '' then
    -- Yalnızca testler bu argümanı gönderir.
    now_ms = tonumber(ARGV[3])
else
    local t = redis.call('TIME')
    now_ms = math.floor(tonumber(t[1]) * 1000 + tonumber(t[2]) / 1000)
end

local state = redis.call('HMGET', key, 'tokens', 'last_refill_ms')
local tokens = tonumber(state[1])
local last_refill_ms = tonumber(state[2])

-- Kova hiç yoksa doluyla başlar: bir subject'in ilk isteği her zaman
-- izinlidir.
if tokens == nil then
    tokens = capacity
    last_refill_ms = now_ms
end

local elapsed_ms = now_ms - last_refill_ms
if elapsed_ms < 0 then
    -- Saat geri gitmemeli (TIME monoton artar; testler now_ms'i de yalnızca
    -- ileri taşımalı) ama savunmacı: negatif dolum yapma.
    elapsed_ms = 0
end

local refill = elapsed_ms * capacity / window_ms
tokens = math.min(capacity, tokens + refill)

local allowed
local remaining
local retry_after_ms

if tokens >= 1 then
    allowed = 1
    tokens = tokens - 1
    remaining = math.floor(tokens)
    retry_after_ms = 0
else
    allowed = 0
    remaining = 0
    local deficit = 1 - tokens
    -- En az 1ms: kesirli artıklar yüzünden 0 dönüp çağıranın 'hemen tekrar
    -- dene' sanmasını istemiyoruz.
    retry_after_ms = math.max(1, math.ceil(deficit * window_ms / capacity))
end

-- Kalan token sayısından kovanın tamamen dolmasına kalan süre.
local missing = capacity - tokens
local reset_after_ms = math.ceil(missing * window_ms / capacity)

-- TTL: kova tamamen dolana kadar geçecek süre + pay. Pay olmasaydı, tam
-- dolum anındaki bir yarış anahtarı erken silebilirdi; ayrıca kova durgun
-- bir subject için sonsuza dek Redis'te birikmemeli.
local ttl_ms = reset_after_ms + 1000

redis.call('HMSET', key, 'tokens', tostring(tokens), 'last_refill_ms', tostring(now_ms))
redis.call('PEXPIRE', key, ttl_ms)

return {allowed, remaining, reset_after_ms, retry_after_ms}
";

/// `KEYS[1]` = `key_id -> son_kullanım_ms` HASH'i. Tüm içeriği okuyup aynı
/// anda temizler; `HGETALL` ile `DEL` arasına başka bir `record_key_use`
/// giremez (tek Lua script = tek atomik yürütme).
const DRAIN_HASH_SCRIPT_SRC: &str = r"
local key = KEYS[1]
if redis.call('EXISTS', key) == 0 then
    return {}
end
local data = redis.call('HGETALL', key)
redis.call('DEL', key)
return data
";

static BUCKET_SCRIPT: LazyLock<Script> = LazyLock::new(|| Script::new(BUCKET_SCRIPT_SRC));
static DRAIN_HASH_SCRIPT: LazyLock<Script> = LazyLock::new(|| Script::new(DRAIN_HASH_SCRIPT_SRC));

/// `record_key_use`/`drain_key_uses`'ın kullandığı, süreç genelinde tek bir
/// HASH anahtarı. Sabit/global olması bilinçli: amaç, birden fazla API
/// instance'ının `last_used_at` dokunuşlarını **tek bir yerde** biriktirip,
/// Faz 12'de kurulacak periyodik iş tarafından tek seferde boşaltılmasıdır.
///
/// Yine de [`RateLimiter::key_prefix`]'e tabidir (bkz. [`RateLimiter::
/// key_touch_hash`]): üretimde prefix boş olduğu için bu sabit değişmeden
/// kullanılır, ama testlerde prefixlenmezse bu HASH da bucket anahtarları
/// gibi paralel test binary'leri arasında paylaşılır — nitekim
/// `crate::middleware::identity` her kimlikli istekte `record_key_use`
/// çağırdığından, `actos-api`nin `auth_api.rs` testleri ile `actos-core`nin
/// kendi `record_key_use_ve_drain_key_uses` testi aynı anda koşunca bu HASH
/// üzerinde çakışıp o testin "tam olarak 2 giriş" varsayımını bozuyordu.
const KEY_TOUCH_HASH: &str = "rl:key_touches";

// --- RateLimiter -----------------------------------------------------------

/// Redis destekli hız sınırlayıcı. Ucuz bir şekilde klonlanabilir olması
/// gerekmiyor (Havuz zaten `Arc` tabanlı) — çağıran taraf tek bir
/// `RateLimiter`'ı `Arc`'layıp paylaşabilir.
pub struct RateLimiter {
    pool: Pool,
    limits: LimitTable,
    /// Bkz. yukarıdaki "Redis anahtar şeması" bölümü. Üretimde her zaman
    /// boş string (`RateLimiter::new`); yalnızca testler `with_prefix` ile
    /// doldurur.
    key_prefix: String,
}

impl RateLimiter {
    #[must_use]
    pub fn new(pool: Pool, limits: LimitTable) -> Self {
        Self {
            pool,
            limits,
            key_prefix: String::new(),
        }
    }

    /// [`Self::new`] ile aynı, ama tüm Redis kova anahtarlarının başına
    /// `key_prefix` eklenir.
    ///
    /// **Yalnızca testler için.** Üretim kodu bu fonksiyonu hiç çağırmamalı
    /// — tek bir API sürecinde birden fazla `RateLimiter` yaşamıyor, anahtar
    /// uzayını bölmenin bir gerekçesi yok. Testlerde ise gerçek bir Redis'e
    /// karşı çalışan paralel testlerin (`cargo test` varsayılan olarak
    /// paralel koşar) aynı kovayı paylaşıp birbirinin token'ını tüketmesini
    /// önler (bkz. anahtar şeması bölümündeki gerekçe). Her test kendi
    /// benzersiz önekiyle (ör. bir `Uuid`) bir `RateLimiter` kurmalı.
    #[must_use]
    pub fn with_prefix(pool: Pool, limits: LimitTable, key_prefix: impl Into<String>) -> Self {
        Self {
            pool,
            limits,
            key_prefix: key_prefix.into(),
        }
    }

    /// Bir istek için hız sınırlama kararı üretir.
    ///
    /// The default capacity comes from [`crate::config::LimitTable::resolve`]:
    /// if `subject` is a `Subject::Actor`, the single table shared by all
    /// authenticated actors (see REFACTOR.md §1 — there's no longer a
    /// separate tier that varies by `actor_type`); if it's `Subject::Ip`,
    /// the per-IP (anonymous) table.
    ///
    /// `override_cfg` **completely replaces** this default — its purpose
    /// isn't to select a tier, but to apply a **per-actor** limit read from
    /// the `actors.rate_limit_config` jsonb column via [`config_from_json`].
    /// When `None` is given, the default is already used directly — which
    /// is enough for most calls.
    ///
    /// **Hata döndürmez.** Redis'e erişilemezse modül başındaki fail-open/
    /// fail-closed politikasına düşülür (bkz. modül dokümantasyonu).
    pub async fn check(
        &self,
        scope: Scope,
        subject: &Subject,
        override_cfg: Option<&RateLimitConfig>,
    ) -> RateLimitDecision {
        self.check_with_clock(scope, subject, override_cfg, None)
            .await
    }

    /// [`Self::check`] ile aynı, ama saat kaynağını sabitler.
    ///
    /// **Yalnızca testler için.** Üretim kodu bu fonksiyonu hiç çağırmamalı;
    /// tek doğruluk kaynağı olarak Redis'in kendi `TIME`'ının kullanılması
    /// (bkz. modül dokümantasyonu), birden fazla API instance'ı arasındaki
    /// saat kaymasına karşı korumanın temeli. Testler zaman geçmesini
    /// beklemeden simüle edebilsin diye buradadır.
    pub async fn check_at(
        &self,
        scope: Scope,
        subject: &Subject,
        override_cfg: Option<&RateLimitConfig>,
        now_ms: u64,
    ) -> RateLimitDecision {
        self.check_with_clock(scope, subject, override_cfg, Some(now_ms))
            .await
    }

    async fn check_with_clock(
        &self,
        scope: Scope,
        subject: &Subject,
        override_cfg: Option<&RateLimitConfig>,
        now_ms: Option<u64>,
    ) -> RateLimitDecision {
        let cfg = override_cfg
            .copied()
            .unwrap_or_else(|| self.limits.resolve(scope, subject));
        let key = bucket_key(&self.key_prefix, scope, subject);

        let mut conn = match self.pool.get().await {
            Ok(conn) => conn,
            Err(err) => {
                return Self::fallback_decision(
                    scope,
                    cfg,
                    "redis havuzundan bağlantı alınamadı",
                    &err.to_string(),
                );
            }
        };

        let mut invocation = BUCKET_SCRIPT.prepare_invoke();
        invocation.key(key.as_str());
        invocation.arg(cfg.capacity);
        invocation.arg(cfg.window.as_millis() as u64);
        if let Some(now_ms) = now_ms {
            invocation.arg(now_ms);
        }

        let result: RedisResult<(i64, i64, i64, i64)> = invocation.invoke_async(&mut conn).await;

        match result {
            Ok((allowed, remaining, reset_after_ms, retry_after_ms)) => RateLimitDecision {
                allowed: allowed == 1,
                limit: cfg.capacity,
                remaining: remaining.max(0) as u32,
                reset_after: Duration::from_millis(reset_after_ms.max(0) as u64),
                retry_after: (allowed != 1)
                    .then(|| Duration::from_millis(retry_after_ms.max(0) as u64)),
            },
            Err(err) => Self::fallback_decision(
                scope,
                cfg,
                "redis script çalıştırılamadı",
                &err.to_string(),
            ),
        }
    }

    /// Redis'e erişilemediğinde uygulanan politika. Bkz. modül başı
    /// dokümantasyonundaki "fail-open mu fail-closed mu" bölümü.
    fn fallback_decision(
        scope: Scope,
        cfg: RateLimitConfig,
        context: &str,
        error: &str,
    ) -> RateLimitDecision {
        // `Search` bilerek `Read`'in dışında bırakıldı: bu bir okuma
        // olduğu için "fail-open" ilkesi ilk bakışta ona da uygulanabilir
        // görünüyor, ama arama diğer okumalardan farklı — pahalı bir GIN
        // index taraması + `ts_rank` hesaplaması (bkz. `Scope::Search`
        // üzerindeki yorum). Redis çökmüşken sınırsız aramaya izin vermek,
        // bir altyapı arızasını veritabanını pahalı sorgularla boğan bir
        // DoS penceresine çevirebilir; platformun geri kalanı (post/yorum/
        // profil okuma) `Read` fail-open sayesinde çalışmaya devam ederken
        // yalnızca arama geçici olarak kısılıyor — bu, tüm okumaların
        // durması kadar felç edici değil.
        let fail_open = matches!(scope, Scope::Read);
        tracing::warn!(
            scope = scope.as_key_str(),
            fail_open,
            error,
            "{context}: hız sınırlama fallback politikasına düşülüyor",
        );

        if fail_open {
            RateLimitDecision {
                allowed: true,
                limit: cfg.capacity,
                remaining: cfg.capacity,
                reset_after: cfg.window,
                retry_after: None,
            }
        } else {
            RateLimitDecision {
                allowed: false,
                limit: cfg.capacity,
                remaining: 0,
                reset_after: cfg.window,
                retry_after: Some(cfg.window),
            }
        }
    }

    // --- `last_used_at` tamponu (Faz 5'ten devir) ---------------------

    /// [`KEY_TOUCH_HASH`]'in bu limiter'ın `key_prefix`'iyle sarılmış hâli.
    /// Bkz. `KEY_TOUCH_HASH` üzerindeki yorum: prefix üretimde boş olduğu
    /// için davranış değişmiyor, testlerde ise bu HASH'i de bucket
    /// anahtarları gibi izole eder.
    fn key_touch_hash(&self) -> String {
        format!("{}{KEY_TOUCH_HASH}", self.key_prefix)
    }

    /// Bir API key'in kullanıldığını Redis'te biriktirir.
    ///
    /// `auth::touch_key` şu an dakikada bir doğrudan `UPDATE` atıyor; bu iki
    /// fonksiyon (`record_key_use` + `drain_key_uses`), bunu Redis'te
    /// biriktirip periyodik olarak toplu yazmaya taşımanın temelini
    /// oluşturuyor. Periyodik iş Faz 12'de kurulacak — burada sadece
    /// biriktirme/boşaltma ilkelleri var.
    ///
    /// **Hata döndürmez** (`auth::touch_key` ile aynı desen): key kullanım
    /// zamanı isteğin başarısını etkileyecek kritik bir bilgi değil,
    /// kaydedilemedi diye isteği düşürmenin bir anlamı yok — sadece loglanır.
    pub async fn record_key_use(&self, key_id: Uuid) {
        let now_ms = Utc::now().timestamp_millis();

        let mut conn = match self.pool.get().await {
            Ok(conn) => conn,
            Err(err) => {
                tracing::warn!(
                    key_id = %key_id,
                    error = %err,
                    "redis bağlantısı alınamadı, key kullanım zamanı kaydedilemedi",
                );
                return;
            }
        };

        if let Err(err) = conn
            .hset(self.key_touch_hash(), key_id.to_string(), now_ms)
            .await
        {
            tracing::warn!(
                key_id = %key_id,
                error = %err,
                "key kullanım zamanı redis'e yazılamadı",
            );
        }
    }

    /// Birikmiş tüm `key_id -> son_kullanım` çiftlerini **atomik olarak**
    /// okuyup Redis'ten temizler (`HGETALL` + `DEL`, tek Lua script).
    ///
    /// Redis'e hiç ulaşılamazsa (ör. bağlantı kurulamadı) boş `Vec` döner ve
    /// `warn` loglar — bir sonraki periyodik çalıştırmada tekrar denenir,
    /// veri kalıcı olarak kaybolmaz (drain edilmediği sürece Redis'teki HASH
    /// büyümeye devam eder).
    pub async fn drain_key_uses(&self) -> Vec<(Uuid, DateTime<Utc>)> {
        let mut conn = match self.pool.get().await {
            Ok(conn) => conn,
            Err(err) => {
                tracing::warn!(error = %err, "redis bağlantısı alınamadı, key kullanım tamponu boşaltılamadı");
                return Vec::new();
            }
        };

        let raw: RedisResult<std::collections::HashMap<String, String>> = DRAIN_HASH_SCRIPT
            .key(self.key_touch_hash())
            .invoke_async(&mut conn)
            .await;

        let raw = match raw {
            Ok(map) => map,
            Err(err) => {
                tracing::warn!(error = %err, "key kullanım tamponu redis'ten okunamadı");
                return Vec::new();
            }
        };

        let mut out = Vec::with_capacity(raw.len());
        for (field, value) in raw {
            let Ok(key_id) = field.parse::<Uuid>() else {
                tracing::warn!(field, "key kullanım tamponunda bozuk key_id, atlanıyor");
                continue;
            };
            let Ok(millis) = value.parse::<i64>() else {
                tracing::warn!(
                    field,
                    value,
                    "key kullanım tamponunda bozuk zaman damgası, atlanıyor"
                );
                continue;
            };
            let Some(at) = DateTime::<Utc>::from_timestamp_millis(millis) else {
                tracing::warn!(
                    field,
                    millis,
                    "key kullanım tamponunda çözümlenemeyen zaman damgası, atlanıyor"
                );
                continue;
            };
            out.push((key_id, at));
        }

        out
    }
}

// --- Actor'e özel override: `actors.rate_limit_config` jsonb -----------

/// `actors.rate_limit_config` jsonb'sinden bu `scope`'a karşılık gelen
/// override'ı okur. Okuma bu fonksiyonun işi; JSON'u nereden aldığı
/// (veritabanı satırı) çağıranın işi — bu modül veritabanını bilmiyor.
///
/// Recognized keys: `posts_per_hour`, `comments_per_hour`, `votes_per_hour`,
/// `reads_per_minute`, `searches_per_minute`, `inbox_per_minute`. The other
/// scopes (`Register`, `Recover`, `Write`) have no per-actor override —
/// they're already applied per unauthenticated IP.
///
/// Tanınmayan bir anahtar, eksik bir alan ya da beklenmeyen bir tip (string,
/// negatif sayı, ondalık, sıfır, `u32`'ye sığmayan bir değer) sessizce
/// yok sayılır (`None` döner) — bozuk/kısmi bir `rate_limit_config` isteği
/// reddetmemeli, sadece o override uygulanmamalı.
#[must_use]
pub fn config_from_json(value: &serde_json::Value, scope: Scope) -> Option<RateLimitConfig> {
    let (key, window) = match scope {
        Scope::Post => ("posts_per_hour", Duration::from_secs(3600)),
        Scope::Comment => ("comments_per_hour", Duration::from_secs(3600)),
        Scope::Vote => ("votes_per_hour", Duration::from_secs(3600)),
        Scope::Read => ("reads_per_minute", Duration::from_secs(60)),
        Scope::Search => ("searches_per_minute", Duration::from_secs(60)),
        Scope::Inbox => ("inbox_per_minute", Duration::from_secs(60)),
        Scope::Register | Scope::Recover | Scope::Write => return None,
    };

    let capacity = value.get(key)?.as_u64()?;
    if capacity == 0 || capacity > u64::from(u32::MAX) {
        return None;
    }

    Some(RateLimitConfig {
        capacity: capacity as u32,
        window,
    })
}

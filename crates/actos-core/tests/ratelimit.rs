//! `actos_core::ratelimit` entegrasyon testleri.
//!
//! Gerçek bir Redis'e karşı çalışır (`127.0.0.1:3102`, bkz.
//! `docker-compose.yml`). `#[sqlx::test]`'in aksine Redis için otomatik bir
//! izolasyon mekanizması yok; bu yüzden her test kendi **benzersiz** anahtarını
//! (rastgele `actor_id`/port, ya da doğrudan `Uuid`) kullanır ve sonunda
//! `cleanup` ile temizler — testler birbirini etkilemez, paralel çalışabilir.

use std::{
    net::{IpAddr, Ipv6Addr},
    sync::Arc,
    time::Duration,
};

use actos_core::{
    auth::ActorType,
    config::{AnonymousLimits, LimitTable, ScopeLimits},
    ratelimit::{self, RateLimitConfig, RateLimiter, Scope, Subject},
};
use redis::AsyncTypedCommands;
use serde_json::json;
use uuid::Uuid;

const REDIS_URL: &str = "redis://127.0.0.1:3102/";

/// Testlerin çoğu `check_at` ile sabit bir saat verir; gerçek zamanın hiçbir
/// önemi yok, sadece testler arasında farklı olması gerekmiyor (her testin
/// anahtarı zaten benzersiz).
const BASE_MS: u64 = 1_700_000_000_000;

// --- Test yardımcıları -------------------------------------------------

#[allow(clippy::expect_used)]
fn make_pool() -> deadpool_redis::Pool {
    deadpool_redis::Config::from_url(REDIS_URL)
        .create_pool(Some(deadpool_redis::Runtime::Tokio1))
        .expect("redis havuzu kurulmalı (127.0.0.1:3102 ayakta olmalı)")
}

/// `RateLimiter::new`'in ikinci parametresi olan tabloyu, tüm hücreleri aynı
/// (kullanılmayacak) varsayılana ayarlayarak doldurur. Testler neredeyse
/// hep `override_cfg` ile kendi kapasite/pencerelerini verdiği için bu
/// tablonun içeriği önemli değil — sadece `RateLimiter::new` bir tablo
/// istiyor.
fn dummy_limit_table() -> LimitTable {
    let cfg = RateLimitConfig {
        capacity: 5,
        window: Duration::from_secs(60),
    };
    LimitTable {
        human: ScopeLimits {
            post: cfg,
            comment: cfg,
            vote: cfg,
            read: cfg,
            upload: cfg,
            search: cfg,
            inbox: cfg,
        },
        ai_agent: ScopeLimits {
            post: cfg,
            comment: cfg,
            vote: cfg,
            read: cfg,
            upload: cfg,
            search: cfg,
            inbox: cfg,
        },
        anonymous: AnonymousLimits {
            register: cfg,
            recover: cfg,
            read: cfg,
            search: cfg,
            write: cfg,
            inbox: cfg,
        },
    }
}

fn make_limiter() -> (RateLimiter, deadpool_redis::Pool) {
    let pool = make_pool();
    let limiter = RateLimiter::new(pool.clone(), dummy_limit_table());
    (limiter, pool)
}

/// Rastgele bir `actor_id` — 2^32'lik alan, testler arası çakışma pratikte
/// imkânsız (`auth.rs` testlerindeki `split_key` gibi, testin kendi ürettiği
/// bir girdi, yapısal olarak başarısız olmaz).
///
/// `actor_type`/`trust_level` çoğu testte önemsiz (bu testler `override_cfg`
/// ile kademeyi zaten ezer) — bu yüzden burada sabit `Human` + `trust_level:
/// 1` ("normal" kademe, bkz. `config::TRUST_LEVEL_CAPACITY_MULTIPLIER`
/// üzerindeki gerekçe) kullanılır; kademe seçiminin kendisini sınayan
/// testler kendi `Subject`'lerini elle kurar (bkz.
/// `subjectin_actor_type_ı_doğru_kademeyi_otomatik_seçiyor` ve "Güven
/// kademesi" bölümü).
fn rand_actor() -> Subject {
    Subject::Actor {
        id: i64::from(rand::random::<u32>()),
        actor_type: ActorType::Human,
        trust_level: 1,
    }
}

/// [`actos_core::ratelimit`] içindeki `bucket_key`'in testteki karşılığı —
/// sadece temizlik için, modülün kendi anahtar şemasını (bkz. modül
/// dokümantasyonu) tekrar üretir. `actor_type` anahtara girmediği için
/// burada da yok sayılır.
fn bucket_key(scope: Scope, subject: &Subject) -> String {
    match subject {
        Subject::Actor { id, .. } => format!("rl:{}:a:{id}", scope.as_key_str()),
        Subject::Ip(ip) => format!("rl:{}:i:{ip}", scope.as_key_str()),
    }
}

#[allow(clippy::expect_used)]
async fn cleanup(pool: &deadpool_redis::Pool, scope: Scope, subject: &Subject) {
    let mut conn = pool
        .get()
        .await
        .expect("temizlik için redis bağlantısı alınmalı");
    let _ = conn.del(bucket_key(scope, subject)).await;
}

// --- Temel token bucket davranışı ---------------------------------------

#[tokio::test]
async fn ilk_istek_izinli_ve_remaining_bir_azalıyor() {
    let (limiter, pool) = make_limiter();
    let subject = rand_actor();
    let cfg = RateLimitConfig {
        capacity: 10,
        window: Duration::from_secs(60),
    };

    let decision = limiter
        .check_at(Scope::Post, &subject, Some(&cfg), BASE_MS)
        .await;

    assert!(decision.allowed);
    assert_eq!(decision.limit, 10);
    assert_eq!(decision.remaining, 9);
    assert!(decision.retry_after.is_none());

    cleanup(&pool, Scope::Post, &subject).await;
}

#[tokio::test]
async fn kapasite_dolunca_reddediliyor_ve_retry_after_makul() {
    let (limiter, pool) = make_limiter();
    let subject = rand_actor();
    let cfg = RateLimitConfig {
        capacity: 3,
        window: Duration::from_secs(60),
    };

    for _ in 0..3 {
        let decision = limiter
            .check_at(Scope::Post, &subject, Some(&cfg), BASE_MS)
            .await;
        assert!(decision.allowed);
    }

    let rejected = limiter
        .check_at(Scope::Post, &subject, Some(&cfg), BASE_MS)
        .await;
    assert!(!rejected.allowed);
    assert_eq!(rejected.remaining, 0);

    let retry_after = rejected
        .retry_after
        .expect("reddedilen istekte retry_after dolu olmalı");
    assert!(retry_after > Duration::ZERO, "0'dan büyük olmalı");
    assert!(retry_after < cfg.window, "pencereden küçük olmalı");

    cleanup(&pool, Scope::Post, &subject).await;
}

#[tokio::test]
async fn now_ms_ilerletilince_kova_yarı_pencerede_yaklaşık_yarı_dolar() {
    let (limiter, pool) = make_limiter();
    let subject = rand_actor();
    let cfg = RateLimitConfig {
        capacity: 10,
        window: Duration::from_secs(100),
    };

    // Kovayı tamamen tüket.
    for _ in 0..10 {
        let decision = limiter
            .check_at(Scope::Post, &subject, Some(&cfg), BASE_MS)
            .await;
        assert!(decision.allowed);
    }
    let empty = limiter
        .check_at(Scope::Post, &subject, Some(&cfg), BASE_MS)
        .await;
    assert!(!empty.allowed);

    // Pencerenin tam yarısı kadar ilerlet (50s = 50_000ms).
    let half_window_later = BASE_MS + 50_000;
    let refilled = limiter
        .check_at(Scope::Post, &subject, Some(&cfg), half_window_later)
        .await;

    assert!(
        refilled.allowed,
        "yarım pencere sonra en az 1 token dolmalı"
    );
    // 100s pencerede 50s'de 5 token dolar; bu istek 1'ini tüketir → 4 kalır.
    assert_eq!(refilled.remaining, 4);

    cleanup(&pool, Scope::Post, &subject).await;
}

#[tokio::test]
async fn kova_kapasitenin_üstünde_dolmuyor() {
    let (limiter, pool) = make_limiter();
    let subject = rand_actor();
    let cfg = RateLimitConfig {
        capacity: 5,
        window: Duration::from_secs(10),
    };

    let first = limiter
        .check_at(Scope::Post, &subject, Some(&cfg), BASE_MS)
        .await;
    assert!(first.allowed);
    assert_eq!(first.remaining, 4);

    // Pencerenin çok katı kadar (1000s >> 10s) "bekle".
    let much_later = BASE_MS + 1_000_000;
    let after_long_wait = limiter
        .check_at(Scope::Post, &subject, Some(&cfg), much_later)
        .await;

    assert!(after_long_wait.allowed);
    assert_eq!(
        after_long_wait.remaining,
        cfg.capacity - 1,
        "kova kapasiteyi aşarak dolmamalı"
    );

    cleanup(&pool, Scope::Post, &subject).await;
}

#[tokio::test]
async fn farklı_subjectler_birbirini_etkilemiyor() {
    let (limiter, pool) = make_limiter();
    let subject_a = rand_actor();
    let subject_b = rand_actor();
    let cfg = RateLimitConfig {
        capacity: 1,
        window: Duration::from_secs(60),
    };

    let first = limiter
        .check_at(Scope::Post, &subject_a, Some(&cfg), BASE_MS)
        .await;
    assert!(first.allowed);
    let second_same_subject = limiter
        .check_at(Scope::Post, &subject_a, Some(&cfg), BASE_MS)
        .await;
    assert!(!second_same_subject.allowed);

    // subject_b, subject_a'nın kovasını tüketmesinden etkilenmemeli.
    let other_subject = limiter
        .check_at(Scope::Post, &subject_b, Some(&cfg), BASE_MS)
        .await;
    assert!(other_subject.allowed);

    cleanup(&pool, Scope::Post, &subject_a).await;
    cleanup(&pool, Scope::Post, &subject_b).await;
}

#[tokio::test]
async fn farklı_scopelar_birbirini_etkilemiyor() {
    let (limiter, pool) = make_limiter();
    let subject = rand_actor();
    let cfg = RateLimitConfig {
        capacity: 1,
        window: Duration::from_secs(60),
    };

    let post_first = limiter
        .check_at(Scope::Post, &subject, Some(&cfg), BASE_MS)
        .await;
    assert!(post_first.allowed);
    let post_second = limiter
        .check_at(Scope::Post, &subject, Some(&cfg), BASE_MS)
        .await;
    assert!(!post_second.allowed);

    // Aynı subject, farklı scope — post'un tükenmesinden etkilenmemeli.
    let comment_first = limiter
        .check_at(Scope::Comment, &subject, Some(&cfg), BASE_MS)
        .await;
    assert!(comment_first.allowed);

    cleanup(&pool, Scope::Post, &subject).await;
    cleanup(&pool, Scope::Comment, &subject).await;
}

// --- Kademe seçimi: `Subject`'in `actor_type`'ı `check` içinde otomatik --

#[tokio::test]
async fn subjectin_actor_type_ı_doğru_kademeyi_otomatik_seçiyor() {
    // `dummy_limit_table`'ın aksine, human/ai_agent için **farklı**
    // kapasiteler kuruyoruz — amaç, `check`'in `override_cfg` verilmeden
    // (yani `None`) doğru kademeyi kendi başına seçtiğini kanıtlamak.
    let human_cfg = RateLimitConfig {
        capacity: 5,
        window: Duration::from_secs(60),
    };
    let ai_agent_cfg = RateLimitConfig {
        capacity: 15, // insanın tam üç katı
        window: Duration::from_secs(60),
    };
    let mut table = dummy_limit_table();
    table.human.post = human_cfg;
    table.ai_agent.post = ai_agent_cfg;

    let pool = make_pool();
    let limiter = RateLimiter::new(pool.clone(), table);

    // `trust_level: 1` ("normal" kademe, 1.0× çarpan) — bu test yalnızca
    // `actor_type`'ın seçimini sınıyor, güven kademesinin ölçeklemesini
    // değil (bu, ayrı "Güven kademesi" testlerinin işi), o yüzden çarpanı
    // nötr tutuyoruz ki `human_cfg`/`ai_agent_cfg` değerleri değişmeden
    // gözlemlenebilsin.
    let human_subject = Subject::Actor {
        id: i64::from(rand::random::<u32>()),
        actor_type: ActorType::Human,
        trust_level: 1,
    };
    let ai_subject = Subject::Actor {
        id: i64::from(rand::random::<u32>()),
        actor_type: ActorType::AiAgent,
        trust_level: 1,
    };

    // override_cfg = None: kademe seçimi tamamen `check`'in işi.
    let human_first = limiter
        .check_at(Scope::Post, &human_subject, None, BASE_MS)
        .await;
    assert_eq!(
        human_first.limit, 5,
        "human subject human kademesini almalı"
    );

    let ai_first = limiter
        .check_at(Scope::Post, &ai_subject, None, BASE_MS)
        .await;
    assert_eq!(
        ai_first.limit, 15,
        "ai_agent subject ai_agent kademesini almalı"
    );
    assert_eq!(
        ai_first.limit,
        human_first.limit * 3,
        "ai_agent kapasitesi insanın tam üç katı olmalı"
    );

    // İnsan kovasını tüket (1 zaten tüketildi, 4 kaldı).
    for _ in 0..4 {
        let decision = limiter
            .check_at(Scope::Post, &human_subject, None, BASE_MS)
            .await;
        assert!(decision.allowed);
    }
    let human_exhausted = limiter
        .check_at(Scope::Post, &human_subject, None, BASE_MS)
        .await;
    assert!(
        !human_exhausted.allowed,
        "insan kovası 5 istekten sonra tükenmeli"
    );

    // ai_agent aynı scope'ta, insanın tükendiği noktada hâlâ istek
    // geçirebiliyor olmalı (1 zaten tüketildi, 14 kaldı — tam kapasite).
    for _ in 0..14 {
        let decision = limiter
            .check_at(Scope::Post, &ai_subject, None, BASE_MS)
            .await;
        assert!(
            decision.allowed,
            "ai_agent insanın tükendiği noktada hâlâ istek geçirebilmeli"
        );
    }
    let ai_exhausted = limiter
        .check_at(Scope::Post, &ai_subject, None, BASE_MS)
        .await;
    assert!(
        !ai_exhausted.allowed,
        "ai_agent kovası da kendi kapasitesi (15) dolunca tükenmeli"
    );

    cleanup(&pool, Scope::Post, &human_subject).await;
    cleanup(&pool, Scope::Post, &ai_subject).await;
}

// --- Güven kademesi: `Subject.trust_level` kapasiteyi otomatik ölçekliyor -
//
// Faz 18.A (bkz. NOTES.md §9.3, §9.8) — `config::TRUST_LEVEL_CAPACITY_
// MULTIPLIER` ([0.5, 1.0, 2.0]) `LimitTable::resolve` içinde `for_actor_type`
// çıktısına uygulanıyor. Aşağıdaki testler taban kapasiteyi 10 seçiyor:
// 10 × 0.5 = 5 (kademe 0), 10 × 1.0 = 10 (kademe 1), 10 × 2.0 = 20 (kademe 2)
// — hepsi tam sayı, yuvarlama belirsizliği testin dışında kalsın diye.

#[tokio::test]
async fn guven_kademesi_0_dar_kademe_2_geniş_limit_alıyor() {
    let mut table = dummy_limit_table();
    table.human.post = RateLimitConfig {
        capacity: 10,
        window: Duration::from_secs(60),
    };
    let pool = make_pool();
    let limiter = RateLimiter::new(pool.clone(), table);

    let level0 = Subject::Actor {
        id: i64::from(rand::random::<u32>()),
        actor_type: ActorType::Human,
        trust_level: 0,
    };
    let level2 = Subject::Actor {
        id: i64::from(rand::random::<u32>()),
        actor_type: ActorType::Human,
        trust_level: 2,
    };

    // override_cfg = None: kapasite tamamen `resolve`'un otomatik seçtiği
    // (actor_type × trust_level) değerden geliyor.
    let d0 = limiter.check_at(Scope::Post, &level0, None, BASE_MS).await;
    assert_eq!(
        d0.limit, 5,
        "kademe 0, taban kapasitenin (10) yarısını almalı"
    );

    let d2 = limiter.check_at(Scope::Post, &level2, None, BASE_MS).await;
    assert_eq!(
        d2.limit, 20,
        "kademe 2, taban kapasitenin (10) iki katını almalı"
    );

    // Kademe 0 kovasını tüket (1 istek zaten yukarıda gitti, 4 kaldı).
    for _ in 0..4 {
        let d = limiter.check_at(Scope::Post, &level0, None, BASE_MS).await;
        assert!(d.allowed);
    }
    let level0_tukendi = limiter.check_at(Scope::Post, &level0, None, BASE_MS).await;
    assert!(
        !level0_tukendi.allowed,
        "kademe 0 (kapasite 5) 5 istekten sonra tükenmeli — bu \
         gecede-100-hesap senaryosunun (NOTES.md §9.3) doğrudan savunması"
    );

    // Kademe 2, kademe 0'ın tükendiği toplam istek sayısında (6) hâlâ rahat:
    // kendi kapasitesi 20, buraya kadar yalnızca 5 istek yaptı.
    for _ in 0..4 {
        let d = limiter.check_at(Scope::Post, &level2, None, BASE_MS).await;
        assert!(
            d.allowed,
            "kademe 2, kademe 0'ın tükendiği istek sayısında hâlâ izinli olmalı"
        );
    }

    cleanup(&pool, Scope::Post, &level0).await;
    cleanup(&pool, Scope::Post, &level2).await;
}

/// Öncelik sırası: kişiye özel `rate_limit_config` override'ı, otomatik
/// seçilen güven kademesi çarpanının **yerine tamamen geçer** — ne üstüne
/// biner (ör. çarpanı override'a da uygulamaz) ne de yalnızca bir taban/tavan
/// olarak davranır. İki yönde de sınanıyor: override otomatik seçilenden
/// hem daha geniş hem daha dar olabilir, ikisinde de override kazanır.
#[tokio::test]
async fn kişiye_özel_override_güven_kademesi_çarpanını_tamamen_eziyor() {
    let mut table = dummy_limit_table();
    table.human.post = RateLimitConfig {
        capacity: 10,
        window: Duration::from_secs(60),
    };
    let pool = make_pool();
    let limiter = RateLimiter::new(pool.clone(), table);

    // Kademe 0: otomatik seçilen kapasite 5 (10 × 0.5) olurdu — ama operatör
    // bu actor'e bilinçli bir istisna koymuş (ör. güvenilir olduğu bilinen
    // ama henüz kademesi düşmüş bir hesap): 100 kapasiteli bir override.
    let dar_kademe_geniş_override = Subject::Actor {
        id: i64::from(rand::random::<u32>()),
        actor_type: ActorType::Human,
        trust_level: 0,
    };
    let genis_override = RateLimitConfig {
        capacity: 100,
        window: Duration::from_secs(60),
    };
    let d1 = limiter
        .check_at(
            Scope::Post,
            &dar_kademe_geniş_override,
            Some(&genis_override),
            BASE_MS,
        )
        .await;
    assert_eq!(
        d1.limit, 100,
        "override, kademe 0'ın dar otomatik kapasitesini (5) tamamen ezmeli"
    );

    // Kademe 2: otomatik seçilen kapasite 20 (10 × 2.0) olurdu — operatör
    // ters yönde bir istisna koymuş (ör. kötüye kullanım şüphesiyle
    // daraltılmış kıdemli bir hesap): 3 kapasiteli bir override.
    let genis_kademe_dar_override = Subject::Actor {
        id: i64::from(rand::random::<u32>()),
        actor_type: ActorType::Human,
        trust_level: 2,
    };
    let dar_override = RateLimitConfig {
        capacity: 3,
        window: Duration::from_secs(60),
    };
    let d2 = limiter
        .check_at(
            Scope::Post,
            &genis_kademe_dar_override,
            Some(&dar_override),
            BASE_MS,
        )
        .await;
    assert_eq!(
        d2.limit, 3,
        "override, kademe 2'nin geniş otomatik kapasitesini (20) tamamen ezmeli \
         — çarpan override'ın üstüne de binmemeli (3 × 2.0 = 6 değil, tam 3)"
    );

    cleanup(&pool, Scope::Post, &dar_kademe_geniş_override).await;
    cleanup(&pool, Scope::Post, &genis_kademe_dar_override).await;
}

// --- Eşzamanlılık: Lua atomikliğinin asıl kanıtı ------------------------

#[tokio::test]
async fn eşzamanlı_elli_istekten_tam_on_tanesi_izinli() {
    let (limiter, pool) = make_limiter();
    let limiter = Arc::new(limiter);
    let subject = rand_actor();
    let cfg = RateLimitConfig {
        capacity: 10,
        window: Duration::from_secs(60),
    };

    let mut handles = Vec::with_capacity(50);
    for _ in 0..50 {
        let limiter = Arc::clone(&limiter);
        handles.push(tokio::spawn(async move {
            limiter
                .check_at(Scope::Vote, &subject, Some(&cfg), BASE_MS)
                .await
                .allowed
        }));
    }

    #[allow(clippy::expect_used)]
    let allowed_count = {
        let mut count = 0usize;
        for handle in handles {
            if handle.await.expect("spawn edilen görev panic atmamalı") {
                count += 1;
            }
        }
        count
    };

    assert_eq!(
        allowed_count, 10,
        "kapasitesi 10 olan kovaya 50 eşzamanlı istekten tam olarak 10'u izinli olmalı \
         (Lua script'i atomik değilse bu sayı 10'dan büyük çıkar)"
    );

    cleanup(&pool, Scope::Vote, &subject).await;
}

// --- IPv6 --------------------------------------------------------------

#[tokio::test]
async fn ipv6_adresli_subject_çalışıyor() {
    let (limiter, pool) = make_limiter();
    let ip = IpAddr::V6(Ipv6Addr::new(
        0x2001,
        0x0db8,
        rand::random::<u16>(),
        0,
        0,
        0,
        0,
        1,
    ));
    let subject = Subject::Ip(ip);
    let cfg = RateLimitConfig {
        capacity: 2,
        window: Duration::from_secs(60),
    };

    let decision = limiter
        .check_at(Scope::Register, &subject, Some(&cfg), BASE_MS)
        .await;

    assert!(decision.allowed);
    assert_eq!(decision.remaining, 1);

    cleanup(&pool, Scope::Register, &subject).await;
}

// --- Redis erişilemezken fail-open / fail-closed ------------------------

#[tokio::test]
#[allow(clippy::expect_used)]
async fn redis_erişilemezken_read_izin_veriyor_post_reddediyor() {
    // Bağlanılabilir bir port değil (loopback'te 1 numaralı port ayrıcalıklı
    // ve dinlenmiyor) — havuz kurulur (lazy), ama ilk `get()` bağlantı
    // hatasıyla başarısız olur.
    let broken_pool = deadpool_redis::Config::from_url("redis://127.0.0.1:1/")
        .create_pool(Some(deadpool_redis::Runtime::Tokio1))
        .expect("havuzun kendisi lazy — kurulması Redis'e ulaşmayı gerektirmez");
    let limiter = RateLimiter::new(broken_pool, dummy_limit_table());
    let subject = rand_actor();

    let read_decision = limiter.check(Scope::Read, &subject, None).await;
    assert!(
        read_decision.allowed,
        "okuma fail-open olmalı: redis çökse bile platform kullanılabilir kalmalı"
    );

    let post_decision = limiter.check(Scope::Post, &subject, None).await;
    assert!(
        !post_decision.allowed,
        "yazma fail-closed olmalı: redis çökünce sınırsız yazmaya izin vermemeli"
    );
    assert!(post_decision.retry_after.is_some());
}

// --- `last_used_at` tamponu ----------------------------------------------

#[tokio::test]
async fn record_key_use_ve_drain_key_uses() {
    // Bu test kova anahtarlarına dokunmuyor (yalnızca `KEY_TOUCH_HASH`),
    // dolayısıyla ayrı bir temizliğe gerek yok — `drain_key_uses` zaten
    // kendi durumunu temizliyor.
    let (limiter, _pool) = make_limiter();
    let key_a = Uuid::new_v4();
    let key_b = Uuid::new_v4();

    limiter.record_key_use(key_a).await;
    limiter.record_key_use(key_b).await;

    let drained = limiter.drain_key_uses().await;
    let ids: std::collections::HashSet<Uuid> = drained.iter().map(|(id, _)| *id).collect();
    assert!(ids.contains(&key_a));
    assert!(ids.contains(&key_b));
    assert_eq!(
        drained.len(),
        2,
        "yalnızca bu testin yazdığı iki giriş olmalı"
    );

    let second_drain = limiter.drain_key_uses().await;
    assert!(
        second_drain.is_empty(),
        "ilk drain her şeyi temizlemiş olmalı, ikinci drain boş dönmeli"
    );
}

// --- `config_from_json` --------------------------------------------------

#[test]
fn config_from_json_tanınan_anahtarı_okuyor() {
    let value = json!({ "posts_per_hour": 42 });
    let cfg = ratelimit::config_from_json(&value, Scope::Post)
        .expect("tanınan bir anahtar için Some dönmeli");
    assert_eq!(cfg.capacity, 42);
    assert_eq!(cfg.window, Duration::from_secs(3600));

    let value = json!({ "reads_per_minute": 7 });
    let cfg = ratelimit::config_from_json(&value, Scope::Read)
        .expect("tanınan bir anahtar için Some dönmeli");
    assert_eq!(cfg.capacity, 7);
    assert_eq!(cfg.window, Duration::from_secs(60));
}

#[test]
fn config_from_json_tanınmayan_anahtarı_yok_sayıyor() {
    // Yanlış/eksik anahtar.
    assert!(ratelimit::config_from_json(&json!({ "unknown_key": 5 }), Scope::Post).is_none());
    assert!(ratelimit::config_from_json(&json!({}), Scope::Post).is_none());

    // Bu scope'lar için hiç override tanımlı değil (yalnızca IP başına
    // uygulanıyorlar).
    assert!(
        ratelimit::config_from_json(&json!({ "posts_per_hour": 5 }), Scope::Register).is_none()
    );
    assert!(ratelimit::config_from_json(&json!({ "posts_per_hour": 5 }), Scope::Recover).is_none());
    assert!(ratelimit::config_from_json(&json!({ "posts_per_hour": 5 }), Scope::Write).is_none());
}

#[test]
fn config_from_json_bozuk_tip_çökertmiyor() {
    // String.
    assert!(
        ratelimit::config_from_json(&json!({ "posts_per_hour": "elli" }), Scope::Post).is_none()
    );
    // Negatif.
    assert!(ratelimit::config_from_json(&json!({ "posts_per_hour": -5 }), Scope::Post).is_none());
    // Ondalık.
    assert!(ratelimit::config_from_json(&json!({ "posts_per_hour": 4.5 }), Scope::Post).is_none());
    // Sıfır (kapasite sıfır olamaz, Lua script'inde sıfıra bölmeye yol açar).
    assert!(ratelimit::config_from_json(&json!({ "posts_per_hour": 0 }), Scope::Post).is_none());
    // `u32`'ye sığmayan.
    assert!(
        ratelimit::config_from_json(&json!({ "posts_per_hour": u64::MAX }), Scope::Post).is_none()
    );
    // Yanlış JSON tipi (dizi, obje).
    assert!(
        ratelimit::config_from_json(&json!({ "posts_per_hour": [1, 2] }), Scope::Post).is_none()
    );
    assert!(ratelimit::config_from_json(&json!({ "posts_per_hour": null }), Scope::Post).is_none());
}

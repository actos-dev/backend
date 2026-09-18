//! `actos_core::auth` entegrasyon testleri.
//!
//! Her test `#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]` ile
//! kendi izole veritabanını alır (bkz. `crates/actos-core/src/db.rs`
//! üzerindeki `MIGRATOR` yorumu) — testler birbirini etkilemez, paralel
//! çalışabilir.

use actos_core::{
    Error,
    auth::{self, ActorType, Permission, PermissionScope},
};
use sqlx::PgPool;

/// `actos_<key_id>_<secret>` biçimindeki bir key'i üç parçasına ayırır.
/// Sahte/forge edilmiş key testleri için: bir actor'ün `key_id`'siyle
/// başka bir actor'ün `secret`'ini birleştirmek gibi.
// `clippy::expect_used`in "testte serbest" istisnası yalnızca `#[test]`
// gibi doğrudan test olarak işaretli fonksiyonları kapsıyor, bu yardımcıyı
// değil; girdi burada testin kendi ürettiği geçerli bir key olduğu için
// `expect` yapısal olarak başarısız olmaz.
#[allow(clippy::expect_used)]
fn split_key(raw: &str) -> (&str, &str, &str) {
    let mut parts = raw.splitn(3, '_');
    let prefix = parts.next().expect("prefix bölümü olmalı");
    let key_id = parts.next().expect("key_id bölümü olmalı");
    let secret = parts.next().expect("secret bölümü olmalı");
    (prefix, key_id, secret)
}

// --- register + authenticate: mutlu yol -------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn kayıt_olunca_dönen_key_ile_authenticate_başarılı(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let reg = auth::register(&pool, "alice", ActorType::Human, Some("Alice")).await?;
    assert_eq!(reg.actor.username, "alice");
    assert_eq!(reg.actor.display_name.as_deref(), Some("Alice"));
    assert_eq!(reg.recovery_codes.len(), auth::RECOVERY_CODE_COUNT);

    let authed = auth::authenticate(&pool, &reg.api_key).await?;
    assert_eq!(authed.actor.id, reg.actor.id);
    assert_eq!(authed.actor.username, "alice");
    assert!(authed.permissions.is_empty());

    Ok(())
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn aynı_kullanıcı_adı_ikinci_kez_çakışma_veriyor(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    auth::register(&pool, "bob", ActorType::Human, None).await?;
    let second = auth::register(&pool, "bob", ActorType::Human, None).await;
    assert!(matches!(second, Err(Error::Conflict(_))));

    // citext: büyük/küçük harf farkı da çakışma sayılmalı.
    let third = auth::register(&pool, "BOB", ActorType::Human, None).await;
    assert!(matches!(
        third,
        Err(Error::Conflict(_)) | Err(Error::Validation(_))
    ));

    Ok(())
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn geçersiz_kullanıcı_adı_doğrulama_hatası_veriyor(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    assert!(matches!(
        auth::register(&pool, "ab", ActorType::Human, None).await,
        Err(Error::Validation(_))
    ));
    assert!(matches!(
        auth::register(&pool, "Kullanici", ActorType::Human, None).await,
        Err(Error::Validation(_))
    ));
    assert!(matches!(
        auth::register(&pool, "admin", ActorType::Human, None).await,
        Err(Error::Validation(_))
    ));

    Ok(())
}

// --- authenticate: kötü niyetli/bozuk girdiler -------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn bozuk_veya_uydurma_key_invalid_key_veriyor(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    assert!(matches!(
        auth::authenticate(&pool, "uydurma-key").await,
        Err(Error::InvalidKey)
    ));
    assert!(matches!(
        auth::authenticate(&pool, "").await,
        Err(Error::InvalidKey)
    ));
    // Doğru biçimde ama var olmayan bir key_id/secret.
    let fake = actos_core::secret::generate_api_key();
    assert!(matches!(
        auth::authenticate(&pool, &fake.plaintext).await,
        Err(Error::InvalidKey)
    ));

    Ok(())
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn iptal_edilmiş_key_invalid_key_veriyor(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let reg = auth::register(&pool, "carol", ActorType::Human, None).await?;
    let keys = auth::list_keys(&pool, reg.actor.id).await?;
    let key_id = keys[0].id;

    auth::revoke_key(&pool, reg.actor.id, key_id).await?;

    assert!(matches!(
        auth::authenticate(&pool, &reg.api_key).await,
        Err(Error::InvalidKey)
    ));

    Ok(())
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn başka_actorün_secretıyla_üretilmiş_sahte_key_invalid_key_veriyor(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let dave = auth::register(&pool, "dave", ActorType::Human, None).await?;
    let erin = auth::register(&pool, "erin", ActorType::Human, None).await?;

    // dave'in gerçek key_id'si + erin'in secret'i: key_id veritabanında var
    // (satır bulunur), ama secret_hash eşleşmez.
    let (_, dave_key_id, _) = split_key(&dave.api_key);
    let (_, _, erin_secret) = split_key(&erin.api_key);
    let forged = format!("actos_{dave_key_id}_{erin_secret}");

    assert!(matches!(
        auth::authenticate(&pool, &forged).await,
        Err(Error::InvalidKey)
    ));

    Ok(())
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn silinmiş_actorün_key_i_invalid_key_veriyor(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let reg = auth::register(&pool, "faye", ActorType::Human, None).await?;

    sqlx::query!(
        r#"UPDATE actors SET deleted_at = now() WHERE id = $1"#,
        reg.actor.id,
    )
    .execute(&pool)
    .await?;

    assert!(matches!(
        auth::authenticate(&pool, &reg.api_key).await,
        Err(Error::InvalidKey)
    ));

    Ok(())
}

// --- authenticate: ban ------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn banlı_actor_doğrulanıyor_ama_işaretleniyor(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let admin = auth::register(&pool, "modadmin", ActorType::Human, None).await?;
    let victim = auth::register(&pool, "banneduser", ActorType::Human, None).await?;

    sqlx::query!(
        r#"
        INSERT INTO bans (actor_id, banned_by, reason, expires_at)
        VALUES ($1, $2, 'kural ihlali', NULL)
        "#,
        victim.actor.id,
        admin.actor.id,
    )
    .execute(&pool)
    .await?;

    // **Faz 14'te değişen davranış:** ban artık kimlik doğrulamayı
    // düşürmüyor, yalnızca `banned` bayrağını işaretliyor. Yazma engelini
    // HTTP katmanı (`actos-api`'deki `CurrentActor` extractor'ı) güvenli
    // olmayan metotlarda uyguluyor; okuma serbest kalıyor. Gerekçe
    // `AuthenticatedActor::banned` üzerinde.
    let kimlik = auth::authenticate(&pool, &victim.api_key).await?;
    assert!(kimlik.banned, "banlı actor işaretlenmeli");
    assert_eq!(kimlik.actor.username, "banneduser");

    Ok(())
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn süresi_dolmuş_ban_authenticate_i_engellemiyor(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let admin = auth::register(&pool, "modadmin2", ActorType::Human, None).await?;
    let victim = auth::register(&pool, "formerlybanned", ActorType::Human, None).await?;

    sqlx::query!(
        r#"
        INSERT INTO bans (actor_id, banned_by, reason, banned_at, expires_at)
        VALUES ($1, $2, 'geçmiş ban', now() - interval '2 hours', now() - interval '1 hour')
        "#,
        victim.actor.id,
        admin.actor.id,
    )
    .execute(&pool)
    .await?;

    let authed = auth::authenticate(&pool, &victim.api_key).await?;
    assert_eq!(authed.actor.id, victim.actor.id);
    assert!(
        !authed.banned,
        "süresi dolmuş ban `banned` bayrağını da kaldırmalı"
    );

    Ok(())
}

// --- key yönetimi ------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn ikinci_key_üretilir_biri_iptal_edilince_diğeri_çalışmaya_devam_eder(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let reg = auth::register(&pool, "frank", ActorType::Human, None).await?;
    let (second_key, second_plain) = auth::issue_key(&pool, reg.actor.id, Some("cli")).await?;

    auth::authenticate(&pool, &reg.api_key).await?;
    auth::authenticate(&pool, &second_plain).await?;

    auth::revoke_key(&pool, reg.actor.id, second_key.id).await?;

    assert!(matches!(
        auth::authenticate(&pool, &second_plain).await,
        Err(Error::InvalidKey)
    ));
    // İlk key hâlâ çalışıyor.
    auth::authenticate(&pool, &reg.api_key).await?;

    let keys = auth::list_keys(&pool, reg.actor.id).await?;
    assert_eq!(keys.len(), 2);

    Ok(())
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn başkasının_key_ini_iptal_etmek_notfound_veriyor_key_çalışmaya_devam_eder(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let victim = auth::register(&pool, "grace", ActorType::Human, None).await?;
    let attacker = auth::register(&pool, "henry", ActorType::Human, None).await?;

    let victim_keys = auth::list_keys(&pool, victim.actor.id).await?;
    let victim_key_id = victim_keys[0].id;

    let result = auth::revoke_key(&pool, attacker.actor.id, victim_key_id).await;
    assert!(matches!(result, Err(Error::NotFound(_))));

    auth::authenticate(&pool, &victim.api_key).await?;

    Ok(())
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn iptal_edilmiş_key_tekrar_iptal_edilince_hata_vermez(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let reg = auth::register(&pool, "ivan", ActorType::Human, None).await?;
    let keys = auth::list_keys(&pool, reg.actor.id).await?;
    let key_id = keys[0].id;

    auth::revoke_key(&pool, reg.actor.id, key_id).await?;
    auth::revoke_key(&pool, reg.actor.id, key_id).await?; // idempotent, hata yok

    Ok(())
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn touch_key_last_used_atı_günceller_ama_bir_dakika_içinde_tekrar_güncellemez(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let reg = auth::register(&pool, "oscar", ActorType::Human, None).await?;
    let keys = auth::list_keys(&pool, reg.actor.id).await?;
    let key_id = keys[0].id;
    assert!(keys[0].last_used_at.is_none());

    auth::touch_key(&pool, key_id).await;
    let after_first = auth::list_keys(&pool, reg.actor.id).await?;
    let first_touch = after_first[0].last_used_at;
    assert!(first_touch.is_some());

    auth::touch_key(&pool, key_id).await;
    let after_second = auth::list_keys(&pool, reg.actor.id).await?;
    assert_eq!(
        after_second[0].last_used_at, first_touch,
        "bir dakikadan kısa sürede tekrar güncellenmemeli"
    );

    Ok(())
}

// --- kurtarma ----------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn kurtarma_geçerli_kod_yeni_key_veriyor_aynı_kod_ikinci_kez_çalışmıyor(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let reg = auth::register(&pool, "julia", ActorType::Human, None).await?;
    let code = reg.recovery_codes[0].clone();

    let (new_key, remaining) = auth::recover(&pool, "julia", &code).await?;
    assert_eq!(remaining, (auth::RECOVERY_CODE_COUNT - 1) as i64);
    auth::authenticate(&pool, &new_key).await?;

    let second_try = auth::recover(&pool, "julia", &code).await;
    assert!(matches!(second_try, Err(Error::InvalidKey)));

    Ok(())
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn kurtarma_yanlış_kod_ve_olmayan_kullanıcı_aynı_hatayı_veriyor(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let reg = auth::register(&pool, "kayla", ActorType::Human, None).await?;

    let wrong_code = auth::recover(&pool, "kayla", "0000-0000-0000").await;
    assert!(matches!(wrong_code, Err(Error::InvalidKey)));

    let missing_user = auth::recover(&pool, "no_such_user", &reg.recovery_codes[0]).await;
    assert!(matches!(missing_user, Err(Error::InvalidKey)));

    Ok(())
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn banlı_actor_kurtarma_yapamıyor(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let admin = auth::register(&pool, "modadmin3", ActorType::Human, None).await?;
    let victim = auth::register(&pool, "kevin", ActorType::Human, None).await?;

    sqlx::query!(
        r#"
        INSERT INTO bans (actor_id, banned_by, reason, expires_at)
        VALUES ($1, $2, 'kural ihlali', NULL)
        "#,
        victim.actor.id,
        admin.actor.id,
    )
    .execute(&pool)
    .await?;

    let result = auth::recover(&pool, "kevin", &victim.recovery_codes[0]).await;
    assert!(matches!(result, Err(Error::Banned)));

    Ok(())
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn kurtarma_kodları_yenilenince_eskiler_çalışmıyor(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let reg = auth::register(&pool, "laura", ActorType::Human, None).await?;
    let old_code = reg.recovery_codes[0].clone();

    let new_codes = auth::regenerate_recovery_codes(&pool, reg.actor.id).await?;
    assert_eq!(new_codes.len(), auth::RECOVERY_CODE_COUNT);

    let old_result = auth::recover(&pool, "laura", &old_code).await;
    assert!(matches!(old_result, Err(Error::InvalidKey)));

    let (_, remaining) = auth::recover(&pool, "laura", &new_codes[0]).await?;
    assert_eq!(remaining, (auth::RECOVERY_CODE_COUNT - 1) as i64);

    Ok(())
}

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn kurtarma_doğrulama_sayısı_kalan_kod_sayısından_bağımsız(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Bir hesapta kodların 9'u tükensin, yalnızca 1 tanesi kalsın.
    let almost_empty = auth::register(&pool, "penny", ActorType::Human, None).await?;
    for code in &almost_empty.recovery_codes[..9] {
        auth::recover(&pool, "penny", code).await?;
    }

    // Başka bir hesapta kodların tamamı (10'u da) hâlâ kullanılmamış.
    auth::register(&pool, "quinn", ActorType::Human, None).await?;

    // Ölçüm süre değil, `auth::recovery_verification_count()` sayacı
    // üzerinden yapılıyor — süre gürültülü, sayaç değil (bkz.
    // `auth::verify_fixed_slots` üzerindeki yorum).
    auth::reset_recovery_verification_count();
    let result_almost_empty = auth::recover(&pool, "penny", "0000-0000-0000").await;
    let count_almost_empty = auth::recovery_verification_count();
    assert!(matches!(result_almost_empty, Err(Error::InvalidKey)));

    auth::reset_recovery_verification_count();
    let result_full = auth::recover(&pool, "quinn", "0000-0000-0000").await;
    let count_full = auth::recovery_verification_count();
    assert!(matches!(result_full, Err(Error::InvalidKey)));

    // Hiç var olmayan bir kullanıcı için de aynı sayı çalışmalı.
    auth::reset_recovery_verification_count();
    let result_missing = auth::recover(&pool, "no_such_user_at_all", "0000-0000-0000").await;
    let count_missing = auth::recovery_verification_count();
    assert!(matches!(result_missing, Err(Error::InvalidKey)));

    assert_eq!(count_almost_empty, auth::RECOVERY_CODE_COUNT);
    assert_eq!(count_full, auth::RECOVERY_CODE_COUNT);
    assert_eq!(count_missing, auth::RECOVERY_CODE_COUNT);
    assert_eq!(
        count_almost_empty, count_full,
        "1 kod kalan hesapla 10 kod kalan hesap aynı sayıda Argon2 doğrulaması yapmalı \
         (aksi halde yanlış kod denemesinin süresi kalan kod sayısını sızdırır)"
    );

    Ok(())
}

// --- register: transaction atomicity ----------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn eşzamanlı_aynı_kullanıcı_adı_kaydında_yarım_kayıt_kalmıyor(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let pool_a = pool.clone();
    let pool_b = pool.clone();

    let (res_a, res_b) = tokio::join!(
        auth::register(&pool_a, "raceuser", ActorType::Human, None),
        auth::register(&pool_b, "raceuser", ActorType::Human, None),
    );

    let winner = match (res_a, res_b) {
        (Ok(reg), Err(Error::Conflict(_))) => reg,
        (Err(Error::Conflict(_)), Ok(reg)) => reg,
        other => panic!("beklenen: biri Ok, diğeri Conflict; gelen: {other:?}"),
    };

    let actor_count: i64 = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM actors WHERE username = 'raceuser'"#
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(actor_count, 1);

    let key_count: i64 = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM api_keys WHERE actor_id = $1"#,
        winner.actor.id,
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(key_count, 1, "kazanan actor'ün tam olarak bir key'i olmalı");

    let code_count: i64 = sqlx::query_scalar!(
        r#"SELECT count(*) AS "count!" FROM recovery_codes WHERE actor_id = $1"#,
        winner.actor.id,
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        code_count,
        auth::RECOVERY_CODE_COUNT as i64,
        "kazanan actor'ün tam olarak RECOVERY_CODE_COUNT kurtarma kodu olmalı"
    );

    Ok(())
}

// --- izinler -----------------------------------------------------------

#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn grant_permission_sonrası_authenticate_izni_görüyor(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let reg = auth::register(&pool, "mia", ActorType::Human, None).await?;

    // Başlangıçta hiç izin yok.
    let authed = auth::authenticate(&pool, &reg.api_key).await?;
    assert!(authed.permissions.is_empty());

    // İki izin ver; ikisi de global kapsamda, topluluksuz görünmeli.
    auth::grant_permission(
        &pool,
        reg.actor.id,
        Permission::ContentDelete,
        PermissionScope::Global,
        None,
        None,
    )
    .await?;
    auth::grant_permission(
        &pool,
        reg.actor.id,
        Permission::MemberBan,
        PermissionScope::Global,
        None,
        None,
    )
    .await?;

    let authed = auth::authenticate(&pool, &reg.api_key).await?;
    assert_eq!(authed.permissions.len(), 2);
    assert!(authed.permissions.contains(&auth::Grant {
        permission: Permission::ContentDelete,
        scope: PermissionScope::Global,
        community_id: None,
    }));
    assert!(authed.permissions.contains(&auth::Grant {
        permission: Permission::MemberBan,
        scope: PermissionScope::Global,
        community_id: None,
    }));

    // Aynı izni başka bir granter ile tekrar vermek upsert: satır çoğalmaz.
    let granter = auth::register(&pool, "nora", ActorType::Human, None).await?;
    auth::grant_permission(
        &pool,
        reg.actor.id,
        Permission::ContentDelete,
        PermissionScope::Global,
        None,
        Some(granter.actor.id),
    )
    .await?;
    let authed = auth::authenticate(&pool, &reg.api_key).await?;
    assert_eq!(authed.permissions.len(), 2, "upsert satır çoğaltmamalı");

    // Kaldırma: gerçekten silindiyse `true`, tekrar denemede `false`
    // (idempotent).
    assert!(
        auth::revoke_permission(
            &pool,
            reg.actor.id,
            Permission::ContentDelete,
            PermissionScope::Global,
            None,
        )
        .await?
    );
    assert!(
        !auth::revoke_permission(
            &pool,
            reg.actor.id,
            Permission::ContentDelete,
            PermissionScope::Global,
            None,
        )
        .await?
    );

    let authed = auth::authenticate(&pool, &reg.api_key).await?;
    assert_eq!(
        authed.permissions,
        vec![auth::Grant {
            permission: Permission::MemberBan,
            scope: PermissionScope::Global,
            community_id: None,
        }]
    );

    Ok(())
}

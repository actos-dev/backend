//! `actos_core::tag::cleanup_unused` entegrasyon testleri.
//!
//! Etiket **uçları** `crates/actos-api/tests/tags_api.rs`'te; burada
//! yalnızca HTTP karşılığı olmayan periyodik temizlik işi sınanıyor.
//!
//! Her test `#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]` ile
//! kendi izole veritabanını alır.
//!
//! **Advisory lock testleri neden bozmuyor:** [`actos_core::tag::cleanup_unused`]
//! `pg_try_advisory_lock` kullanıyor ve PostgreSQL'de advisory lock'lar
//! **veritabanı kapsamlı** (`pg_locks.database` mevcut veritabanının OID'i;
//! canlı sunucuda doğrulandı). `sqlx::test` her teste kendi veritabanını
//! verdiği için paralel testler aynı anahtar üzerinde çekişmez — biri
//! kilidi alırken diğerinin `Ok(0)` ile sessizce boş dönmesi ve iddiaların
//! yanlış negatif üretmesi mümkün değil.

use actos_core::{
    Storage,
    auth::{self, ActorType},
    config::StorageConfig,
    content,
    id::IdCodec,
    tag,
};
use sqlx::PgPool;

/// See the identical helper (and its rationale) in `crates/actos-core/tests/
/// notification.rs` — `seed_post` here never carries a file either.
#[allow(clippy::expect_used)]
fn test_storage() -> Storage {
    Storage::new(&StorageConfig {
        endpoint: "http://127.0.0.1:1".to_owned(),
        region: "us-east-1".to_owned(),
        bucket: "test-bucket".to_owned(),
        access_key: "test".to_owned(),
        secret_key: "test".to_owned(),
        public_base_url: "http://127.0.0.1:1/test-bucket".to_owned(),
    })
}

#[allow(clippy::expect_used)]
fn test_id_codec() -> IdCodec {
    IdCodec::new("test-id-obfuscation-key-en-az-otuz-iki-karakter").expect("geçerli anahtar")
}

/// Bir actor + verilen etiketlerle bir post oluşturur.
#[allow(clippy::expect_used)]
async fn seed_post(pool: &PgPool, username: &str, tags: &[&str]) -> i64 {
    let reg = auth::register(pool, username, ActorType::Human, None)
        .await
        .expect("actor oluşturulabilmeli");

    let owned: Vec<String> = tags.iter().map(|t| (*t).to_owned()).collect();
    let post = content::create_post(
        pool,
        &test_storage(),
        &test_id_codec(),
        &reg.actor,
        "başlık",
        "gövde",
        &owned,
        &[],
        8 * 1024 * 1024,
        i64::MAX,
    )
    .await
    .expect("post oluşturulabilmeli");

    post.id
}

#[allow(clippy::expect_used)]
async fn tag_var_mi(pool: &PgPool, name: &str) -> bool {
    sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM tags WHERE name = $1) AS "var!""#,
        name
    )
    .fetch_one(pool)
    .await
    .expect("etiket varlığı sorgulanabilmeli")
}

/// Hiçbir içeriğe bağlı olmayan etiket silinir; bağlı olan durur.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn bagsiz_etiket_siliniyor_bagli_etiket_duruyor(pool: PgPool) {
    seed_post(&pool, "temizlik_bir", &["kullanilan"]).await;

    // Hiçbir post'a bağlı olmayan bir etiket: doğrudan ekliyoruz, çünkü
    // API'den bağsız etiket yaratmanın bir yolu yok (etiketler yalnızca
    // bir post'a eklenirken doğuyor).
    sqlx::query!(r#"INSERT INTO tags (name) VALUES ('yetim')"#)
        .execute(&pool)
        .await
        .expect("yetim etiket eklenebilmeli");

    assert!(tag_var_mi(&pool, "yetim").await);

    let silinen = tag::cleanup_unused(&pool)
        .await
        .expect("temizlik çalışabilmeli");

    assert_eq!(silinen, 1, "yalnızca yetim etiket silinmeli");
    assert!(
        !tag_var_mi(&pool, "yetim").await,
        "yetim etiket silinmeliydi"
    );
    assert!(
        tag_var_mi(&pool, "kullanilan").await,
        "kullanılan etiket durmalıydı"
    );
}

/// Post'u **soft-delete** edilmiş bir etiket silinmez: `content_tags`
/// satırı duruyor ve silinen post geri alınabilir, etiketleri yerinde
/// kalmalı. Böyle etiketler yalnızca `list_popular`'ın `HAVING`'iyle
/// listeden düşer.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn silinmis_postun_etiketi_temizlikte_silinmiyor(pool: PgPool) {
    let post_id = seed_post(&pool, "temizlik_iki", &["soft-silinen"]).await;

    sqlx::query!(
        r#"UPDATE contents SET deleted_at = now() WHERE id = $1"#,
        post_id
    )
    .execute(&pool)
    .await
    .expect("post soft-delete edilebilmeli");

    let silinen = tag::cleanup_unused(&pool)
        .await
        .expect("temizlik çalışabilmeli");

    assert_eq!(
        silinen, 0,
        "soft-delete edilmiş post'un etiketi silinmemeli"
    );
    assert!(tag_var_mi(&pool, "soft-silinen").await);
}

/// Temizlenecek bir şey yokken sıfır döner ve hata vermez — periyodik iş
/// her turda bunu çağırıyor.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn temizlenecek_yokken_sifir_donuyor(pool: PgPool) {
    seed_post(&pool, "temizlik_uc", &["duran"]).await;

    let silinen = tag::cleanup_unused(&pool)
        .await
        .expect("temizlik çalışabilmeli");

    assert_eq!(silinen, 0);
    assert!(tag_var_mi(&pool, "duran").await);
}

/// Temizlik idempotent: arka arkaya iki çağrıdan ikincisi hiçbir şey
/// silmez ve kilit ilk çağrıdan sonra düzgün bırakılmış olmalı — kilit
/// bırakılmasaydı ikinci çağrı `Ok(0)` dönerdi ama etiket de silinmiş
/// olmazdı, bu yüzden iddia iki yönlü.
#[sqlx::test(migrator = "actos_core::db::MIGRATOR")]
async fn temizlik_arka_arkaya_calisabiliyor(pool: PgPool) {
    seed_post(&pool, "temizlik_dort", &["kalan"]).await;

    sqlx::query!(r#"INSERT INTO tags (name) VALUES ('yetim-bir'), ('yetim-iki')"#)
        .execute(&pool)
        .await
        .expect("yetim etiketler eklenebilmeli");

    let ilk = tag::cleanup_unused(&pool)
        .await
        .expect("ilk temizlik çalışabilmeli");
    assert_eq!(ilk, 2);

    let ikinci = tag::cleanup_unused(&pool)
        .await
        .expect("ikinci temizlik çalışabilmeli");
    assert_eq!(ikinci, 0, "ikinci turda silinecek bir şey kalmamalı");

    assert!(tag_var_mi(&pool, "kalan").await);
    assert!(!tag_var_mi(&pool, "yetim-bir").await);
    assert!(!tag_var_mi(&pool, "yetim-iki").await);
}

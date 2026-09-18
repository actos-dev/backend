//! Görünürlük kapısı: "bu içerik bu okuyucuya görünür mü?" (COMMUNITY_PLAN.md
//! §2 + §9, Faz 4A).
//!
//! Özel topluluklar **listelenmemiştir, gizli değildir** (§2): içerikleri
//! hiçbir public yüzeyde görünmez, ama `/c/<ad>` ölü bir adres de değildir
//! (kapak sayfası Faz 4B). 4A yalnızca **okuma yolunu** kapatıyor; uygulama
//! hâlâ `private` topluluk oluşturmayı reddediyor (§2 → `community::
//! create_community`), kapı ise 4B'nin anahtarı çevirebilmesi için şimdiden
//! doğru olmak zorunda.
//!
//! ## Tek paylaşılan filtre, her sorguda
//!
//! Karar SQL'de, `migrations/0031_visibility.up.sql`'deki
//! [`content_visible_to`] fonksiyonunda veriliyor. Her okuma sorgusu kendi
//! kontrolünü yazmıyor; tek bir fonksiyonu çağırıyor. Bir yolu atlamak
//! yanlış bir yanıt değil, bir **sızıntıdır** (§12) — bu yüzden karar tek
//! yerde.
//!
//! `viewer_communities = '{}'` (boş) **koşulsuz public-only** demektir:
//! bağımsız içerik (`community_id IS NULL`) ve **public** topluluk içeriği.
//! §9'un "başkasının etkinliğini listeleyen public yüzey koşulsuz olarak
//! özel içeriği dışlar" kuralı budur — ana akış, takip akışı, arama, etiket
//! sayfaları, bir profilin post/yorum listeleri ve istatistikleri, okuyucu
//! üye olsa bile `'{}'` geçer. Sayı her izleyici için aynı kalır ve
//! önbelleklenebilir.
//!
//! Boş olmayan bir küme, o topluluklardaki içeriği **ek olarak** kabul eder:
//! kişinin kendi listeleri (`GET /me/saves`, gelen kutusu, `GET /me/votes`)
//! ve doğrudan tekil okumalar (bir üye özel bir postu bağlantıyla açabilir).
//!
//! ## `visible_community_ids` nedir, ne değildir
//!
//! [`visible_community_ids`] bir aktörün **üye olduğu** topluluklarla
//! **topluluk kapsamlı izin tuttuğu** toplulukların birleşimidir. Yani bir
//! moderatörün/sahibin toplulukları da burada: "üye değilim ama bu
//! topluluğun moderatörüyüm" durumunda içeriği okuyabilmeliyim. İzin
//! kontrolü değildir — "bu toplulukta şunu yapabilir miyim" sorusu
//! [`crate::authz::has_for`]'a aittir; buradaki yalnızca "görebilir miyim".
//!
//! ### Global moderatör istisnası
//!
//! Platform geneli `content.delete`, `report.view` veya `report.resolve`
//! tutan bir aktör **tüm** toplulukları görür. Gerekçe §7: bir şikayeti
//! inceleyen global yönetici, şikayet edilen özel içeriği açabilmeli;
//! aksi hâlde kuyruktaki kayda tıklayınca `404` alırdı. Bu istisna §9'un
//! "public yüzey koşulsuz olarak özel içeriği dışlar" kuralını **bozmaz**:
//! ana akış/arama/profil gibi yüzeyler zaten `'{}'` geçiyor, yani global
//! moderatör de orada özel içeriği görmez. İstisna yalnızca tekil okuma ve
//! kişinin kendi listeleri gibi `viewer_communities` kullanan yollarda
//! etkilidir.

use sqlx::PgPool;

use crate::error::Result;

/// Bir aktörün görebildiği topluluklar: üyelikleri + topluluk kapsamlı
/// izinleri.
///
/// `viewer` `None` ise (anonim) boş döner — anonim bir okuyucu yalnızca
/// public içerik görür, ki `content_visible_to`'nun `'{}'` semantiği tam
/// olarak budur.
///
/// İki kaynak `UNION` ile birleştiriliyor çünkü ikisi de aynı sorunun
/// cevabı ("bu aktör bu toplulukla ilişkili mi") ve ayrı ayrı `OR`'lanmış
/// alt sorgular sorguyu ikiye katlardı. `ORDER BY` yalnızca sonucu
/// deterministik kılıyor (test edilebilirlik); semantik bir önemi yok.
///
/// # Errors
/// Veritabanı hatası [`crate::Error::Database`].
pub async fn visible_community_ids(pool: &PgPool, viewer: Option<i64>) -> Result<Vec<i64>> {
    let Some(actor_id) = viewer else {
        return Ok(Vec::new());
    };

    // Platform geneli moderatör tüm toplulukları görür — gerekçe modül
    // dokümantasyonundaki "Global moderatör istisnası".
    let global_moderator = sqlx::query_scalar!(
        r#"
        SELECT EXISTS (
            SELECT 1 FROM permissions
            WHERE actor_id = $1
              AND scope = 'global'
              AND permission IN ('content.delete', 'report.view', 'report.resolve')
        ) AS "exists!"
        "#,
        actor_id,
    )
    .fetch_one(pool)
    .await?;

    if global_moderator {
        let all = sqlx::query_scalar!(r#"SELECT id FROM communities ORDER BY id"#)
            .fetch_all(pool)
            .await?;
        return Ok(all);
    }

    let ids = sqlx::query_scalar!(
        r#"
        SELECT community_id AS "community_id!"
        FROM community_members
        WHERE actor_id = $1
        UNION
        SELECT community_id AS "community_id!"
        FROM permissions
        WHERE actor_id = $1 AND community_id IS NOT NULL
        ORDER BY 1
        "#,
        actor_id,
    )
    .fetch_all(pool)
    .await?;

    Ok(ids)
}

/// [`visible_community_ids`]'in çağrı yerlerini kısaltan ince sarmalayıcı.
///
/// Tamamen aynı işi yapıyor: ayrı bir isim yalnızca okuma yolundaki
/// handler'ların "bu isteğin izleyicisi için görünür topluluklar" niyetini
/// açıkça yazmasını sağlıyor. Ayrı bir mantık taşımıyor, o yüzden ikisinden
/// biri değişirse diğeri kendiliğinden değişir.
///
/// # Errors
/// [`visible_community_ids`] ile aynı.
pub async fn viewer_communities(pool: &PgPool, viewer: Option<i64>) -> Result<Vec<i64>> {
    visible_community_ids(pool, viewer).await
}

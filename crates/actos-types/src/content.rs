//! İçerik (post + yorum) uçlarının istek/yanıt tipleri.
//!
//! **Tek DTO, hem post hem yorum:** [`ContentSummary`] post'a özel değil,
//! `contents` tablosunun kendisi gibi *içerik*'e geneldir — sebebi
//! `actos_core::id` modülünün "Ortak ön ek" bölümündeki gerekçeyle aynı:
//! post ve yorum aynı tabloda, aynı ID uzayında yaşıyor (`c_...`). Bu
//! yanıt şeklinin ömrü bu fazı çok aşıyor: Faz 9 (yorumlar), Faz 10
//! (`GET /tags/{name}/posts`), Faz 12 (feed) ve Faz 7'den devredilen
//! `GET /actors/{username}/posts` hepsi bunu aynen yeniden kullanacak — bu
//! yüzden burada post'a özgü hiçbir alan (ör. yorum sayısı hariç, o zaten
//! post ve yorum ikisinde de anlamlı) yok.
//!
//! ## `title: Option<String>`
//!
//! Yorumlarda her zaman `None` (bkz. `migrations/0005_contents.up.sql` →
//! `ck_contents_shape`: `content_type='comment'` iken `title IS NULL`
//! şema seviyesinde zorunlu).
//!
//! ## `deleted` alanı neden var — `GET /posts/{id}` zaten `410` dönüyorken
//!
//! Tek bir içeriği doğrudan çeken uçlar (`GET /posts/{id}`) silinmiş bir
//! kayıt için gövde hiç üretmeden `410 Gone` döner (bkz.
//! `actos_core::content::get_post`) — yani bu DTO'nun `deleted: true`
//! hâli o uçtan asla çıkmaz. Ama bu DTO tek başına "bir içeriği tarif eden
//! genel şekil"; Faz 9'un yorum ağacı listelemesi ("silinen yorumun
//! çocukları yaşamaya devam eder, `[silindi]` gövdesiyle") ve Faz 12'nin
//! feed'i gibi *liste* bağlamlarında silinmiş bir öğe listenin geri
//! kalanını bozmadan satır içinde `[silindi]` olarak görünmek zorunda —
//! tüm sayfayı 410'a düşürmek orada yanlış olurdu. `deleted` + maskelenmiş
//! `title`/`body` bu ileriki kullanım için şimdiden burada.
//!
//! ## Silinmiş yazar maskelemesi
//!
//! `author`/`author_deleted` çiftinin nasıl dolduğu (hangi alanların
//! maskelendiği ve neden) `actos-api/src/routes/posts.rs` içindeki
//! `masked_actor_summary` üzerinde anlatılıyor — bu crate `actos-core`'a
//! bağımlı olamadığı için (bkz. crate kök dokümantasyonu) maskeleme
//! *kararının* kendisi burada değil, HTTP çeviri katmanında veriliyor; bu
//! modül yalnızca sonucu taşıyacak alanı tanımlıyor.
//!
//! Bu modül **hiçbir sunucu bağımlılığı içermez** (yalnızca `serde` +
//! `serde_json`), crate kök dokümantasyonundaki kuralla aynı.

use serde::{Deserialize, Serialize};

use crate::auth::ActorSummary;

/// Bir içeriğin (post ya da yorum) dışa dönük özeti.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentSummary {
    /// `actos_core::id::IdCodec`'le kodlanmış dış id (`c_7fGh2Kd`) — ham
    /// `bigint` asla buraya sızmaz.
    pub id: String,
    /// `"post"` veya `"comment"`. `actos_core::content::ContentType`
    /// bilerek `String` (bkz. modül başındaki `actos-core` bağımsızlığı
    /// kuralı — `ActorSummary.actor_type` ile aynı desen).
    pub content_type: String,
    pub author: ActorSummary,
    /// `true` ise `author` maskelenmiş demektir (bkz. modül dokümantasyonu
    /// "Silinmiş yazar maskelemesi").
    pub author_deleted: bool,
    /// Yalnızca `content_type == "post"` iken dolu; yorumlarda her zaman
    /// `None`.
    pub title: Option<String>,
    /// `deleted == true` iken maskelenmiş bir yer tutucudur, gerçek gövde
    /// değildir (bkz. modül dokümantasyonu).
    pub body: String,
    /// `"markdown"` veya `"plain"`.
    pub body_format: String,
    pub tags: Vec<String>,
    pub score: i32,
    pub upvotes: i32,
    pub downvotes: i32,
    pub comment_count: i32,
    /// RFC 3339.
    pub created_at: String,
    /// RFC 3339. `None` ise hiç düzenlenmedi.
    pub edited_at: Option<String>,
    /// `true` ise bu içerik soft-delete edilmiş; `title`/`body` gerçek
    /// değerleri taşımaz (bkz. modül dokümantasyonu).
    pub deleted: bool,
}

/// `POST /posts` istek gövdesi.
#[derive(Debug, Clone, Deserialize)]
pub struct CreatePostRequest {
    pub title: String,
    pub body: String,
    /// Boş olabilir. Var olmayan etiketler aynı transaction içinde
    /// oluşturulur (bkz. `actos_core::content::create_post`).
    #[serde(default)]
    pub tags: Vec<String>,
    /// Verilmezse boş obje (`{}`) varsayılır.
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

/// `PATCH /posts/{id}` istek gövdesi.
///
/// Kasıtlı olarak `Option<String>` — `Option<Option<String>>` DEĞİL: bir
/// post'un `title`'ı şema seviyesinde `NOT NULL` (bkz.
/// `migrations/0005_contents.up.sql` → `ck_contents_shape`), yani "temizle"
/// diye bir durum yok, yalnızca "dokunma" (`None`) / "güncelle"
/// (`Some(v)`) ayrımı var. `actos_types::actor::UpdateProfileRequest`'in
/// çift-`Option` kalıbı burada gereksiz.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdatePostRequest {
    pub title: Option<String>,
    pub body: Option<String>,
}

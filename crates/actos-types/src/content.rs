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
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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
    /// `body`'nin sanitize edilmiş HTML'i (Faz 18.A, bkz. NOTES.md §8.3).
    ///
    /// **Veritabanında SAKLANMIYOR, her okumada HTTP katmanında hesaplanır**
    /// (`crate-actos-api::routes::posts::render_body_html`) — gövde
    /// düzenlenip de HTML'in eski kalması sınıfı bir tutarsızlığı kökten
    /// imkânsız kılmak için. Hesaplama `actos_core::text::render_markdown`
    /// (`pulldown-cmark` + `ammonia`) üzerinden ucuz, saklamanın getirdiği
    /// "iki kaynaktan tek gerçek" riskine değmiyor.
    ///
    /// **`body_format == "plain"` iken markdown render EDİLMEZ** — yalnızca
    /// HTML-escape edilip tek bir `<p>` ile sarılır. Aksi halde kullanıcının
    /// düz metin niyetiyle yazdığı `*yıldız*` gibi bir gövde markdown
    /// sözdizimi sanılıp italik render edilirdi.
    ///
    /// `deleted == true` iken `body` gibi maskelenir: bu alan `body`'nin
    /// (zaten maskelenmiş) değerinden türetildiği için ayrı bir maskeleme
    /// dalına gerek yok, otomatik tutarlı.
    ///
    /// **`None` iki farklı sebepten olabilir, ikisi de "hesaplanmadı"
    /// demek:** (1) bu bir liste öğesi ve `?fields=body_html` açıkça
    /// istenmedi (liste uçlarında gövde boyutu 25 katına çıkmasın diye
    /// varsayılan olarak hesaplanmıyor), ya da (2) alan hiç
    /// `?fields=`'le filtrelenmedi ama çağıran uç zaten hesaplamıyor.
    /// Tekil uçlar (`GET /posts/{id}`, `GET /comments/{id}`) `?fields=`'ten
    /// bağımsız her zaman doldurur. `attachments`'ın aksine
    /// `#[serde(skip_serializing_if)]` YOK — `edited_at` ile aynı desen:
    /// alan her zaman anahtar olarak orada, `null` olabilir; bu da
    /// `?fields=body_html` filtresinin (bkz. `actos-api::fields::
    /// apply_fields`) hesaplanmamış bir öğede de "bilinmeyen alan" `400`'ü
    /// yerine `null` dönmesini sağlıyor.
    pub body_html: Option<String>,
    /// Serbest biçimli ek veri, her zaman bir JSON nesnesi (veri yoksa
    /// `{}`).
    ///
    /// **Karar: `{}` iken de alan hep gösterilir, hiçbir zaman
    /// atlanmıyor.** Alternatif ("boşsa alanı hiç serialize etme",
    /// `#[serde(skip_serializing_if = "...")]`) bant genişliğinde birkaç
    /// bayt kazandırırdı, ama bu DTO'daki `tags` (post'un hiç etiketi
    /// yoksa da `[]` olarak hep dolu) ile aynı ilkeyi bozardı: bir alanın
    /// var/yok'u onun *tipinden* değil *içeriğinden* etkileniyorsa,
    /// istemci (özellikle bunu ayrıştıran bir ajan) her alan için iki ayrı
    /// kod yolu yazmak zorunda kalır ("varsa oku, yoksa `{}` varsay").
    /// Sabit bir şema — alan her zaman orada, gerekirse boş — hem
    /// `?fields=metadata` ile açıkça istenebilmesini hem de istemci
    /// tarafında tek bir ayrıştırma kuralını garanti eder.
    pub metadata: serde_json::Value,
    pub tags: Vec<String>,
    pub score: i32,
    pub upvotes: i32,
    pub downvotes: i32,
    pub comment_count: i32,
    /// RFC 3339.
    pub created_at: String,
    /// RFC 3339. `None` ise hiç düzenlenmedi.
    pub edited_at: Option<String>,
    /// Bu içeriğe bağlı yüklemeler.
    ///
    /// **`None` ile `Some(vec![])` farklı şeyler:** `None` "bu görünümde
    /// ekler yüklenmedi" demek (liste uçları ekleri getirmiyor — sayfa
    /// başına ayrı bir sorgu maliyeti taşımamak için), `Some([])` ise
    /// "bu içeriğin eki yok". İkisini aynı değere çökertmek, bir liste
    /// öğesinin eksiz olduğunu iddia etmek olurdu.
    ///
    /// Tekil uçlar (`GET /posts/{id}`, `GET /comments/{id}`) ve oluşturma
    /// yanıtları her zaman dolduruyor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<crate::upload::UploadResponse>>,
    /// `true` ise bu içerik soft-delete edilmiş; `title`/`body` gerçek
    /// değerleri taşımaz (bkz. modül dokümantasyonu).
    pub deleted: bool,
}

/// `POST /posts` istek gövdesi.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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
    /// `POST /uploads`'tan dönen ek id'leri. Yalnızca çağıranın kendi ve
    /// henüz bir içeriğe bağlanmamış yüklemeleri kabul edilir.
    #[serde(default)]
    pub attachment_ids: Option<Vec<String>>,
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
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UpdatePostRequest {
    pub title: Option<String>,
    pub body: Option<String>,
}

/// `GET /actors/{username}/posts` yanıt gövdesi.
///
/// `actos_types::actor::ActorListResponse` ile aynı sarmalayıcı şekli
/// (öğe listesi + varsa sonraki sayfanın cursor'ı) — burada alan adı
/// `posts` (`actors` değil), çünkü uç özellikle post'lara özgü.
///
/// **`?fields=` ile alan seçimi bu sarmalayıcıya değil, `posts` içindeki
/// her öğeye uygulanır** (bkz. `actos-api/src/fields.rs` modül
/// dokümantasyonu) — yani HTTP katmanı bu tipi hiç kullanmadan, filtrelenmiş
/// öğelerle aynı şekle (`{"posts": [...], "next_cursor": ...}`) sahip ham
/// bir `serde_json::Value` üretebilir. Tip yine de burada tanımlı: SDK'lar
/// filtresiz (tam) yanıtı bu struct'a deserialize edebilsin diye.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PostListResponse {
    pub posts: Vec<ContentSummary>,
    /// `None` ise bu son sayfadır.
    pub next_cursor: Option<String>,
}

// --- Yorumlar (Faz 9) ------------------------------------------------------

/// `POST /posts/{id}/comments` isteği.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CreateCommentRequest {
    pub body: String,
    /// `POST /uploads`'tan dönen ek id'leri. Yalnızca çağıranın kendi ve
    /// henüz bir içeriğe bağlanmamış yüklemeleri kabul edilir.
    #[serde(default)]
    pub attachment_ids: Option<Vec<String>>,
    /// Verilmezse yorum post'un doğrudan çocuğu olur; verilirse o yoruma
    /// yanıt olur. Dış id (`c_...`) biçiminde.
    #[serde(default)]
    pub parent_id: Option<String>,
}

/// `PATCH /comments/{id}` isteği.
///
/// Post'un `PATCH`'inin aksine `Option` değil: yorumların düzenlenebilecek
/// tek alanı gövde, dolayısıyla "hangi alan gönderildi" ayrımına gerek yok
/// — gövdesiz bir yorum güncellemesi zaten anlamsız.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UpdateCommentRequest {
    pub body: String,
}

/// Bir yorum ağacındaki tek düğüm: içeriğin kendisi + doğrudan yanıtları.
///
/// [`ContentSummary`] alanları `flatten` ile düğümün kendisine açılıyor,
/// ayrı bir `content` sarmalayıcısı yok: istemci (özellikle bir ajan) bir
/// yorumu okurken `node.body` yazabilmeli, `node.content.body` değil.
/// `replies` bu düz alanların yanına eklenen tek fazladan anahtar.
///
/// **Boş `replies` yine de gönderiliyor** (atlanmıyor): bir ajanın
/// "yanıtlar alanı yok mu, yoksa boş mu" ayrımını yapmak zorunda kalmaması
/// için — her düğümde aynı şekil.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CommentNodeResponse {
    #[serde(flatten)]
    pub content: ContentSummary,
    /// `Vec<CommentNodeResponse>` — kendi tipine dönen bir döngü. utoipa'nın
    /// `ToSchema` türetmesi bunu `no_recursion` işaretlenmeden bırakırsa
    /// şema toplama fonksiyonu (`schemas()`) sonsuz döngüye girip **yığın
    /// taşmasıyla çöküyor** (ölçüldü: `cargo test` bu alan işaretsizken
    /// `has overflowed its stack` ile abort ediyordu — bkz. utoipa'nın kendi
    /// dokümanı, `#[schema(no_recursion)]` "Pet -> Owner -> Pet" örneği).
    /// `$ref` ile bir kere referans verip döngüyü burada kesiyoruz.
    #[cfg_attr(feature = "openapi", schema(no_recursion))]
    pub replies: Vec<CommentNodeResponse>,
}

/// `GET /posts/{id}/comments` yanıtı.
///
/// `next_cursor` **yalnızca üst seviye yorumları** sayfalar; iç içe
/// yanıtlar sayfalanmaz (bkz. `actos_core::comment::list_comment_tree`).
/// Daha derin bir alt ağaç `?parent=<id>` ile ayrıca çekilir.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CommentThreadResponse {
    pub comments: Vec<CommentNodeResponse>,
    /// `None` ise bu son sayfadır.
    pub next_cursor: Option<String>,
}

/// `GET /comments/{id}` yanıtı: yorum + kökten kendisine kadar ata zinciri.
///
/// `ancestors` kökten başlar (ilk öğe her zaman post'tur) ve yorumun
/// kendisini **içermez** — bir breadcrumb'ın doğal sırası bu.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CommentDetailResponse {
    pub comment: ContentSummary,
    pub ancestors: Vec<ContentSummary>,
}

/// `GET /actors/{username}/comments` yanıtı (Faz 7'den devir).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CommentListResponse {
    pub comments: Vec<ContentSummary>,
    /// `None` ise bu son sayfadır.
    pub next_cursor: Option<String>,
}

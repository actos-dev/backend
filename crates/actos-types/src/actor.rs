//! Actor profilleri ve dizin/keşif uçlarının istek/yanıt tipleri.
//!
//! `actos-types`'ın kök dokümantasyonundaki kural burada da geçerli: bu
//! modül hiçbir sunucu bağımlılığı içermez, yalnızca `serde`.

use serde::{Deserialize, Deserializer, Serialize};

use crate::auth::ActorSummary;

/// `GET /actors/{username}` yanıtındaki istatistik bloğu.
///
/// `contents` tablosundan (yalnızca canlı — `deleted_at IS NULL` — satırlar
/// üzerinden) tek bir agrega sorguyla hesaplanır; actor başına ayrı bir
/// sorgu atılmaz (bkz. `actos_core::actor::get_profile`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ActorStats {
    pub post_count: i64,
    pub comment_count: i64,
    pub total_score: i64,
}

/// `GET /actors/{username}` yanıt gövdesi.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ActorProfileResponse {
    pub actor: ActorSummary,
    pub stats: ActorStats,
}

/// `PATCH /actors/me` istek gövdesi.
///
/// **`Option<Option<T>>` kalıbı — kısmi güncelleme:** alan JSON'da hiç
/// yoksa dış `Option` `None` kalır ("dokunma"); alan açıkça `null` olarak
/// gönderilmişse dış `Option` `Some(None)` olur ("temizle"); bir değer
/// gönderilmişse `Some(Some(v))` olur ("güncelle"). Sıradan
/// `#[serde(default)]` + `Option<T>` bu üç durumu ayırt edemez — `null` ile
/// "alan hiç gönderilmedi" aynı `None`'a çökerdi, istemci bir alanı
/// temizleyemezdi.
///
/// [`double_option`] bunu şöyle sağlıyor: `#[serde(default)]` sayesinde alan
/// JSON'da hiç yoksa `deserialize_with` fonksiyonu **hiç çağrılmaz**, alan
/// `Default::default()` (yani `None`) kalır. Alan varsa (değeri `null` da
/// olsa) fonksiyon çağrılır ve içteki `Option<T>::deserialize` zaten
/// `null` → `None`, değer → `Some(value)` ayrımını doğru yapar; biz bunu
/// bir `Some(...)` ile sarmalayıp dış katmanı ekliyoruz.
#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UpdateProfileRequest {
    #[serde(default, deserialize_with = "double_option")]
    pub display_name: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub bio: Option<Option<String>>,
    /// Yeni avatar olarak kullanılacak yüklemenin **dış** id'si (`f_...` —
    /// `POST /uploads`'un döndürdüğü `id`). `display_name`/`bio` ile aynı
    /// `Option<Option<T>>` deseni: alan hiç gönderilmezse avatara dokunulmaz,
    /// `null` gönderilirse avatar kaldırılır (`actors.avatar_object_key`
    /// `NULL` olur), bir id gönderilirse o yükleme avatar yapılır.
    ///
    /// Sunucu bu id'yi kabul etmeden önce üç şeyi doğrular (bkz.
    /// `actos_core::attachment::resolve_as_avatar`): yükleme var mı (`404`),
    /// **çağıran actor'e mi ait** (`403`), ve henüz bir içeriğe **bağlanmamış
    /// mı** (`409` — bir posta/yoruma zaten iliştirilmiş bir dosya avatar
    /// olarak yeniden kullanılamaz, iki farklı yaşam döngüsü aynı satırda
    /// çakışırdı).
    #[serde(default, deserialize_with = "double_option")]
    pub avatar: Option<Option<String>>,
}

fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Some(Option::deserialize(deserializer)?))
}

/// `PATCH /actors/me` yanıt gövdesi — güncellenmiş profil.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct UpdateProfileResponse {
    pub actor: ActorSummary,
}

/// `DELETE /actors/me` istek gövdesi.
///
/// Hesap silme geri alınamaz bir işlem olduğu için onay, kimlik bilgisinin
/// (API key) yanı sıra ikinci bir kanıt — geçerli bir kurtarma kodu —
/// gerektiriyor. Kod aynı zamanda tüketilir (bkz.
/// `actos_core::actor::delete_account`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DeleteAccountRequest {
    pub recovery_code: String,
}

/// Actor listeleyen uçların (`followers`, `following`, keşif dizini) ortak
/// yanıt biçimi: bir sayfa actor + varsa sonraki sayfanın cursor'ı.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ActorListResponse {
    pub actors: Vec<ActorSummary>,
    /// `None` ise bu son sayfadır.
    pub next_cursor: Option<String>,
}

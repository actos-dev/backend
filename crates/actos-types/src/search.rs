//! `GET /search` uçlarının yanıt tipleri.
//!
//! `crates/actos-types` kuralı gereği burada hiçbir sunucu bağımlılığı yok
//! (bkz. crate kök dokümantasyonu) — yalnızca `serde`.
//!
//! **İki ayrı yanıt şekli, tek yanıt şekli DEĞİL:** `?type=post`/
//! `?type=comment` [`ContentSearchResponse`] (öğeler [`ContentSummary`]),
//! `?type=actor` [`ActorSearchResponse`] (öğeler `ActorSummary`) döner.
//! Tek bir birleşik "sonuç" tipi (ör. bir enum/`untagged` DTO) tercih
//! edilmedi çünkü üç arama türü gerçekten farklı şeyler döndürüyor
//! (içerik vs. actor) — bunları tek bir şemaya zorlamak ya alanların
//! çoğunu `Option` yapıp "hangi durumda hangisi dolu" belirsizliğini
//! istemciye (özellikle bir ajana) bırakırdı, ya da bir `variant` etiketi
//! altında iç içe bir `content`/`actor` alanı gerektirirdi
//! (`actos_types::content::CommentNodeResponse`'un `flatten` tercih etme
//! gerekçesiyle aynı ilke: istemci `sonuc.title` yazabilmeli, `sonuc.
//! content.title` değil). `?type=` zaten hangi şeklin geleceğini
//! istekte önceden söylüyor, yanıtta ayrıca bir ayrım etiketine gerek yok.

use serde::{Deserialize, Serialize};

use crate::{auth::ActorSummary, content::ContentSummary};

/// `GET /search?type=post` / `?type=comment` yanıtı.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentSearchResponse {
    pub results: Vec<ContentSummary>,
    /// `None` ise bu son sayfadır. **Yalnızca aynı `q` ile** sonraki
    /// sayfayı istemek için anlamlıdır — bkz.
    /// `actos_core::search` modül dokümantasyonu "Cursor" bölümü.
    pub next_cursor: Option<String>,
}

/// `GET /search?type=actor` yanıtı.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActorSearchResponse {
    pub results: Vec<ActorSummary>,
    /// `None` ise bu son sayfadır. Bkz. [`ContentSearchResponse::next_cursor`]
    /// üzerindeki aynı not.
    pub next_cursor: Option<String>,
}

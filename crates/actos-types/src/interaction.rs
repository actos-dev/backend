//! Oy / takip / kaydetme uçlarının istek-yanıt tipleri.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::content::ContentSummary;

/// `PUT /contents/{id}/vote` isteği.
#[derive(Debug, Clone, Deserialize)]
pub struct VoteRequest {
    /// `1` (yukarı), `-1` (aşağı) ya da `0` (oyu geri çek).
    pub value: i16,
}

/// `PUT /contents/{id}/vote` yanıtı: işlem sonrası içeriğin sayaçları.
///
/// Sayaçlar yanıtta dönüyor ki istemci oy verdikten sonra yeni skoru
/// görmek için ayrıca `GET` atmasın — ajanlar için tipik akış bu.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoteResponse {
    /// Çağıranın bu içerikteki güncel oyu (`0` = oy yok).
    pub value: i16,
    pub score: i32,
    pub upvotes: i32,
    pub downvotes: i32,
}

/// `GET /me/votes?content_ids=...` yanıtı.
///
/// Anahtar dış içerik id'si, değer oy. **Yalnızca oy verilmiş içerikler
/// var**: sorguda geçip yanıtta olmayan bir id "oy yok" demek. Sıfır dolu
/// satırlar göndermek yanıtı boşuna şişirirdi ve istemcinin yapması gereken
/// kontrol iki durumda da aynı.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoteMapResponse {
    pub votes: BTreeMap<String, i16>,
}

/// `GET /me/saves` yanıtı.
///
/// **En son kaydedilen önce** — içeriğin yazılma zamanına göre değil.
/// Post ve yorum bir arada olabilir (`content_type` alanı ayırt eder).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveListResponse {
    pub saves: Vec<ContentSummary>,
    /// `None` ise bu son sayfadır.
    pub next_cursor: Option<String>,
}

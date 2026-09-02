//! `?fields=id,title,score` alan seçimi.
//!
//! Ajanlar için bant genişliği tasarrufu (bkz. PLAN.md Faz 8): bir istemci
//! yalnızca ihtiyacı olan alanları isteyip geri kalan (ör. `body`, birkaç
//! KB'a kadar çıkabilen bir Markdown metni) gövdeye hiç girmesin.
//!
//! ## Uygulama: dinamik SQL değil, serialize-sonrası JSON filtreleme
//!
//! İki yol vardı: (1) `SELECT` listesini `fields`'e göre dinamik kurmak,
//! (2) DTO'yu her zaman tam olarak sorgulayıp/serialize edip, JSON'u
//! filtrelemek. İkincisi seçildi:
//!
//! - **Basitlik**: bu uçlardaki `SELECT`ler zaten çok tablolu (`JOIN` +
//!   `array_agg`) ve `sqlx::query_as!`'in derleme zamanı doğrulaması
//!   sabit bir sorgu metni gerektiriyor (bkz. `actos_core::content::
//!   get_post` dokümantasyonu) — `fields`'e göre sütun listesini çalışma
//!   zamanında değiştirmek bu doğrulamayı tamamen devre dışı bırakırdı.
//! - **Saldırı yüzeyi**: kullanıcı girdisinden `SELECT` sütun listesi
//!   kurmak (ne kadar dikkatli allowlist'lenirse edilsin) SQL injection
//!   sınıfı bir riski koda sokmanın gereksiz bir yolu; JSON son-işleme bu
//!   riski yapısal olarak imkânsız kılıyor.
//! - **Bedeli önemsiz**: post'lar tekil satır ya da küçük bir sayfa
//!   (`clamp_page_size`, azami 100), yani veritabanından "gereksiz" birkaç
//!   ekstra sütun okumanın maliyeti göz ardı edilebilir — asıl tasarruf
//!   zaten ağ üzerinden istemciye giden JSON gövdesinde.
//!
//! ## Tanınmayan alan adı → `400`
//!
//! Sessizce yok saymak (`fields=titel` yazım hatasını hiç fark ettirmeden
//! `title`'ı atlamak) bir ajanın kendi hatasını fark etmesini engeller —
//! `actos_core::Error::Validation` üzerinden her zaman açık bir `400` döner.
//!
//! ## Liste yanıtlarında: öğelere uygulanır, sarmalayıcıya değil
//!
//! `GET /actors/{username}/posts` gibi bir liste ucunda `fields` yalnızca
//! `posts` dizisindeki her öğeye uygulanır; `next_cursor` gibi sarmalayıcı
//! alanlar her zaman olduğu gibi kalır — `fields=next_cursor` diye bir şey
//! yok, `fields` her zaman *içerik* DTO'sunun alan adlarını konuşur. Bu
//! modül bilerek yalnızca tek bir DTO'yu filtreleyen [`apply_fields`]'i
//! sağlıyor; liste ucu bunu her öğe için ayrı ayrı çağırıp sarmalayıcıyı
//! kendisi kuruyor (bkz. `crate::routes::posts::list_actor_posts`).

use axum::http::HeaderMap;
use serde::Serialize;
use serde_json::Value;

use actos_core::Error;

use crate::error::ApiError;

/// Ham `?fields=a, b ,c` query değerini ayrıştırır.
///
/// `None`/boş/yalnızca boşluk-virgül → `None` (filtre yok, tüm alanlar
/// gösterilir). Her alan adı `trim` edilir; bu sayede `fields=id,%20title`
/// gibi istemcinin bıraktığı zararsız boşluklar reddedilmez.
#[must_use]
pub fn parse_fields(raw: Option<&str>) -> Option<Vec<String>> {
    let raw = raw?;
    let fields: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();

    if fields.is_empty() {
        None
    } else {
        Some(fields)
    }
}

/// Liste uçlarında `body_html`'in hesaplanıp hesaplanmayacağına karar
/// verir (Faz 18.A, bkz. `actos_types::content::ContentSummary::body_html`
/// dokümanı "Nerede dolu döner").
///
/// **Neden `apply_fields`'ten önce, ayrı bir kontrol:** `apply_fields`
/// zaten serialize edilmiş bir DTO üzerinde çalışıyor — hesaplamanın
/// kendisi (markdown render + `ammonia` sanitize) o noktada çoktan
/// yapılmış ya da hiç yapılmamış olmalı. Liste uçlarında gövde boyutu
/// 25 katına çıkmasın diye `body_html` varsayılan olarak hesaplanmıyor;
/// yalnızca istemci `?fields=body_html` (ya da `body_html`'i içeren daha
/// geniş bir küme) ile açıkça istediğinde hesaplanıyor. `fields` `None`
/// ise (filtre yok, tüm alanlar isteniyor) bilerek `false` dönüyor —
/// "filtresiz istek her şeyi ister" liste uçları için geçerli değil,
/// tekil uçlar zaten kendi `body_html`'ini `fields`'ten bağımsız hep
/// dolduruyor (bkz. `crate::routes::posts::content_summary_with_body_html`).
#[must_use]
pub fn wants_body_html(fields: Option<&[String]>) -> bool {
    fields.is_some_and(|f| f.iter().any(|name| name == "body_html"))
}

/// Bir DTO'yu serialize edip yalnızca `fields` içindeki anahtarları
/// bırakır. `fields` `None` ise DTO'nun tamamı (filtresiz) döner.
///
/// # Errors
/// `fields` içinde DTO'da karşılığı olmayan bir ad varsa
/// [`actos_core::Error::Validation`] (→ `400`, bkz. modül dokümantasyonu).
pub fn apply_fields<T: Serialize>(
    value: &T,
    fields: Option<&[String]>,
    headers: &HeaderMap,
) -> Result<Value, ApiError> {
    // `T` bu crate'teki tüm `ContentSummary` gibi DTO'lar için her zaman
    // bir struct'tır, yani `to_value` burada asla `Err` dönmez (döngüsel
    // referans ya da harita anahtarı olmayan bir tip yok) — yine de
    // `serde_json::Value`'nun genel API'si `Result` döndürdüğü için
    // savunmacı olarak ele alınıyor.
    let json = serde_json::to_value(value).map_err(|err| {
        ApiError::new(Error::Internal(format!("DTO serialize edilemedi: {err}")))
            .with_request_id(headers)
    })?;

    let Some(fields) = fields else {
        return Ok(json);
    };

    let Value::Object(obj) = json else {
        // Bu crate'teki her yanıt DTO'su bir struct'tan geldiği için bu
        // kol pratikte hiç çalışmaz; yine de bir DTO ileride bir tuple/enum
        // olursa filtrelemeden (sessizce) geçmek, panik atmaktan iyidir.
        return Ok(json);
    };

    let mut filtered = serde_json::Map::with_capacity(fields.len());
    for field in fields {
        match obj.get(field.as_str()) {
            Some(v) => {
                filtered.insert(field.clone(), v.clone());
            }
            None => {
                return Err(ApiError::new(Error::Validation(format!(
                    "bilinmeyen alan: \"{field}\""
                )))
                .with_request_id(headers));
            }
        }
    }

    Ok(Value::Object(filtered))
}

#[cfg(test)]
mod tests {
    use axum::{http::HeaderMap, response::IntoResponse as _};
    use serde::Serialize;
    use serde_json::json;

    use super::*;

    #[derive(Serialize)]
    struct Sample {
        id: &'static str,
        title: &'static str,
        score: i32,
    }

    #[test]
    fn fields_yoksa_filtre_uygulanmiyor() {
        assert!(parse_fields(None).is_none());
        assert!(parse_fields(Some("")).is_none());
        assert!(parse_fields(Some("   ,  ,")).is_none());
    }

    #[test]
    fn fields_ayristiriliyor_ve_trim_ediliyor() {
        let fields = parse_fields(Some("id, title ,score")).expect("dolu olmalı");
        assert_eq!(fields, vec!["id", "title", "score"]);
    }

    #[test]
    fn apply_fields_filtresiz_tum_dtoyu_donuyor() {
        let sample = Sample {
            id: "c_1",
            title: "başlık",
            score: 5,
        };
        let result = apply_fields(&sample, None, &HeaderMap::new()).expect("başarılı olmalı");
        assert_eq!(result, json!({"id": "c_1", "title": "başlık", "score": 5}));
    }

    #[test]
    fn apply_fields_yalnizca_istenenleri_birakiyor() {
        let sample = Sample {
            id: "c_1",
            title: "başlık",
            score: 5,
        };
        let fields = vec!["id".to_owned(), "score".to_owned()];
        let result =
            apply_fields(&sample, Some(&fields), &HeaderMap::new()).expect("başarılı olmalı");
        assert_eq!(result, json!({"id": "c_1", "score": 5}));
    }

    #[test]
    fn apply_fields_bilinmeyen_alan_400_uretiyor() {
        let sample = Sample {
            id: "c_1",
            title: "başlık",
            score: 5,
        };
        let fields = vec!["nope".to_owned()];
        let err = apply_fields(&sample, Some(&fields), &HeaderMap::new())
            .expect_err("bilinmeyen alan reddedilmeli");
        let response = err.into_response();
        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    }
}

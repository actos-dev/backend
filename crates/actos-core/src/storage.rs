//! S3-uyumlu nesne depolama (geliştirmede MinIO).

use aws_credential_types::Credentials;
use aws_sdk_s3::{Client, config::Region};
use aws_smithy_http_client::{Builder as HttpClientBuilder, tls};

use crate::config::StorageConfig;

/// Depolama istemcisi + hangi bucket'a yazacağı.
#[derive(Clone, Debug)]
pub struct Storage {
    client: Client,
    bucket: String,
    public_base_url: String,
}

impl Storage {
    /// İstemciyi kur. Ağ erişimi burada denenmez — `ping` ayrı çağrılır.
    #[must_use]
    pub fn new(cfg: &StorageConfig) -> Self {
        let credentials = Credentials::new(
            cfg.access_key.clone(),
            cfg.secret_key.clone(),
            None,
            None,
            "actos-config",
        );

        // HTTPS istemcisini elle kuruyoruz: SDK'nın hazır istemcisi aws-lc-rs
        // tabanlı, sqlx ise ring kullanıyor. Tek süreçte iki rustls kripto
        // sağlayıcısı bulunması istenmeyen bir durum, o yüzden ring'te birleştik.
        let http_client = HttpClientBuilder::new()
            .tls_provider(tls::Provider::Rustls(
                tls::rustls_provider::CryptoMode::Ring,
            ))
            .build_https();

        let s3_config = aws_sdk_s3::Config::builder()
            .http_client(http_client)
            .region(Region::new(cfg.region.clone()))
            .endpoint_url(cfg.endpoint.clone())
            .credentials_provider(credentials)
            // MinIO sanal-host stili adresleme yapmadığı için yol stili şart:
            // http://host/bucket/key  (http://bucket.host/key değil)
            .force_path_style(true)
            .behavior_version_latest()
            .build();

        Self {
            client: Client::from_conf(s3_config),
            bucket: cfg.bucket.clone(),
            public_base_url: cfg.public_base_url.trim_end_matches('/').to_owned(),
        }
    }

    #[must_use]
    pub fn client(&self) -> &Client {
        &self.client
    }

    #[must_use]
    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    /// Bir nesne anahtarının herkese açık URL'i.
    #[must_use]
    pub fn public_url(&self, object_key: &str) -> String {
        format!(
            "{}/{}",
            self.public_base_url,
            object_key.trim_start_matches('/')
        )
    }

    /// Bucket'ın erişilebilir olduğunu doğrular.
    ///
    /// # Errors
    /// Bucket yoksa, kimlik bilgileri yanlışsa veya depolama erişilemezse.
    pub async fn ping(&self) -> Result<(), StorageError> {
        self.client
            .head_bucket()
            .bucket(&self.bucket)
            .send()
            .await
            .map(|_| ())
            .map_err(|e| StorageError::Unreachable(e.to_string()))
    }

    /// Bir nesneyi bucket'a yazar.
    ///
    /// `content_type` yanıt header'ı olarak saklanıyor: bucket public-read
    /// olduğu için tarayıcı dosyayı doğrudan bu tipe göre yorumluyor
    /// (bkz. [`Self::public_url`]). Yanlış tip göndermek bir görselin
    /// indirilmesine ya da daha kötüsü yanlış yorumlanmasına yol açardı —
    /// bu yüzden çağıran onu tahmin etmiyor, `crate::media` normalize
    /// sonrası sabit `image/webp` veriyor.
    ///
    /// # Errors
    /// Yükleme başarısız olursa [`StorageError::Unreachable`].
    pub async fn put_object(
        &self,
        object_key: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> Result<(), StorageError> {
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(object_key)
            .content_type(content_type)
            .body(bytes.into())
            .send()
            .await
            .map(|_| ())
            .map_err(|e| StorageError::Unreachable(e.to_string()))
    }

    /// Bir nesneyi bucket'tan siler.
    ///
    /// S3 semantiği gereği **var olmayan bir anahtarı silmek de başarılı
    /// sayılır**; bu idempotency çağıranın işine geliyor (bkz.
    /// `crate::avatar::set_avatar`/`clear_avatar` — veritabanı satırı ile
    /// nesnenin ayrı düşmesi hâlinde tekrar denenebilsin).
    ///
    /// # Errors
    /// Silme başarısız olursa [`StorageError::Unreachable`].
    pub async fn delete_object(&self, object_key: &str) -> Result<(), StorageError> {
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(object_key)
            .send()
            .await
            .map(|_| ())
            .map_err(|e| StorageError::Unreachable(e.to_string()))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("nesne depolama erişilemiyor: {0}")]
    Unreachable(String),
}

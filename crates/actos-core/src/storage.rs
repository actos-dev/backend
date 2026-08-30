//! S3-uyumlu nesne depolama (geliştirmede MinIO).

use aws_credential_types::Credentials;
use aws_sdk_s3::{Client, config::Region};

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

        let s3_config = aws_sdk_s3::Config::builder()
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
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("nesne depolama erişilemiyor: {0}")]
    Unreachable(String),
}

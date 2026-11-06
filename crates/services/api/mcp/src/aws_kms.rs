//! AG-31 cloud KMS `install` — the aws-sdk-kms implementation of the
//! kernel's [`KmsOps`] seam. The kernel itself stays SDK-free; this binary
//! supplies the real AWS calls. A current-thread runtime bridges the SDK's
//! async API onto the seam's blocking methods (the bin has no other async
//! runtime).

use aikoql_kernel::security::kms::KmsOps;
use aws_config::BehaviorVersion;
use aws_sdk_kms::primitives::Blob;
use aws_sdk_kms::types::{KeyUsageType, OriginType};

/// The SDK behind [`aikoql_kernel::security::kms::AwsKms`].
pub(crate) struct SdkKmsOps {
    rt: tokio::runtime::Runtime,
    client: aws_sdk_kms::Client,
}

impl SdkKmsOps {
    /// Load the default AWS chain (env → ~/.aws → IAM); `region` overrides
    /// the chain's region when set.
    pub(crate) fn new(region: Option<String>) -> Result<Self, String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("tokio runtime: {e}"))?;
        let client = rt.block_on(async {
            let mut loader = aws_config::defaults(BehaviorVersion::latest());
            if let Some(r) = region {
                loader = loader.region(aws_config::Region::new(r));
            }
            let cfg = loader.load().await;
            aws_sdk_kms::Client::new(&cfg)
        });
        Ok(SdkKmsOps { rt, client })
    }
}

impl KmsOps for SdkKmsOps {
    fn create_key(&self, alias: &str) -> Result<String, String> {
        self.rt.block_on(async {
            let out = self
                .client
                .create_key()
                .description(format!("aikoql master key ({alias})"))
                .key_usage(KeyUsageType::EncryptDecrypt)
                .origin(OriginType::AwsKms)
                .send()
                .await
                .map_err(|e| format!("create_key: {e}"))?;
            let arn = out
                .key_metadata()
                .and_then(|m| m.arn())
                .ok_or("create_key returned no ARN")?
                .to_string();
            // The alias makes the key findable by name; a duplicate (a second
            // install under the same DB name) is a warning, not a failure.
            if let Err(e) = self
                .client
                .create_alias()
                .alias_name(format!("alias/{alias}"))
                .target_key_id(&arn)
                .send()
                .await
            {
                eprintln!("aikoql: warning: could not alias the CMK: {e}");
            }
            Ok(arn)
        })
    }

    fn encrypt(&self, key_id: &str, plaintext: &[u8]) -> Result<Vec<u8>, String> {
        self.rt.block_on(async {
            self.client
                .encrypt()
                .key_id(key_id)
                .plaintext(Blob::new(plaintext))
                .send()
                .await
                .map_err(|e| format!("kms encrypt: {e}"))?
                .ciphertext_blob()
                .map(|b| b.clone().into_inner())
                .ok_or_else(|| "kms encrypt returned no ciphertext".to_string())
        })
    }

    fn decrypt(&self, key_id: &str, ciphertext: &[u8]) -> Result<Vec<u8>, String> {
        self.rt.block_on(async {
            self.client
                .decrypt()
                .key_id(key_id)
                .ciphertext_blob(Blob::new(ciphertext))
                .send()
                .await
                .map_err(|e| format!("kms decrypt: {e}"))?
                .plaintext()
                .map(|b| b.clone().into_inner())
                .ok_or_else(|| "kms decrypt returned no plaintext".to_string())
        })
    }
}

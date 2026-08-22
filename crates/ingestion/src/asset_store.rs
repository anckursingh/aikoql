//! D3.5: Content-addressed asset store (DoD row 12).
//!
//! Visual assets (embedded images, extracted figures) are identified and
//! persisted by their sha256 content hash — identical bytes dedupe, and a
//! `VisualAssetRef` on an AST node references the hash, never a mutable path.

use std::path::Path;

/// MIME type for a file name, from its extension. Unknown → octet-stream.
pub fn mime_from_extension(name: &str) -> String {
    let ext = Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// sha256 of `bytes`, hex-encoded — the content address.
pub fn content_hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Persist `bytes` as `{dir}/{hash}.bin`, skipping the write when the asset
/// already exists (content-addressed dedupe). Returns the content hash.
pub fn store_asset(dir: &str, bytes: &[u8]) -> Result<String, String> {
    let hash = content_hash(bytes);
    let path = Path::new(dir).join(format!("{}.bin", hash));
    if !path.exists() {
        std::fs::create_dir_all(dir).map_err(|e| format!("asset store dir '{}': {}", dir, e))?;
        std::fs::write(&path, bytes)
            .map_err(|e| format!("write asset '{}': {}", path.display(), e))?;
    }
    Ok(hash)
}

/// Load asset bytes by content hash. `None` when not stored.
pub fn load_asset(dir: &str, hash: &str) -> Option<Vec<u8>> {
    let path = Path::new(dir).join(format!("{}.bin", hash));
    std::fs::read(&path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_is_sha256_hex() {
        let hash = content_hash(b"hello");
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        // Known sha256 of "hello".
        assert_eq!(
            hash,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn store_then_load_roundtrip() {
        let dir = std::env::temp_dir().join("aikoql-asset-store-test");
        let hash = store_asset(dir.to_str().unwrap(), b"asset bytes").unwrap();
        assert_eq!(
            load_asset(dir.to_str().unwrap(), &hash).unwrap(),
            b"asset bytes"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn store_dedupes_identical_bytes() {
        let dir = std::env::temp_dir().join("aikoql-asset-store-dedupe");
        let h1 = store_asset(dir.to_str().unwrap(), b"same bytes").unwrap();
        let h2 = store_asset(dir.to_str().unwrap(), b"same bytes").unwrap();
        assert_eq!(h1, h2);
        // One stored file — the second store was a no-op.
        let files: Vec<_> = std::fs::read_dir(&dir)
            .map(|rd| rd.filter_map(|e| e.ok()).collect::<Vec<_>>())
            .unwrap_or_default();
        assert_eq!(files.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_asset_loads_none() {
        let dir = std::env::temp_dir().join("aikoql-asset-store-empty");
        assert!(load_asset(dir.to_str().unwrap(), "0".repeat(64).as_str()).is_none());
    }

    #[test]
    fn mime_from_extension_maps_common_types() {
        assert_eq!(mime_from_extension("logo.png"), "image/png");
        assert_eq!(mime_from_extension("photo.JPG"), "image/jpeg");
        assert_eq!(mime_from_extension("diagram.svg"), "image/svg+xml");
        assert_eq!(
            mime_from_extension("mystery.xyz"),
            "application/octet-stream"
        );
    }
}

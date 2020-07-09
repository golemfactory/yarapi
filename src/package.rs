use anyhow::Result;
use sha3::{Digest, Sha3_512};
use std::path::PathBuf;
use tokio::fs;
use url::Url;

#[derive(Debug, Clone)]
pub enum Package {
    /// Path to Yagna package. Hash information will be computed automatically.
    Archive(PathBuf),
    /// URL to a resource already published using `gftp` protocol.
    ///
    /// # Example:
    /// ```rust
    /// use ya_requestor_sdk::Package;
    /// let package = Package::Url { hash: "beefdead".to_string(), url: "gftp:deadbeef/deadbeef".to_string() };
    /// ```
    Url { digest: String, url: String },
}

impl Package {
    /// Publishes the `Package` if specified as `Package::Archive`, and computes
    /// the package's `sha3` hash.
    ///
    /// If the `Package` is specified as `Package::Url`, verifies the url is correct
    /// but does not re-publish the package (assumes it is already published).
    ///
    /// In all cases, `gftp` is the assumed communication medium.
    pub async fn publish(&self) -> Result<(String, Url)> {
        match self {
            Self::Archive(path) => {
                let image_path = path.canonicalize()?;

                log::info!("image file path: {}", image_path.display());

                let url = gftp::publish(&path).await?;

                log::info!("image published at: {}", url);

                let contents = fs::read(&image_path).await?;
                let digest = Sha3_512::digest(&contents);
                let digest = format!("{:x}", digest);

                log::info!("image's computed digest: {}", digest);

                Ok((digest, url))
            }
            Self::Url { digest, url } => {
                let url = Url::parse(&url)?;

                log::info!("parsed url for image file: {}", url);
                log::info!("digest of the published image: {}", digest);

                Ok((digest.clone(), url))
            }
        }
    }
}

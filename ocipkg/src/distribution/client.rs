use anyhow::Context;
use bytes::Bytes;
use oci_spec::{distribution::*, image::*};
use serde::Deserialize;
use url::Url;

use crate::{distribution::*, Digest};

/// A client for `/v2/<name>/` API endpoint
pub struct Client {
    client: reqwest::Client,
    /// URL to registry server
    url: Url,
    /// Name of repository
    name: Name,
}

/// Response of `/v2/<name>/tags/list`
#[derive(Debug, Clone, PartialEq, Deserialize)]
struct TagList {
    name: String,
    tags: Vec<String>,
}

impl Client {
    pub fn new(url: &Url, name: &str) -> anyhow::Result<Self> {
        let client = reqwest::Client::new();
        let name = Name::new(name)?;
        Ok(Client {
            client,
            url: url.clone(),
            name,
        })
    }

    /// Get tags of `<name>` repository.
    ///
    /// ```text
    /// GET /v2/<name>/tags/list
    /// ```
    ///
    /// See [corresponding OCI distribution spec document](https://github.com/opencontainers/distribution-spec/blob/main/spec.md#content-discovery) for detail.
    pub async fn get_tags(&self) -> anyhow::Result<Vec<String>> {
        let tag_list = self
            .client
            .get(
                self.url
                    .join(&format!("/v2/{}/tags/list", self.name.as_str()))?,
            )
            .send()
            .await?
            .json::<TagList>()
            .await?;
        Ok(tag_list.tags)
    }

    /// Get manifest for given repository
    ///
    /// ```text
    /// GET /v2/<name>/manifests/<reference>
    /// ```
    ///
    /// See [corresponding OCI distribution spec document](https://github.com/opencontainers/distribution-spec/blob/main/spec.md#pulling-manifests) for detail.
    pub async fn get_manifest(&self, reference: &str) -> anyhow::Result<ImageManifest> {
        let reference = Reference::new(reference)?;
        let manifest = self
            .client
            .get(self.url.join(&format!(
                "/v2/{}/manifests/{}",
                self.name.as_str(),
                reference.as_str()
            ))?)
            .header("Accept", MediaType::ImageManifest.to_docker_v2s2()?)
            .send()
            .await?
            .text()
            .await?;
        let manifest = ImageManifest::from_reader(manifest.as_bytes())?;
        Ok(manifest)
    }

    /// Push manifest to registry
    ///
    /// ```text
    /// PUT /v2/<name>/manifests/<reference>
    /// ```
    ///
    /// Manifest must be pushed after blobs are updated.
    ///
    /// See [corresponding OCI distribution spec document](https://github.com/opencontainers/distribution-spec/blob/main/spec.md#pushing-manifests) for detail.
    pub async fn push_manifest(
        &self,
        reference: &str,
        manifest: &ImageManifest,
    ) -> anyhow::Result<Url> {
        let reference = Reference::new(reference)?;
        let mut buf = Vec::new();
        manifest.to_writer(&mut buf)?;
        let res = self
            .client
            .put(
                self.url
                    .join(&format!("/v2/{}/manifests/{}", self.name, reference))?,
            )
            .header("Content-Type", MediaType::ImageManifest.to_string())
            .body(buf)
            .send()
            .await?;
        let url = response_with_location(res).await?;
        Ok(url)
    }

    /// Get blob for given digest
    ///
    /// ```text
    /// GET /v2/<name>/blobs/<digest>
    /// ```
    ///
    /// See [corresponding OCI distribution spec document](https://github.com/opencontainers/distribution-spec/blob/main/spec.md#pulling-blobs) for detail.
    pub async fn get_blob(&self, digest: &str) -> anyhow::Result<Bytes> {
        let digest = Digest::new(digest)?;
        let blob = self
            .client
            .get(
                self.url
                    .join(&format!("/v2/{}/blobs/{}", self.name.as_str(), digest,))?,
            )
            .send()
            .await?
            .bytes()
            .await?;
        Ok(blob)
    }

    /// Push blob to registry
    ///
    /// ```text
    /// POST /v2/<name>/blobs/uploads/
    /// ```
    ///
    /// and following `PUT` to URL obtained by `POST`.
    ///
    /// See [corresponding OCI distribution spec document](https://github.com/opencontainers/distribution-spec/blob/main/spec.md#pushing-manifests) for detail.
    pub async fn push_blob(&self, blob: &[u8]) -> anyhow::Result<Url> {
        let res = self
            .client
            .post(
                self.url
                    .join(&format!("/v2/{}/blobs/uploads/", self.name))?,
            )
            .send()
            .await?;
        let url = response_with_location(res)
            .await
            .context("POST /v2/<name>/blobs/uploads/ failed")?;

        let digest = Digest::from_buf_sha256(blob);
        let res = self
            .client
            .put(url.clone())
            .query(&[("digest", digest.to_string())])
            .header("Content-Length", blob.len())
            .header("Content-Type", "application/octet-stream")
            .body(blob.to_vec())
            .send()
            .await?;
        let url = response_with_location(res)
            .await
            .with_context(|| format!("PUT to {} failed", url))?;
        Ok(url)
    }
}

// Most of API returns `Location: <location>`
async fn response_with_location(res: reqwest::Response) -> anyhow::Result<Url> {
    if res.status().is_success() {
        let location = res
            .headers()
            .get("Location")
            .context("Location not included in response")?;
        Ok(Url::parse(location.to_str()?)?)
    } else {
        let err = res.json::<ErrorResponse>().await?;
        Err(anyhow::Error::new(err))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    //
    // Following tests need registry server. See test/fixture.sh for setting.
    // These tests are ignored by default.
    //

    fn test_url() -> Url {
        Url::parse("http://localhost:5000").unwrap()
    }
    const TEST_REPO: &str = "test_repo";

    #[tokio::test]
    #[ignore]
    async fn get_tags() -> anyhow::Result<()> {
        let client = Client::new(&test_url(), TEST_REPO)?;
        let mut tags = client.get_tags().await?;
        tags.sort_unstable();
        assert_eq!(
            tags,
            &["tag1".to_string(), "tag2".to_string(), "tag3".to_string()]
        );
        Ok(())
    }

    #[tokio::test]
    #[ignore]
    async fn get_images() -> anyhow::Result<()> {
        let client = Client::new(&test_url(), TEST_REPO)?;
        for tag in ["tag1", "tag2", "tag3"] {
            let manifest = client.get_manifest(tag).await?;
            for layer in manifest.layers() {
                let buf = client.get_blob(layer.digest()).await?;
                dbg!(buf.len());
            }
        }
        Ok(())
    }

    #[tokio::test]
    #[ignore]
    async fn push_blob() -> anyhow::Result<()> {
        let client = Client::new(&test_url(), TEST_REPO)?;
        let url = client.push_blob("test string".as_bytes()).await?;
        dbg!(url);
        Ok(())
    }
}
use plugin_toolkit::http::{Client as HttpClient, HttpError};
use plugin_toolkit::serde::de::DeserializeOwned;
use plugin_toolkit::serde::{Deserialize, Serialize};
use plugin_toolkit::thiserror::Error;

#[derive(Debug, Error)]
pub enum ImmichError {
    #[error("immich http: {0}")]
    Http(#[from] HttpError),
}

#[derive(Clone)]
pub struct Immich {
    base_url: String,
    api_key: String,
    http: HttpClient,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(crate = "plugin_toolkit::serde")]
pub struct Ping {
    pub res: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(crate = "plugin_toolkit::serde")]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    #[serde(default)]
    pub prerelease: Option<u32>,
}

impl Version {
    pub fn semver(&self) -> String {
        match self.prerelease {
            Some(p) => format!("{}.{}.{}-{p}", self.major, self.minor, self.patch),
            None => format!("{}.{}.{}", self.major, self.minor, self.patch),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(crate = "plugin_toolkit::serde", rename_all = "camelCase")]
pub struct ServerStats {
    pub photos: u64,
    pub videos: u64,
    pub usage: u64,
    pub usage_photos: u64,
    pub usage_videos: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(crate = "plugin_toolkit::serde")]
pub struct AssetStats {
    pub images: u64,
    pub videos: u64,
    pub total: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(crate = "plugin_toolkit::serde")]
pub struct CheckItem {
    pub id: String,
    pub checksum: String,
}

#[derive(Serialize)]
#[serde(crate = "plugin_toolkit::serde")]
struct CheckRequest<'a> {
    assets: &'a [CheckItem],
}

#[derive(Debug, Clone, Deserialize)]
#[serde(crate = "plugin_toolkit::serde", rename_all = "camelCase")]
pub struct CheckResult {
    pub id: String,
    pub action: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub asset_id: Option<String>,
    #[serde(default)]
    pub is_trashed: Option<bool>,
}

#[derive(Deserialize)]
#[serde(crate = "plugin_toolkit::serde")]
struct CheckResponse {
    results: Vec<CheckResult>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(crate = "plugin_toolkit::serde", rename_all = "camelCase")]
pub struct Asset {
    pub id: String,
    pub original_file_name: String,
    #[serde(default)]
    pub original_path: Option<String>,
    #[serde(default)]
    pub owner_id: Option<String>,
    pub checksum: String,
    #[serde(default)]
    pub is_trashed: bool,
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[derive(Serialize)]
#[serde(crate = "plugin_toolkit::serde", rename_all = "camelCase")]
struct MetadataSearch<'a> {
    with_deleted: bool,
    trashed_after: &'a str,
    page: u32,
    size: u32,
}

#[derive(Deserialize)]
#[serde(crate = "plugin_toolkit::serde")]
struct SearchResponse {
    assets: SearchAssets,
}

#[derive(Deserialize)]
#[serde(crate = "plugin_toolkit::serde", rename_all = "camelCase")]
struct SearchAssets {
    items: Vec<Asset>,
    #[serde(default)]
    next_page: Option<String>,
}

const TRASH_PAGE_SIZE: u32 = 1000;
pub const TRASH_MAX_PAGES: u32 = 100;

impl Immich {
    pub fn new(base_url: &str, api_key: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            http: HttpClient::new(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ImmichError> {
        let resp = self
            .http
            .get(self.url(path))
            .header("x-api-key", &self.api_key)
            .send()
            .await?;
        Ok(resp.json()?)
    }

    async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ImmichError> {
        let resp = self
            .http
            .post(self.url(path))
            .header("x-api-key", &self.api_key)
            .json(body)
            .retry(true)
            .send()
            .await?;
        Ok(resp.json()?)
    }

    pub async fn ping(&self) -> Result<Ping, ImmichError> {
        self.get("/api/server/ping").await
    }

    pub async fn version(&self) -> Result<Version, ImmichError> {
        self.get("/api/server/version").await
    }

    pub async fn server_statistics(&self) -> Result<ServerStats, ImmichError> {
        self.get("/api/server/statistics").await
    }

    pub async fn asset_statistics(&self, trashed: bool) -> Result<AssetStats, ImmichError> {
        self.get(&format!("/api/assets/statistics?isTrashed={trashed}"))
            .await
    }

    pub async fn bulk_upload_check(
        &self,
        items: &[CheckItem],
    ) -> Result<Vec<CheckResult>, ImmichError> {
        let resp: CheckResponse = self
            .post(
                "/api/assets/bulk-upload-check",
                &CheckRequest { assets: items },
            )
            .await?;
        Ok(resp.results)
    }

    pub async fn trashed_assets(&self, limit: usize) -> Result<(Vec<Asset>, bool), ImmichError> {
        let mut out = Vec::new();
        let mut page = 1;
        loop {
            let body = MetadataSearch {
                with_deleted: true,
                trashed_after: "1970-01-01T00:00:00.000Z",
                page,
                size: TRASH_PAGE_SIZE,
            };
            let resp: SearchResponse = self.post("/api/search/metadata", &body).await?;
            out.extend(resp.assets.items.into_iter().filter(|a| a.is_trashed));
            let next = resp.assets.next_page.and_then(|p| p.parse::<u32>().ok());
            if out.len() >= limit {
                let more = out.len() > limit || next.is_some();
                out.truncate(limit);
                return Ok((out, more));
            }
            match next {
                Some(n) if n > page && n <= TRASH_MAX_PAGES => page = n,
                Some(n) if n > page => return Ok((out, true)),
                _ => break,
            }
        }
        Ok((out, false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plugin_toolkit::serde_json::json;
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn server_endpoints_parse_and_send_key() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/server/ping"))
            .and(header("x-api-key", "k"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"res": "pong"})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/server/version"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"major": 2, "minor": 1, "patch": 0, "prerelease": null})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/server/statistics"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "photos": 10, "videos": 2, "usage": 300, "usagePhotos": 200,
                "usageVideos": 100, "usageByUser": []
            })))
            .mount(&server)
            .await;
        let c = Immich::new(&format!("{}/", server.uri()), "k");
        assert_eq!(c.ping().await.unwrap().res, "pong");
        assert_eq!(c.version().await.unwrap().semver(), "2.1.0");
        let s = c.server_statistics().await.unwrap();
        assert_eq!((s.photos, s.videos, s.usage_videos), (10, 2, 100));
    }

    #[tokio::test]
    async fn asset_statistics_passes_trash_filter() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/assets/statistics"))
            .and(query_param("isTrashed", "true"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"images": 3, "videos": 1, "total": 4})),
            )
            .mount(&server)
            .await;
        let s = Immich::new(&server.uri(), "k")
            .asset_statistics(true)
            .await
            .unwrap();
        assert_eq!(s.total, 4);
    }

    #[tokio::test]
    async fn bulk_upload_check_round_trips() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/assets/bulk-upload-check"))
            .and(header("x-api-key", "k"))
            .and(body_json(
                json!({"assets": [{"id": "a.jpg", "checksum": "ab"}]}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": [
                {"id": "a.jpg", "action": "reject", "reason": "duplicate",
                 "assetId": "u1", "isTrashed": true}
            ]})))
            .mount(&server)
            .await;
        let r = Immich::new(&server.uri(), "k")
            .bulk_upload_check(&[CheckItem {
                id: "a.jpg".into(),
                checksum: "ab".into(),
            }])
            .await
            .unwrap();
        assert_eq!(r[0].asset_id.as_deref(), Some("u1"));
        assert_eq!(r[0].is_trashed, Some(true));
    }

    #[tokio::test]
    async fn trashed_assets_follows_pages() {
        let server = MockServer::start().await;
        let asset = |id: &str, trashed: bool| {
            json!({"id": id, "originalFileName": format!("{id}.jpg"), "checksum": "c",
                   "isTrashed": trashed, "updatedAt": "2026-01-01T00:00:00.000Z"})
        };
        Mock::given(method("POST"))
            .and(path("/api/search/metadata"))
            .and(body_json(json!({"withDeleted": true,
                "trashedAfter": "1970-01-01T00:00:00.000Z", "page": 1, "size": 1000})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"assets": {
                "items": [asset("a", true), asset("b", false)], "nextPage": "2"
            }})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/search/metadata"))
            .and(body_json(json!({"withDeleted": true,
                "trashedAfter": "1970-01-01T00:00:00.000Z", "page": 2, "size": 1000})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"assets": {
                "items": [asset("c", true)], "nextPage": null
            }})))
            .mount(&server)
            .await;
        let ids: Vec<String> = Immich::new(&server.uri(), "k")
            .trashed_assets(10)
            .await
            .unwrap()
            .0
            .into_iter()
            .map(|a| a.id)
            .collect();
        assert_eq!(ids, ["a", "c"]);
    }

    #[tokio::test]
    async fn trashed_assets_respects_limit() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/search/metadata"))
            .and(header("x-api-key", "k"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"assets": {
                "items": [
                    {"id": "a", "originalFileName": "a", "checksum": "c", "isTrashed": true},
                    {"id": "b", "originalFileName": "b", "checksum": "c", "isTrashed": true}
                ], "nextPage": "2"
            }})))
            .expect(1)
            .mount(&server)
            .await;
        let (assets, truncated) = Immich::new(&server.uri(), "k")
            .trashed_assets(1)
            .await
            .unwrap();
        assert_eq!(assets.len(), 1);
        assert!(truncated);
    }

    #[tokio::test]
    async fn status_error_surfaces() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/server/ping"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        assert!(Immich::new(&server.uri(), "k").ping().await.is_err());
    }
}

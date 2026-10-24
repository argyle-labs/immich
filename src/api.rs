use plugin_toolkit::reqwest::header::HeaderValue;
use plugin_toolkit::reqwest::{Client as HttpClient, RequestBuilder};
use plugin_toolkit::serde::de::DeserializeOwned;
use plugin_toolkit::serde::{Deserialize, Serialize};
use plugin_toolkit::thiserror::Error;

#[derive(Debug, Error)]
pub enum ImmichError {
    #[error("immich http: {0}")]
    Http(String),
    #[error("immich http {status}")]
    Status { status: u16 },
    #[error("immich decode: {0}")]
    Decode(String),
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
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
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

    async fn send<T: DeserializeOwned>(&self, req: RequestBuilder) -> Result<T, ImmichError> {
        let mut key = HeaderValue::from_str(&self.api_key)
            .map_err(|_| ImmichError::Http("invalid api key".into()))?;
        key.set_sensitive(true);
        let resp = req
            .header("x-api-key", key)
            .header("accept", HeaderValue::from_static("application/json"))
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|e| ImmichError::Http(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(ImmichError::Status {
                status: status.as_u16(),
            });
        }
        let body = resp
            .bytes()
            .await
            .map_err(|e| ImmichError::Http(e.to_string()))?;
        serde_json::from_slice(&body).map_err(|e| ImmichError::Decode(e.to_string()))
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ImmichError> {
        self.send(self.http.get(self.url(path))).await
    }

    async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ImmichError> {
        self.send(self.http.post(self.url(path)).json(body)).await
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
    use crate::testing::{block, serve, Reply};
    use serde_json::json;

    const BASE: &str = "http://immich.test";

    #[test]
    fn server_endpoints_parse_and_send_key() {
        let calls = serve(
            |_, path, _| match path {
                "/api/server/ping" => Reply::ok(json!({"res": "pong"})),
                "/api/server/version" => {
                    Reply::ok(json!({"major": 2, "minor": 1, "patch": 0, "prerelease": null}))
                }
                "/api/server/statistics" => Reply::ok(json!({
                    "photos": 10, "videos": 2, "usage": 300, "usagePhotos": 200,
                    "usageVideos": 100, "usageByUser": []
                })),
                _ => Reply::status(404),
            },
            || {
                let c = Immich::new(&format!("{BASE}/"), "k");
                assert_eq!(block(c.ping()).unwrap().res, "pong");
                assert_eq!(block(c.version()).unwrap().semver(), "2.1.0");
                let s = block(c.server_statistics()).unwrap();
                assert_eq!((s.photos, s.videos, s.usage_videos), (10, 2, 100));
            },
        );
        assert_eq!(calls.len(), 3);
        assert!(calls.iter().all(|c| c.api_key.as_deref() == Some("k")));
    }

    #[test]
    fn asset_statistics_passes_trash_filter() {
        let calls = serve(
            |_, _, _| Reply::ok(json!({"images": 3, "videos": 1, "total": 4})),
            || {
                let s = block(Immich::new(BASE, "k").asset_statistics(true)).unwrap();
                assert_eq!(s.total, 4);
            },
        );
        assert_eq!(calls[0].path, "/api/assets/statistics?isTrashed=true");
    }

    #[test]
    fn bulk_upload_check_round_trips_with_key() {
        let calls = serve(
            |_, _, _| {
                Reply::ok(json!({"results": [
                    {"id": "a.jpg", "action": "reject", "reason": "duplicate",
                     "assetId": "u1", "isTrashed": true}
                ]}))
            },
            || {
                let r = block(Immich::new(BASE, "k").bulk_upload_check(&[CheckItem {
                    id: "a.jpg".into(),
                    checksum: "ab".into(),
                }]))
                .unwrap();
                assert_eq!(r[0].asset_id.as_deref(), Some("u1"));
                assert_eq!(r[0].is_trashed, Some(true));
            },
        );
        assert_eq!(calls[0].method, "POST");
        assert_eq!(calls[0].path, "/api/assets/bulk-upload-check");
        assert_eq!(calls[0].api_key.as_deref(), Some("k"));
        assert_eq!(
            calls[0].body,
            json!({"assets": [{"id": "a.jpg", "checksum": "ab"}]})
        );
    }

    fn asset(id: &str, trashed: bool) -> serde_json::Value {
        json!({"id": id, "originalFileName": format!("{id}.jpg"), "checksum": "c",
               "isTrashed": trashed, "updatedAt": "2026-01-01T00:00:00.000Z"})
    }

    #[test]
    fn trashed_assets_follows_pages() {
        let calls = serve(
            |_, _, body| match body["page"].as_u64() {
                Some(1) => Reply::ok(json!({"assets": {
                    "items": [asset("a", true), asset("b", false)], "nextPage": "2"}})),
                _ => Reply::ok(json!({"assets": {"items": [asset("c", true)], "nextPage": null}})),
            },
            || {
                let (assets, truncated) = block(Immich::new(BASE, "k").trashed_assets(10)).unwrap();
                let ids: Vec<String> = assets.into_iter().map(|a| a.id).collect();
                assert_eq!(ids, ["a", "c"]);
                assert!(!truncated);
            },
        );
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].api_key.as_deref(), Some("k"));
        assert_eq!(
            calls[0].body,
            json!({"withDeleted": true, "trashedAfter": "1970-01-01T00:00:00.000Z",
                   "page": 1, "size": 1000})
        );
    }

    #[test]
    fn trashed_assets_respects_limit() {
        let calls = serve(
            |_, _, _| {
                Reply::ok(json!({"assets": {
                    "items": [asset("a", true), asset("b", true)], "nextPage": "2"}}))
            },
            || {
                let (assets, truncated) = block(Immich::new(BASE, "k").trashed_assets(1)).unwrap();
                assert_eq!(assets.len(), 1);
                assert!(truncated);
            },
        );
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn status_error_surfaces() {
        serve(
            |_, _, _| Reply::status(401),
            || {
                let e = block(Immich::new(BASE, "k").ping()).unwrap_err();
                assert!(matches!(e, ImmichError::Status { status: 401 }));
            },
        );
    }
}

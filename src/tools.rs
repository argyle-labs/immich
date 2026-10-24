use std::path::PathBuf;

use plugin_toolkit::prelude::*;

use crate::api::{CheckItem, CheckResult, Immich};
use crate::scan;

const CHECK_BATCH: usize = 1000;
const DEFAULT_HASH_CONCURRENCY: usize = 4;

#[endpoint_resource(plugin = "immich")]
pub struct ImmichEndpoint {
    pub name: String,
    /// Immich API key, sent as `x-api-key`.
    #[secret]
    pub api_key: String,
    pub enabled: bool,
}

pub async fn connect(name: &str) -> Result<Immich> {
    let row = endpoint_db::require(name)?;
    if !row.enabled {
        bail!("immich endpoint '{name}' is disabled");
    }
    let api_key = plugin_toolkit::secrets::resolve_scoped("immich", name, "api_key")?;
    let base_url = route::resolve_reachable(name, &row.routes, false).await?;
    Ok(Immich::new(&base_url, api_key.expose_secret()))
}

#[orca_struct(args)]
pub struct EndpointArgs {
    /// Registered immich endpoint name.
    #[arg(long)]
    pub name: String,
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq)]
pub struct StatusReport {
    pub reachable: bool,
    pub version: String,
    pub photos: Option<u64>,
    pub videos: Option<u64>,
    pub usage_bytes: Option<u64>,
    /// Set when `/api/server/statistics` fails; it requires an admin API key.
    pub statistics_error: Option<String>,
}

pub async fn status_of(c: &Immich) -> Result<StatusReport> {
    let ping = c.ping().await?;
    let version = c.version().await?.semver();
    let (stats, statistics_error) = match c.server_statistics().await {
        Ok(s) => (Some(s), None),
        Err(e) => (None, Some(e.to_string())),
    };
    Ok(StatusReport {
        reachable: ping.res == "pong",
        version,
        photos: stats.as_ref().map(|s| s.photos),
        videos: stats.as_ref().map(|s| s.videos),
        usage_bytes: stats.as_ref().map(|s| s.usage),
        statistics_error,
    })
}

/// Server reachability, version, and instance-wide asset counts.
#[orca_tool(domain = "immich", verb = "status", role = "read")]
async fn immich_status(args: EndpointArgs, _ctx: &ToolCtx) -> Result<StatusReport> {
    status_of(&connect(&args.name).await?).await
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq)]
pub struct AssetStatsReport {
    pub images: u64,
    pub videos: u64,
    pub total: u64,
    pub trashed_images: u64,
    pub trashed_videos: u64,
    pub trashed_total: u64,
}

/// Asset counts owned by the API key's user, active and trashed.
#[orca_tool(domain = "immich", verb = "assets.stats", role = "read")]
async fn immich_assets_stats(args: EndpointArgs, _ctx: &ToolCtx) -> Result<AssetStatsReport> {
    let c = connect(&args.name).await?;
    let active = c.asset_statistics(false).await?;
    let trashed = c.asset_statistics(true).await?;
    Ok(AssetStatsReport {
        images: active.images,
        videos: active.videos,
        total: active.total,
        trashed_images: trashed.images,
        trashed_videos: trashed.videos,
        trashed_total: trashed.total,
    })
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq)]
pub struct TrashedAsset {
    pub id: String,
    pub original_file_name: String,
    pub original_path: Option<String>,
    /// Base64 SHA1, as Immich reports it.
    pub checksum: String,
    /// Immich exposes no `deletedAt`; `updatedAt` is bumped when an asset is trashed.
    pub updated_at: Option<String>,
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq)]
pub struct TrashList {
    pub count: usize,
    pub assets: Vec<TrashedAsset>,
}

/// Trashed assets owned by the API key's user.
#[orca_tool(domain = "immich", verb = "trash.list", role = "read")]
async fn immich_trash_list(args: EndpointArgs, _ctx: &ToolCtx) -> Result<TrashList> {
    let assets: Vec<TrashedAsset> = connect(&args.name)
        .await?
        .trashed_assets()
        .await?
        .into_iter()
        .map(|a| TrashedAsset {
            id: a.id,
            original_file_name: a.original_file_name,
            original_path: a.original_path,
            checksum: a.checksum,
            updated_at: a.updated_at,
        })
        .collect();
    Ok(TrashList {
        count: assets.len(),
        assets,
    })
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq)]
pub struct CheckInput {
    /// Caller identifier (path or id) echoed back in the report.
    pub id: String,
    /// SHA1 of the file, hex or base64.
    pub sha1: String,
}

pub fn parse_check_input(s: &str) -> std::result::Result<CheckInput, String> {
    let t = s.trim();
    if t.starts_with('{') {
        return serde_json::from_str(t).map_err(|e| format!("item {t}: {e}"));
    }
    match t.rsplit_once('=') {
        Some((id, sha1)) if !id.is_empty() && !sha1.is_empty() => Ok(CheckInput {
            id: id.into(),
            sha1: sha1.into(),
        }),
        _ => Err(format!("item '{t}': expected <id>=<sha1> or a JSON object")),
    }
}

#[orca_struct(args)]
pub struct AssetsCheckArgs {
    /// Registered immich endpoint name.
    #[arg(long)]
    pub name: String,
    /// Precomputed `<id>=<sha1>` or `{"id":..,"sha1":..}`. Repeatable.
    #[arg(long = "item", value_parser = parse_check_input, action = clap::ArgAction::Append)]
    #[serde(default)]
    pub items: Vec<CheckInput>,
    /// Directory on this member to walk and hash (symlinks are not followed).
    #[arg(long)]
    pub path: Option<String>,
    /// Files hashed in parallel. Default 4.
    #[arg(long)]
    pub concurrency: Option<usize>,
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq)]
pub struct PresentAsset {
    pub id: String,
    pub sha1: String,
    pub asset_id: String,
    pub trashed: bool,
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq)]
pub struct CheckFailure {
    pub id: String,
    pub error: String,
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq)]
pub struct AssetsCheckReport {
    pub checked: usize,
    pub present_count: usize,
    pub trashed_count: usize,
    pub missing_count: usize,
    pub present: Vec<PresentAsset>,
    pub missing: Vec<CheckInput>,
    pub failures: Vec<CheckFailure>,
}

pub fn classify(inputs: &[CheckInput], results: Vec<CheckResult>) -> AssetsCheckReport {
    let by_id: std::collections::HashMap<String, CheckResult> =
        results.into_iter().map(|r| (r.id.clone(), r)).collect();
    let mut present = Vec::new();
    let mut missing = Vec::new();
    let mut failures = Vec::new();
    for input in inputs {
        match by_id.get(&input.id) {
            Some(r) if r.reason.as_deref() == Some("duplicate") => present.push(PresentAsset {
                id: input.id.clone(),
                sha1: input.sha1.clone(),
                asset_id: r.asset_id.clone().unwrap_or_default(),
                trashed: r.is_trashed.unwrap_or(false),
            }),
            Some(r) if r.action == "accept" => missing.push(input.clone()),
            Some(r) => failures.push(CheckFailure {
                id: input.id.clone(),
                error: format!("{} {}", r.action, r.reason.as_deref().unwrap_or("")),
            }),
            None => failures.push(CheckFailure {
                id: input.id.clone(),
                error: "no result returned".into(),
            }),
        }
    }
    AssetsCheckReport {
        checked: inputs.len(),
        present_count: present.len(),
        trashed_count: present.iter().filter(|p| p.trashed).count(),
        missing_count: missing.len(),
        present,
        missing,
        failures,
    }
}

pub async fn check(c: &Immich, inputs: &[CheckInput]) -> Result<AssetsCheckReport> {
    let mut results = Vec::with_capacity(inputs.len());
    for chunk in inputs.chunks(CHECK_BATCH) {
        let items: Vec<CheckItem> = chunk
            .iter()
            .map(|i| CheckItem {
                id: i.id.clone(),
                checksum: i.sha1.clone(),
            })
            .collect();
        results.extend(c.bulk_upload_check(&items).await?);
    }
    Ok(classify(inputs, results))
}

/// Which files are already in Immich, by SHA1, flagging those only present in trash.
#[orca_tool(domain = "immich", verb = "assets.check", role = "read")]
async fn immich_assets_check(args: AssetsCheckArgs, _ctx: &ToolCtx) -> Result<AssetsCheckReport> {
    let mut inputs = args.items;
    let mut hash_failures = Vec::new();
    if let Some(root) = args.path.as_deref() {
        let root = PathBuf::from(root);
        let files = scan::walk_media(&root).with_context(|| format!("walk {}", root.display()))?;
        let concurrency = args.concurrency.unwrap_or(DEFAULT_HASH_CONCURRENCY);
        for (path, hashed) in scan::hash_all(files, concurrency).await {
            let id = path.display().to_string();
            match hashed {
                Ok(sha1) => inputs.push(CheckInput { id, sha1 }),
                Err(e) => hash_failures.push(CheckFailure {
                    id,
                    error: e.to_string(),
                }),
            }
        }
    }
    if inputs.is_empty() && hash_failures.is_empty() {
        bail!("pass --item or --path");
    }
    let mut report = check(&connect(&args.name).await?, &inputs).await?;
    report.failures.extend(hash_failures);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(id: &str, action: &str, reason: Option<&str>, trashed: Option<bool>) -> CheckResult {
        CheckResult {
            id: id.into(),
            action: action.into(),
            reason: reason.map(Into::into),
            asset_id: reason.map(|_| format!("asset-{id}")),
            is_trashed: trashed,
        }
    }

    fn input(id: &str) -> CheckInput {
        CheckInput {
            id: id.into(),
            sha1: format!("sha-{id}"),
        }
    }

    #[test]
    fn classify_splits_present_trashed_missing() {
        let inputs = [input("a"), input("b"), input("c"), input("d")];
        let report = classify(
            &inputs,
            vec![
                result("a", "reject", Some("duplicate"), Some(false)),
                result("b", "reject", Some("duplicate"), Some(true)),
                result("c", "accept", None, None),
            ],
        );
        assert_eq!(report.checked, 4);
        assert_eq!(report.present_count, 2);
        assert_eq!(report.trashed_count, 1);
        assert_eq!(report.missing_count, 1);
        assert_eq!(report.present[1].asset_id, "asset-b");
        assert_eq!(report.missing[0].id, "c");
        assert_eq!(report.failures[0].id, "d");
    }

    #[test]
    fn parse_check_input_accepts_pair_and_json() {
        assert_eq!(parse_check_input("x/a=b.jpg=ff").unwrap().sha1, "ff");
        assert_eq!(
            parse_check_input(r#"{"id":"a","sha1":"ff"}"#).unwrap().id,
            "a"
        );
        assert!(parse_check_input("nope").is_err());
    }

    #[tokio::test]
    async fn check_batches_requests() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, Request, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/assets/bulk-upload-check"))
            .respond_with(|req: &Request| {
                let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
                let results: Vec<serde_json::Value> = body["assets"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|a| serde_json::json!({"id": a["id"], "action": "accept"}))
                    .collect();
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"results": results}))
            })
            .expect(2)
            .mount(&server)
            .await;
        let inputs: Vec<CheckInput> = (0..CHECK_BATCH + 1)
            .map(|i| input(&i.to_string()))
            .collect();
        let report = check(&Immich::new(&server.uri(), "k"), &inputs)
            .await
            .unwrap();
        assert_eq!(report.missing_count, CHECK_BATCH + 1);
    }
}

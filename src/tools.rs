use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use plugin_toolkit::prelude::*;

use crate::api::{CheckItem, CheckResult, Immich};
use crate::scan;

const CHECK_BATCH: usize = 1000;
const DEFAULT_HASH_CONCURRENCY: usize = 4;
const DEFAULT_MAX_FILES: usize = 100_000;
const MAX_SCAN_TIMEOUT_SECS: u64 = 3600;
const DEFAULT_TRASH_LIMIT: usize = 10_000;

#[endpoint_resource(plugin = "immich")]
pub struct ImmichEndpoint {
    pub name: String,
    /// Immich API key, sent as `x-api-key`.
    #[secret]
    pub api_key: String,
    /// Absolute directories `assets.scan` may read: a JSON array, or `:`-separated (paths containing `:` need the JSON form). Unset disables scanning.
    pub scan_roots: Option<String>,
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
    pub owner_id: Option<String>,
    /// Base64 SHA1, as Immich reports it.
    pub checksum: String,
    pub updated_at: Option<String>,
}

#[orca_struct]
#[derive(Debug, Clone, PartialEq)]
pub struct TrashList {
    pub count: usize,
    pub truncated: bool,
    pub assets: Vec<TrashedAsset>,
}

#[orca_struct(args)]
pub struct TrashListArgs {
    /// Registered immich endpoint name.
    #[arg(long)]
    pub name: String,
    /// Maximum assets returned. Default 10000.
    #[arg(long)]
    pub limit: Option<usize>,
}

/// Trashed assets visible to the API key's user (excludes locked-folder assets).
#[orca_tool(domain = "immich", verb = "trash.list", role = "read")]
async fn immich_trash_list(args: TrashListArgs, _ctx: &ToolCtx) -> Result<TrashList> {
    let limit = args.limit.unwrap_or(DEFAULT_TRASH_LIMIT);
    let (raw, truncated) = connect(&args.name).await?.trashed_assets(limit).await?;
    let assets: Vec<TrashedAsset> = raw
        .into_iter()
        .map(|a| TrashedAsset {
            id: a.id,
            original_file_name: a.original_file_name,
            original_path: a.original_path,
            owner_id: a.owner_id,
            checksum: a.checksum,
            updated_at: a.updated_at,
        })
        .collect();
    Ok(TrashList {
        count: assets.len(),
        truncated,
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
}

#[orca_struct(args)]
pub struct AssetsScanArgs {
    /// Registered immich endpoint name.
    #[arg(long)]
    pub name: String,
    /// Directory on this member under one of the endpoint's `scan_roots`.
    #[arg(long)]
    pub path: String,
    /// Files hashed in parallel, 1-16. Default 4.
    #[arg(long)]
    pub concurrency: Option<usize>,
    /// Maximum files walked, up to 1000000. Default 100000.
    #[arg(long)]
    pub max_files: Option<usize>,
    /// Overall time budget in seconds, up to 3600. Default 3600.
    #[arg(long)]
    pub timeout_secs: Option<u64>,
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
    pub truncated: bool,
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
        truncated: false,
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

pub fn reject_duplicate_ids(inputs: &[CheckInput]) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    for i in inputs {
        if !seen.insert(i.id.as_str()) {
            bail!("duplicate item id '{}'", i.id);
        }
    }
    Ok(())
}

pub fn ensure_enabled(name: &str, enabled: bool) -> Result<()> {
    if !enabled {
        bail!("immich endpoint '{name}' is disabled");
    }
    Ok(())
}

/// Which items are already in Immich, by SHA1, flagging those only present in trash.
#[orca_tool(domain = "immich", verb = "assets.check", role = "read")]
async fn immich_assets_check(args: AssetsCheckArgs, _ctx: &ToolCtx) -> Result<AssetsCheckReport> {
    if args.items.is_empty() {
        bail!("pass at least one --item");
    }
    reject_duplicate_ids(&args.items)?;
    check(&connect(&args.name).await?, &args.items).await
}

async fn run_scan(args: AssetsScanArgs, cancel: scan::Cancel) -> Result<AssetsCheckReport> {
    let row = endpoint_db::require(&args.name)?;
    ensure_enabled(&args.name, row.enabled)?;
    let roots =
        scan::parse_roots(row.scan_roots.as_deref().unwrap_or_default()).map_err(|e| anyhow!(e))?;
    let root = scan::resolve_scan_root(Path::new(&args.path), &roots).map_err(|e| anyhow!(e))?;
    let _lock = scan::ScanLock::acquire().ok_or_else(|| anyhow!("scan in progress"))?;
    let root_fd = Arc::new(scan::open_root(&root).map_err(|_| anyhow!(scan::OUTSIDE_ROOTS))?);
    let max_files = args
        .max_files
        .unwrap_or(DEFAULT_MAX_FILES)
        .min(scan::MAX_FILES_CAP);
    let (walk_fd, walk_cancel) = (root_fd.clone(), cancel.clone());
    let walk = tokio::task::spawn_blocking(move || {
        scan::walk_media(&walk_fd, max_files, scan::MAX_ENTRIES, &walk_cancel)
    })
    .await?;
    if walk.cancelled {
        bail!("scan cancelled");
    }
    let id_of = |rel: &Path| root.join(rel).display().to_string();
    let mut failures: Vec<CheckFailure> = walk
        .failures
        .iter()
        .map(|(p, error)| CheckFailure {
            id: id_of(p),
            error: error.clone(),
        })
        .collect();
    let concurrency = args.concurrency.unwrap_or(DEFAULT_HASH_CONCURRENCY);
    let mut inputs = Vec::new();
    for (rel, hashed) in scan::hash_all(root_fd, walk.files, concurrency, cancel.clone()).await {
        let id = id_of(&rel);
        match hashed {
            Ok(sha1) => inputs.push(CheckInput { id, sha1 }),
            Err(error) => failures.push(CheckFailure { id, error }),
        }
    }
    if cancel.is_cancelled() {
        bail!("scan cancelled");
    }
    let mut report = check(&connect(&args.name).await?, &inputs).await?;
    report.failures.extend(failures);
    report.truncated = walk.truncated;
    Ok(report)
}

/// Walk a member directory, hash media, and check which files are already in Immich.
#[orca_tool(domain = "immich", verb = "assets.scan", role = "admin")]
async fn immich_assets_scan(args: AssetsScanArgs, _ctx: &ToolCtx) -> Result<AssetsCheckReport> {
    let cancel = scan::Cancel::default();
    let _guard = scan::CancelOnDrop(cancel.clone());
    let secs = args
        .timeout_secs
        .unwrap_or(MAX_SCAN_TIMEOUT_SECS)
        .min(MAX_SCAN_TIMEOUT_SECS);
    tokio::time::timeout(Duration::from_secs(secs), run_scan(args, cancel))
        .await
        .map_err(|_| anyhow!("immich.assets.scan exceeded {secs}s"))?
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
    fn disabled_endpoint_rejected() {
        assert!(ensure_enabled("x", true).is_ok());
        assert!(ensure_enabled("x", false).is_err());
    }

    #[test]
    fn duplicate_ids_rejected() {
        assert!(reject_duplicate_ids(&[input("a"), input("b")]).is_ok());
        assert!(reject_duplicate_ids(&[input("a"), input("a")]).is_err());
    }

    #[tokio::test]
    async fn status_survives_failing_statistics() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/server/ping"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"res": "pong"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/server/version"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"major": 3, "minor": 3, "patch": 5, "prerelease": null}),
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/server/statistics"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let r = status_of(&Immich::new(&server.uri(), "k")).await.unwrap();
        assert!(r.reachable);
        assert_eq!(r.version, "3.3.5");
        assert_eq!(r.photos, None);
        assert!(r.statistics_error.is_some());
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

//! immich service backend — Self-hosted photo/video (multi-service).
//!
//! Implements `ServiceBackend` so the generic `service.*` tools
//! (deploy/backup/restore/configure/status/connect/sync) drive immich. No
//! `#[orca_tool]`s — the only orca dep is `plugin-toolkit`. Modeled on the
//! nfs StorageBackend. See orca/docs/PLUGIN-PROGRAM.md.
#![allow(clippy::disallowed_types)]

use plugin_toolkit::client::Client;
use plugin_toolkit::contract::health::Health;
use plugin_toolkit::service::{
    BoxFuture, Endpoint, Runtime, ServiceBackend, ServiceCapability, ServiceError, ServiceInfo,
    ServiceStatus, StackContainer, WorkloadSpec,
};

/// immich backend. Holds only the provider name; per-instance endpoint/creds
/// come from the `Endpoint` the generic `service.*` tools hand each op.
#[derive(Debug, Clone)]
pub struct ImmichBackend {
    provider: &'static str,
}

impl ImmichBackend {
    pub fn new(provider: &'static str) -> Self {
        Self { provider }
    }
}

impl ServiceBackend for ImmichBackend {
    fn provider(&self) -> &str {
        self.provider
    }

    /// Runtimes immich can be placed on. `service.deploy` hands the
    /// `workload_spec` below to a matching deploy target — this backend never
    /// drives pct/docker itself (that mechanic lives in the deploy-target domain).
    fn runtimes(&self) -> Vec<Runtime> {
        vec![Runtime::Docker, Runtime::Podman, Runtime::Lxc, Runtime::Vm]
    }

    fn capabilities(&self) -> Vec<ServiceCapability> {
        vec![
            ServiceCapability::Deploy,
            ServiceCapability::Backup,
            ServiceCapability::Restore,
            ServiceCapability::Configure,
            ServiceCapability::Status,
        ]
    }

    fn default_port(&self) -> u16 {
        2283
    }

    /// In-workload paths holding config/data. This is ALL immich declares for
    /// backup — the generic pluggable backup (tar for containers/LXC, PBS for
    /// Proxmox guests when available) snapshots these. No backup/restore code
    /// here; those are inherited from ServiceBackend's defaults.
    fn data_paths(&self) -> Vec<String> {
        vec!["/config".to_string()]
    }

    fn workload_spec<'a>(
        &'a self,
        _runtime: Runtime,
        _ep: &'a Endpoint,
    ) -> BoxFuture<'a, Result<WorkloadSpec, ServiceError>> {
        // TODO: describe the immich workload (image/template, ports, mounts,
        // env) for the chosen runtime. The deploy target turns this into a
        // compose service / LXC config / VM. See deploy-target::WorkloadSpec.
        Box::pin(async move { Err(ServiceError::unimplemented("immich.workload_spec")) })
    }

    fn configure<'a>(
        &'a self,
        _ep: &'a Endpoint,
        _config: &'a str,
    ) -> BoxFuture<'a, Result<(), ServiceError>> {
        // TODO: apply immich-specific config idempotently.
        Box::pin(async move { Err(ServiceError::unimplemented("immich.configure")) })
    }

    /// Health via immich's own HTTP API, surfaced as a typed
    /// [`ServiceInfo::ContainerStack`].
    ///
    /// The server runs a startup folder-integrity check that writes a `.immich`
    /// marker into each upload subdirectory; if the upload volume is unwritable
    /// that write fails with `EACCES` and the worker exits — the server
    /// crash-loops and never answers `/api/server/ping`. So a passing ping is a
    /// positive proxy that the upload mount is writable, and a server that is
    /// unreachable-but-its-host-is-up is the signature of exactly that
    /// unwritable-upload failure — which the `detail` calls out.
    ///
    /// Scope: this reads what the HTTP API truthfully exposes — the server
    /// container's health and version. Per-container health for the postgres /
    /// redis / machine-learning containers and a direct upload write-probe are
    /// not reachable over the service `Endpoint` (they need a runtime/exec seam
    /// the `ServiceBackend` does not own yet); they stay `Unknown` / empty until
    /// that seam lands. The typed `ContainerStack` shape already has room for
    /// them.
    fn status<'a>(
        &'a self,
        ep: &'a Endpoint,
    ) -> BoxFuture<'a, Result<ServiceStatus, ServiceError>> {
        Box::pin(async move {
            let base = ep.base_url.trim_end_matches('/');
            if base.is_empty() {
                return Err(ServiceError::Other(
                    "immich.status: endpoint has no base_url".to_string(),
                ));
            }
            let client = Client::new();

            // Liveness — /api/server/ping returns {"res":"pong"} (no auth).
            let pong = client
                .get(format!("{base}/api/server/ping"))
                .map(|r| r.is_success() && r.text().contains("pong"))
                .unwrap_or(false);

            // Best-effort version — /api/server/version → {major,minor,patch}.
            let version = client
                .get(format!("{base}/api/server/version"))
                .ok()
                .filter(|r| r.is_success())
                .and_then(|r| r.json::<plugin_toolkit::serde_json::Value>().ok())
                .and_then(|v| {
                    Some(format!(
                        "v{}.{}.{}",
                        v.get("major")?.as_u64()?,
                        v.get("minor")?.as_u64()?,
                        v.get("patch")?.as_u64()?,
                    ))
                });

            let server_health = if pong {
                Health::Healthy
            } else {
                Health::Unhealthy
            };

            let detail = if pong {
                match &version {
                    Some(v) => format!("immich-server healthy ({v})"),
                    None => "immich-server healthy".to_string(),
                }
            } else {
                "immich-server not responding on /api/server/ping — if the host \
                 is up, the server is likely crash-looping; the most common cause \
                 is the upload volume being unwritable (EACCES on the .immich \
                 folder check)"
                    .to_string()
            };

            Ok(ServiceStatus {
                healthy: pong,
                detail,
                info: ServiceInfo::ContainerStack {
                    containers: vec![StackContainer {
                        name: "immich-server".to_string(),
                        state: if pong { "running" } else { "unknown" }.to_string(),
                        health: server_health,
                        restart_count: 0,
                    }],
                    mounts: Vec::new(),
                    version,
                },
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declares_provider() {
        let b = ImmichBackend::new("immich");
        assert_eq!(b.provider(), "immich");
    }
}

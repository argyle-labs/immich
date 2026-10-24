//! immich service backend — Self-hosted photo/video (multi-service).
//!
//! Implements `ServiceBackend` so the generic `service.*` tools drive immich,
//! and exposes read tools over the Immich API (`immich.status`,
//! `immich.assets.check`, `immich.assets.stats`, `immich.trash.list`) and the admin
//! `immich.assets.scan`.
#![allow(clippy::disallowed_types)]

pub mod api;
pub mod scan;
pub mod tools;

#[cfg(test)]
mod testing;

use plugin_toolkit::service::{
    BoxFuture, Routes, Runtime, ServiceBackend, ServiceCapability, ServiceError, ServiceStatus,
    WorkloadSpec,
};

/// immich backend. Holds only the provider name; per-instance routes/creds
/// come from the `Routes` the generic `service.*` tools hand each op.
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
        _instance: &'a str,
        _routes: &'a Routes,
    ) -> BoxFuture<'a, Result<WorkloadSpec, ServiceError>> {
        // TODO: describe the immich workload (image/template, ports, mounts,
        // env) for the chosen runtime. The deploy target turns this into a
        // compose service / LXC config / VM. See deploy-target::WorkloadSpec.
        Box::pin(async move { Err(ServiceError::unimplemented("immich.workload_spec")) })
    }

    fn configure<'a>(
        &'a self,
        _instance: &'a str,
        _routes: &'a Routes,
        _config: &'a str,
    ) -> BoxFuture<'a, Result<(), ServiceError>> {
        // TODO: apply immich-specific config idempotently.
        Box::pin(async move { Err(ServiceError::unimplemented("immich.configure")) })
    }

    fn status<'a>(
        &'a self,
        instance: &'a str,
        _routes: &'a Routes,
    ) -> BoxFuture<'a, Result<ServiceStatus, ServiceError>> {
        Box::pin(async move {
            let report = match tools::connect(instance).await {
                Ok(c) => tools::status_of(&c).await,
                Err(e) => Err(e),
            };
            Ok(match report {
                Ok(r) => ServiceStatus {
                    healthy: r.reachable,
                    detail: format!("immich {}", r.version),
                    info: Default::default(),
                },
                Err(e) => ServiceStatus {
                    healthy: false,
                    detail: e.to_string(),
                    info: Default::default(),
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

# immich — ServiceBackend contract

Pure-Rust plugin (**no bash/compose/provision scripts**) driven by the single
generic `service.*` surface, plus read tools over the Immich API
(`immich.status`, `immich.assets.check`, `immich.assets.stats`, `immich.trash.list`) and the admin `immich.assets.scan`. Runtimes: **docker,podman,lxc,vm**.

## Per-plugin code (the only work this repo owns)
- [x] `provider` / `runtimes` / `default_port` / `capabilities` / `data_paths` — declarative descriptor
- [ ] `workload_spec(runtime)` — *what* to run; `deploy_target` renders it to a container / LXC / VM
- [ ] `configure` — apply immich config via its upstream API
- [x] `status` — ping + version via the registered `immich` endpoint (rich `ServiceStatus.info` pending)

> Declarative descriptor is implemented and the plugin **registers + loads live**
> in orca today (`service.list` shows it). `workload_spec`/`configure`/`status`
> are being filled in per plugin.

## Provided generically by orca (NO code here)
- `deploy` — `service.deploy` → `deploy_target.launch(WorkloadSpec)`
- `backup` / `restore` — pluggable `BackupMethod` (tar; **PBS** for Proxmox guests)
- single `service.*` tool surface, exposed over CLI / REST / MCP

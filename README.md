<p align="center">
  <img src="assets/icon-256.png" width="120" alt="immich" />
</p>

# immich

Immich is a high-performance, self-hosted photo and video backup solution (multi-service).

A first-party [orca](https://github.com/argyle-labs/orca) plugin (service-backend).

immich is multi-service — deploy it **by hand, without orca** from the upstream compose:

---

## Run it without orca

Follow the upstream install (which provides the official multi-container `docker-compose`): <https://immich.app/>.


See [immich.md](docs/immich.md) for worked operator notes.


### Backup & restore

Back up the config/data volume(s) above — that's the whole service state (stop the container first for a clean copy). Restore by putting them back and starting it.

> With orca this is **`service.backup` / `service.restore`** — location-agnostic (docker / podman / lxc / vm), one command regardless of where immich runs. No per-service backup script.

## With orca

orca drives this plugin through the single generic `service.*` surface — no per-plugin tools:

```sh
orca service.deploy immich      # render + launch on any supported runtime
orca service.status immich      # health + rich diagnostics (typed payload)
orca service.backup immich      # location-agnostic backup (tar; PBS on Proxmox)
orca service.configure immich   # apply config via the upstream API
```

## Layout

- `src/` — the plugin (pure Rust): the `ServiceBackend` descriptor + `configure` / `status`.
- `docs/` — standalone operator notes.
- [CAPABILITIES.md](CAPABILITIES.md) — the service-backend contract checklist.
- `assets/` — plugin icon.

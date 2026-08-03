# Immich — operator notes

[Immich](https://immich.app/) is a high-performance, self-hosted photo and
video backup server — a Google Photos alternative. It is **multi-service**:
the API/web server, a PostgreSQL database (with the pgvecto.rs vector
extension), Redis, and a machine-learning container for face and object
recognition and smart search.

- **Web / API port**: `2283`
- **Runtime**: the upstream multi-container compose, runnable under docker or
  podman, deployable to an LXC, VM, or Unraid.
- **Dependencies**: PostgreSQL (pgvecto.rs) + Redis — both provisioned by the
  upstream compose.

These are generic operator notes for this plugin. For install and configuration
reference, follow the upstream docs at <https://immich.app/>.

---

## Service state

Immich's persistent state is a small number of volumes. Everything else
(images, containers) is reproducible from the compose file.

| Data | What it is | Notes |
|------|------------|-------|
| Upload library | The original photos and videos plus generated thumbnails | The irreplaceable data — the bulk of the volume size. |
| PostgreSQL database | Albums, users, faces, smart-search vectors, metadata | Critical: without it Immich cannot organize or read the library. |
| ML model cache | Downloaded machine-learning models | Not worth backing up — re-downloads automatically. |

Keep the upload library and the database together as a consistent set: a
database that references files no longer present (or vice versa) will need a
library re-scan to reconcile.

---

## Deploy

Deploy the upstream compose on any supported runtime (docker or podman; bare, in
an LXC, a VM, or on Unraid). Set the required environment (timezone, image tag,
the database password, and the upload location) per the upstream reference, then
start the stack.

First-run setup, in the web UI at `http://<server>:2283`:

1. Create the admin account.
2. Confirm the upload location.
3. Enable machine learning for face recognition and smart search.

> The database password must match between the server and postgres containers.
> PostgreSQL initializes with whatever password it sees on first start — if you
> change it later you must reinitialize the postgres volume.

---

## Backup & restore

Immich's whole state is the volumes above. Back them up as a consistent set —
stop the container first for a clean copy — and restore by putting them back and
starting the service. A logical database dump (`pg_dumpall`) is a more portable
and reliable capture of the database than a raw copy of the postgres data
directory.

> With orca this is **`service.backup` / `service.restore`** — location-agnostic
> (docker / podman / lxc / vm), one command regardless of where Immich runs.
> There is no per-service backup script.

```sh
orca service.backup immich    # location-agnostic backup
orca service.restore immich   # restore that backup
```

After a database restore, trigger a library re-scan from the UI
(**Administration → Jobs → Library → Scan All Libraries**) so Immich reconciles
the restored database against the files on disk.

---

## Resource notes

- The machine-learning container is the memory-hungry component; plan RAM for
  the full stack accordingly.
- ML processing is CPU-bound — no GPU is required.
- Bulk imports are limited by the write throughput of the upload volume's
  storage.

---

## Troubleshooting

```sh
# Are all the containers up?
docker compose ps

# Follow logs for a specific service
docker compose logs -f immich-server
docker compose logs -f immich-postgres
docker compose logs -f immich-machine-learning
```

- **Database won't start** — check ownership of the postgres data directory
  against the postgres container's user; a mismatched owner is the usual cause.
- **Photos missing after a restore** — run a library re-scan (see above) so the
  database and the files on disk are reconciled.

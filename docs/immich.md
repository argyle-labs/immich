# Immich

Self-hosted photo and video backup — Google Photos alternative. Includes PostgreSQL, Redis, and a machine learning container for face/object recognition.

- **Port**: 2283
- **Planned host**: <host> (Docker VM on <host>, <ip>)
- **Type**: Docker multi-container stack (server + postgres + redis + machine-learning)
- **Tier**: 1 — Core Data (photos are irreplaceable)

See [lxc-ha-plan.md](../infrastructure/lxc-ha-plan.md) for placement rules and failover.

---

## Data Architecture

Photos (the actual files) must live on <host> NFS — not inside the Docker container or on <host>'s local disk. This ensures photos survive any single host failure.

| Data | Location | Backed up by |
|------|----------|-------------|
| Photos / videos | `<host>:/mnt/user/data/photos` → `/mnt/<host>/data/photos` | Syncthing (<host> → <host>) |
| PostgreSQL database | Docker named volume `immich_immich_postgres_data` (local to <host>) | Nightly pg_dump to <host> backups |
| ML model cache | Docker named volume `immich_immich_model_cache` (local to <host>) | Not backed up — re-downloads automatically |

> The Postgres DB is critical. Without it Immich cannot read or organize photos. Back it up separately — a DB dump is more reliable than a filesystem backup of the postgres data directory.

---

## Setup (on <host>)

Prerequisites: <host> VM must be running with Docker installed and NFS mounted. See [<host>-setup.md](../infrastructure/<host>-setup.md).

### Step 1 — Create photos directory on NFS

```bash
# Run from any host with <host> mounted (e.g. <host>)
mkdir -p /mnt/<host>/data/photos
```

> Postgres and ML model cache use Docker named volumes (`immich_immich_postgres_data`, `immich_immich_model_cache`) — Docker creates these automatically on first deploy. No local appdata dirs needed for immich.

### Step 2 — Set NFS permissions on <host>

```bash
# On <host>
chown nobody:users /mnt/user/data/photos
chmod 775 /mnt/user/data/photos
```

### Step 3 — Deploy via Portainer

In Portainer on <host> → **Stacks → Add Stack → Git Repository**:

- Repo: `<github-org>/<repo>`, branch: `main`
- Compose file: `compose/immich/docker-compose.yml`
- Credentials: `github`
- Auto-update: 5 minutes

Set environment variables in Portainer:

```
TZ=Etc/UTC
IMMICH_IMAGE_TAG=release
IMMICH_DB_PASSWORD=<strong-password — set in Portainer, never commit to repo>
IMMICH_UPLOAD_PATH=/mnt/<host>/data/photos
```

> `IMMICH_UPLOAD_PATH` points directly at the photos NFS path — maps to `/usr/src/app/upload` inside the container. Photos are kept separate from the media library (`/mnt/<host>/data/media`).
> `IMMICH_DB_PASSWORD` must match across both the server and postgres containers. If you change it after first deploy, you must wipe the postgres volume and redeploy (postgres initializes with whatever password it sees on first start).

### Step 4 — Initial Immich setup

Access: **http://<ip>:2283**

1. Create admin account
2. Confirm upload path is set to `/usr/src/app/upload` (which maps to the NFS photos path via compose volume)
3. Enable machine learning for face recognition and smart search (uses the ML container automatically)

---

## Backup

### Nightly backup script

Run on <host>. Backs up the Postgres DB (via `pg_dump`) and Immich config to <host>.

```bash
cat > /usr/local/bin/backup-immich.sh << 'EOF'
#!/bin/sh
set -e

DEST=/mnt/<host>/backups/appdata_<host>/immich
DATE=$(date +%Y%m%d_%H%M%S)
mkdir -p "$DEST"

# PostgreSQL dump (more reliable than backing up the named volume directly)
docker exec immich-immich-postgres-1 pg_dumpall -U postgres | gzip > "$DEST/immich_db_${DATE}.sql.gz"

# Keep 14 most recent DB dumps
ls -dt "$DEST"/immich_db_*.sql.gz | tail -n +15 | xargs -r rm -f

echo "Backup complete: $DATE"
EOF
chmod +x /usr/local/bin/backup-immich.sh

# Alpine uses crontab, not cron.d
(crontab -l 2>/dev/null; echo "0 2 * * * /usr/local/bin/backup-immich.sh >> /var/log/backup-immich.log 2>&1") | crontab -
```

---

## Failover Procedure

If <host> goes down:

1. Photos are on <host> NFS — safe. The data is not lost.
2. Stand up a new <host> VM on <host> (or temporarily on <host>)
3. Install Docker, mount same NFS paths — see [<host>-setup.md](../infrastructure/<host>-setup.md)
4. Restore Postgres DB:
   ```bash
   # Start just the postgres container first
   docker compose -p immich up -d immich-postgres
   # Restore from latest dump
   gunzip -c /mnt/<host>/backups/appdata_<host>/immich/immich_db_<latest>.sql.gz \
     | docker exec -i immich-immich-postgres-1 psql -U postgres
   ```
5. Deploy Immich stack — it reconnects to the restored database
6. Photos are on NFS — no restore needed for the actual files

---

## Resource Notes

- **Machine learning container** needs ~4 GB RAM for face recognition — plan <host>'s RAM accordingly (8 GB minimum for the full stack)
- ML processing is CPU-bound; no GPU required
- Photo upload speed is limited by NFS write throughput — ensure <host> is healthy before bulk imports

---

## Troubleshooting

```bash
# Check all containers are running
docker compose ps

# Follow logs
docker compose logs -f immich-server
docker compose logs -f immich-postgres
docker compose logs -f immich-machine-learning
```

### Database won't start

Check postgres data directory permissions:
```bash
ls -la /opt/appdata/immich/postgres/
# Should be owned by 999:999 (postgres container user)
chown -R 999:999 /opt/appdata/immich/postgres
```

### Photos not showing after restore

Trigger a re-scan after DB restore:
In Immich UI → **Administration → Jobs → Library → Scan All Libraries**

---

## Related Documentation

- [<host>-setup.md](../infrastructure/<host>-setup.md) — Docker VM setup (Immich runs here)
- [lxc-ha-plan.md](../infrastructure/lxc-ha-plan.md) — tier 1 data resilience requirements
- [<host>-shares.md](../infrastructure/<host>-shares.md) — NFS share layout, Syncthing replication

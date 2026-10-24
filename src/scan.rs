use std::collections::HashMap;
use std::io::Read;
use std::os::fd::OwnedFd;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags};
use sha1::{Digest, Sha1};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

const MEDIA_EXTENSIONS: &[&str] = &[
    "3fr", "3gp", "3gpp", "ari", "arw", "avi", "avif", "bmp", "cap", "cin", "cr2", "cr3", "crw",
    "dcr", "dng", "erf", "fff", "flv", "gif", "heic", "heif", "hif", "iiq", "insp", "insv", "jfif",
    "jp2", "jpe", "jpeg", "jpg", "jxl", "k25", "kdc", "m2t", "m2ts", "m4v", "mkv", "mov", "mp4",
    "mpe", "mpeg", "mpg", "mpo", "mrw", "mts", "mxf", "nef", "nrw", "orf", "ori", "pef", "png",
    "psd", "raf", "raw", "rw2", "rwl", "sr2", "srf", "srw", "svg", "tif", "tiff", "ts", "vob",
    "webm", "webp", "wmv", "x3f",
];

pub const MAX_CONCURRENCY: usize = 16;
pub const MAX_FILES_CAP: usize = 1_000_000;
pub const MAX_ENTRIES: usize = 5_000_000;
pub const OUTSIDE_ROOTS: &str = "path is outside the allowed scan roots";
const READ_CHUNK: usize = 1 << 16;

static SCAN_ACTIVE: AtomicBool = AtomicBool::new(false);

pub struct ScanLock(());

impl ScanLock {
    pub fn acquire() -> Option<Self> {
        SCAN_ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| ScanLock(()))
    }
}

impl Drop for ScanLock {
    fn drop(&mut self) {
        SCAN_ACTIVE.store(false, Ordering::Release);
    }
}

#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

pub struct CancelOnDrop(pub Cancel);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

pub fn clamp_concurrency(n: usize) -> usize {
    n.clamp(1, MAX_CONCURRENCY)
}

pub fn is_media(path: &Path) -> bool {
    let hidden = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_none_or(|n| n.starts_with('.'));
    !hidden
        && path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| MEDIA_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

pub fn parse_roots(roots: &str) -> Result<Vec<PathBuf>, String> {
    let t = roots.trim();
    let raw: Vec<String> = if t.starts_with('[') {
        serde_json::from_str(t)
            .map_err(|_| "scan_roots is not a JSON array of strings".to_string())?
    } else {
        t.split(':')
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .map(String::from)
            .collect()
    };
    raw.into_iter()
        .map(|r| {
            let p = PathBuf::from(&r);
            if !p.is_absolute() {
                return Err(format!("scan root '{r}' is not absolute"));
            }
            p.canonicalize()
                .map_err(|_| format!("scan root '{r}' cannot be resolved"))
        })
        .collect()
}

fn lexically_under(path: &Path, roots: &[PathBuf]) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        && roots.iter().any(|r| path.starts_with(r))
}

pub fn resolve_scan_root(path: &Path, roots: &[PathBuf]) -> Result<PathBuf, String> {
    if roots.is_empty() {
        return Err("endpoint has no scan_roots; scanning is disabled".into());
    }
    if !lexically_under(path, roots) {
        return Err(OUTSIDE_ROOTS.into());
    }
    let canon = path.canonicalize().map_err(|_| OUTSIDE_ROOTS.to_string())?;
    if roots.iter().any(|r| canon.starts_with(r)) {
        Ok(canon)
    } else {
        Err(OUTSIDE_ROOTS.into())
    }
}

pub fn open_root(root: &Path) -> std::io::Result<OwnedFd> {
    Ok(rustix::fs::open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?)
}

fn open_dir_at(dir: &OwnedFd, name: &Path) -> std::io::Result<OwnedFd> {
    Ok(rustix::fs::openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?)
}

pub fn open_dir_beneath(root: &OwnedFd, rel: &Path) -> std::io::Result<OwnedFd> {
    let mut fd = rustix::io::dup(root)?;
    for c in rel.components() {
        let Component::Normal(name) = c else {
            return Err(std::io::Error::other("invalid path component"));
        };
        fd = open_dir_at(&fd, Path::new(name))?;
    }
    Ok(fd)
}

pub fn open_file_beneath(root: &OwnedFd, rel: &Path) -> std::io::Result<std::fs::File> {
    let name = rel
        .file_name()
        .ok_or_else(|| std::io::Error::other("no file name"))?;
    let parent = open_dir_beneath(root, rel.parent().unwrap_or(Path::new("")))?;
    let fd = rustix::fs::openat(
        &parent,
        Path::new(name),
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )?;
    let st = rustix::fs::fstat(&fd)?;
    if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile {
        return Err(std::io::Error::other("not a regular file"));
    }
    Ok(std::fs::File::from(fd))
}

#[derive(Debug, Default)]
pub struct Walk {
    pub files: Vec<PathBuf>,
    pub failures: Vec<(PathBuf, String)>,
    pub truncated: bool,
    pub cancelled: bool,
}

pub fn walk_media(root: &OwnedFd, max_files: usize, max_entries: usize, cancel: &Cancel) -> Walk {
    let mut walk = Walk::default();
    let mut entries_seen = 0usize;
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        if cancel.is_cancelled() {
            walk.cancelled = true;
            break;
        }
        let opened = open_dir_beneath(root, &rel)
            .and_then(|fd| Ok::<_, std::io::Error>((Dir::read_from(&fd)?, fd)));
        let (dir, parent) = match opened {
            Ok(pair) => pair,
            Err(e) => {
                walk.failures.push((rel, e.kind().to_string()));
                continue;
            }
        };
        for entry in dir {
            if cancel.is_cancelled() {
                walk.cancelled = true;
                break;
            }
            entries_seen += 1;
            if entries_seen > max_entries {
                walk.truncated = true;
                break;
            }
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    walk.failures
                        .push((rel.clone(), std::io::Error::from(e).kind().to_string()));
                    continue;
                }
            };
            let Ok(name) = entry.file_name().to_str() else {
                continue;
            };
            if name == "." || name == ".." {
                continue;
            }
            let path = rel.join(name);
            let mut ft = entry.file_type();
            if ft == FileType::Unknown {
                ft = match rustix::fs::statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW) {
                    Ok(st) => FileType::from_raw_mode(st.st_mode),
                    Err(_) => continue,
                };
            }
            if ft == FileType::Directory {
                stack.push(path);
            } else if ft == FileType::RegularFile && is_media(&path) {
                if walk.files.len() >= max_files {
                    walk.truncated = true;
                    break;
                }
                walk.files.push(path);
            }
        }
        if walk.truncated || walk.cancelled {
            break;
        }
    }
    walk.files.sort();
    walk
}

pub fn sha1_reader(mut r: impl Read, cancel: &Cancel) -> std::io::Result<String> {
    let mut hasher = Sha1::new();
    let mut buf = vec![0u8; READ_CHUNK];
    loop {
        if cancel.is_cancelled() {
            return Err(std::io::Error::other("cancelled"));
        }
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub async fn hash_all(
    root: Arc<OwnedFd>,
    paths: Vec<PathBuf>,
    concurrency: usize,
    cancel: Cancel,
) -> Vec<(PathBuf, Result<String, String>)> {
    let permits = Arc::new(Semaphore::new(clamp_concurrency(concurrency)));
    let mut set = JoinSet::new();
    let mut by_task = HashMap::new();
    for path in paths {
        if cancel.is_cancelled() {
            break;
        }
        let Ok(permit) = permits.clone().acquire_owned().await else {
            break;
        };
        let (task_path, root, cancel) = (path.clone(), root.clone(), cancel.clone());
        let handle = set.spawn_blocking(move || {
            let r = open_file_beneath(&root, &task_path)
                .and_then(|f| sha1_reader(f, &cancel))
                .map_err(|e| e.kind().to_string());
            drop(permit);
            (task_path, r)
        });
        by_task.insert(handle.id(), path);
    }
    let mut out = Vec::new();
    while let Some(joined) = set.join_next_with_id().await {
        match joined {
            Ok((_, pair)) => out.push(pair),
            Err(e) => {
                let path = by_task.remove(&e.id()).unwrap_or_default();
                out.push((path, Err("hash task failed".into())));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root_fd(p: &Path) -> OwnedFd {
        open_root(p).unwrap()
    }

    #[test]
    fn media_filter_skips_hidden_and_non_media() {
        assert!(is_media(Path::new("/x/IMG_1.HEIC")));
        assert!(is_media(Path::new("/x/clip.mov")));
        assert!(!is_media(Path::new("/x/.osxphotos_export.db")));
        assert!(!is_media(Path::new("/x/._IMG_1.jpg")));
        assert!(!is_media(Path::new("/x/IMG_1.xmp")));
        assert!(!is_media(Path::new("/x/notes.txt")));
    }

    #[test]
    fn concurrency_is_clamped() {
        assert_eq!(clamp_concurrency(0), 1);
        assert_eq!(clamp_concurrency(4), 4);
        assert_eq!(clamp_concurrency(1000), MAX_CONCURRENCY);
    }

    #[test]
    fn scan_lock_is_exclusive() {
        let first = ScanLock::acquire().unwrap();
        assert!(ScanLock::acquire().is_none());
        drop(first);
        assert!(ScanLock::acquire().is_some());
    }

    #[test]
    fn roots_parse_and_validate() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let (ca, cb) = (
            a.path().canonicalize().unwrap(),
            b.path().canonicalize().unwrap(),
        );
        let colon = format!("{}: {} ::", a.path().display(), b.path().display());
        assert_eq!(parse_roots(&colon).unwrap(), vec![ca.clone(), cb.clone()]);
        let json = serde_json::to_string(&[a.path(), b.path()]).unwrap();
        assert_eq!(parse_roots(&json).unwrap(), vec![ca, cb]);
        assert!(parse_roots("relative/dir")
            .unwrap_err()
            .contains("not absolute"));
        assert!(parse_roots("/does/not/exist/xyz")
            .unwrap_err()
            .contains("cannot be resolved"));
        assert!(parse_roots("[1]").is_err());
        assert!(parse_roots("").unwrap().is_empty());
    }

    #[test]
    fn outside_roots_errors_are_uniform() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let croot = root.path().canonicalize().unwrap();
        std::fs::create_dir(croot.join("sub")).unwrap();
        let roots = vec![croot.clone()];
        assert_eq!(
            resolve_scan_root(&croot.join("sub"), &roots).unwrap(),
            croot.join("sub")
        );
        for bad in [
            croot.join("sub/../.."),
            croot.join("missing"),
            other.path().canonicalize().unwrap(),
            PathBuf::from("relative"),
        ] {
            assert_eq!(resolve_scan_root(&bad, &roots).unwrap_err(), OUTSIDE_ROOTS);
        }
        assert!(resolve_scan_root(&croot, &[]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn scan_root_rejects_symlink_escape() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let croot = root.path().canonicalize().unwrap();
        std::os::unix::fs::symlink(outside.path(), croot.join("out")).unwrap();
        assert_eq!(
            resolve_scan_root(&croot.join("out"), std::slice::from_ref(&croot)).unwrap_err(),
            OUTSIDE_ROOTS
        );
    }

    #[cfg(unix)]
    #[test]
    fn walk_skips_symlinks() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("leak.jpg"), b"x").unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("sub")).unwrap();
        std::fs::write(root.path().join("sub/a.jpg"), b"a").unwrap();
        std::fs::write(root.path().join(".osxphotos_export.db"), b"db").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("out")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("leak.jpg"),
            root.path().join("link.jpg"),
        )
        .unwrap();
        let walk = walk_media(&root_fd(root.path()), 100, MAX_ENTRIES, &Cancel::default());
        assert_eq!(walk.files, vec![PathBuf::from("sub/a.jpg")]);
        assert!(!walk.truncated);
    }

    #[cfg(unix)]
    #[test]
    fn open_beneath_refuses_symlinks_and_non_regular() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("leak.jpg"), b"x").unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("d.jpg")).unwrap();
        std::fs::write(root.path().join("ok.jpg"), b"ok").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("leak.jpg"),
            root.path().join("swap.jpg"),
        )
        .unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("dirlink")).unwrap();
        let fd = root_fd(root.path());
        assert!(open_file_beneath(&fd, Path::new("ok.jpg")).is_ok());
        assert!(open_file_beneath(&fd, Path::new("swap.jpg")).is_err());
        assert!(open_file_beneath(&fd, Path::new("dirlink/leak.jpg")).is_err());
        assert!(open_file_beneath(&fd, Path::new("d.jpg")).is_err());
        assert!(open_file_beneath(&fd, Path::new("../x.jpg")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn walk_records_unreadable_dir_and_continues() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let locked = root.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::write(locked.join("hidden.jpg"), b"h").unwrap();
        std::fs::write(root.path().join("a.jpg"), b"a").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let readable = std::fs::read_dir(&locked).is_ok();
        let walk = walk_media(&root_fd(root.path()), 100, MAX_ENTRIES, &Cancel::default());
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(walk.files.contains(&PathBuf::from("a.jpg")));
        if !readable {
            assert_eq!(walk.failures.len(), 1);
            assert_eq!(walk.failures[0].0, PathBuf::from("locked"));
        }
    }

    #[test]
    fn walk_stops_at_max_files_and_entries() {
        let root = tempfile::tempdir().unwrap();
        for i in 0..5 {
            std::fs::write(root.path().join(format!("{i}.jpg")), b"x").unwrap();
        }
        let fd = root_fd(root.path());
        let walk = walk_media(&fd, 3, MAX_ENTRIES, &Cancel::default());
        assert_eq!(walk.files.len(), 3);
        assert!(walk.truncated);
        let all = walk_media(&fd, 5, MAX_ENTRIES, &Cancel::default());
        assert_eq!(all.files.len(), 5);
        assert!(!all.truncated);
        let capped = walk_media(&fd, 100, 2, &Cancel::default());
        assert!(capped.truncated);
        assert!(capped.files.len() <= 2);
    }

    #[test]
    fn walk_and_hash_honor_cancel() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.jpg"), b"a").unwrap();
        let cancel = Cancel::default();
        {
            let _guard = CancelOnDrop(cancel.clone());
        }
        assert!(cancel.is_cancelled());
        let walk = walk_media(&root_fd(root.path()), 100, MAX_ENTRIES, &cancel);
        assert!(walk.cancelled);
        assert!(walk.files.is_empty());
        assert!(sha1_reader(&b"abc"[..], &cancel).is_err());
    }

    #[tokio::test]
    async fn hashes_match_known_sha1() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.jpg"), b"abc").unwrap();
        std::fs::write(dir.path().join("b.jpg"), b"").unwrap();
        let fd = Arc::new(root_fd(dir.path()));
        let paths = vec!["b.jpg".into(), "a.jpg".into(), "gone.jpg".into()];
        let got = hash_all(fd, paths, 2, Cancel::default()).await;
        assert_eq!(got[0].0, PathBuf::from("a.jpg"));
        assert_eq!(
            got[0].1.as_ref().unwrap(),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            got[1].1.as_ref().unwrap(),
            "da39a3ee5e6b4b0d3255bfef95601890afd80709"
        );
        assert!(got[2].1.is_err());
    }
}

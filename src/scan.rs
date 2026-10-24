use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

pub fn parse_roots(roots: &str) -> Vec<PathBuf> {
    roots
        .split(':')
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .map(PathBuf::from)
        .collect()
}

pub fn resolve_scan_root(path: &Path, roots: &[PathBuf]) -> Result<PathBuf, String> {
    if roots.is_empty() {
        return Err("endpoint has no scan_roots; --path is disabled".into());
    }
    let canon = path
        .canonicalize()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let allowed = roots
        .iter()
        .filter_map(|r| r.canonicalize().ok())
        .any(|r| canon.starts_with(&r));
    if allowed {
        Ok(canon)
    } else {
        Err(format!(
            "{} is not under an allowed scan root",
            canon.display()
        ))
    }
}

#[derive(Debug, Default)]
pub struct Walk {
    pub files: Vec<PathBuf>,
    pub failures: Vec<(PathBuf, String)>,
    pub truncated: bool,
}

pub fn walk_media(root: &Path, max_files: usize) -> Walk {
    let mut walk = Walk::default();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => {
                walk.failures.push((dir, e.to_string()));
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    walk.failures.push((dir.clone(), e.to_string()));
                    continue;
                }
            };
            let path = entry.path();
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(e) => {
                    walk.failures.push((path, e.to_string()));
                    continue;
                }
            };
            if ft.is_dir() {
                stack.push(path);
            } else if ft.is_file() && is_media(&path) {
                if walk.files.len() >= max_files {
                    walk.truncated = true;
                    walk.files.sort();
                    return walk;
                }
                walk.files.push(path);
            }
        }
    }
    walk.files.sort();
    walk
}

pub fn sha1_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha1::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub async fn hash_all(
    paths: Vec<PathBuf>,
    concurrency: usize,
) -> Vec<(PathBuf, Result<String, String>)> {
    let permits = Arc::new(Semaphore::new(clamp_concurrency(concurrency)));
    let mut set = JoinSet::new();
    let mut by_task = HashMap::new();
    for path in paths {
        let Ok(permit) = permits.clone().acquire_owned().await else {
            break;
        };
        let task_path = path.clone();
        let handle = set.spawn_blocking(move || {
            let r = sha1_file(&task_path).map_err(|e| e.to_string());
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
                out.push((path, Err(format!("hash task failed: {e}"))));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn scan_root_must_contain_path() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("sub")).unwrap();
        let roots = vec![root.path().to_path_buf()];
        assert!(resolve_scan_root(&root.path().join("sub"), &roots).is_ok());
        assert!(resolve_scan_root(&root.path().join("sub/../.."), &roots).is_err());
        assert!(resolve_scan_root(other.path(), &roots).is_err());
        assert!(resolve_scan_root(root.path(), &[]).is_err());
        assert_eq!(
            parse_roots("/a: /b ::"),
            vec![PathBuf::from("/a"), PathBuf::from("/b")]
        );
    }

    #[cfg(unix)]
    #[test]
    fn scan_root_rejects_symlink_escape() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("out")).unwrap();
        let roots = vec![root.path().to_path_buf()];
        assert!(resolve_scan_root(&root.path().join("out"), &roots).is_err());
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
        let walk = walk_media(root.path(), 100);
        assert_eq!(walk.files, vec![root.path().join("sub/a.jpg")]);
        assert!(!walk.truncated);
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
        let walk = walk_media(root.path(), 100);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(walk.files.contains(&root.path().join("a.jpg")));
        if !readable {
            assert_eq!(walk.failures.len(), 1);
            assert_eq!(walk.failures[0].0, locked);
        }
    }

    #[test]
    fn walk_stops_at_max_files() {
        let root = tempfile::tempdir().unwrap();
        for i in 0..5 {
            std::fs::write(root.path().join(format!("{i}.jpg")), b"x").unwrap();
        }
        let walk = walk_media(root.path(), 3);
        assert_eq!(walk.files.len(), 3);
        assert!(walk.truncated);
        let all = walk_media(root.path(), 5);
        assert_eq!(all.files.len(), 5);
        assert!(!all.truncated);
    }

    #[tokio::test]
    async fn hashes_match_known_sha1() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.jpg");
        let b = dir.path().join("b.jpg");
        std::fs::write(&a, b"abc").unwrap();
        std::fs::write(&b, b"").unwrap();
        let got = hash_all(vec![b.clone(), a.clone(), dir.path().join("gone.jpg")], 2).await;
        assert_eq!(got[0].0, a);
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

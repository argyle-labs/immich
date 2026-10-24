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

pub fn walk_media(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let ft = entry.file_type()?;
            let path = entry.path();
            if ft.is_dir() {
                stack.push(path);
            } else if ft.is_file() && is_media(&path) {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
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
) -> Vec<(PathBuf, std::io::Result<String>)> {
    let permits = Arc::new(Semaphore::new(concurrency.max(1)));
    let mut set = JoinSet::new();
    for path in paths {
        let Ok(permit) = permits.clone().acquire_owned().await else {
            break;
        };
        set.spawn_blocking(move || {
            let r = sha1_file(&path);
            drop(permit);
            (path, r)
        });
    }
    let mut out = Vec::new();
    while let Some(r) = set.join_next().await {
        if let Ok(pair) = r {
            out.push(pair);
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
        let found = walk_media(root.path()).unwrap();
        assert_eq!(found, vec![root.path().join("sub/a.jpg")]);
    }

    #[tokio::test]
    async fn hashes_match_known_sha1() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.jpg");
        let b = dir.path().join("b.jpg");
        std::fs::write(&a, b"abc").unwrap();
        std::fs::write(&b, b"").unwrap();
        let got = hash_all(vec![b.clone(), a.clone()], 2).await;
        assert_eq!(got[0].0, a);
        assert_eq!(
            got[0].1.as_ref().unwrap(),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            got[1].1.as_ref().unwrap(),
            "da39a3ee5e6b4b0d3255bfef95601890afd80709"
        );
    }
}

//! Bounded history: one startup read, writes only for paths absent from the current list.
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::PathBuf;

pub const LIMIT: usize = 20;
const MAX_BYTES: usize = 64 * 1024;

#[derive(Default)]
pub struct Recent {
    pub files: Vec<PathBuf>,
    pub folders: Vec<PathBuf>,
    storage: Option<PathBuf>,
    directory_ready: bool,
}

impl Recent {
    #[cfg_attr(test, allow(dead_code))]
    pub fn load() -> Self {
        #[cfg(windows)]
        let base = std::env::var_os("APPDATA").map(PathBuf::from);
        #[cfg(not(windows))]
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
        base.map(|base| Self::load_from(base.join("NativeMarkdown/recent.json")))
            .unwrap_or_default()
    }

    pub(crate) fn load_from(path: PathBuf) -> Self {
        let mut recent = Self {
            storage: Some(path.clone()),
            ..Self::default()
        };
        // No exists/metadata probe and no access to any recorded path.
        let read = || -> io::Result<Vec<u8>> {
            let mut bytes = Vec::new();
            File::open(path)?
                .take((MAX_BYTES + 1) as u64)
                .read_to_end(&mut bytes)?;
            Ok(bytes)
        };
        if let Ok(bytes) = read() {
            if bytes.len() <= MAX_BYTES {
                if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                    recent.files = parse_paths(&value["files"]);
                    recent.folders = parse_paths(&value["folders"]);
                }
            }
        }
        recent
    }

    pub fn file(&mut self, path: PathBuf) -> io::Result<()> {
        let added = remember(&mut self.files, path);
        self.persist_if_new(added)
    }

    pub fn folder(&mut self, path: PathBuf) -> io::Result<()> {
        let added = remember(&mut self.folders, path);
        self.persist_if_new(added)
    }

    fn persist_if_new(&mut self, added: bool) -> io::Result<()> {
        let Some(path) = self.storage.as_ref().filter(|_| added) else {
            return Ok(());
        };
        let bytes = serde_json::to_vec(&serde_json::json!({
            "files": self.files, "folders": self.folders,
        }))?;
        if bytes.len() > MAX_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "recent history exceeds 64 KiB",
            ));
        }
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("history has no parent"))?;
        if !self.directory_ready {
            fs::create_dir_all(parent)?;
            self.directory_ready = true;
        }
        // One bounded snapshot, atomically replaced; no sync_all, reread, timer, or exit flush.
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(&bytes)?;
        temporary.persist(path).map_err(|error| error.error)?;
        Ok(())
    }
}

fn parse_paths(value: &serde_json::Value) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(entries) = value.as_array() {
        for entry in entries.iter().filter_map(|entry| entry.as_str()) {
            let path = PathBuf::from(entry);
            if path.is_absolute() && !paths.contains(&path) {
                paths.push(path);
                if paths.len() == LIMIT {
                    break;
                }
            }
        }
    }
    paths
}

fn remember(paths: &mut Vec<PathBuf>, path: PathBuf) -> bool {
    if paths.first() == Some(&path) {
        return false;
    }
    let added = !paths.contains(&path);
    paths.retain(|entry| entry != &path);
    paths.insert(0, path);
    paths.truncate(LIMIT);
    added
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_new_paths_write_and_evicted_paths_write_again() {
        let directory = tempfile::tempdir().unwrap();
        let storage = directory.path().join("config/recent.json");
        let mut recent = Recent::load_from(storage.clone());
        assert!(
            !storage.exists(),
            "loading missing history must not create it"
        );
        for i in 0..LIMIT {
            recent
                .file(directory.path().join(format!("missing-{i}.md")))
                .unwrap();
        }
        let before = fs::read(&storage).unwrap();
        // Reopening any of the 20 entries must neither read nor rewrite the snapshot.
        fs::write(&storage, b"sentinel").unwrap();
        for path in recent.files.clone() {
            recent.file(path).unwrap();
        }
        assert_eq!(fs::read(&storage).unwrap(), b"sentinel");
        fs::write(&storage, &before).unwrap();
        let reloaded = Recent::load_from(storage.clone());
        assert_eq!(reloaded.files.len(), LIMIT);
        assert_eq!(reloaded.files[0], directory.path().join("missing-19.md"));
        recent.file(directory.path().join("new.md")).unwrap();
        assert_eq!(
            Recent::load_from(storage.clone()).files[0],
            directory.path().join("new.md")
        );
        let evicted = reloaded.files[0].clone();
        assert!(!recent.files.contains(&evicted));
        recent.file(evicted.clone()).unwrap();
        assert_eq!(Recent::load_from(storage.clone()).files[0], evicted);
        let folder = directory.path().join("missing-folder");
        recent.folder(folder.clone()).unwrap();
        assert_eq!(
            Recent::load_from(storage.clone()).folders,
            vec![folder.clone()]
        );
        fs::write(&storage, b"sentinel").unwrap();
        recent.folder(folder).unwrap();
        drop(recent);
        assert_eq!(
            fs::read(storage).unwrap(),
            b"sentinel",
            "no duplicate or exit writes"
        );
    }

    #[test]
    fn corrupt_and_oversized_history_is_ignored_without_repair_writes() {
        let directory = tempfile::tempdir().unwrap();
        let storage = directory.path().join("recent.json");
        for bytes in [b"invalid".to_vec(), vec![b' '; MAX_BYTES + 1]] {
            fs::write(&storage, &bytes).unwrap();
            let recent = Recent::load_from(storage.clone());
            assert!(recent.files.is_empty() && recent.folders.is_empty());
            assert_eq!(fs::read(&storage).unwrap(), bytes);
        }
    }

    #[test]
    fn write_failure_keeps_memory_history() {
        let directory = tempfile::tempdir().unwrap();
        let blocked = directory.path().join("blocked");
        fs::write(&blocked, b"file").unwrap();
        let mut recent = Recent::load_from(blocked.join("recent.json"));
        let path = directory.path().join("note.md");
        assert!(recent.file(path.clone()).is_err());
        assert_eq!(recent.files, vec![path.clone()]);
        assert!(
            recent.file(path).is_ok(),
            "existing paths do not trigger retries"
        );
    }
}

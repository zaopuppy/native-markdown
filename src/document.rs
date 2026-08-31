use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tempfile::NamedTempFile;

const RECOVERY_FILE: &str = "native-markdown-recovery.md";
const RECOVERY_DEBOUNCE: Duration = Duration::from_millis(900);

pub struct Document {
    pub content: String,
    pub path: Option<PathBuf>,
    saved_content: String,
    last_recovery_write: Instant,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            content: String::new(),
            path: None,
            saved_content: String::new(),
            last_recovery_write: Instant::now(),
        }
    }
}

impl Document {
    pub fn new_document() -> Self {
        Self {
            content: "# Untitled\n\n".to_owned(),
            path: None,
            saved_content: String::new(),
            last_recovery_write: Instant::now(),
        }
    }

    pub fn open(path: PathBuf) -> io::Result<Self> {
        let content = fs::read_to_string(&path)?;
        Ok(Self {
            saved_content: content.clone(),
            content,
            path: Some(path),
            last_recovery_write: Instant::now(),
        })
    }

    pub fn recover() -> io::Result<Self> {
        let content = fs::read_to_string(Self::recovery_path())?;
        Ok(Self {
            content,
            path: None,
            saved_content: String::new(),
            last_recovery_write: Instant::now(),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.path.is_none() && self.content.is_empty()
    }

    pub fn is_dirty(&self) -> bool {
        self.content != self.saved_content
    }

    pub fn display_name(&self) -> String {
        self.path
            .as_deref()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or(if self.is_empty() {
                "No document"
            } else {
                "Untitled.md"
            })
            .to_owned()
    }

    pub fn save(&mut self) -> io::Result<()> {
        let path = self
            .path
            .clone()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "document has no file path"))?;
        self.write_to(path)
    }

    pub fn save_as(&mut self, path: PathBuf) -> io::Result<()> {
        self.write_to(path)
    }

    fn write_to(&mut self, path: PathBuf) -> io::Result<()> {
        atomic_write(&path, self.content.as_bytes())?;
        self.path = Some(path);
        self.saved_content.clone_from(&self.content);
        Self::clear_recovery();
        Ok(())
    }

    pub fn maybe_write_recovery(&mut self) -> io::Result<()> {
        if !self.is_dirty() || self.last_recovery_write.elapsed() < RECOVERY_DEBOUNCE {
            return Ok(());
        }

        atomic_write(&Self::recovery_path(), self.content.as_bytes())?;
        self.last_recovery_write = Instant::now();
        Ok(())
    }

    pub fn recovery_exists() -> bool {
        Self::recovery_path().is_file()
    }

    pub fn clear_recovery() {
        let path = Self::recovery_path();
        if path.is_file() {
            let _ = fs::remove_file(path);
        }
    }

    fn recovery_path() -> PathBuf {
        std::env::temp_dir().join(RECOVERY_FILE)
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dirty_state_tracks_saved_content() {
        let mut document = Document::default();
        assert!(!document.is_dirty());
        document.content.push_str("hello");
        assert!(document.is_dirty());
    }

    #[test]
    fn atomic_write_replaces_existing_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("note.md");
        fs::write(&path, "old").unwrap();
        atomic_write(&path, b"new").unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), "new");
    }
}

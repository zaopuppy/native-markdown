use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const COMMON_HEAVY_DIRECTORIES: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".idea",
    ".vscode",
    "node_modules",
    "target",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Directory { traversable: bool },
    Markdown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeEntry {
    pub path: PathBuf,
    pub name: String,
    pub kind: EntryKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadState {
    Unloaded,
    Loading { request: u64 },
    Loaded,
    Error(String),
}

#[derive(Clone, Debug)]
struct DirectoryNode {
    expanded: bool,
    entries: Vec<TreeEntry>,
    load_state: LoadState,
}

impl DirectoryNode {
    fn new(expanded: bool) -> Self {
        Self {
            expanded,
            entries: Vec::new(),
            load_state: LoadState::Unloaded,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VisibleRow {
    Entry {
        entry: TreeEntry,
        depth: usize,
        expanded: bool,
    },
    Loading {
        path: PathBuf,
        depth: usize,
        refreshing: bool,
    },
    Empty {
        path: PathBuf,
        depth: usize,
    },
    Error {
        path: PathBuf,
        depth: usize,
        message: String,
    },
}

#[derive(Debug)]
pub struct FileTree {
    root: Option<PathBuf>,
    directories: HashMap<PathBuf, DirectoryNode>,
    selected: Option<PathBuf>,
    show_hidden: bool,
    next_request: u64,
}

impl FileTree {
    pub fn new(root: Option<PathBuf>) -> Self {
        let mut tree = Self {
            root: None,
            directories: HashMap::new(),
            selected: None,
            show_hidden: false,
            next_request: 1,
        };
        tree.set_root(root);
        tree
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    pub fn selected(&self) -> Option<&Path> {
        self.selected.as_deref()
    }

    pub fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    pub fn set_selected(&mut self, selected: Option<PathBuf>) {
        self.selected = selected;
    }

    pub fn set_root(&mut self, root: Option<PathBuf>) {
        self.root = root;
        self.directories.clear();
        self.selected = None;
        if let Some(root) = self.root.clone() {
            self.directories.insert(root, DirectoryNode::new(true));
        }
    }

    pub fn set_show_hidden(&mut self, show_hidden: bool) -> Vec<PathBuf> {
        if self.show_hidden == show_hidden {
            return Vec::new();
        }
        self.show_hidden = show_hidden;
        self.refresh_paths()
    }

    pub fn toggle_directory(&mut self, path: &Path) -> Option<PathBuf> {
        let node = self
            .directories
            .entry(path.to_path_buf())
            .or_insert_with(|| DirectoryNode::new(false));
        node.expanded = !node.expanded;
        if node.expanded && matches!(node.load_state, LoadState::Unloaded | LoadState::Error(_)) {
            Some(path.to_path_buf())
        } else {
            None
        }
    }

    pub fn collapse_selected(&mut self) -> bool {
        let Some(path) = self.selected.clone() else {
            return false;
        };
        let Some(node) = self.directories.get_mut(&path) else {
            return false;
        };
        if !node.expanded {
            return false;
        }
        node.expanded = false;
        true
    }

    pub fn expand_selected(&mut self) -> Option<PathBuf> {
        let path = self.selected.clone()?;
        let node = self.directories.get_mut(&path)?;
        if node.expanded {
            return None;
        }
        node.expanded = true;
        matches!(node.load_state, LoadState::Unloaded | LoadState::Error(_)).then_some(path)
    }

    pub fn begin_load(&mut self, path: &Path) -> u64 {
        let request = self.next_request;
        self.next_request = self.next_request.wrapping_add(1).max(1);
        let node = self
            .directories
            .entry(path.to_path_buf())
            .or_insert_with(|| DirectoryNode::new(path == self.root.as_deref().unwrap_or(path)));
        node.load_state = LoadState::Loading { request };
        request
    }

    pub fn finish_load(
        &mut self,
        path: &Path,
        request: u64,
        result: Result<Vec<TreeEntry>, String>,
    ) -> bool {
        let Some(node) = self.directories.get_mut(path) else {
            return false;
        };
        if node.load_state != (LoadState::Loading { request }) {
            return false;
        }
        match result {
            Ok(entries) => {
                let live_directories = entries
                    .iter()
                    .filter_map(|entry| match entry.kind {
                        EntryKind::Directory { .. } => Some(entry.path.clone()),
                        EntryKind::Markdown => None,
                    })
                    .collect::<HashSet<_>>();
                node.entries = entries;
                node.load_state = LoadState::Loaded;
                self.directories.retain(|directory, _| {
                    directory == path
                        || !directory.starts_with(path)
                        || live_directories
                            .iter()
                            .any(|live| directory == live || directory.starts_with(live))
                });
            }
            Err(error) => node.load_state = LoadState::Error(error),
        }
        true
    }

    pub fn refresh_paths(&mut self) -> Vec<PathBuf> {
        let Some(root) = self.root.clone() else {
            return Vec::new();
        };
        let mut paths = vec![root];
        paths.extend(
            self.directories
                .iter()
                .filter(|(_, node)| node.expanded)
                .map(|(path, _)| path.clone()),
        );
        paths.sort();
        paths.dedup();
        paths
    }

    pub fn visible_rows(&self) -> Vec<VisibleRow> {
        let mut rows = Vec::new();
        if let Some(root) = self.root.as_deref() {
            self.append_directory_rows(root, 0, &mut rows);
        }
        rows
    }

    pub fn selectable_paths(&self) -> Vec<PathBuf> {
        self.visible_rows()
            .into_iter()
            .filter_map(|row| match row {
                VisibleRow::Entry { entry, .. } => Some(entry.path),
                _ => None,
            })
            .collect()
    }

    fn append_directory_rows(&self, path: &Path, depth: usize, rows: &mut Vec<VisibleRow>) {
        let Some(node) = self.directories.get(path) else {
            return;
        };
        for entry in &node.entries {
            let expanded = matches!(entry.kind, EntryKind::Directory { .. })
                && self
                    .directories
                    .get(&entry.path)
                    .is_some_and(|child| child.expanded);
            rows.push(VisibleRow::Entry {
                entry: entry.clone(),
                depth,
                expanded,
            });
            if expanded {
                self.append_directory_rows(&entry.path, depth + 1, rows);
            }
        }

        match &node.load_state {
            LoadState::Loading { .. } => rows.push(VisibleRow::Loading {
                path: path.to_path_buf(),
                depth,
                refreshing: !node.entries.is_empty(),
            }),
            LoadState::Loaded if node.entries.is_empty() => rows.push(VisibleRow::Empty {
                path: path.to_path_buf(),
                depth,
            }),
            LoadState::Error(message) => rows.push(VisibleRow::Error {
                path: path.to_path_buf(),
                depth,
                message: message.clone(),
            }),
            LoadState::Unloaded | LoadState::Loaded => {}
        }
    }
}

pub fn scan_directory(path: &Path, show_hidden: bool) -> Result<Vec<TreeEntry>, String> {
    let iterator = fs::read_dir(path).map_err(|error| friendly_io_error(path, &error))?;
    let mut entries = Vec::new();
    for item in iterator {
        let item = match item {
            Ok(item) => item,
            Err(_) => continue,
        };
        let path = item.path();
        let name = item.file_name().to_string_lossy().into_owned();
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        let hidden = is_hidden(&name, &metadata);
        if !show_hidden && (hidden || is_common_heavy_directory(&name, &metadata)) {
            continue;
        }

        let kind = if metadata.is_dir() {
            EntryKind::Directory {
                traversable: !is_reparse_or_symlink(&metadata),
            }
        } else if is_markdown_path(&path) {
            EntryKind::Markdown
        } else {
            continue;
        };
        entries.push(TreeEntry { path, name, kind });
    }
    entries.sort_by(|left, right| {
        entry_rank(left.kind)
            .cmp(&entry_rank(right.kind))
            .then_with(|| natural_cmp(&left.name, &right.name))
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(entries)
}

pub fn is_markdown_path(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
        })
}

fn entry_rank(kind: EntryKind) -> u8 {
    match kind {
        EntryKind::Directory { .. } => 0,
        EntryKind::Markdown => 1,
    }
}

fn is_common_heavy_directory(name: &str, metadata: &fs::Metadata) -> bool {
    metadata.is_dir()
        && COMMON_HEAVY_DIRECTORIES
            .iter()
            .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

fn is_hidden(name: &str, metadata: &fs::Metadata) -> bool {
    if name.starts_with('.') {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
        const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
        metadata.file_attributes() & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        false
    }
}

fn is_reparse_or_symlink(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn friendly_io_error(path: &Path, error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::PermissionDenied => "Permission denied".to_owned(),
        io::ErrorKind::NotFound => "Directory is unavailable".to_owned(),
        _ => format!("Could not read {}: {error}", path.display()),
    }
}

fn natural_cmp(left: &str, right: &str) -> Ordering {
    let left = left.to_lowercase();
    let right = right.to_lowercase();
    let mut left_chars = left.chars().peekable();
    let mut right_chars = right.chars().peekable();

    loop {
        match (left_chars.peek(), right_chars.peek()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(left_char), Some(right_char))
                if left_char.is_ascii_digit() && right_char.is_ascii_digit() =>
            {
                let left_number = take_number(&mut left_chars);
                let right_number = take_number(&mut right_chars);
                match left_number.cmp(&right_number) {
                    Ordering::Equal => {}
                    ordering => return ordering,
                }
            }
            _ => match left_chars.next().cmp(&right_chars.next()) {
                Ordering::Equal => {}
                ordering => return ordering,
            },
        }
    }
}

fn take_number(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> u128 {
    let mut value = 0u128;
    while let Some(character) = chars.peek().copied().filter(char::is_ascii_digit) {
        chars.next();
        value = value
            .saturating_mul(10)
            .saturating_add(character.to_digit(10).unwrap_or(0) as u128);
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn scan_filters_and_naturally_sorts_entries() {
        let directory = tempdir().unwrap();
        fs::create_dir(directory.path().join("chapter10")).unwrap();
        fs::create_dir(directory.path().join("chapter2")).unwrap();
        fs::create_dir(directory.path().join("node_modules")).unwrap();
        fs::write(directory.path().join("note10.md"), "").unwrap();
        fs::write(directory.path().join("note2.MARKDOWN"), "").unwrap();
        fs::write(directory.path().join("notes.txt"), "").unwrap();

        let entries = scan_directory(directory.path(), false).unwrap();
        let names = entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            ["chapter2", "chapter10", "note2.MARKDOWN", "note10.md"]
        );
    }

    #[test]
    fn stale_load_results_are_ignored() {
        let root = PathBuf::from("root");
        let mut tree = FileTree::new(Some(root.clone()));
        let stale = tree.begin_load(&root);
        let current = tree.begin_load(&root);
        assert!(!tree.finish_load(&root, stale, Ok(Vec::new())));
        assert!(tree.finish_load(&root, current, Ok(Vec::new())));
        assert!(matches!(tree.visible_rows()[0], VisibleRow::Empty { .. }));
    }

    #[test]
    fn changing_root_drops_expansion_state() {
        let first = PathBuf::from("first");
        let child = first.join("child");
        let second = PathBuf::from("second");
        let mut tree = FileTree::new(Some(first.clone()));
        let request = tree.begin_load(&first);
        tree.finish_load(
            &first,
            request,
            Ok(vec![TreeEntry {
                path: child.clone(),
                name: "child".to_owned(),
                kind: EntryKind::Directory { traversable: true },
            }]),
        );
        assert_eq!(tree.toggle_directory(&child), Some(child));

        tree.set_root(Some(second.clone()));
        assert_eq!(tree.root(), Some(second.as_path()));
        assert!(tree.selectable_paths().is_empty());
    }

    #[test]
    fn markdown_extensions_are_intentionally_narrow() {
        assert!(is_markdown_path(Path::new("README.md")));
        assert!(is_markdown_path(Path::new("README.MARKDOWN")));
        assert!(!is_markdown_path(Path::new("README.mdown")));
        assert!(!is_markdown_path(Path::new("notes.txt")));
    }
}

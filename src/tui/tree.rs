use std::fs;
use std::path::PathBuf;

/// One entry in the drive tree. Children are loaded lazily on expand so
/// startup is instant even with big disks attached.
#[derive(Debug, Clone)]
pub struct Node {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub expanded: bool,
    pub loaded: bool,
    pub children: Vec<Node>,
}

impl Node {
    pub fn new(path: PathBuf, is_dir: bool) -> Node {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        Node {
            path,
            name,
            is_dir,
            expanded: false,
            loaded: false,
            children: Vec::new(),
        }
    }

    /// Expand this directory, loading children from disk on first use.
    pub fn expand(&mut self) {
        if !self.is_dir {
            return;
        }
        if !self.loaded {
            self.children = load_children(&self.path);
            self.loaded = true;
        }
        self.expanded = true;
    }

    pub fn collapse(&mut self) {
        self.expanded = false;
    }
}

fn load_children(path: &PathBuf) -> Vec<Node> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(path) {
        for e in entries.flatten() {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            // Skip hidden entries (dotfiles, .Trashes, bundles).
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            if ft.is_dir() {
                dirs.push(Node::new(p, true));
            } else if ft.is_file() {
                files.push(Node::new(p, false));
            }
        }
    }
    dirs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    dirs.extend(files);
    dirs
}

/// A flattened visible row for rendering and cursor navigation.
#[derive(Debug, Clone)]
pub struct Row {
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
}

/// The whole tree, starting at /Volumes (plus the system disk).
pub struct Tree {
    pub roots: Vec<Node>,
    pub cursor: usize,
}

impl Tree {
    pub fn new() -> Tree {
        let mut roots = Vec::new();
        for base in base_paths() {
            if let Ok(entries) = fs::read_dir(&base) {
                let mut vols: Vec<Node> = entries
                    .flatten()
                    .map(|e| e.path())
                    .map(|p| Node::new(p, true))
                    .collect();
                vols.sort_by(|a, b| a.name.cmp(&b.name));
                roots.extend(vols);
            }
        }
        // On macOS, the system disk is still browsable, just sorted last.
        if let Some(pos) = roots.iter().position(|n| n.name == "Macintosh HD") {
            let sys = roots.remove(pos);
            roots.push(sys);
        }
        Tree { roots, cursor: 0 }
    }

    /// Depth-first flatten of currently expanded nodes.
    pub fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for root in &self.roots {
            flatten(root, 0, &mut rows);
        }
        rows
    }

    pub fn move_cursor(&mut self, delta: i64) {
        let len = self.rows().len();
        if len == 0 {
            return;
        }
        let cur = self.cursor as i64 + delta;
        self.cursor = cur.clamp(0, len as i64 - 1) as usize;
    }

    pub fn selected(&self) -> Option<Row> {
        let rows = self.rows();
        rows.get(self.cursor).cloned()
    }

    /// Right arrow / Enter on a directory: expand (loading children if
    /// needed). Returns true if the tree shape changed (cursor may need
    /// revalidation).
    pub fn expand_selected(&mut self) -> bool {
        let Some(sel) = self.selected() else { return false };
        self.expand_at(&sel.path)
    }

    pub fn expand_at(&mut self, path: &PathBuf) -> bool {
        for root in &mut self.roots {
            if let Some(node) = find_mut(root, path) {
                if node.is_dir && (!node.loaded || !node.expanded) {
                    node.expand();
                    return true;
                }
                return false;
            }
        }
        false
    }

    /// Left arrow: collapse the selected directory, or step up to its parent.
    pub fn collapse_selected(&mut self) {
        let Some(sel) = self.selected() else { return };
        let parent_path = sel.path.parent().map(|p| p.to_path_buf());
        for root in &mut self.roots {
            if let Some(node) = find_mut(root, &sel.path) {
                if node.expanded {
                    node.collapse();
                    return;
                }
            }
        }
        // Not expanded: move the cursor up to the parent row.
        if let Some(par) = parent_path {
            let rows = self.rows();
            if let Some(pos) = rows.iter().position(|r| r.path == par) {
                self.cursor = pos;
            }
        }
    }

    /// Enter on an expanded directory selects it as import destination.
    pub fn revalidate_cursor(&mut self) {
        let len = self.rows().len();
        if self.cursor >= len {
            self.cursor = len.saturating_sub(1);
        }
    }
}

fn flatten(node: &Node, depth: usize, out: &mut Vec<Row>) {
    out.push(Row {
        path: node.path.clone(),
        name: node.name.clone(),
        depth,
        is_dir: node.is_dir,
        expanded: node.expanded,
    });
    if node.expanded {
        for child in &node.children {
            flatten(child, depth + 1, out);
        }
    }
}

fn find_mut<'a>(node: &'a mut Node, path: &PathBuf) -> Option<&'a mut Node> {
    if &node.path == path {
        return Some(node);
    }
    for child in &mut node.children {
        if let Some(found) = find_mut(child, path) {
            return Some(found);
        }
    }
    None
}

/// Where the drive tree starts per platform.
fn base_paths() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        vec![PathBuf::from("/Volumes")]
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        vec![PathBuf::from("/")]
    }
    #[cfg(windows)]
    {
        (b'C'..=b'Z')
            .map(|c| PathBuf::from(format!("{}:\\", c as char)))
            .filter(|p| p.exists())
            .collect()
    }
}

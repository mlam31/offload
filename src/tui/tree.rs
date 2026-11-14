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
    dirs.sort_by_key(|a| a.name.to_lowercase());
    files.sort_by_key(|a| a.name.to_lowercase());
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

impl Default for Tree {
    fn default() -> Self {
        Self::new()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tree_with(root: Node) -> Tree {
        Tree {
            roots: vec![root],
            cursor: 0,
        }
    }

    fn dir(path: &str) -> Node {
        let mut n = Node::new(PathBuf::from(path), true);
        n.loaded = true;
        n.expanded = true;
        n
    }

    fn file(path: &str) -> Node {
        Node::new(PathBuf::from(path), false)
    }

    #[test]
    fn node_name_falls_back_to_full_path_for_roots() {
        assert_eq!(Node::new(PathBuf::from("/Volumes/CARD"), true).name, "CARD");
        assert_eq!(Node::new(PathBuf::from("/"), true).name, "/");
        assert_eq!(Node::new(PathBuf::from("f.jpg"), false).name, "f.jpg");
    }

    #[test]
    fn expand_loads_children_sorted_dirs_first_case_insensitively_skipping_hidden() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("b_dir")).unwrap();
        std::fs::create_dir_all(tmp.path().join("A_dir")).unwrap();
        std::fs::create_dir_all(tmp.path().join(".hidden")).unwrap();
        std::fs::write(tmp.path().join("z.jpg"), "x").unwrap();
        std::fs::write(tmp.path().join("._junk"), "x").unwrap();

        let mut node = Node::new(tmp.path().to_path_buf(), true);
        assert!(!node.loaded);
        node.expand();
        assert!(node.expanded && node.loaded);
        let names: Vec<&str> = node.children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["A_dir", "b_dir", "z.jpg"]);

        // collapse/expand again must not re-read from disk.
        node.collapse();
        assert!(!node.expanded);
        node.expand();
        assert_eq!(node.children.len(), 3);
    }

    #[test]
    fn expand_on_a_file_is_a_no_op() {
        let mut n = file("/x/f.jpg");
        n.expand();
        assert!(!n.loaded && !n.expanded && n.children.is_empty());
    }

    #[test]
    fn flatten_is_depth_first_and_only_expands_open_nodes() {
        let mut root = dir("/r");
        let mut child = dir("/r/sub");
        child.children = vec![file("/r/sub/f.jpg")];
        root.children = vec![child];

        let mut rows = Vec::new();
        flatten(&root, 0, &mut rows);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].depth, 0);
        assert_eq!(rows[1].depth, 1);
        assert_eq!(rows[2].depth, 2);
        assert_eq!(rows[2].path, PathBuf::from("/r/sub/f.jpg"));

        root.expanded = false;
        let mut rows = Vec::new();
        flatten(&root, 0, &mut rows);
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn move_cursor_clamps_to_bounds() {
        let mut root = dir("/r");
        root.children = (0..3).map(|i| file(&format!("/r/{i}"))).collect();
        let mut t = tree_with(root);
        assert_eq!(t.rows().len(), 4);

        t.move_cursor(100);
        assert_eq!(t.cursor, 3);
        t.move_cursor(-100);
        assert_eq!(t.cursor, 0);
        t.move_cursor(2);
        assert_eq!(t.cursor, 2);
        // Empty tree never panics.
        let mut empty = Tree { roots: Vec::new(), cursor: 0 };
        empty.move_cursor(1);
        assert_eq!(empty.cursor, 0);
    }

    #[test]
    fn collapse_selected_collapses_or_steps_up_to_parent() {
        let mut root = dir("/r");
        let mut sub = dir("/r/sub");
        sub.children = vec![file("/r/sub/f")];
        root.children = vec![sub];
        let mut t = tree_with(root);
        t.cursor = 2; // /r/sub/f

        // Not expanded → cursor steps up to the parent row.
        t.collapse_selected();
        assert_eq!(t.cursor, 1); // /r/sub

        // Expanded → collapses in place.
        t.collapse_selected();
        assert_eq!(t.cursor, 1);
        assert!(!t.roots[0].children[0].expanded);
    }

    #[test]
    fn expand_at_expands_nested_nodes_and_reports_change() {
        let mut root = dir("/r");
        let mut sub = Node::new(PathBuf::from("/r/sub"), true);
        sub.children = vec![file("/r/sub/f")];
        sub.loaded = true;
        root.children = vec![sub];
        let mut t = tree_with(root);

        assert!(t.expand_at(&PathBuf::from("/r/sub")));
        assert!(t.roots[0].children[0].expanded);
        // Already expanded → no change.
        assert!(!t.expand_at(&PathBuf::from("/r/sub")));
        // Unknown path → no change.
        assert!(!t.expand_at(&PathBuf::from("/nowhere")));
    }

    #[test]
    fn revalidate_cursor_keeps_it_in_range() {
        let mut root = dir("/r");
        root.children = vec![file("/r/a")];
        let mut t = tree_with(root);
        t.cursor = 99;
        t.revalidate_cursor();
        assert_eq!(t.cursor, 1);
    }
}

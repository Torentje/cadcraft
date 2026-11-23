//! AutoCAD-compatible drawing lock files (`.dwl` and `.dwl2`).
//!
//! When a `.dwg` drawing is opened from the filesystem, CADCraft acquires a lock
//! by creating sibling `<drawing>.dwl` and `<drawing>.dwl2` files atomically (`create_new`).
//!
//! If a lock file already exists:
//! - CADCraft reads the lock information (username, machine name, timestamp).
//! - If the lock is held by another user/process, the drawing is opened in **read-only** mode.
//! - Read-only drawings prevent accidental overwrites (`QSAVE` fails and prompts `SAVEAS`).
//!
//! When the drawing is closed or saved to another file via `SAVEAS`, the lock guard deletes
//! the `.dwl` and `.dwl2` files.

use std::path::{Path, PathBuf};

/// Maximum size in bytes of a `.dwl` or `.dwl2` lock file read from disk.
#[cfg(not(target_arch = "wasm32"))]
const MAX_LOCK_FILE_BYTES: usize = 64 * 1024;

/// Drawing lock information parsed from an existing `.dwl` or `.dwl2` file.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LockInfo {
    pub user: String,
    pub machine: String,
    pub date_time: String,
    pub drawing_path: Option<String>,
}

impl LockInfo {
    /// Human-readable description of who holds the lock.
    pub fn description(&self) -> String {
        if !self.machine.is_empty() {
            format!("in use by {} on {} ({})", self.user, self.machine, self.date_time)
        } else {
            format!("in use by {} ({})", self.user, self.date_time)
        }
    }
}

/// Sibling `.dwl` path for `drawing_path`.
pub fn dwl_path(drawing_path: &Path) -> PathBuf {
    drawing_path.with_extension("dwl")
}

/// Sibling `.dwl2` path for `drawing_path`.
pub fn dwl2_path(drawing_path: &Path) -> PathBuf {
    drawing_path.with_extension("dwl2")
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::{LockInfo, MAX_LOCK_FILE_BYTES, Path, PathBuf, dwl_path, dwl2_path};

    /// RAII guard that holds a drawing lock and deletes `.dwl` and `.dwl2` on drop.
    #[derive(Debug)]
    pub struct DrawingLockGuard {
        dwl: PathBuf,
        dwl2: PathBuf,
        active: bool,
    }

    impl DrawingLockGuard {
        pub fn new(dwl: PathBuf, dwl2: PathBuf) -> Self {
            Self { dwl, dwl2, active: true }
        }

        /// Explicitly release and remove the lock files.
        pub fn release(&mut self) {
            if self.active {
                self.active = false;
                if crate::file_access::check_write(&self.dwl).is_ok() {
                    let _ = std::fs::remove_file(&self.dwl);
                }
                if crate::file_access::check_write(&self.dwl2).is_ok() {
                    let _ = std::fs::remove_file(&self.dwl2);
                }
            }
        }

        pub fn dwl(&self) -> &Path {
            &self.dwl
        }

        pub fn dwl2(&self) -> &Path {
            &self.dwl2
        }
    }

    impl Drop for DrawingLockGuard {
        fn drop(&mut self) {
            self.release();
        }
    }

    /// Result of attempting to acquire a drawing lock.
    pub enum LockResult {
        /// Lock acquired successfully.
        Acquired(DrawingLockGuard),
        /// Already locked by another session or user.
        AlreadyLocked(LockInfo),
        /// Failed to write lock files (e.g. read-only filesystem or insufficient permissions).
        Failed(String),
    }

    fn current_user() -> String {
        std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_else(|_| "User".to_string())
    }

    fn current_machine() -> String {
        std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "localhost".to_string())
    }

    fn xml_escape(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            match c {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                '"' => out.push_str("&quot;"),
                '\'' => out.push_str("&apos;"),
                _ => out.push(c),
            }
        }
        out
    }

    fn xml_unescape(s: &str) -> String {
        s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
    }

    fn extract_tag(content: &str, tag: &str) -> Option<String> {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        let start_pos = content.find(&open)?;
        let val_start = start_pos + open.len();
        let end_rel = content[val_start..].find(&close)?;
        Some(xml_unescape(content[val_start..val_start + end_rel].trim()))
    }

    pub fn parse_dwl2(content: &str) -> Option<LockInfo> {
        let user = extract_tag(content, "UserName")?;
        let machine = extract_tag(content, "MachineName").unwrap_or_default();
        let date_time = extract_tag(content, "DateTime").unwrap_or_default();
        let drawing_path = extract_tag(content, "DrawingPath");
        Some(LockInfo { user, machine, date_time, drawing_path })
    }

    pub fn parse_dwl(content: &str) -> Option<LockInfo> {
        let clean = content.trim_start_matches('\u{feff}');
        let mut lines = clean.lines().map(str::trim);
        let user = lines.next()?.to_string();
        let machine = lines.next().unwrap_or("").to_string();
        let date_time = lines.next().unwrap_or("").to_string();
        if user.is_empty() && machine.is_empty() {
            return None;
        }
        Some(LockInfo { user, machine, date_time, drawing_path: None })
    }

    /// True if `p` is a regular file (rejects symlinks, FIFOs, and directories).
    fn is_regular_file(p: &Path) -> bool {
        std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file())
    }

    /// Read a regular lock file up to [`MAX_LOCK_FILE_BYTES`] while honoring automation read roots.
    fn read_lock_file_capped(p: &Path) -> Option<String> {
        if crate::file_access::check_read(p).is_err() || !is_regular_file(p) {
            return None;
        }
        let bytes = crate::capped_read::read_capped(p, MAX_LOCK_FILE_BYTES).ok()?;
        String::from_utf8(bytes).ok()
    }

    /// Whether two filesystem paths refer to the same drawing (resolving relative/case differences).
    pub fn same_path(a: &Path, b: &Path) -> bool {
        if let (Ok(ca), Ok(cb)) = (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            return ca == cb;
        }
        #[cfg(windows)]
        {
            a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy())
        }
        #[cfg(not(windows))]
        {
            a == b
        }
    }

    /// Whether a sibling `.dwl` or `.dwl2` entry exists for `drawing_path` (even if unparseable).
    pub fn has_lock_files(drawing_path: &Path) -> bool {
        let dwl = dwl_path(drawing_path);
        let dwl2 = dwl2_path(drawing_path);
        (crate::file_access::check_read(&dwl).is_ok() && std::fs::symlink_metadata(&dwl).is_ok())
            || (crate::file_access::check_read(&dwl2).is_ok() && std::fs::symlink_metadata(&dwl2).is_ok())
    }

    /// Read lock information from `<drawing>.dwl2` or `<drawing>.dwl` if either exists.
    pub fn read_lock_info(drawing_path: &Path) -> Option<LockInfo> {
        let dwl2 = dwl2_path(drawing_path);
        if let Some(content) = read_lock_file_capped(&dwl2)
            && let Some(info) = parse_dwl2(&content)
        {
            return Some(info);
        }
        let dwl = dwl_path(drawing_path);
        if let Some(content) = read_lock_file_capped(&dwl)
            && let Some(info) = parse_dwl(&content)
        {
            return Some(info);
        }
        None
    }

    fn unknown_lock_info() -> LockInfo {
        LockInfo { user: "Unknown".to_string(), machine: String::new(), date_time: "unknown time".to_string(), drawing_path: None }
    }

    /// Attempt to acquire lock files for `drawing_path` atomically (`create_new`), respecting automation roots.
    pub fn try_acquire_lock(drawing_path: &Path) -> LockResult {
        let dwl = dwl_path(drawing_path);
        let dwl2 = dwl2_path(drawing_path);

        if let Err(e) = crate::file_access::check_write(&dwl) {
            return LockResult::Failed(format!("Lock write refused for {}: {e}", dwl.display()));
        }
        if let Err(e) = crate::file_access::check_write(&dwl2) {
            return LockResult::Failed(format!("Lock write refused for {}: {e}", dwl2.display()));
        }

        let user = current_user();
        let machine = current_machine();
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let dt = chrono::DateTime::from_timestamp(secs, 0);
        let date_text = dt.map(|d| d.format("%Y-%m-%d %H:%M:%S UTC").to_string()).unwrap_or_default();
        let iso_date = dt.map(|d| d.format("%Y-%m-%dT%H:%M:%SZ").to_string()).unwrap_or_default();
        let path_str = drawing_path.to_string_lossy();

        let dwl_content = format!("{user}\r\n{machine}\r\n{date_text}\r\n");
        let esc_user = xml_escape(&user);
        let esc_machine = xml_escape(&machine);
        let esc_path = xml_escape(&path_str);
        let dwl2_content = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\r\n<DrawingLock xmlns=\"dwl.xsd\">\r\n  <UserName>{esc_user}</UserName>\r\n  <MachineName>{esc_machine}</MachineName>\r\n  <DateTime>{iso_date}</DateTime>\r\n  <DrawingPath>{esc_path}</DrawingPath>\r\n</DrawingLock>\r\n"
        );

        use std::io::Write;
        // Atomic create_new on .dwl prevents TOCTOU races and refuses pre-existing symlinks (CWE-59).
        let mut file = match std::fs::OpenOptions::new().write(true).create_new(true).open(&dwl) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return LockResult::AlreadyLocked(read_lock_info(drawing_path).unwrap_or_else(unknown_lock_info));
            }
            Err(e) => return LockResult::Failed(format!("Cannot create {}: {e}", dwl.display())),
        };

        if let Err(e) = file.write_all(dwl_content.as_bytes()) {
            let _ = std::fs::remove_file(&dwl);
            return LockResult::Failed(format!("Cannot write {}: {e}", dwl.display()));
        }
        drop(file);

        // Atomic create_new on .dwl2 prevents symlink overwrite (CWE-59) and clobbering existing .dwl2 locks.
        let mut file2 = match std::fs::OpenOptions::new().write(true).create_new(true).open(&dwl2) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let _ = std::fs::remove_file(&dwl);
                return LockResult::AlreadyLocked(read_lock_info(drawing_path).unwrap_or_else(unknown_lock_info));
            }
            Err(e) => {
                let _ = std::fs::remove_file(&dwl);
                return LockResult::Failed(format!("Cannot create {}: {e}", dwl2.display()));
            }
        };

        if let Err(e) = file2.write_all(dwl2_content.as_bytes()) {
            drop(file2);
            let _ = std::fs::remove_file(&dwl);
            let _ = std::fs::remove_file(&dwl2);
            return LockResult::Failed(format!("Cannot write {}: {e}", dwl2.display()));
        }
        drop(file2);

        LockResult::Acquired(DrawingLockGuard::new(dwl, dwl2))
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::*;

#[cfg(target_arch = "wasm32")]
mod wasm {
    use super::*;

    #[derive(Clone, Debug)]
    pub struct DrawingLockGuard;

    pub enum LockResult {
        Acquired(DrawingLockGuard),
        AlreadyLocked(LockInfo),
        Failed(String),
    }

    pub fn same_path(a: &Path, b: &Path) -> bool {
        a == b
    }

    pub fn has_lock_files(_drawing_path: &Path) -> bool {
        false
    }

    pub fn read_lock_info(_drawing_path: &Path) -> Option<LockInfo> {
        None
    }

    pub fn try_acquire_lock(_drawing_path: &Path) -> LockResult {
        LockResult::Acquired(DrawingLockGuard)
    }
}

#[cfg(target_arch = "wasm32")]
pub use wasm::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dwl_paths_derive_correctly() {
        let p = Path::new("tests/floorplan.dwg");
        assert_eq!(dwl_path(p), PathBuf::from("tests/floorplan.dwl"));
        assert_eq!(dwl2_path(p), PathBuf::from("tests/floorplan.dwl2"));

        let dxf = Path::new("tests/sample.dxf");
        assert_eq!(dwl_path(dxf), PathBuf::from("tests/sample.dwl"));
        assert_eq!(dwl2_path(dxf), PathBuf::from("tests/sample.dwl2"));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn parse_dwl_text() {
        let text = "johndoe\nWORKSTATION-1\nFriday, October 30, 2026  08:15:30 PM\n";
        let info = parse_dwl(text).expect("parsed dwl");
        assert_eq!(info.user, "johndoe");
        assert_eq!(info.machine, "WORKSTATION-1");
        assert_eq!(info.date_time, "Friday, October 30, 2026  08:15:30 PM");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn parse_dwl2_xml_unescapes_entities() {
        let xml = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<DrawingLock xmlns=\"dwl.xsd\">\n  <UserName>alice &amp; bob</UserName>\n  <MachineName>CAD-PC</MachineName>\n  <DateTime>2026-10-30T20:15:00</DateTime>\n  <DrawingPath>C:\\Projects\\site &lt;1&gt;.dwg</DrawingPath>\n</DrawingLock>\n";
        let info = parse_dwl2(xml).expect("parsed dwl2");
        assert_eq!(info.user, "alice & bob");
        assert_eq!(info.machine, "CAD-PC");
        assert_eq!(info.date_time, "2026-10-30T20:15:00");
        assert_eq!(info.drawing_path.as_deref(), Some("C:\\Projects\\site <1>.dwg"));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn acquire_and_release_lock_lifecycle() {
        let unique_dir = std::env::temp_dir().join(format!("cadcraft-dwl-test-{}", std::process::id()));
        std::fs::create_dir_all(&unique_dir).expect("create test dir");
        let fake_drawing = unique_dir.join("dummy.dwg");

        let _ = std::fs::remove_file(dwl_path(&fake_drawing));
        let _ = std::fs::remove_file(dwl2_path(&fake_drawing));

        match try_acquire_lock(&fake_drawing) {
            LockResult::Acquired(guard) => {
                assert!(guard.dwl().is_file(), "dwl should exist");
                assert!(guard.dwl2().is_file(), "dwl2 should exist");

                match try_acquire_lock(&fake_drawing) {
                    LockResult::AlreadyLocked(info) => {
                        assert!(!info.user.is_empty());
                    }
                    _ => panic!("Expected AlreadyLocked"),
                }

                drop(guard);
            }
            _ => panic!("Expected Acquired"),
        }

        assert!(!dwl_path(&fake_drawing).exists(), "dwl should be deleted on drop");
        assert!(!dwl2_path(&fake_drawing).exists(), "dwl2 should be deleted on drop");
        let _ = std::fs::remove_dir_all(&unique_dir);
    }
}

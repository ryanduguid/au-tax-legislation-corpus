//! `pathlib.Path` and `os.path` as CPython 3.15 parses, prints, joins and resolves paths.
//!
//! The radar prints its output paths and names its inputs in messages, so a
//! path is carried in pathlib's parsed form: drive, root and the non-empty,
//! non-`.` parts. Resolution follows `ntpath.realpath` on Windows (lexical
//! `..`, then the final path name of the longest existing prefix) and
//! `posixpath.realpath` elsewhere (symbolic links in order).
use std::path::PathBuf;

#[cfg(windows)]
pub const SEP: &str = "\\";
#[cfg(not(windows))]
pub const SEP: &str = "/";

#[derive(Clone, Debug)]
pub struct PyPath {
    drv: String,
    root: String,
    tail: Vec<String>,
}

/// `os.path.splitroot`.
#[cfg(windows)]
pub fn splitroot(p: &str) -> (&str, &str, &str) {
    let normp = p.replace('/', "\\");
    let n = normp.as_bytes();
    if n.first() == Some(&b'\\') {
        if n.get(1) == Some(&b'\\') {
            let start = if n.len() >= 8 && normp[..8].eq_ignore_ascii_case("\\\\?\\UNC\\") {
                8
            } else {
                2
            };
            let Some(index) = normp[start..].find('\\').map(|i| i + start) else {
                return (p, "", "");
            };
            let Some(index2) = normp[index + 1..].find('\\').map(|i| i + index + 1) else {
                return (p, "", "");
            };
            return (&p[..index2], &p[index2..=index2], &p[index2 + 1..]);
        }
        return ("", &p[..1], &p[1..]);
    }
    if n.get(1) == Some(&b':') && p.is_char_boundary(2) {
        if n.get(2) == Some(&b'\\') {
            return (&p[..2], &p[2..3], &p[3..]);
        }
        return (&p[..2], "", &p[2..]);
    }
    ("", "", p)
}

/// `os.path.splitroot`.
#[cfg(not(windows))]
pub fn splitroot(p: &str) -> (&str, &str, &str) {
    if !p.starts_with('/') {
        ("", "", p)
    } else if !p[1..].starts_with('/') || p[2..].starts_with('/') {
        ("", "/", &p[1..])
    } else {
        ("", &p[..2], &p[2..])
    }
}

fn is_sep(c: char) -> bool {
    c == '/' || cfg!(windows) && c == '\\'
}

/// `os.path.join(a, b)`.
#[cfg(windows)]
pub fn join(a: &str, b: &str) -> String {
    let (mut drive, mut root, path) = splitroot(a);
    let mut path = path.to_string();
    let (p_drive, p_root, p_path) = splitroot(b);
    if !p_root.is_empty() {
        if !p_drive.is_empty() || drive.is_empty() {
            drive = p_drive;
        }
        root = p_root;
        path = p_path.to_string();
    } else {
        let mut relative = true;
        if !p_drive.is_empty() && p_drive != drive {
            if p_drive.to_lowercase() != drive.to_lowercase() {
                drive = p_drive;
                root = p_root;
                path = p_path.to_string();
                relative = false;
            } else {
                drive = p_drive;
            }
        }
        if relative {
            if path.ends_with(|c: char| c != '\\' && c != '/') {
                path.push('\\');
            }
            path.push_str(p_path);
        }
    }
    if !path.is_empty() && root.is_empty() && drive.ends_with(|c: char| !":\\/".contains(c)) {
        return format!("{drive}\\{path}");
    }
    format!("{drive}{root}{path}")
}

/// `os.path.join(a, b)`.
#[cfg(not(windows))]
pub fn join(a: &str, b: &str) -> String {
    if b.starts_with('/') || a.is_empty() {
        b.to_string()
    } else if a.ends_with('/') {
        format!("{a}{b}")
    } else {
        format!("{a}/{b}")
    }
}

/// `os.path.split(p)`.
pub fn split(p: &str) -> (String, String) {
    let (d, r, rest) = splitroot(p);
    let i = rest.rfind(is_sep).map_or(0, |i| i + 1);
    let head = rest[..i].trim_end_matches(is_sep);
    (format!("{d}{r}{head}"), rest[i..].to_string())
}

/// `os.path.isabs(p)`.
pub fn isabs(p: &str) -> bool {
    if cfg!(windows) {
        let start: String = p.chars().take(3).collect::<String>().replace('/', "\\");
        start.get(1..).is_some_and(|s| s.starts_with(":\\")) || start.starts_with("\\\\")
    } else {
        p.starts_with('/')
    }
}

/// `os.path.normpath(p)` (Windows).
#[cfg(windows)]
pub fn normpath(p: &str) -> String {
    let p = p.replace('/', "\\");
    let (drive, root, rest) = splitroot(&p);
    let mut comps: Vec<&str> = Vec::new();
    for comp in rest.split('\\') {
        if comp.is_empty() || comp == "." {
            continue;
        }
        if comp == ".." {
            if comps.last().is_some_and(|c| *c != "..") {
                comps.pop();
                continue;
            }
            if comps.is_empty() && !root.is_empty() {
                continue;
            }
        }
        comps.push(comp);
    }
    if drive.is_empty() && root.is_empty() && comps.is_empty() {
        return ".".into();
    }
    format!("{drive}{root}{}", comps.join("\\"))
}

/// `os.path.normpath(p)` (POSIX): two leading slashes are kept, three or more
/// become one.
#[cfg(not(windows))]
pub fn normpath(p: &str) -> String {
    let leading = p.len() - p.trim_start_matches('/').len();
    let prefix = match leading {
        0 => "",
        2 => "//",
        _ => "/",
    };
    let mut comps: Vec<&str> = Vec::new();
    for comp in p.split('/') {
        if comp.is_empty() || comp == "." {
            continue;
        }
        if comp != ".." || (prefix.is_empty() && comps.is_empty()) || comps.last() == Some(&"..") {
            comps.push(comp);
        } else if !comps.is_empty() {
            comps.pop();
        }
    }
    let path = format!("{prefix}{}", comps.join("/"));
    if path.is_empty() { ".".into() } else { path }
}

impl PyPath {
    /// `Path(text)`.
    pub fn new(text: &str) -> Self {
        if text.is_empty() {
            return PyPath {
                drv: String::new(),
                root: String::new(),
                tail: Vec::new(),
            };
        }
        let text = if cfg!(windows) {
            text.replace('/', "\\")
        } else {
            text.to_string()
        };
        let (drv, mut root, rel) = splitroot(&text);
        let sep = SEP.chars().next().unwrap_or('/');
        let mut root_owned = root.to_string();
        if root.is_empty() && drv.starts_with(sep) && !drv.ends_with(sep) {
            let drv_parts: Vec<&str> = drv.split(sep).collect();
            if drv_parts.len() == 4 && !"?.".contains(drv_parts[2]) || drv_parts.len() == 6 {
                root = SEP;
                root_owned = root.to_string();
            }
        }
        PyPath {
            drv: drv.to_string(),
            root: root_owned,
            tail: rel
                .split(sep)
                .filter(|part| !part.is_empty() && *part != ".")
                .map(str::to_string)
                .collect(),
        }
    }

    /// `str(path)`.
    pub fn display(&self) -> String {
        if !self.drv.is_empty() || !self.root.is_empty() {
            return format!("{}{}{}", self.drv, self.root, self.tail.join(SEP));
        }
        if cfg!(windows)
            && let Some(first) = self.tail.first()
            && !splitroot(first).0.is_empty()
        {
            return format!(".{SEP}{}", self.tail.join(SEP));
        }
        if self.tail.is_empty() {
            return ".".into();
        }
        self.tail.join(SEP)
    }

    /// The path to hand to the operating system.
    pub fn fs(&self) -> PathBuf {
        PathBuf::from(self.display())
    }

    /// `self / other`.
    pub fn join(&self, other: &str) -> Self {
        PyPath::new(&join(&self.display(), other))
    }

    pub fn name(&self) -> &str {
        self.tail.last().map_or("", String::as_str)
    }

    pub fn parent(&self) -> Self {
        let mut out = self.clone();
        out.tail.pop();
        out
    }

    /// `path.with_suffix(suffix)` for a suffix that starts with a dot.
    pub fn with_suffix(&self, suffix: &str) -> Result<Self, String> {
        let name = self.name();
        let stem = match name.rfind('.') {
            Some(i) if !name[..i].trim_start_matches('.').is_empty() => &name[..i],
            _ => name,
        };
        if stem.is_empty() {
            return Err(format!("{} has an empty name", self.repr()));
        }
        let mut out = self.clone();
        if let Some(last) = out.tail.last_mut() {
            *last = format!("{stem}{suffix}");
        }
        Ok(out)
    }

    /// `repr(path)`.
    pub fn repr(&self) -> String {
        let class = if cfg!(windows) {
            "WindowsPath"
        } else {
            "PosixPath"
        };
        format!("{class}({})", crate::py::repr_str(&self.display()))
    }

    pub fn is_absolute(&self) -> bool {
        isabs(&self.display())
    }

    fn normcase(&self) -> String {
        if cfg!(windows) {
            self.display().to_lowercase()
        } else {
            self.display()
        }
    }

    /// `self == other`.
    pub fn same(&self, other: &PyPath) -> bool {
        self.normcase() == other.normcase()
    }

    /// `self.is_relative_to(other)`: `other` is this path or one of its parents.
    pub fn is_relative_to(&self, other: &PyPath) -> bool {
        let mut path = self.clone();
        loop {
            if path.same(other) {
                return true;
            }
            if path.tail.is_empty() {
                return false;
            }
            path.tail.pop();
        }
    }

    /// `Path.cwd()`.
    pub fn cwd() -> Result<Self, crate::pyos::OsError> {
        std::env::current_dir()
            .map(|dir| PyPath::new(&dir.to_string_lossy()))
            .map_err(|error| crate::pyos::OsError::new(&error, None, crate::pyos::Style::Native))
    }

    /// `path.resolve()`.
    pub fn resolve(&self) -> Result<Self, crate::pyos::OsError> {
        realpath(&self.display()).map(|text| PyPath::new(&text))
    }
}

impl std::fmt::Display for PyPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.display())
    }
}

#[cfg(windows)]
fn final_path_name(path: &str) -> Result<String, std::io::Error> {
    std::fs::canonicalize(path).map(|p| p.to_string_lossy().into_owned())
}

#[cfg(windows)]
fn winerror(error: &std::io::Error) -> i32 {
    error.raw_os_error().unwrap_or(0)
}

/// `ntpath._readlink_deep`, enough for broken symbolic links and junctions.
#[cfg(windows)]
fn readlink_deep(path: &str) -> String {
    let mut path = path.to_string();
    let mut seen = std::collections::HashSet::new();
    while seen.insert(path.to_lowercase()) {
        let old = path.clone();
        let Ok(target) = std::fs::read_link(&path) else {
            break;
        };
        path = target.to_string_lossy().into_owned();
        if !isabs(&path) {
            let is_link = std::fs::symlink_metadata(&old).is_ok_and(|m| m.file_type().is_symlink());
            if !is_link {
                path = old;
                break;
            }
            path = normpath(&join(&split(&old).0, &path));
        }
    }
    path
}

/// `ntpath._getfinalpathname_nonstrict`.
///
/// shortcut: CPython asks `FindFirstFileW` for the stored name of a component
/// it may not open (access denied, sharing violation); this keeps the name as
/// given there. Only the letter case of such a component can differ.
#[cfg(windows)]
fn final_path_name_nonstrict(path: &str) -> Result<String, crate::pyos::OsError> {
    const ALLOWED: &[i32] = &[
        1, 2, 3, 5, 21, 32, 50, 53, 65, 67, 87, 123, 161, 1005, 1920, 1921,
    ];
    let mut path = path.to_string();
    let mut tail = String::new();
    while !path.is_empty() {
        match final_path_name(&path) {
            Ok(found) => {
                return Ok(if tail.is_empty() {
                    found
                } else {
                    join(&found, &tail)
                });
            }
            Err(error) => {
                if !ALLOWED.contains(&winerror(&error)) {
                    return Err(crate::pyos::OsError::new(
                        &error,
                        Some(&path),
                        crate::pyos::Style::Native,
                    ));
                }
                let new_path = readlink_deep(&path);
                if new_path != path {
                    return Ok(if tail.is_empty() {
                        new_path
                    } else {
                        join(&new_path, &tail)
                    });
                }
                let (head, name) = split(&path);
                path = head;
                if !path.is_empty() && name.is_empty() {
                    return Ok(format!("{path}{tail}"));
                }
                tail = if tail.is_empty() {
                    name
                } else {
                    join(&name, &tail)
                };
            }
        }
    }
    Ok(tail)
}

/// `os.path.realpath(path)`.
#[cfg(windows)]
pub fn realpath(path: &str) -> Result<String, crate::pyos::OsError> {
    const PREFIX: &str = "\\\\?\\";
    const UNC_PREFIX: &str = "\\\\?\\UNC\\";
    let mut path = normpath(path);
    if path.to_lowercase() == "nul" {
        return Ok("\\\\.\\NUL".into());
    }
    let had_prefix = path.starts_with(PREFIX);
    if !had_prefix && !isabs(&path) {
        let cwd = std::env::current_dir()
            .map_err(|error| crate::pyos::OsError::new(&error, None, crate::pyos::Style::Native))?;
        path = join(&cwd.to_string_lossy(), &path);
    }
    let initial_winerror;
    match final_path_name(&path) {
        Ok(found) => {
            path = found;
            initial_winerror = 0;
        }
        Err(error) => {
            initial_winerror = winerror(&error);
            path = final_path_name_nonstrict(&path)?;
        }
    }
    if !had_prefix && path.starts_with(PREFIX) {
        let spath = match path.strip_prefix(UNC_PREFIX) {
            Some(rest) => format!("\\\\{rest}"),
            None => path[PREFIX.len()..].to_string(),
        };
        match final_path_name(&spath) {
            Ok(found) if found == path => path = spath,
            Ok(_) => {}
            Err(error) if winerror(&error) == initial_winerror => path = spath,
            Err(_) => {}
        }
    }
    Ok(path)
}

/// `os.path.realpath(path)`.
#[cfg(not(windows))]
pub fn realpath(filename: &str) -> Result<String, crate::pyos::OsError> {
    use std::collections::HashMap;
    let mut rest: Vec<Option<String>> = filename
        .trim_end_matches('/')
        .split('/')
        .rev()
        .map(|s| Some(s.to_string()))
        .collect();
    let mut part_count = rest.len();
    let mut path = if filename.starts_with('/') {
        "/".to_string()
    } else {
        std::env::current_dir()
            .map_err(|error| crate::pyos::OsError::new(&error, None, crate::pyos::Style::Native))?
            .to_string_lossy()
            .into_owned()
    };
    let mut seen: HashMap<String, Option<String>> = HashMap::new();
    while part_count > 0 {
        let Some(entry) = rest.pop() else { break };
        let Some(name) = entry else {
            if let Some(Some(link)) = rest.pop() {
                seen.insert(link, Some(path.clone()));
            }
            continue;
        };
        part_count -= 1;
        if name.is_empty() || name == "." {
            continue;
        }
        if name == ".." {
            path = match path.rfind('/') {
                Some(0) | None => "/".into(),
                Some(i) => path[..i].to_string(),
            };
            continue;
        }
        let newpath = if path == "/" {
            format!("/{name}")
        } else {
            format!("{path}/{name}")
        };
        let Ok(meta) = std::fs::symlink_metadata(&newpath) else {
            path = newpath;
            continue;
        };
        if !meta.file_type().is_symlink() {
            path = newpath;
            continue;
        }
        if let Some(cached) = seen.get(&newpath) {
            match cached {
                Some(resolved) => path = resolved.clone(),
                None => path = newpath,
            }
            continue;
        }
        let Ok(target) = std::fs::read_link(&newpath) else {
            path = newpath;
            continue;
        };
        let target = target.to_string_lossy().into_owned();
        if target.starts_with('/') {
            path = "/".into();
        }
        seen.insert(newpath.clone(), None);
        rest.push(Some(newpath));
        rest.push(None);
        let parts: Vec<&str> = target.split('/').collect();
        part_count += parts.len();
        rest.extend(parts.into_iter().rev().map(|s| Some(s.to_string())));
    }
    Ok(path)
}

/// `os.path.normcase(path)`: on Windows lower case with backslashes; elsewhere unchanged.
pub fn normcase(path: &str) -> String {
    if cfg!(windows) {
        path.replace('/', "\\").to_lowercase()
    } else {
        path.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_print_like_pathlib() {
        assert_eq!(PyPath::new("").display(), ".");
        assert_eq!(PyPath::new("./a//b/.").display(), format!("a{SEP}b"));
        assert_eq!(PyPath::new("a/b/").display(), format!("a{SEP}b"));
        assert_eq!(PyPath::new("a").join("b").display(), format!("a{SEP}b"));
        assert_eq!(
            PyPath::new("a/b.json")
                .with_suffix(".md")
                .unwrap()
                .display(),
            format!("a{SEP}b.md")
        );
        assert_eq!(
            PyPath::new(".json").with_suffix(".md").unwrap().display(),
            ".json.md"
        );
        assert_eq!(
            PyPath::new("a.").with_suffix(".md").unwrap().display(),
            "a.md"
        );
        assert!(PyPath::new("/").with_suffix(".md").is_err());
        assert!(PyPath::new("a/b/c").is_relative_to(&PyPath::new("a")));
        assert!(!PyPath::new("ab/c").is_relative_to(&PyPath::new("a")));
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_follow_ntpath() {
        assert_eq!(
            PyPath::new("//server/share/x").display(),
            "\\\\server\\share\\x"
        );
        assert_eq!(
            PyPath::new("//server/share").display(),
            "\\\\server\\share\\"
        );
        assert_eq!(PyPath::new("C:a/b").display(), "C:a\\b");
        assert_eq!(PyPath::new("./c:x").display(), ".\\c:x");
        assert_eq!(join("C:\\x", "\\y"), "C:\\y");
        assert_eq!(join("C:\\x", "D:y"), "D:y");
        assert_eq!(join("C:\\x", "c:y"), "c:\\x\\y");
        assert_eq!(normpath("C:a\\..\\..\\b"), "C:..\\b");
        assert_eq!(normpath("\\\\server\\share\\.."), "\\\\server\\share\\");
        assert!(PyPath::new("A\\B").same(&PyPath::new("a/b")));
        assert_eq!(realpath("nul").unwrap(), "\\\\.\\NUL");
    }
}

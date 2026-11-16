//! ConfigVault core: snapshot creation, restore with dry-run, import/export.
//!
//! Archive layout (.tar.gz):
//!   manifest.json          - full manifest (targets are absolute paths
//!                            recorded at backup time; re-validated on restore)
//!   data/<source_idx>/<relative path of file within source>
//!
//! Safety rules enforced everywhere:
//!   - no `..` components, no NUL bytes, paths must be absolute
//!   - symlinks in walked trees are skipped (never followed)
//!   - per-file (256 MiB), total (4 GiB), entry-count (20 000) caps
//!   - restore always creates a pre-restore backup first unless dry_run
//!   - every entry is sha256-verified after being written on restore

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const MAX_FILES: usize = 20_000;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ManifestEntry {
    /// Absolute target path captured at backup time.
    pub target: String,
    /// Path inside the archive, e.g. "data/0/sub/dir/file.conf".
    pub archive: String,
    pub size: u64,
    pub sha256: String,
    #[serde(default)]
    pub mode: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Manifest {
    pub format: u32,
    pub id: String,
    pub name: String,
    pub created_at: u64,
    pub sources: Vec<String>,
    pub excludes: Vec<String>,
    pub entries: Vec<ManifestEntry>,
    pub total_bytes: u64,
}

/// Small sidecar persisted next to the archive so listing backups does not
/// require opening every tar.gz.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct BackupMeta {
    pub id: String,
    pub name: String,
    pub created_at: u64,
    pub sources: Vec<String>,
    pub total_bytes: u64,
    pub file_count: usize,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RestoreAction {
    Create { target: String, size: u64 },
    Overwrite { target: String, size: u64 },
    Skip { target: String, reason: String },
}

#[derive(Serialize, Clone, Debug)]
pub struct RestoreReport {
    pub dry_run: bool,
    pub actions: Vec<RestoreAction>,
    pub errors: Vec<String>,
    pub verified: usize,
    pub failed_verify: Vec<String>,
    pub backup_id: Option<String>,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn new_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    format!("{:x}-{:x}", nanos, pid)
}

/// Reject anything that could escape its root or confuse the archive:
/// relative paths, `..` components, NUL bytes. Returns the PathBuf on success.
pub fn sanitize_target_path(raw: &str) -> Result<PathBuf, String> {
    if raw.contains('\0') {
        return Err("path contains NUL byte".into());
    }
    let p = Path::new(raw);
    if !p.is_absolute() {
        return Err(format!("path is not absolute: {raw}"));
    }
    for c in p.components() {
        match c {
            Component::ParentDir => return Err(format!("path contains '..': {raw}")),
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {}
            Component::CurDir => {}
        }
    }
    Ok(p.to_path_buf())
}

/// Exclude matcher. Supported pattern shapes (documented in README):
///   "name"     - matches a path component exactly (e.g. "node_modules")
///   "*.ext"    - matches file names ending in .ext (e.g. "*.log")
///   "/abs/dir" - matches any path under that absolute prefix
///   "*"        - matches everything
pub fn match_exclude(pattern: &str, rel_path: &Path, abs_root: &Path) -> bool {
    let pat = pattern.trim();
    if pat.is_empty() {
        return false;
    }
    if pat == "*" {
        return true;
    }
    if let Some(ext) = pat.strip_prefix("*.") {
        return rel_path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.to_ascii_lowercase().ends_with(&format!(".{}", ext.to_ascii_lowercase())))
            .unwrap_or(false);
    }
    if let Some(prefix) = pat.strip_prefix('/') {
        let prefix = PathBuf::from(format!("/{prefix}"));
        let abs = abs_root.join(rel_path);
        return abs.starts_with(&prefix);
    }
    rel_path
        .components()
        .any(|c| matches!(c, Component::Normal(n) if n == std::ffi::OsStr::new(pat)))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

fn unix_mode(meta: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o7777
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        0o644
    }
}

struct WalkItem {
    abs: PathBuf,
    rel: PathBuf,
    source_idx: usize,
    size: u64,
    mode: u32,
}

fn walk_source(source: &Path, source_idx: usize, excludes: &[String], out: &mut Vec<WalkItem>) -> Result<(), String> {
    let meta = fs::symlink_metadata(source).map_err(|e| format!("cannot read {}: {e}", source.display()))?;
    if meta.file_type().is_symlink() {
        return Ok(()); // never follow symlinks
    }
    if meta.is_file() {
        let size = meta.len();
        if size > MAX_FILE_BYTES {
            return Err(format!("{} exceeds per-file limit", source.display()));
        }
        out.push(WalkItem { abs: source.to_path_buf(), rel: PathBuf::from(source.file_name().unwrap_or_default()), source_idx, size, mode: unix_mode(&meta) });
        return Ok(());
    }
    if !meta.is_dir() {
        return Ok(());
    }
    let root = source.to_path_buf();
    let mut stack = vec![(root.clone(), PathBuf::new(), 0usize)];
    while let Some((dir, rel, depth)) = stack.pop() {
        if depth > 32 {
            continue;
        }
        let rd = match fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for entry in rd.flatten() {
            let name = entry.file_name();
            let child_rel = rel.join(&name);
            let ft = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if ft.is_symlink() {
                continue;
            }
            if excludes.iter().any(|p| match_exclude(p, &child_rel, &root)) {
                continue;
            }
            let abs = entry.path();
            if ft.is_dir() {
                stack.push((abs, child_rel, depth + 1));
            } else if ft.is_file() {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                if size > MAX_FILE_BYTES {
                    continue; // silently skip oversize files in trees
                }
                out.push(WalkItem { abs, rel: child_rel, source_idx, size, mode: entry.metadata().map(|m| unix_mode(&m)).unwrap_or(0o644) });
            }
        }
    }
    Ok(())
}

pub fn create_backup(dir: &Path, name: &str, sources: &[String], excludes: &[String]) -> Result<BackupMeta, String> {
    if sources.is_empty() {
        return Err("no sources selected".into());
    }
    let mut items = Vec::new();
    for (i, s) in sources.iter().enumerate() {
        let p = sanitize_target_path(s)?;
        if !p.exists() {
            return Err(format!("source does not exist: {s}"));
        }
        walk_source(&p, i, excludes, &mut items)?;
        if items.len() > MAX_FILES {
            return Err(format!("too many files (>{MAX_FILES})"));
        }
    }
    let total: u64 = items.iter().map(|i| i.size).sum();
    if total > MAX_TOTAL_BYTES {
        return Err("sources exceed 4 GiB total".into());
    }

    let id = new_id();
    let backups = dir.join("backups");
    fs::create_dir_all(&backups).map_err(|e| format!("cannot create backups dir: {e}"))?;

    let tmp = backups.join(format!(".{id}.tar.gz.tmp"));
    let file = fs::File::create(&tmp).map_err(|e| format!("cannot create archive: {e}"))?;
    let enc = GzEncoder::new(file, Compression::default());
    let mut builder = tar::Builder::new(enc);
    builder.follow_symlinks(false);

    let mut entries = Vec::with_capacity(items.len());
    let mut total_written = 0u64;
    for item in &items {
        let rel_archive = PathBuf::from("data")
            .join(item.source_idx.to_string())
            .join(&item.rel);
        let archive_name = rel_archive.to_string_lossy().replace('\\', "/");
        let bytes = fs::read(&item.abs).map_err(|e| format!("cannot read {}: {e}", item.abs.display()))?;
        let sha = sha256_hex(&bytes);
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(item.mode);
        header.set_mtime(now_secs());
        header.set_cksum();
        builder
            .append_data(&mut header, &archive_name, bytes.as_slice())
            .map_err(|e| format!("archive write failed: {e}"))?;
        total_written += bytes.len() as u64;
        entries.push(ManifestEntry { target: item.abs.to_string_lossy().into_owned(), archive: archive_name, size: bytes.len() as u64, sha256: sha, mode: item.mode });
    }

    let manifest = Manifest {
        format: 1,
        id: id.clone(),
        name: if name.trim().is_empty() { format!("Backup {}", id) } else { name.trim().to_string() },
        created_at: now_secs(),
        sources: sources.to_vec(),
        excludes: excludes.to_vec(),
        entries,
        total_bytes: total_written,
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    let mut header = tar::Header::new_gnu();
    header.set_size(manifest_bytes.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(now_secs());
    header.set_cksum();
    builder
        .append_data(&mut header, "manifest.json", manifest_bytes.as_slice())
        .map_err(|e| e.to_string())?;

    let enc = builder.into_inner().map_err(|e| e.to_string())?;
    enc.finish().map_err(|e| e.to_string())?;

    let archive = backups.join(format!("{id}.tar.gz"));
    fs::rename(&tmp, &archive).map_err(|e| format!("cannot finalize archive: {e}"))?;

    let meta = BackupMeta { id: id.clone(), name: manifest.name.clone(), created_at: manifest.created_at, sources: sources.to_vec(), total_bytes: total_written, file_count: manifest.entries.len() };
    write_sidecar(&backups, &meta)?;
    Ok(meta)
}

fn sidecar_path(backups: &Path, id: &str) -> PathBuf {
    backups.join(format!("{id}.json"))
}

fn write_sidecar(backups: &Path, meta: &BackupMeta) -> Result<(), String> {
    let tmp = backups.join(format!(".{}.json.tmp", meta.id));
    fs::write(&tmp, serde_json::to_vec_pretty(meta).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    fs::rename(&tmp, sidecar_path(backups, &meta.id)).map_err(|e| e.to_string())
}

pub fn list_backups(dir: &Path) -> Vec<BackupMeta> {
    let backups = dir.join("backups");
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(&backups) {
        for entry in rd.flatten() {
            if entry.path().extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            if let Ok(text) = fs::read_to_string(entry.path()) {
                if let Ok(meta) = serde_json::from_str::<BackupMeta>(&text) {
                    if backups.join(format!("{}.tar.gz", meta.id)).exists() {
                        out.push(meta);
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    out
}

fn load_archive_manifest(archive: &Path) -> Result<Manifest, String> {
    let file = fs::File::open(archive).map_err(|e| format!("cannot open backup: {e}"))?;
    let dec = GzDecoder::new(file);
    let mut ar = tar::Archive::new(dec);
    let entries = ar.entries().map_err(|e| format!("invalid archive: {e}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("invalid archive entry: {e}"))?;
        if entry.path().map(|p| p == Path::new("manifest.json")).unwrap_or(false) {
            let mut text = String::new();
            entry.read_to_string(&mut text).map_err(|e| format!("cannot read manifest: {e}"))?;
            let m: Manifest = serde_json::from_str(&text).map_err(|e| format!("invalid manifest: {e}"))?;
            if m.format != 1 {
                return Err(format!("unsupported manifest format {}", m.format));
            }
            return Ok(m);
        }
    }
    Err("manifest.json missing from archive".into())
}

pub fn backup_entry_count(dir: &Path, id: &str) -> Option<Manifest> {
    let archive = dir.join("backups").join(format!("{id}.tar.gz"));
    load_archive_manifest(&archive).ok()
}

pub fn delete_backup(dir: &Path, id: &str) -> Result<(), String> {
    let backups = dir.join("backups");
    let archive = backups.join(format!("{id}.tar.gz"));
    let sidecar = sidecar_path(&backups, id);
    if !archive.exists() && !sidecar.exists() {
        return Err("backup not found".into());
    }
    if archive.exists() {
        fs::remove_file(&archive).map_err(|e| e.to_string())?;
    }
    if sidecar.exists() {
        fs::remove_file(&sidecar).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn set_mode(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o7777));
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
}

/// Restore a backup. With dry_run=true nothing is written; a preview report
/// is returned instead (pre-restore backup is also not created).
pub fn restore_backup(dir: &Path, id: &str, dry_run: bool) -> Result<RestoreReport, String> {
    let backups = dir.join("backups");
    let archive = backups.join(format!("{id}.tar.gz"));
    if !archive.exists() {
        return Err("backup not found".into());
    }
    let manifest = load_archive_manifest(&archive)?;

    // Validate targets before touching anything.
    let mut validated = Vec::with_capacity(manifest.entries.len());
    let mut errors = Vec::new();
    for e in &manifest.entries {
        match sanitize_target_path(&e.target) {
            Ok(p) => validated.push((e.clone(), p)),
            Err(msg) => errors.push(format!("{}: {msg}", e.target)),
        }
    }

    let mut actions = Vec::new();
    for (e, target) in &validated {
        if target.exists() {
            actions.push(RestoreAction::Overwrite { target: target.to_string_lossy().into_owned(), size: e.size });
        } else {
            actions.push(RestoreAction::Create { target: target.to_string_lossy().into_owned(), size: e.size });
        }
    }

    if dry_run {
        return Ok(RestoreReport { dry_run: true, actions, errors, verified: 0, failed_verify: Vec::new(), backup_id: None });
    }

    // Pre-restore backup of every existing target (best effort; failure aborts).
    let existing: Vec<String> = validated
        .iter()
        .filter(|(_, p)| p.exists())
        .map(|(_, p)| p.to_string_lossy().into_owned())
        .collect();
    let pre_id = if existing.is_empty() {
        None
    } else {
        Some(create_backup(dir, &format!("Pre-restore backup {}", manifest.name), &existing, &[]).map_err(|e| format!("pre-restore backup failed: {e}"))?.id)
    };

    // Extract + write entries.
    let file = fs::File::open(&archive).map_err(|e| e.to_string())?;
    let dec = GzDecoder::new(file);
    let mut ar = tar::Archive::new(dec);
    let mut files: BTreeMap<String, ManifestEntry> = BTreeMap::new();
    for e in &manifest.entries {
        files.insert(e.archive.clone(), e.clone());
    }
    let mut verified = 0usize;
    let mut failed_verify = Vec::new();
    for entry in ar.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let archive_name = entry.path().map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_default();
        let Some(meta) = files.get(&archive_name) else { continue };
        let target = match sanitize_target_path(&meta.target) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let mut bytes = Vec::with_capacity(meta.size.min(MAX_FILE_BYTES) as usize);
        entry.read_to_end(&mut bytes).map_err(|e| format!("cannot read {archive_name}: {e}"))?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        fs::write(&target, &bytes).map_err(|e| format!("cannot write {}: {e}", target.display()))?;
        set_mode(&target, meta.mode);
        // Verify after write.
        match fs::read(&target) {
            Ok(written) if sha256_hex(&written) == meta.sha256 => verified += 1,
            _ => failed_verify.push(target.to_string_lossy().into_owned()),
        }
    }

    Ok(RestoreReport { dry_run: false, actions, errors, verified, failed_verify, backup_id: pre_id })
}

/// Copy an archive to `dest` (export) after basic validation.
pub fn export_backup(dir: &Path, id: &str, dest: &str) -> Result<(), String> {
    let archive = dir.join("backups").join(format!("{id}.tar.gz"));
    if !archive.exists() {
        return Err("backup not found".into());
    }
    let _ = load_archive_manifest(&archive)?; // validate before exporting
    let dest_p = sanitize_target_path(dest)?;
    if let Some(parent) = dest_p.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::copy(&archive, &dest_p).map_err(|e| format!("cannot export: {e}"))?;
    Ok(())
}

/// Import a user-selected .tar.gz into the backup store under a new id.
pub fn import_backup(dir: &Path, src: &str) -> Result<BackupMeta, String> {
    let src_p = sanitize_target_path(src)?;
    if !src_p.exists() {
        return Err("file not found".into());
    }
    let manifest = load_archive_manifest(&src_p)?;
    if manifest.entries.len() > MAX_FILES {
        return Err("archive has too many entries".into());
    }
    let total: u64 = manifest.entries.iter().map(|e| e.size).sum();
    if total > MAX_TOTAL_BYTES {
        return Err("archive exceeds 4 GiB".into());
    }
    let id = new_id();
    let backups = dir.join("backups");
    fs::create_dir_all(&backups).map_err(|e| e.to_string())?;
    let dest = backups.join(format!("{id}.tar.gz"));
    fs::copy(&src_p, &dest).map_err(|e| format!("cannot import: {e}"))?;
    let meta = BackupMeta {
        id: id.clone(),
        name: if manifest.name.trim().is_empty() { format!("Imported {id}") } else { manifest.name.clone() },
        created_at: manifest.created_at,
        sources: manifest.sources.clone(),
        total_bytes: manifest.total_bytes,
        file_count: manifest.entries.len(),
    };
    write_sidecar(&backups, &meta)?;
    Ok(meta)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("configvault-test-{}-{}", std::process::id(), tag));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_tree(root: &Path, files: &[(&str, &str)]) {
        for (rel, contents) in files {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, contents).unwrap();
        }
    }

    #[test]
    fn sanitize_rejects_traversal_and_relative() {
        assert!(sanitize_target_path("/tmp/ok.conf").is_ok());
        assert!(sanitize_target_path("relative/path").is_err());
        assert!(sanitize_target_path("/tmp/../etc/passwd").is_err());
        assert!(sanitize_target_path("/tmp/\0evil").is_err());
    }

    #[test]
    fn exclude_matcher_shapes() {
        let root = Path::new("/home/u/proj");
        assert!(match_exclude("node_modules", Path::new("node_modules/a/index.js"), root));
        assert!(match_exclude("*.log", Path::new("logs/app.log"), root));
        assert!(match_exclude("*.LOG", Path::new("logs/app.LOG"), root));
        assert!(!match_exclude("*.log", Path::new("logs/app.txt"), root));
        assert!(match_exclude("/home/u/proj/build", Path::new("build/out.bin"), root));
        assert!(!match_exclude("/home/u/other", Path::new("build/out.bin"), root));
        assert!(match_exclude("*", Path::new("anything"), root));
        assert!(!match_exclude("", Path::new("anything"), root));
    }

    #[test]
    fn backup_restore_roundtrip() {
        let base = tmp_dir("roundtrip");
        let src = base.join("src");
        let dst = base.join("dst");
        fs::create_dir_all(&src).unwrap();
        write_tree(&src, &[(".bashrc", "export A=1\n"), (".config/app/settings.json", "{\"theme\":\"dark\"}")]);
        let backups_root = base.join("store");
        let meta = create_backup(&backups_root, "test", &[src.to_string_lossy().into_owned()], &[]).unwrap();
        assert_eq!(meta.file_count, 2);
        // Simulate damage: delete the source tree, restore into nothing.
        fs::remove_dir_all(&src).unwrap();
        let report = restore_backup(&backups_root, &meta.id, false).unwrap();
        assert_eq!(report.verified, 2);
        assert!(report.failed_verify.is_empty());
        assert!(report.backup_id.is_none()); // nothing existed -> no pre-restore backup
        assert_eq!(fs::read_to_string(src.join(".bashrc")).unwrap(), "export A=1\n");
        // Dry-run on an existing tree reports overwrite but writes nothing.
        let dry = restore_backup(&backups_root, &meta.id, true).unwrap();
        assert!(dry.dry_run);
        assert!(dry.actions.iter().all(|a| matches!(a, RestoreAction::Overwrite { .. })));
        let _ = fs::remove_dir_all(&base);
        let _ = dst;
    }

    #[test]
    fn restore_creates_pre_restore_backup() {
        let base = tmp_dir("prebackup");
        let src = base.join("src");
        fs::create_dir_all(&src).unwrap();
        write_tree(&src, &[("keep.conf", "original")]);
        let store = base.join("store");
        let meta = create_backup(&store, "first", &[src.to_string_lossy().into_owned()], &[]).unwrap();
        fs::write(src.join("keep.conf"), "modified-by-user").unwrap();
        let report = restore_backup(&store, &meta.id, false).unwrap();
        assert!(report.backup_id.is_some(), "pre-restore backup must be created");
        assert_eq!(fs::read_to_string(src.join("keep.conf")).unwrap(), "original");
        let pre = report.backup_id.unwrap();
        let listed = list_backups(&store);
        assert!(listed.iter().any(|m| m.id == pre));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn excludes_are_applied() {
        let base = tmp_dir("excludes");
        let src = base.join("proj");
        fs::create_dir_all(&src).unwrap();
        write_tree(&src, &[(".git/config", "git"), ("src/main.rs", "fn main(){}"), ("build.log", "noise")]);
        let store = base.join("store");
        let meta = create_backup(&store, "ex", &[src.to_string_lossy().into_owned()], &[".git".to_string(), "*.log".to_string()]).unwrap();
        assert_eq!(meta.file_count, 1);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn import_rejects_traversal_manifest() {
        let base = tmp_dir("import");
        let evil = base.join("evil.tar.gz");
        // Craft an archive whose manifest contains a traversal target.
        let file = fs::File::create(&evil).unwrap();
        let enc = GzEncoder::new(file, Compression::default());
        let mut builder = tar::Builder::new(enc);
        let m = Manifest {
            format: 1,
            id: "evil".into(),
            name: "evil".into(),
            created_at: 0,
            sources: vec![],
            excludes: vec![],
            entries: vec![ManifestEntry { target: "/tmp/../etc/passwd".into(), archive: "data/0/x".into(), size: 4, sha256: "00".into(), mode: 0o644 }],
            total_bytes: 4,
        };
        let mb = serde_json::to_vec(&m).unwrap();
        let mut header = tar::Header::new_gnu();
        header.set_size(mb.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_cksum();
        builder.append_data(&mut header, "manifest.json", mb.as_slice()).unwrap();
        builder.into_inner().unwrap().finish().unwrap();
        // Import itself succeeds (manifest parses), but restore must refuse entries.
        let store = base.join("store");
        let meta = import_backup(&store, evil.to_str().unwrap()).unwrap();
        let report = restore_backup(&store, &meta.id, false).unwrap();
        assert!(!report.errors.is_empty(), "traversal target must be reported as error");
        assert!(report.actions.is_empty());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn manifest_roundtrip() {
        let m = Manifest {
            format: 1, id: "i".into(), name: "n".into(), created_at: 42,
            sources: vec!["/a".into()], excludes: vec!["*.log".into()],
            entries: vec![ManifestEntry { target: "/a/f".into(), archive: "data/0/f".into(), size: 3, sha256: "abc".into(), mode: 0o600 }],
            total_bytes: 3,
        };
        let text = serde_json::to_string(&m).unwrap();
        let back: Manifest = serde_json::from_str(&text).unwrap();
        assert_eq!(back.entries[0].sha256, "abc");
        assert_eq!(back.format, 1);
    }
}

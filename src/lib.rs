//! Client versions of FINAL FANTASY XI, kept whole.
//!
//! FFXiMain.dll decides how the DATs are read, so a version is the DLLs and the DATs together: a
//! snapshot records every file of an install (path, size, SHA-256) as one manifest, and the files
//! themselves go in a content-addressed store, once each however many versions share them. From
//! there a version can be diffed against another, packed as a delta (only what changed),
//! published as plain static files for a server to hand out, fetched by the launcher, checked
//! against an install, and put back together as an install of its own.
//!
//! The vault:
//!   objects/ab/<sha256>        each file's bytes, by hash
//!   versions/<version>.json    a manifest (Manifest)
//!
//! A published site (publish; any static file server, or `xi-vault serve`):
//!   index.json                 Index: the versions, the one the server wants, the packs
//!   versions/<version>.json    their manifests
//!   objects/ab/<sha256>.zst    each file, zstd-compressed
//!   packs/<from>..<to>.tar.zst deltas, one download each (optional)
//!
//! This holds Square Enix's files. It never ships with the launcher: a vault is made from the
//! player's own install, and a site is whatever its operator chooses to host.

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const FORMAT: &str = "xi-vault/1";

/// Folders of an install that are the player's, not the version's: USER (settings, macros), TEMP, and
/// SYS, which the game writes while it runs (error and info logs, login bookmarks, the user file).
const SKIP_DIRS: &[&str] = &["USER", "TEMP", "SYS"];

fn players(path: &str) -> bool {
    let top = path.split('/').next().unwrap_or("");
    path.contains('/') && SKIP_DIRS.iter().any(|d| d.eq_ignore_ascii_case(top))
}
const SKIP_FILES: &[&str] = &[".DS_Store", "Thumbs.db", "desktop.ini"];

/// The builds the ffxi-native launcher can run (known-builds.json): FFXiMain.dll's hash -> build label, version.
const BUILDS_JSON: &str = include_str!("../known-builds.json");

pub type Result<T> = std::result::Result<T, String>;

fn err<E: std::fmt::Display>(what: impl std::fmt::Display) -> impl FnOnce(E) -> String {
    move |e| format!("{what}: {e}")
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// relative to the FINAL FANTASY XI folder, with '/'
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Manifest {
    pub format: String,
    /// The client version the lobby sees ("30260805_0"), or a name given to it.
    pub version: String,
    /// The launcher's build label (known-builds.json), empty when it does not know this build.
    pub build: String,
    pub ffximain_sha256: String,
    pub ffxi_sha256: String,
    /// seconds since 1970
    pub created: u64,
    /// sorted by path
    pub files: Vec<Entry>,
}

/// A manifest's path is a plain relative path inside the game folder: '/'-separated names, none
/// empty, "." or "..", and none with '\\' or ':' (path syntax on Windows). A manifest from a site
/// is untrusted: a path that climbs out would write anywhere.
pub fn plain_path(p: &str) -> bool {
    !p.is_empty() && p.len() <= 260 && p.split('/').all(|c| !c.is_empty() && c != "." && c != ".." && !c.contains(['\\', ':', '\0']))
}

impl Manifest {
    /// Refuses a manifest with a path that is not plain_path, or a hash that is not one.
    pub fn check(&self) -> Result<()> {
        for e in &self.files {
            if !plain_path(&e.path) {
                return Err(format!("version {}: a file path that is not inside the game folder: {:?}", self.version, e.path));
            }
            if e.sha256.len() != 64 || !e.sha256.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
                return Err(format!("version {}: {} has no proper SHA-256", self.version, e.path));
            }
        }
        Ok(())
    }

    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|e| e.size).sum()
    }

    /// What the version is, whatever it is called: SHA-256 over its sorted "path NUL sha256 LF"
    /// lines. Two servers can name their own customised versions alike; this tells them apart.
    pub fn digest(&self) -> String {
        let mut lines: Vec<(&str, &str)> = self.files.iter().map(|e| (e.path.as_str(), e.sha256.as_str())).collect();
        lines.sort();
        let mut h = Sha256::new();
        for (p, sha) in lines {
            h.update(p.as_bytes());
            h.update([0]);
            h.update(sha.as_bytes());
            h.update([b'\n']);
        }
        hex(&h.finalize())
    }
    pub fn by_path(&self) -> BTreeMap<&str, &Entry> {
        self.files.iter().map(|e| (e.path.as_str(), e)).collect()
    }
}

/// What a server publishes (index.json).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Index {
    pub format: String,
    /// The version the server's lobby wants.
    pub current: String,
    pub versions: Vec<IndexVersion>,
    pub packs: Vec<IndexPack>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct IndexVersion {
    pub version: String,
    pub build: String,
    pub files: usize,
    pub bytes: u64,
    /// Published with --since: only the files this version does not share with `base` are
    /// hosted; a player brings the rest from an install of `base` (their own). Empty: all hosted.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub base: String,
    /// Manifest::digest: what the version is, so a player's copy of another server's version of the
    /// same name is never taken for it. Empty from sites made before it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub digest: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct IndexPack {
    pub from: String,
    pub to: String,
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// Progress: (what, done, total). Bytes where there are bytes, else files.
pub type Progress<'a> = &'a (dyn Fn(&str, u64, u64) + Sync);

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = BufReader::with_capacity(1 << 20, File::open(path).map_err(err(path.display()))?);
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(err(path.display()))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

/// The build and version for FFXiMain.dll's hash, from known-builds.json.
pub fn known_build(ffximain_sha: &str) -> Option<(String, String)> {
    let v: serde_json::Value = serde_json::from_str(BUILDS_JSON).ok()?;
    for (label, b) in v["builds"].as_object()? {
        if b["FFXiMain.dll"]["sha256"].as_str() == Some(ffximain_sha) {
            return Some((label.clone(), b["version"].as_str().unwrap_or_default().to_string()));
        }
    }
    None
}

/// A program file (Windows runs it, or loads it into the game): .dll, .exe and the like.
pub fn is_program(path: &str) -> bool {
    let low = path.to_ascii_lowercase();
    [".dll", ".exe", ".com", ".scr", ".cpl", ".ocx", ".drv", ".bat", ".cmd", ".ps1", ".vbs", ".js", ".msi"].iter().any(|x| low.ends_with(x))
}

/// Whether a program file's content is one Square Enix shipped: a program of a retail version in
/// known-builds.json. The updater installs no other program: a server can customise data, not code.
pub fn known_program(sha: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(BUILDS_JSON) else { return false };
    v["builds"].as_object().is_some_and(|b| {
        b.values().any(|build| build["programs"].as_object().is_some_and(|p| p.values().any(|h| h.as_str() == Some(sha))))
    })
}

// --- the install ---------------------------------------------------------------------------------

/// Every file of the install that belongs to the version, relative, sorted.
pub fn install_files(game: &Path) -> Result<Vec<(String, u64)>> {
    fn walk(root: &Path, rel: &str, out: &mut Vec<(String, u64)>) -> Result<()> {
        let dir = if rel.is_empty() { root.to_path_buf() } else { root.join(rel) };
        for e in fs::read_dir(&dir).map_err(err(dir.display()))? {
            let e = e.map_err(err(dir.display()))?;
            let name = e.file_name().to_string_lossy().into_owned();
            let path = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            let ft = e.file_type().map_err(err(&path))?;
            if ft.is_dir() {
                if !(rel.is_empty() && SKIP_DIRS.iter().any(|d| d.eq_ignore_ascii_case(&name))) {
                    walk(root, &path, out)?;
                }
            } else if ft.is_file() && !SKIP_FILES.contains(&name.as_str()) {
                out.push((path, e.metadata().map_err(err(&name))?.len()));
            }
        }
        Ok(())
    }
    if !game.join("FFXiMain.dll").is_file() {
        return Err(format!("{}: no FFXiMain.dll; this is not a FINAL FANTASY XI folder", game.display()));
    }
    let mut out = Vec::new();
    walk(game, "", &mut out)?;
    out.sort();
    Ok(out)
}

/// The client version an install was last patched to: the newest one PlayOnline's patch.cfg names.
/// Each of its entries is a file and the versions it went through ("30260805_0 <size> ..."), so an
/// update that changes only DATs still shows, where FFXiMain.dll's build would not.
pub fn patched_version(game: &Path) -> Option<String> {
    let data = fs::read(game.join("patch.cfg")).ok()?;
    let key = |v: &str| {
        let (date, n) = v.split_once('_')?;
        Some((date.parse::<u64>().ok()?, n.parse::<u64>().ok()?))
    };
    data.split(|&b| b == b'\n')
        .filter_map(|line| std::str::from_utf8(line.split(|&b| b == b' ').next()?).ok())
        .filter(|v| v.len() >= 10 && v.as_bytes()[8] == b'_' && key(v).is_some())
        .max_by_key(|v| key(v))
        .map(str::to_string)
}

/// Each file patch.cfg lists, and the version it is at now (the last it names for it); lowercase
/// paths.
fn patch_history(game: &Path) -> Option<BTreeMap<String, String>> {
    let text = String::from_utf8_lossy(&fs::read(game.join("patch.cfg")).ok()?).into_owned();
    let mut out = BTreeMap::new();
    let mut file: Option<String> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("file ") {
            file = rest.strip_suffix(" {").map(|p| p.trim().to_ascii_lowercase());
        } else if line.starts_with('}') {
            file = None;
        } else if let (Some(f), Some(v)) = (&file, line.split(' ').next()) {
            if v.len() >= 10 && v.as_bytes()[8] == b'_' && v[..8].bytes().all(|b| b.is_ascii_digit()) {
                out.insert(f.clone(), v.to_string());
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

/// What a fresh install from Square Enix's installer has that version `m` still uses: the files
/// patch.cfg says are at the installer's version (the one most files are still at, e.g. 30210706_0,
/// as the 2021 installer left them) and the files PlayOnline never patches. Named
/// "installer-<version>". Published as a version's base, a site hosts everything else, so a
/// player with nothing but a fresh install gets the whole version from it. `game` is an install of
/// `m` (its patch.cfg says which files the updates changed).
pub fn installer_base(game: &Path, m: &Manifest) -> Option<Manifest> {
    let history = patch_history(game)?;
    let mut count: BTreeMap<&str, usize> = BTreeMap::new();
    for v in history.values() {
        *count.entry(v.as_str()).or_default() += 1;
    }
    let installer = count.iter().max_by_key(|(_, n)| **n)?.0.to_string();
    let files: Vec<Entry> = m
        .files
        .iter()
        .filter(|e| {
            let low = e.path.to_ascii_lowercase();
            // PlayOnline's own records change with every update
            !low.starts_with("patch") && history.get(&low).map(|v| *v == installer).unwrap_or(true)
        })
        .cloned()
        .collect();
    Some(Manifest {
        format: FORMAT.into(),
        version: format!("installer-{installer}"),
        build: String::new(),
        ffximain_sha256: String::new(),
        ffxi_sha256: String::new(),
        created: m.created,
        files,
    })
}

/// Every file of an install hashed into a manifest (and put in `store` when given). Its version is
/// the one patch.cfg names, else the one known-builds.json gives its FFXiMain.dll, else unknown-<hash>.
pub fn hash_install(game: &Path, store: Option<&Vault>, what: &str, progress: Progress) -> Result<Manifest> {
    let list = install_files(game)?;
    let total: u64 = list.iter().map(|(_, s)| s).sum();
    let done = AtomicU64::new(0);
    let files = list
        .par_iter()
        .map(|(rel, size)| {
            let src = game.join(rel);
            let sha = sha256_file(&src)?;
            if let Some(v) = store {
                v.put_file(&src, &sha)?;
            }
            progress(what, done.fetch_add(*size, Ordering::Relaxed) + size, total);
            Ok(Entry { path: rel.clone(), size: *size, sha256: sha })
        })
        .collect::<Result<Vec<Entry>>>()?;
    let sha_of = |p: &str| files.iter().find(|e| e.path.eq_ignore_ascii_case(p)).map(|e| e.sha256.clone()).unwrap_or_default();
    let (main, ffxi) = (sha_of("FFXiMain.dll"), sha_of("FFXi.dll"));
    let (build, known) = known_build(&main).unwrap_or_default();
    let version = patched_version(game).or((!known.is_empty()).then_some(known)).unwrap_or_else(|| format!("unknown-{}", &main[..12]));
    Ok(Manifest {
        format: FORMAT.into(),
        version,
        build,
        ffximain_sha256: main,
        ffxi_sha256: ffxi,
        created: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        files,
    })
}

// --- the vault -----------------------------------------------------------------------------------

pub struct Vault {
    pub root: PathBuf,
}

impl Vault {
    pub fn open(root: impl Into<PathBuf>) -> Result<Vault> {
        let root = root.into();
        fs::create_dir_all(root.join("objects")).map_err(err(root.display()))?;
        fs::create_dir_all(root.join("versions")).map_err(err(root.display()))?;
        Ok(Vault { root })
    }

    pub fn object(&self, sha: &str) -> PathBuf {
        self.root.join("objects").join(&sha[..2]).join(sha)
    }

    pub fn has(&self, e: &Entry) -> bool {
        fs::metadata(self.object(&e.sha256)).map(|m| m.len() == e.size).unwrap_or(false)
    }

    /// Puts a file in the store under its hash (a clone on APFS, so free on the same volume).
    pub fn put_file(&self, src: &Path, sha: &str) -> Result<()> {
        let dst = self.object(sha);
        if dst.exists() {
            return Ok(());
        }
        fs::create_dir_all(dst.parent().unwrap()).map_err(err(dst.display()))?;
        let tmp = dst.with_extension(format!("tmp{}", std::process::id()));
        fs::copy(src, &tmp).map_err(err(src.display()))?;
        fs::rename(&tmp, &dst).map_err(err(dst.display()))
    }

    /// Puts bytes from a reader in the store, checking they are what the hash says.
    pub fn put_reader(&self, mut r: impl Read, sha: &str) -> Result<u64> {
        let dst = self.object(sha);
        fs::create_dir_all(dst.parent().unwrap()).map_err(err(dst.display()))?;
        let tmp = dst.with_extension(format!("tmp{}-{:?}", std::process::id(), std::thread::current().id()));
        let mut f = File::create(&tmp).map_err(err(tmp.display()))?;
        let mut h = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        let mut n = 0u64;
        loop {
            let k = r.read(&mut buf).map_err(err(sha))?;
            if k == 0 {
                break;
            }
            h.update(&buf[..k]);
            f.write_all(&buf[..k]).map_err(err(tmp.display()))?;
            n += k as u64;
        }
        drop(f);
        let got = hex(&h.finalize());
        if got != sha {
            let _ = fs::remove_file(&tmp);
            return Err(format!("object {sha}: the bytes hash to {got}"));
        }
        fs::rename(&tmp, &dst).map_err(err(dst.display()))?;
        Ok(n)
    }

    pub fn manifest_path(&self, version: &str) -> PathBuf {
        self.root.join("versions").join(format!("{version}.json"))
    }

    pub fn load(&self, version: &str) -> Result<Manifest> {
        let p = self.manifest_path(version);
        let text = fs::read_to_string(&p).map_err(|e| format!("version {version} is not in the vault ({}: {e})", p.display()))?;
        let mut m: Manifest = serde_json::from_str(&text).map_err(err(p.display()))?;
        m.files.retain(|e| !players(&e.path)); // a manifest made before SYS was the player's
        m.check()?;
        Ok(m)
    }

    pub fn save(&self, m: &Manifest) -> Result<()> {
        let p = self.manifest_path(&m.version);
        let tmp = p.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(m).unwrap()).map_err(err(tmp.display()))?;
        fs::rename(&tmp, &p).map_err(err(p.display()))
    }

    pub fn versions(&self) -> Result<Vec<Manifest>> {
        let mut out = Vec::new();
        for e in fs::read_dir(self.root.join("versions")).map_err(err("versions"))?.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "json") {
                if let Ok(mut m) = serde_json::from_str::<Manifest>(&fs::read_to_string(&p).unwrap_or_default()) {
                    m.files.retain(|e| !players(&e.path));
                    out.push(m);
                }
            }
        }
        out.sort_by(|a, b| a.version.cmp(&b.version));
        Ok(out)
    }

    /// Records the install as a version: every file hashed and stored, the manifest saved.
    /// `name` overrides the version name (else scan's).
    pub fn snapshot(&self, game: &Path, name: Option<&str>, progress: Progress) -> Result<Manifest> {
        let mut m = hash_install(game, Some(self), "snapshot", progress)?;
        if let Some(n) = name {
            m.version = n.to_string();
        }
        self.save(&m)?;
        Ok(m)
    }

    /// Puts a version back together as an install of its own (clones on APFS: no extra space).
    pub fn materialize(&self, version: &str, out: &Path, progress: Progress) -> Result<Manifest> {
        let m = self.load(version)?;
        let missing: Vec<&Entry> = m.files.iter().filter(|e| !self.has(e)).collect();
        if !missing.is_empty() {
            return Err(format!("{} files of {version} are not in the vault (the first: {}); fetch them first", missing.len(), missing[0].path));
        }
        let total = m.bytes();
        let done = AtomicU64::new(0);
        m.files.par_iter().try_for_each(|e| -> Result<()> {
            let dst = out.join(&e.path);
            if fs::metadata(&dst).map(|md| md.len() == e.size).unwrap_or(false) {
                // already there: a version put together here before (its files are the vault's
                // clones, and verify checks them by hash when asked)
            } else {
                fs::create_dir_all(dst.parent().unwrap()).map_err(err(dst.display()))?;
                let _ = fs::remove_file(&dst);
                fs::copy(self.object(&e.sha256), &dst).map_err(err(dst.display()))?;
            }
            progress("materialize", done.fetch_add(e.size, Ordering::Relaxed) + e.size, total);
            Ok(())
        })?;
        // the player's folders the game expects to find, empty
        for d in SKIP_DIRS {
            let _ = fs::create_dir_all(out.join(d));
        }
        Ok(m)
    }
}

/// The player's folder (USER: each character's macros, key bindings and settings) shared between
/// their install and a version put together beside it: the copy's is a link to the install's, so
/// every version plays with the same. Whatever a copy had there already is moved aside, never lost.
pub fn share_player_dirs(own: &Path, copy: &Path) -> Result<()> {
    let src = own.join("USER");
    let dst = copy.join("USER");
    fs::create_dir_all(&src).map_err(err(src.display()))?;
    if let (Ok(a), Ok(b)) = (fs::canonicalize(&src), fs::canonicalize(&dst)) {
        if a == b {
            return Ok(()); // linked already
        }
    }
    if let Ok(md) = fs::symlink_metadata(&dst) {
        let empty = md.is_dir() && fs::read_dir(&dst).map(|mut d| d.next().is_none()).unwrap_or(false);
        if empty {
            fs::remove_dir(&dst).map_err(err(dst.display()))?;
        } else if md.file_type().is_symlink() {
            // a link to somewhere else (an install that moved)
            let _ = fs::remove_file(&dst).or_else(|_| fs::remove_dir(&dst));
        } else {
            let mut aside = copy.join("USER.before-shared");
            let mut n = 1;
            while aside.exists() {
                n += 1;
                aside = copy.join(format!("USER.before-shared-{n}"));
            }
            fs::rename(&dst, &aside).map_err(err(dst.display()))?;
        }
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&src, &dst).map_err(err(dst.display()))?;
    #[cfg(windows)]
    {
        // a junction: no administrator or developer mode needed, unlike a symbolic link
        let ok = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&dst)
            .arg(&src)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !ok {
            return Err(format!("{}: could not link it to {}", dst.display(), src.display()));
        }
    }
    Ok(())
}

// --- comparing -----------------------------------------------------------------------------------

#[derive(Serialize, Debug, Default)]
pub struct Diff {
    pub from: String,
    pub to: String,
    pub added: Vec<Entry>,
    pub changed: Vec<Entry>,
    pub removed: Vec<Entry>,
    /// what `to` needs that `from` does not have, by content (files that only moved need nothing)
    pub new_objects: Vec<Entry>,
}

impl Diff {
    pub fn new_bytes(&self) -> u64 {
        self.new_objects.iter().map(|e| e.size).sum()
    }
}

pub fn diff(a: &Manifest, b: &Manifest) -> Diff {
    let (pa, pb) = (a.by_path(), b.by_path());
    let mut d = Diff { from: a.version.clone(), to: b.version.clone(), ..Default::default() };
    for (p, e) in &pb {
        match pa.get(p) {
            None => d.added.push((*e).clone()),
            Some(old) if old.sha256 != e.sha256 => d.changed.push((*e).clone()),
            _ => {}
        }
    }
    for (p, e) in &pa {
        if !pb.contains_key(p) {
            d.removed.push((*e).clone());
        }
    }
    let have: BTreeSet<&str> = a.files.iter().map(|e| e.sha256.as_str()).collect();
    let mut seen = BTreeSet::new();
    for e in d.added.iter().chain(&d.changed) {
        if !have.contains(e.sha256.as_str()) && seen.insert(e.sha256.clone()) {
            d.new_objects.push(e.clone());
        }
    }
    d
}

/// An install checked against a version: what is missing or not what the manifest says.
#[derive(Serialize, Debug, Default)]
pub struct Check {
    pub missing: Vec<Entry>,
    pub wrong: Vec<Entry>,
    /// files the install has that the version does not
    pub extra: Vec<String>,
}

/// `full`: hash every file; else only sizes (fast, catches missing and truncated files).
pub fn verify(game: &Path, m: &Manifest, full: bool, progress: Progress) -> Result<Check> {
    let total = m.bytes();
    let done = AtomicU64::new(0);
    let results: Vec<(u8, Entry)> = m
        .files
        .par_iter()
        .filter_map(|e| {
            let p = game.join(&e.path);
            let r = match fs::metadata(&p) {
                Err(_) => Some((0u8, e.clone())),
                Ok(md) if md.len() != e.size => Some((1, e.clone())),
                Ok(_) if full && sha256_file(&p).ok().as_deref() != Some(e.sha256.as_str()) => Some((1, e.clone())),
                _ => None,
            };
            progress("verify", done.fetch_add(e.size, Ordering::Relaxed) + e.size, total);
            r
        })
        .collect();
    let mut c = Check::default();
    for (kind, e) in results {
        if kind == 0 { c.missing.push(e) } else { c.wrong.push(e) }
    }
    c.missing.sort_by(|a, b| a.path.cmp(&b.path));
    c.wrong.sort_by(|a, b| a.path.cmp(&b.path));
    let known: BTreeSet<&str> = m.files.iter().map(|e| e.path.as_str()).collect();
    c.extra = install_files(game)?.into_iter().map(|(p, _)| p).filter(|p| !known.contains(p.as_str())).collect();
    Ok(c)
}

/// Which version in the vault an install is. The DLLs narrow it down, but many updates change only
/// DATs, so versions can share them: every candidate must have all its files there at their sizes,
/// and when more than one does, the files the candidates disagree on are hashed to tell them apart
/// (the one that matches the most of them wins). None when no version fits.
pub fn identify(vault: &Vault, game: &Path) -> Result<Option<Manifest>> {
    let main = sha256_file(&game.join("FFXiMain.dll"))?;
    let ffxi = sha256_file(&game.join("FFXi.dll"))?;
    let nothing: Progress = &|_: &str, _: u64, _: u64| {};
    let mut fits: Vec<Manifest> = Vec::new();
    for m in vault.versions()? {
        if m.ffximain_sha256 == main && m.ffxi_sha256 == ffxi {
            let c = verify_sizes(game, &m, nothing);
            if c.missing.is_empty() && c.wrong.is_empty() {
                fits.push(m);
            }
        }
    }
    if fits.len() <= 1 {
        return Ok(fits.pop());
    }
    // the paths where the candidates differ: absent from one, or another hash
    let mut disputed: BTreeSet<&str> = BTreeSet::new();
    let maps: Vec<BTreeMap<&str, &Entry>> = fits.iter().map(|m| m.by_path()).collect();
    for (i, a) in maps.iter().enumerate() {
        for b in &maps[i + 1..] {
            for (p, e) in a {
                if b.get(p).map(|f| f.sha256 != e.sha256).unwrap_or(true) {
                    disputed.insert(p);
                }
            }
            for p in b.keys() {
                if !a.contains_key(p) {
                    disputed.insert(p);
                }
            }
        }
    }
    let actual: BTreeMap<&str, Option<String>> = disputed
        .iter()
        .map(|p| (*p, if game.join(p).is_file() { sha256_file(&game.join(p)).ok() } else { None }))
        .collect();
    let score = |m: &BTreeMap<&str, &Entry>| {
        actual.iter().filter(|(p, got)| m.get(*p).map(|e| got.as_deref() == Some(e.sha256.as_str())).unwrap_or(got.is_none())).count()
    };
    let best = (0..fits.len()).max_by_key(|&i| score(&maps[i])).unwrap();
    Ok(Some(fits.swap_remove(best)))
}

/// Whether an install is this version: identify's answer, or another version with the very same
/// files (two names for one version, as when a server publishes the client under its own name).
pub fn is_version(vault: &Vault, game: &Path, version: &str) -> bool {
    let (Ok(want), Ok(Some(got))) = (vault.load(version), identify(vault, game)) else { return false };
    if got.version == version {
        return true;
    }
    let files = |m: &Manifest| m.files.iter().map(|e| (e.path.clone(), e.sha256.clone())).collect::<BTreeSet<_>>();
    files(&got) == files(&want)
}

/// Only whether each file is there at its size (identify's first pass).
fn verify_sizes(game: &Path, m: &Manifest, progress: Progress) -> Check {
    verify(game, m, false, progress).unwrap_or_default()
}

/// Puts the missing and wrong files of a check back from the vault.
pub fn repair(game: &Path, vault: &Vault, c: &Check) -> Result<usize> {
    let todo: Vec<&Entry> = c.missing.iter().chain(&c.wrong).collect();
    for e in &todo {
        if !vault.has(e) {
            return Err(format!("{} is not in the vault; fetch the version first", e.path));
        }
    }
    todo.par_iter().try_for_each(|e| -> Result<()> {
        let dst = game.join(&e.path);
        fs::create_dir_all(dst.parent().unwrap()).map_err(err(dst.display()))?;
        let tmp = dst.with_extension("xi-vault-tmp");
        fs::copy(vault.object(&e.sha256), &tmp).map_err(err(dst.display()))?;
        fs::rename(&tmp, &dst).map_err(err(dst.display()))
    })?;
    Ok(todo.len())
}

// --- delta packs ---------------------------------------------------------------------------------

/// The header of a pack (xipack.json, its first entry).
#[derive(Serialize, Deserialize, Debug)]
pub struct PackHeader {
    pub format: String,
    pub from: String,
    pub to: String,
    /// the whole manifest of `to`: with the objects of `from`, all it needs
    pub manifest: Manifest,
    pub objects: Vec<Entry>,
}

/// One file: the objects `to` needs beyond `from`, and `to`'s manifest, as a zstd-compressed tar.
/// Returns (raw bytes in it, bytes written).
pub fn pack(vault: &Vault, from: &str, to: &str, out: &Path, level: i32, progress: Progress) -> Result<(u64, u64)> {
    let (a, b) = (vault.load(from)?, vault.load(to)?);
    let d = diff(&a, &b);
    let header = PackHeader { format: FORMAT.into(), from: from.into(), to: to.into(), manifest: b, objects: d.new_objects.clone() };
    let tmp = out.with_extension("tmp");
    let f = File::create(&tmp).map_err(err(tmp.display()))?;
    let mut z = zstd::Encoder::new(f, level).map_err(err("zstd"))?;
    z.multithread(std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(4)).map_err(err("zstd"))?;
    z.long_distance_matching(true).map_err(err("zstd"))?;
    let mut t = tar::Builder::new(z);
    let hj = serde_json::to_vec_pretty(&header).unwrap();
    let mut hd = tar::Header::new_gnu();
    hd.set_size(hj.len() as u64);
    hd.set_mode(0o644);
    hd.set_cksum();
    t.append_data(&mut hd, "xipack.json", hj.as_slice()).map_err(err("pack"))?;
    let total = d.new_bytes();
    let mut done = 0;
    for e in &d.new_objects {
        let mut f = File::open(vault.object(&e.sha256)).map_err(err(&e.path))?;
        let mut hd = tar::Header::new_gnu();
        hd.set_size(e.size);
        hd.set_mode(0o644);
        hd.set_cksum();
        t.append_data(&mut hd, format!("objects/{}", e.sha256), &mut f).map_err(err(&e.path))?;
        done += e.size;
        progress("pack", done, total);
    }
    let z = t.into_inner().map_err(err("pack"))?;
    z.finish().map_err(err("zstd"))?;
    fs::rename(&tmp, out).map_err(err(out.display()))?;
    Ok((total, fs::metadata(out).map(|m| m.len()).unwrap_or(0)))
}

/// Takes a pack into the vault: its objects, and the version it brings. Needs `from` already here
/// only when the version is to be put together.
pub fn unpack(vault: &Vault, r: impl Read, progress: Progress) -> Result<Manifest> {
    let z = zstd::Decoder::new(r).map_err(err("zstd"))?;
    let mut t = tar::Archive::new(z);
    let mut header: Option<PackHeader> = None;
    let mut done = 0;
    for entry in t.entries().map_err(err("pack"))? {
        let mut entry = entry.map_err(err("pack"))?;
        let path = entry.path().map_err(err("pack"))?.to_string_lossy().into_owned();
        if path == "xipack.json" {
            let mut s = String::new();
            entry.read_to_string(&mut s).map_err(err("pack"))?;
            header = Some(serde_json::from_str(&s).map_err(err("xipack.json"))?);
        } else if let Some(sha) = path.strip_prefix("objects/") {
            if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(format!("{path}: not an object"));
            }
            done += vault.put_reader(&mut entry, sha)?;
            if let Some(h) = &header {
                progress("unpack", done, h.objects.iter().map(|e| e.size).sum());
            }
        }
    }
    let h = header.ok_or("not a pack: no xipack.json")?;
    h.manifest.check()?;
    vault.save(&h.manifest)?;
    Ok(h.manifest)
}

// --- versions over the install: overlays and shelves -------------------------------------------
//
// A player's install is kept on the newest version (update_install). Another version a server
// wants is played as an overlay: only the files that version has and the install does not, in
// <vault>/overlays/<version>, which the game host lays over the install (xi-host --version-dir). A
// version not needed for now is shelved: what it needs beyond another version, as one compressed
// pack in <vault>/shelves/<version>.xipack, and its own objects removed from the store; unshelve
// brings it back. Disk space is the point: an overlay is the few hundred MB a version changes, a
// shelf that compressed, and a forgotten version nothing.

/// Files every overlay carries, changed or not: a version's build is read from them (the launcher
/// identifies an overlay, and makes its game, from its own FFXiMain.dll and FFXi.dll).
const OVERLAY_ALWAYS: &[&str] = &["FFXiMain.dll", "FFXi.dll"];

/// What an overlay folder is (xi-version.json in it).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OverlayInfo {
    pub version: String,
    pub build: String,
    /// the version of the install it goes over
    pub base: String,
    pub files: usize,
    pub bytes: u64,
}

impl Vault {
    pub fn overlay_dir(&self, version: &str) -> PathBuf {
        self.root.join("overlays").join(version)
    }

    pub fn shelf_path(&self, version: &str) -> PathBuf {
        self.root.join("shelves").join(format!("{version}.xipack"))
    }

    /// Every object a version names that the store does not hold.
    pub fn missing(&self, m: &Manifest) -> Vec<Entry> {
        m.files.iter().filter(|e| !self.has(e)).cloned().collect()
    }
}

/// The overlay at `dir`, if there is one (its xi-version.json).
pub fn overlay_info(dir: &Path) -> Option<OverlayInfo> {
    serde_json::from_str(&fs::read_to_string(dir.join("xi-version.json")).ok()?).ok()
}

/// `want` over an install that is `base`: each file of `want` that `base` does not have as it is
/// (added or changed), and FFXiMain.dll and FFXi.dll always, cloned from the store into
/// <vault>/overlays/<want> (replacing an overlay there). A file `base` has and `want` does not
/// stays visible: the game opens files by name, and one it never asks for does nothing.
pub fn make_overlay(vault: &Vault, base: &Manifest, want: &Manifest, progress: Progress) -> Result<(PathBuf, OverlayInfo)> {
    let d = diff(base, want);
    let mut files: Vec<Entry> = d.added.iter().chain(&d.changed).cloned().collect();
    for name in OVERLAY_ALWAYS {
        if !files.iter().any(|e| e.path.eq_ignore_ascii_case(name)) {
            if let Some(e) = want.files.iter().find(|e| e.path.eq_ignore_ascii_case(name)) {
                files.push(e.clone());
            }
        }
    }
    let lacking: Vec<&Entry> = files.iter().filter(|e| !vault.has(e)).collect();
    if !lacking.is_empty() {
        return Err(format!("{} files of {} are not in the vault (the first: {}); fetch or unshelve it first", lacking.len(), want.version, lacking[0].path));
    }
    let out = vault.overlay_dir(&want.version);
    let tmp = out.with_extension("new");
    let _ = fs::remove_dir_all(&tmp);
    let total: u64 = files.iter().map(|e| e.size).sum();
    let done = AtomicU64::new(0);
    files.par_iter().try_for_each(|e| -> Result<()> {
        let dst = tmp.join(&e.path);
        fs::create_dir_all(dst.parent().unwrap()).map_err(err(dst.display()))?;
        fs::copy(vault.object(&e.sha256), &dst).map_err(err(dst.display()))?;
        progress("overlay", done.fetch_add(e.size, Ordering::Relaxed) + e.size, total);
        Ok(())
    })?;
    let info = OverlayInfo { version: want.version.clone(), build: want.build.clone(), base: base.version.clone(), files: files.len(), bytes: total };
    fs::write(tmp.join("xi-version.json"), serde_json::to_string_pretty(&info).unwrap()).map_err(err(tmp.display()))?;
    let _ = fs::remove_dir_all(&out);
    fs::rename(&tmp, &out).map_err(err(out.display()))?;
    Ok((out, info))
}

/// A shelved version (its pack's header).
#[derive(Serialize, Clone, Debug)]
pub struct ShelfInfo {
    pub version: String,
    pub build: String,
    /// the version it was packed against: unshelving needs that version's files too
    pub from: String,
    pub files: usize,
    /// the version's whole size, and the shelf's on disk
    pub bytes: u64,
    pub shelf_bytes: u64,
}

fn read_pack_header(path: &Path) -> Result<PackHeader> {
    let f = File::open(path).map_err(err(path.display()))?;
    let z = zstd::Decoder::new(f).map_err(err("zstd"))?;
    let mut t = tar::Archive::new(z);
    let mut entries = t.entries().map_err(err("pack"))?;
    let mut first = entries.next().ok_or("an empty pack")?.map_err(err("pack"))?;
    let mut s = String::new();
    first.read_to_string(&mut s).map_err(err("pack"))?;
    serde_json::from_str(&s).map_err(err(path.display()))
}

pub fn shelves(vault: &Vault) -> Vec<ShelfInfo> {
    let mut out = Vec::new();
    for e in fs::read_dir(vault.root.join("shelves")).into_iter().flatten().flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "xipack") {
            if let Ok(h) = read_pack_header(&p) {
                out.push(ShelfInfo {
                    files: h.manifest.files.len(),
                    bytes: h.manifest.bytes(),
                    build: h.manifest.build.clone(),
                    version: h.to,
                    from: h.from,
                    shelf_bytes: fs::metadata(&p).map(|m| m.len()).unwrap_or(0),
                });
            }
        }
    }
    out.sort_by(|a, b| a.version.cmp(&b.version));
    out
}

/// Removes the objects `version` names that no other version in the vault does; (files, bytes).
fn prune(vault: &Vault, version: &Manifest) -> Result<(usize, u64)> {
    let mut keep = BTreeSet::new();
    for m in vault.versions()? {
        if m.version != version.version {
            keep.extend(m.files.into_iter().map(|e| e.sha256));
        }
    }
    let (mut n, mut bytes, mut seen) = (0usize, 0u64, BTreeSet::new());
    for e in &version.files {
        if keep.contains(&e.sha256) || !seen.insert(e.sha256.clone()) {
            continue;
        }
        let p = vault.object(&e.sha256);
        if let Ok(md) = fs::metadata(&p) {
            fs::remove_file(&p).map_err(err(p.display()))?;
            n += 1;
            bytes += md.len();
        }
    }
    Ok((n, bytes))
}

/// Shelves `version`: what it needs beyond `against` (the install's version, which stays) packed
/// into <vault>/shelves/<version>.xipack, then its manifest, its overlay and the objects no other
/// version uses removed. (raw bytes packed, shelf bytes, bytes freed). A shelf made before is kept.
pub fn shelve(vault: &Vault, version: &str, against: &str, level: i32, progress: Progress) -> Result<(u64, u64, u64)> {
    if version == against {
        return Err(format!("{version} is the version it would be packed against"));
    }
    let m = vault.load(version)?;
    let shelf = vault.shelf_path(version);
    let (raw, packed) = if shelf.is_file() {
        (0, fs::metadata(&shelf).map(|md| md.len()).unwrap_or(0))
    } else {
        fs::create_dir_all(shelf.parent().unwrap()).map_err(err(shelf.display()))?;
        pack(vault, against, version, &shelf, level, progress)?
    };
    // the pack is read back whole before anything is removed
    let h = read_pack_header(&shelf)?;
    if h.to != version || h.manifest.files.len() != m.files.len() {
        return Err(format!("{}: not a whole shelf of {version}", shelf.display()));
    }
    let _ = fs::remove_dir_all(vault.overlay_dir(version));
    fs::remove_file(vault.manifest_path(version)).map_err(err(version))?;
    let (_, freed) = prune(vault, &m)?;
    Ok((raw, packed, freed))
}

/// Brings a shelved version back into the vault (and, first, a shelved version it was packed
/// against, when the vault no longer has that one's files either). The shelf stays.
pub fn unshelve(vault: &Vault, version: &str, progress: Progress) -> Result<Manifest> {
    let shelf = vault.shelf_path(version);
    let h = read_pack_header(&shelf).map_err(|e| format!("{version} is not shelved: {e}"))?;
    if vault.load(&h.from).is_err() && vault.shelf_path(&h.from).is_file() {
        unshelve(vault, &h.from, progress)?;
    }
    let m = unpack(vault, File::open(&shelf).map_err(err(shelf.display()))?, progress)?;
    let lacking = vault.missing(&m);
    if !lacking.is_empty() {
        return Err(format!("{version} is back, but {} of its files are not in the vault (version {} is needed as well)", lacking.len(), h.from));
    }
    Ok(m)
}

/// Forgets a version: its shelf, overlay, manifest and the objects no other version uses.
pub fn forget(vault: &Vault, version: &str) -> Result<u64> {
    let mut freed = 0;
    if let Ok(m) = vault.load(version) {
        freed += prune(vault, &m)?.1;
        fs::remove_file(vault.manifest_path(version)).map_err(err(version))?;
    }
    let shelf = vault.shelf_path(version);
    freed += fs::metadata(&shelf).map(|m| m.len()).unwrap_or(0);
    let _ = fs::remove_file(&shelf);
    let _ = fs::remove_dir_all(vault.overlay_dir(version));
    Ok(freed)
}

// --- update bundles ------------------------------------------------------------------------------
//
// A server operator updates the game on one PC (PlayOnline) and the site lives on another machine.
// A bundle carries a new version from the one to the other without a vault on either side: made
// from the install against what the site already hosts, taken into the site where it is served.
//   bundle.json                 BundleHeader
//   versions/<version>.json     the new version's manifest
//   objects/ab/<sha256>.zst     each file the site lacks, compressed as the site keeps it
// (an uncompressed tar: the objects are zstd already)

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct BundleHeader {
    pub format: String,
    pub version: String,
    /// the launcher's build label; empty when the launcher does not know this FFXiMain.dll yet
    pub build: String,
    /// as IndexVersion::base: the version whose files players bring themselves ("" when all hosted)
    pub base: String,
    /// the version the site wanted when this was made, which it was measured against
    pub since: String,
    pub objects: usize,
    /// the objects' size uncompressed
    pub bytes: u64,
}

/// A version name that is safe as a file name on a site.
fn plain_name(v: &str) -> bool {
    !v.is_empty() && v.len() <= 64 && v.chars().all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c)) && !v.starts_with('.')
}

fn tar_bytes<W: Write>(t: &mut tar::Builder<W>, path: &str, data: &[u8]) -> Result<()> {
    let mut hd = tar::Header::new_gnu();
    hd.set_size(data.len() as u64);
    hd.set_mode(0o644);
    hd.set_cksum();
    t.append_data(&mut hd, path, data).map_err(err(path))
}

/// Writes a bundle of version `m` (hashed from the install `game`): its manifest, and every file of
/// it whose content is not in `hosted` (what the site has: its current version, and that version's
/// base, which players bring). `base` and `since` go in the header.
pub fn make_bundle(game: &Path, m: &Manifest, hosted: &BTreeSet<String>, base: &str, base_manifest: Option<&Manifest>, since: &str, out: &Path, progress: Progress) -> Result<BundleHeader> {
    if !plain_name(&m.version) {
        return Err(format!("version name {:?}: letters, digits, _ - . only", m.version));
    }
    let mut objects: BTreeMap<&str, &Entry> = BTreeMap::new();
    for e in m.files.iter().filter(|e| !hosted.contains(&e.sha256)) {
        objects.entry(&e.sha256).or_insert(e);
    }
    let header = BundleHeader {
        format: FORMAT.into(),
        version: m.version.clone(),
        build: m.build.clone(),
        base: base.into(),
        since: since.into(),
        objects: objects.len(),
        bytes: objects.values().map(|e| e.size).sum(),
    };
    // compressed side by side first (every core), then one after another into the tar
    let parts = out.with_extension("parts");
    fs::create_dir_all(&parts).map_err(err(parts.display()))?;
    let done = AtomicU64::new(0);
    objects.par_iter().try_for_each(|(sha, e)| -> Result<()> {
        let dst = parts.join(format!("{sha}.zst"));
        if !dst.exists() {
            let data = fs::read(game.join(&e.path)).map_err(err(&e.path))?;
            if hex(&Sha256::digest(&data)) != **sha {
                return Err(format!("{} changed while this ran; run it again", e.path));
            }
            let z = zstd::bulk::compress(&data, 9).map_err(err("zstd"))?;
            let tmp = dst.with_extension("tmp");
            fs::write(&tmp, z).map_err(err(tmp.display()))?;
            fs::rename(&tmp, &dst).map_err(err(dst.display()))?;
        }
        progress("compress", done.fetch_add(e.size, Ordering::Relaxed) + e.size, header.bytes);
        Ok(())
    })?;
    let tmp = out.with_extension("tmp");
    let mut t = tar::Builder::new(std::io::BufWriter::new(File::create(&tmp).map_err(err(tmp.display()))?));
    tar_bytes(&mut t, "bundle.json", &serde_json::to_vec_pretty(&header).unwrap())?;
    if let Some(b) = base_manifest {
        tar_bytes(&mut t, &format!("versions/{}.json", b.version), &serde_json::to_vec_pretty(b).unwrap())?;
    }
    tar_bytes(&mut t, &format!("versions/{}.json", m.version), &serde_json::to_vec_pretty(m).unwrap())?;
    for sha in objects.keys() {
        let src = parts.join(format!("{sha}.zst"));
        let mut f = File::open(&src).map_err(err(src.display()))?;
        t.append_file(format!("objects/{}/{sha}.zst", &sha[..2]), &mut f).map_err(err(src.display()))?;
    }
    t.into_inner().map_err(err("bundle"))?.flush().map_err(err("bundle"))?;
    fs::rename(&tmp, out).map_err(err(out.display()))?;
    let _ = fs::remove_dir_all(&parts);
    Ok(header)
}

/// Takes a bundle into a published site: every object (each checked against its hash), then, once
/// the site has all the version needs (or its base does), its manifest and its entry in index.json.
/// `make_current`: also the version the server wants. Nothing is listed when anything is missing.
pub fn apply_bundle(site: &Path, r: impl Read, make_current: bool, progress: Progress) -> Result<(BundleHeader, Index)> {
    if !site.join("index.json").is_file() {
        return Err(format!("{}: not a site (no index.json)", site.display()));
    }
    let mut t = tar::Archive::new(r);
    let mut header: Option<BundleHeader> = None;
    let mut manifest: Option<Manifest> = None;
    let mut base_manifest: Option<Manifest> = None;
    let mut done = 0;
    for entry in t.entries().map_err(err("bundle"))? {
        let mut entry = entry.map_err(err("bundle"))?;
        let path = entry.path().map_err(err("bundle"))?.to_string_lossy().into_owned();
        let mut data = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut data).map_err(err(&path))?;
        if path == "bundle.json" {
            let h: BundleHeader = serde_json::from_slice(&data).map_err(err("bundle.json"))?;
            if h.format != FORMAT || !plain_name(&h.version) || !(h.base.is_empty() || plain_name(&h.base)) {
                return Err(format!("not a bundle this xi-vault reads ({} {})", h.format, h.version));
            }
            header = Some(h);
        } else if let Some(name) = path.strip_prefix("versions/").and_then(|p| p.strip_suffix(".json")) {
            let m: Manifest = serde_json::from_slice(&data).map_err(err(&path))?;
            m.check()?;
            let h = header.as_ref().ok_or("bundle.json must come first")?;
            if m.version != name {
                return Err(format!("{path}: not the version it is named for"));
            } else if name == h.version {
                manifest = Some(m);
            } else if name == h.base {
                base_manifest = Some(m);
            } else {
                return Err(format!("{path}: not the version the bundle says, nor its base"));
            }
        } else if let Some(sha) = path.strip_prefix("objects/").and_then(|p| p.get(3..)).and_then(|p| p.strip_suffix(".zst")) {
            if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) || !path.starts_with(&format!("objects/{}/", &sha[..2])) {
                return Err(format!("{path}: not an object"));
            }
            let mut h = Sha256::new();
            let mut n = 0u64;
            let mut z = zstd::Decoder::new(data.as_slice()).map_err(err(&path))?;
            let mut buf = vec![0u8; 1 << 20];
            loop {
                let k = z.read(&mut buf).map_err(err(&path))?;
                if k == 0 {
                    break;
                }
                h.update(&buf[..k]);
                n += k as u64;
            }
            if hex(&h.finalize()) != sha {
                return Err(format!("{path}: damaged (its content does not hash to its name)"));
            }
            let dst = site.join("objects").join(&sha[..2]).join(format!("{sha}.zst"));
            if !dst.exists() {
                fs::create_dir_all(dst.parent().unwrap()).map_err(err(dst.display()))?;
                let tmp = dst.with_extension("tmp");
                fs::write(&tmp, &data).map_err(err(tmp.display()))?;
                fs::rename(&tmp, &dst).map_err(err(dst.display()))?;
            }
            done += n;
            if let Some(h) = &header {
                progress("apply", done, h.bytes);
            }
        }
    }
    let h = header.ok_or("not a bundle: no bundle.json")?;
    let m = manifest.ok_or("the bundle has no manifest")?;
    if let Some(b) = &base_manifest {
        write_manifest(site, b)?;
    }
    let index = list_version(site, &h, &m, make_current, "neither in the bundle nor on this site")?;
    Ok((h, index))
}

/// A manifest into a site's versions/ (a base: players bring its files; it is not listed).
fn write_manifest(site: &Path, m: &Manifest) -> Result<()> {
    if !plain_name(&m.version) {
        return Err(format!("version name {:?}: letters, digits, _ - . only", m.version));
    }
    let p = site.join("versions").join(format!("{}.json", m.version));
    fs::create_dir_all(p.parent().unwrap()).map_err(err("versions"))?;
    let tmp = p.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_string_pretty(m).unwrap()).map_err(err(tmp.display()))?;
    fs::rename(&tmp, &p).map_err(err(p.display()))
}

/// Lists a version on a site whose objects are there: once everything it needs is hosted (or in its
/// base, the players'), its manifest and its entry in index.json; `make_current`: handed out too.
fn list_version(site: &Path, h: &BundleHeader, m: &Manifest, make_current: bool, lacking: &str) -> Result<Index> {
    let index_path = site.join("index.json");
    let mut index: Index = serde_json::from_str(&fs::read_to_string(&index_path).map_err(err(index_path.display()))?)
        .map_err(err(index_path.display()))?;
    let from_base: BTreeSet<String> = if h.base.is_empty() {
        BTreeSet::new()
    } else {
        let p = site.join("versions").join(format!("{}.json", h.base));
        let b: Manifest = serde_json::from_str(&fs::read_to_string(&p).map_err(|e| format!("its base {} is not on this site ({e})", h.base))?)
            .map_err(err(p.display()))?;
        b.files.into_iter().map(|e| e.sha256).collect()
    };
    let missing: Vec<&Entry> = m
        .files
        .iter()
        .filter(|e| !from_base.contains(&e.sha256) && !site.join("objects").join(&e.sha256[..2]).join(format!("{}.zst", e.sha256)).exists())
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "{} files of {} are {lacking} (the first: {}); it was measured against {}, \
             so run it again against what the site has now",
            missing.len(),
            h.version,
            missing[0].path,
            h.since
        ));
    }
    let mp = site.join("versions").join(format!("{}.json", h.version));
    fs::create_dir_all(mp.parent().unwrap()).map_err(err("versions"))?;
    fs::write(&mp, serde_json::to_string_pretty(&m).unwrap()).map_err(err(mp.display()))?;
    index.format = FORMAT.into();
    index.versions.retain(|v| v.version != h.version);
    index.versions.push(IndexVersion {
        version: m.version.clone(),
        build: m.build.clone(),
        files: m.files.len(),
        bytes: m.bytes(),
        base: h.base.clone(),
        digest: m.digest(),
    });
    if make_current || index.current.is_empty() {
        index.current = h.version.clone();
    }
    let tmp = index_path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_string_pretty(&index).unwrap()).map_err(err(tmp.display()))?;
    fs::rename(&tmp, &index_path).map_err(err(index_path.display()))?;
    Ok(index)
}

/// make_bundle and apply_bundle in one, for a site folder this machine can write (here, or a
/// share): each file the site lacks compressed straight into it, then the version listed.
pub fn publish_install(
    site: &Path,
    game: &Path,
    m: &Manifest,
    hosted: &BTreeSet<String>,
    base: &str,
    base_manifest: Option<&Manifest>,
    since: &str,
    make_current: bool,
    progress: Progress,
) -> Result<(BundleHeader, Index)> {
    if !plain_name(&m.version) {
        return Err(format!("version name {:?}: letters, digits, _ - . only", m.version));
    }
    let mut objects: BTreeMap<&str, &Entry> = BTreeMap::new();
    for e in m.files.iter().filter(|e| !hosted.contains(&e.sha256)) {
        objects.entry(&e.sha256).or_insert(e);
    }
    let h = BundleHeader {
        format: FORMAT.into(),
        version: m.version.clone(),
        build: m.build.clone(),
        base: base.into(),
        since: since.into(),
        objects: objects.len(),
        bytes: objects.values().map(|e| e.size).sum(),
    };
    let done = AtomicU64::new(0);
    objects.par_iter().try_for_each(|(sha, e)| -> Result<()> {
        let dst = site.join("objects").join(&sha[..2]).join(format!("{sha}.zst"));
        if !dst.exists() {
            let data = fs::read(game.join(&e.path)).map_err(err(&e.path))?;
            if hex(&Sha256::digest(&data)) != **sha {
                return Err(format!("{} changed while this ran; run it again", e.path));
            }
            let z = zstd::bulk::compress(&data, 9).map_err(err("zstd"))?;
            fs::create_dir_all(dst.parent().unwrap()).map_err(err(dst.display()))?;
            let tmp = dst.with_extension(format!("tmp{}", std::process::id()));
            fs::write(&tmp, z).map_err(err(tmp.display()))?;
            fs::rename(&tmp, &dst).map_err(err(dst.display()))?;
        }
        progress("publish", done.fetch_add(e.size, Ordering::Relaxed) + e.size, h.bytes);
        Ok(())
    })?;
    if let Some(b) = base_manifest {
        write_manifest(site, b)?;
    }
    let index = list_version(site, &h, m, make_current, "not on this site")?;
    Ok((h, index))
}

// --- publishing and fetching ---------------------------------------------------------------------

/// Writes a static site for these versions: index.json, their manifests, every object they use
/// (zstd), and with `packs`, a delta pack from each version to the next. `current` is the version
/// the server wants. With `since`, objects that version already has are left out: the site holds
/// only the difference, and players bring the rest from their own install of it.
pub fn publish(
    vault: &Vault,
    versions: &[String],
    current: &str,
    out: &Path,
    packs: bool,
    since: Option<&str>,
    progress: Progress,
) -> Result<Index> {
    let ms: Vec<Manifest> = versions.iter().map(|v| vault.load(v)).collect::<Result<_>>()?;
    fs::create_dir_all(out.join("versions")).map_err(err(out.display()))?;
    let base = since.map(|b| vault.load(b)).transpose()?;
    let skip: BTreeSet<&str> = base.iter().flat_map(|b| b.files.iter().map(|e| e.sha256.as_str())).collect();
    if let Some(b) = &base {
        // its manifest, so a player can tell whether their install is it
        fs::write(out.join("versions").join(format!("{}.json", b.version)), serde_json::to_string_pretty(b).unwrap())
            .map_err(err("manifest"))?;
    }
    let mut objects: BTreeMap<&str, &Entry> = BTreeMap::new();
    for m in &ms {
        for e in m.files.iter().filter(|e| !skip.contains(e.sha256.as_str())) {
            objects.entry(&e.sha256).or_insert(e);
        }
        fs::write(out.join("versions").join(format!("{}.json", m.version)), serde_json::to_string_pretty(m).unwrap())
            .map_err(err("manifest"))?;
    }
    let total: u64 = objects.values().map(|e| e.size).sum();
    let done = AtomicU64::new(0);
    objects.par_iter().try_for_each(|(sha, e)| -> Result<()> {
        let dst = out.join("objects").join(&sha[..2]).join(format!("{sha}.zst"));
        if !dst.exists() {
            fs::create_dir_all(dst.parent().unwrap()).map_err(err(dst.display()))?;
            let data = fs::read(vault.object(sha)).map_err(err(&e.path))?;
            let z = zstd::bulk::compress(&data, 9).map_err(err("zstd"))?;
            let tmp = dst.with_extension("tmp");
            fs::write(&tmp, z).map_err(err(tmp.display()))?;
            fs::rename(&tmp, &dst).map_err(err(dst.display()))?;
        }
        progress("publish", done.fetch_add(e.size, Ordering::Relaxed) + e.size, total);
        Ok(())
    })?;
    // what the site published before stays (a server can go back to it: set_current), unless
    // republished here
    let mut index = fs::read_to_string(out.join("index.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Index>(&t).ok())
        .unwrap_or_default();
    index.format = FORMAT.into();
    index.current = current.into();
    index.versions.retain(|v| !versions.contains(&v.version));
    index.packs.retain(|p| !(versions.contains(&p.from) && versions.contains(&p.to)));
    for m in &ms {
        index.versions.push(IndexVersion {
            version: m.version.clone(),
            build: m.build.clone(),
            files: m.files.len(),
            bytes: m.bytes(),
            // a version published --since itself: listed, and the players bring all of it
            base: base.as_ref().map(|b| b.version.clone()).unwrap_or_default(),
            digest: m.digest(),
        });
    }
    if packs {
        fs::create_dir_all(out.join("packs")).map_err(err("packs"))?;
        for w in ms.windows(2) {
            let name = format!("{}..{}.tar.zst", w[0].version, w[1].version);
            let p = out.join("packs").join(&name);
            pack(vault, &w[0].version, &w[1].version, &p, 19, progress)?;
            index.packs.push(IndexPack {
                from: w[0].version.clone(),
                to: w[1].version.clone(),
                path: format!("packs/{name}"),
                size: fs::metadata(&p).map(|m| m.len()).unwrap_or(0),
                sha256: sha256_file(&p)?,
            });
        }
    }
    fs::write(out.join("index.json"), serde_json::to_string_pretty(&index).unwrap()).map_err(err("index.json"))?;
    Ok(index)
}

/// Which version a published site's server wants: a rollback, or back again. The version must be
/// published there already (its manifest and objects stay when a newer one is published).
pub fn set_current(site: &Path, version: &str) -> Result<Index> {
    let p = site.join("index.json");
    let mut index: Index = serde_json::from_str(&fs::read_to_string(&p).map_err(err(p.display()))?).map_err(err(p.display()))?;
    if !index.versions.iter().any(|v| v.version == version) {
        let known: Vec<&str> = index.versions.iter().map(|v| v.version.as_str()).collect();
        return Err(format!("{} does not publish version {version} (it has: {})", site.display(), known.join(", ")));
    }
    index.current = version.into();
    let tmp = p.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_string_pretty(&index).unwrap()).map_err(err(tmp.display()))?;
    fs::rename(&tmp, &p).map_err(err(p.display()))?;
    Ok(index)
}

fn url_join(base: &str, rel: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), rel)
}

fn get_within(url: &str, timeout: std::time::Duration) -> Result<Box<dyn Read + Send + Sync>> {
    let r = ureq::get(url).timeout(timeout).call().map_err(err(url))?;
    Ok(Box::new(r.into_reader()))
}

fn get(url: &str) -> Result<Box<dyn Read + Send + Sync>> {
    get_within(url, std::time::Duration::from_secs(60))
}

pub fn fetch_index(base: &str) -> Result<Index> {
    fetch_index_within(base, std::time::Duration::from_secs(20))
}

/// The index, giving up after `timeout` (looking for a site that may not be there).
pub fn fetch_index_within(base: &str, timeout: std::time::Duration) -> Result<Index> {
    let mut s = String::new();
    get_within(&url_join(base, "index.json"), timeout)?.read_to_string(&mut s).map_err(err("index.json"))?;
    let index: Index = serde_json::from_str(&s).map_err(err("index.json"))?;
    if index.format != FORMAT {
        return Err(format!("{base}: not an xi-vault site ({})", index.format));
    }
    Ok(index)
}

/// A published version's manifest.
pub fn fetch_manifest(base: &str, version: &str) -> Result<Manifest> {
    let mut s = String::new();
    get(&url_join(base, &format!("versions/{version}.json")))?.read_to_string(&mut s).map_err(err("manifest"))?;
    let m: Manifest = serde_json::from_str(&s).map_err(err(format!("versions/{version}.json")))?;
    m.check()?;
    Ok(m)
}

// --- bringing an install to a version, in place ----------------------------------------------------
//
// For players who run the game from their own install (xiloader, Ashita, Windower) rather than the
// launcher: the updater changes the install itself, up or down, to the version a server names. Each
// file it replaces goes into a store first, so going back never depends on a site still hosting it.

/// What update_install did.
#[derive(Debug, Default)]
pub struct InstallUpdate {
    /// files written (new, or replaced)
    pub written: usize,
    pub bytes: u64,
    /// files the version does not have, left where they are (unused by it)
    pub extra: usize,
    /// of the objects written, those downloaded (the rest came from the store)
    pub downloaded: usize,
}

/// Brings `game` to version `m`: every file of `m` whose content differs is put in place, from the
/// store when it has it, else from `site` (downloaded into the store first). The file it replaces is
/// kept in the store. `have` is the install's manifest as it is now (hash_install). Nothing is
/// changed until everything needed is in the store.
pub fn update_install(game: &Path, have: &Manifest, m: &Manifest, store: &Vault, site: Option<&str>, progress: Progress) -> Result<InstallUpdate> {
    let now = have.by_path();
    let todo: Vec<&Entry> = m.files.iter().filter(|e| now.get(e.path.as_str()).map(|h| h.sha256 != e.sha256).unwrap_or(true)).collect();
    // data may be a server's own; programs only Square Enix's
    let foreign: Vec<&str> = todo.iter().filter(|e| is_program(&e.path) && !known_program(&e.sha256)).map(|e| e.path.as_str()).collect();
    if !foreign.is_empty() {
        return Err(format!(
            "version {} has program files that are not Square Enix's ({}); the updater installs data a server customises, never programs",
            m.version,
            foreign.join(", ")
        ));
    }
    let wanted: BTreeSet<&str> = m.files.iter().map(|e| e.path.as_str()).collect();
    let mut r = InstallUpdate { extra: have.files.iter().filter(|e| !wanted.contains(e.path.as_str())).count(), ..Default::default() };
    // what the store lacks: from another file of the install with that content, else the site
    let by_sha: BTreeMap<&str, &Entry> = have.files.iter().map(|e| (e.sha256.as_str(), e)).collect();
    let mut need: BTreeMap<&str, &Entry> = BTreeMap::new();
    for e in &todo {
        if store.has(e) {
            continue;
        }
        if let Some(local) = by_sha.get(e.sha256.as_str()) {
            store.put_file(&game.join(&local.path), &e.sha256)?;
            continue;
        }
        need.insert(&e.sha256, e);
    }
    if !need.is_empty() {
        let Some(base) = site else {
            return Err(format!("{} files of {} are neither here nor in the backups (the first: {})", need.len(), m.version, need.values().next().unwrap().path));
        };
        let total: u64 = need.values().map(|e| e.size).sum();
        let done = AtomicU64::new(0);
        let pool = rayon::ThreadPoolBuilder::new().num_threads(8).build().map_err(err("threads"))?;
        pool.install(|| {
            need.par_iter().try_for_each(|(sha, e)| -> Result<()> {
                let url = url_join(base, &format!("objects/{}/{sha}.zst", &sha[..2]));
                let z = zstd::Decoder::new(get(&url).map_err(|_| {
                    format!("{} is not on the update server, nor in this PC's backups: it cannot bring version {} here", e.path, m.version)
                })?)
                .map_err(err(&url))?;
                store.put_reader(z, sha)?;
                progress("download", done.fetch_add(e.size, Ordering::Relaxed) + e.size, total);
                Ok(())
            })
        })?;
        r.downloaded = need.len();
    }
    // everything is here: keep each file that is replaced, then put the version's in place
    let total: u64 = todo.iter().map(|e| e.size).sum();
    let mut done = 0;
    for e in &todo {
        let dst = game.join(&e.path);
        if let Some(old) = now.get(e.path.as_str()) {
            store.put_file(&dst, &old.sha256)?;
        }
        fs::create_dir_all(dst.parent().unwrap()).map_err(err(dst.display()))?;
        let tmp = dst.with_extension("xi-vault-tmp");
        fs::copy(store.object(&e.sha256), &tmp).map_err(err(dst.display()))?;
        if sha256_file(&tmp)? != e.sha256 {
            let _ = fs::remove_file(&tmp);
            return Err(format!("{}: the copy does not check out; nothing more was changed", e.path));
        }
        if let Ok(md) = fs::metadata(&dst) {
            // a read-only file (Windows installs have some) is replaced too
            let mut perm = md.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perm.set_readonly(false);
            let _ = fs::set_permissions(&dst, perm);
            let _ = fs::remove_file(&dst);
        }
        fs::rename(&tmp, &dst).map_err(err(dst.display()))?;
        done += e.size;
        r.written += 1;
        r.bytes += e.size;
        progress("install", done, total);
    }
    store.save(m)?;
    Ok(r)
}

// --- what a game server wants --------------------------------------------------------------------

/// What a LandSandBoat server's login server says about the client it wants (xi_connect's
/// LOGIN_VERSION_INFO, command 0x40: settings/default/login.lua CLIENT_VER, VER_LOCK, UPDATE_URL).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ServerInfo {
    pub client_ver: String,
    /// 0 any version, 1 exactly CLIENT_VER, 2 CLIENT_VER or newer (by year and month)
    #[serde(default)]
    pub ver_lock: u8,
    /// its xi-vault site; empty when it names none
    #[serde(default)]
    pub update_url: String,
    /// the xiloader protocol its login server speaks (major, minor, patch; major.minor must match),
    /// so a launcher signs in with the matching variant; empty from a server that does not say
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loader_version: Vec<u8>,
    /// its PlayOnline profile server's port (xi_profile; loader 2.2); 0 when it does not say
    #[serde(default, skip_serializing_if = "is_zero")]
    pub profile_port: u16,
}

fn is_zero(n: &u16) -> bool {
    *n == 0
}

/// xi_connect's auth port.
pub const LOGIN_PORT: u16 = 54231;

#[derive(Debug)]
struct AnyCertificate(Vec<rustls::SignatureScheme>);

// As xiloader: private servers present self-signed certificates. What this learns is public, and a
// version it names is still only ever taken whole and checked file by file from a site.
impl rustls::client::danger::ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.clone()
    }
}

/// Asks a game server's login server which client version it wants and where it publishes it.
/// `server` is a name or address, with ":port" when not 54231. Err when it does not answer, or is a
/// LandSandBoat without LOGIN_VERSION_INFO (which says nothing and closes).
pub fn server_info(server: &str, timeout: std::time::Duration) -> Result<ServerInfo> {
    use std::net::ToSocketAddrs;
    let server = server.trim();
    let (host, port) = match server.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => (h, p.parse::<u16>().map_err(err(server))?),
        _ => (server, LOGIN_PORT),
    };
    let addr = (host, port).to_socket_addrs().map_err(err(host))?.next().ok_or(format!("{host}: no address"))?;
    let tcp = std::net::TcpStream::connect_timeout(&addr, timeout).map_err(err(format!("{host}:{port}")))?;
    tcp.set_read_timeout(Some(timeout)).ok();
    tcp.set_write_timeout(Some(timeout)).ok();
    let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
    let schemes = provider.signature_verification_algorithms.supported_schemes();
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(err("TLS"))?
        .dangerous()
        .with_custom_certificate_verifier(std::sync::Arc::new(AnyCertificate(schemes)))
        .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(host.to_string()).map_err(err(host))?;
    let conn = rustls::ClientConnection::new(std::sync::Arc::new(config), name).map_err(err("TLS"))?;
    let mut tls = rustls::StreamOwned::new(conn, tcp);
    // the loader version as xiloader 2.1 sends it; an older xi_connect checks it before the command
    tls.write_all(br#"{"command":64,"version":[2,1,2]}"#).map_err(err(format!("{host}:{port}")))?;
    tls.flush().map_err(err(format!("{host}:{port}")))?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match tls.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf);
                let text = text.trim_end_matches('\0');
                if let Ok(info) = serde_json::from_str::<ServerInfo>(text) {
                    return Ok(info);
                }
            }
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&buf).trim_end_matches('\0').to_string();
    Err(if text.is_empty() {
        format!("{host}:{port} did not say which version it wants (a LandSandBoat without the version request)")
    } else {
        format!("{host}:{port}: {text}")
    })
}

/// Where a server's game versions are looked for when nothing names the address, in order: the
/// server itself on xi-vault's port, then its update. (or updates.) host, which an operator points
/// at wherever they host them (a machine at home, say) with a DNS record alone.
pub fn site_candidates(server: &str) -> Vec<String> {
    let server = server.trim();
    if server.is_empty() || server.contains('/') || server.matches(':').count() > 1 {
        return Vec::new();
    }
    let host = server.split(':').next().unwrap_or(server);
    let port = DEFAULT_PORT;
    let mut out = vec![format!("http://{host}:{port}")];
    if host.parse::<std::net::Ipv4Addr>().is_err() && host.contains('.') {
        out.push(format!("http://update.{host}:{port}"));
        out.push(format!("https://update.{host}"));
        out.push(format!("http://updates.{host}:{port}"));
        out.push(format!("https://updates.{host}"));
    }
    out
}

/// The first of these that answers as an xi-vault site (asked all at once).
pub fn first_site(candidates: &[String]) -> Option<(String, Index)> {
    let answers: Vec<_> = candidates
        .iter()
        .map(|url| {
            let url = url.clone();
            std::thread::spawn(move || fetch_index_within(&url, std::time::Duration::from_millis(1500)).ok().map(|i| (url, i)))
        })
        .collect();
    answers.into_iter().filter_map(|t| t.join().ok().flatten()).next()
}

/// Which published version is the one a server's CLIENT_VER names. CLIENT_VER is what the client
/// reports, a retail version; the site may hand out a customised one of it (custom DATs, published
/// as e.g. "30260904_1-custom.2"), and LandSandBoat compares only year and month ("302609"). So:
/// the version the site hands out when it is of that year and month, else that very name, else the
/// newest of that year and month.
pub fn pick_version(index: &Index, client_ver: &str) -> Option<String> {
    let month = client_ver.get(..6)?;
    let listed = |v: &str| index.versions.iter().any(|x| x.version == v);
    if index.current.get(..6) == Some(month) && listed(&index.current) {
        return Some(index.current.clone());
    }
    if listed(client_ver) {
        return Some(client_ver.to_string());
    }
    index.versions.iter().filter(|v| v.version.get(..6) == Some(month)).map(|v| v.version.clone()).max()
}

/// The port a server's game versions are looked for on when it names no address.
pub const DEFAULT_PORT: u16 = 54080;

/// The name a site's version goes by in this vault: its own, unless the vault has another version of
/// that name (another server's customised one, say), then "<name>@<site host>". So versions from
/// different servers never mix, however they are named.
pub fn local_name(vault: &Vault, iv: &IndexVersion, site: &str) -> String {
    let Ok(have) = vault.load(&iv.version) else { return iv.version.clone() };
    if iv.digest.is_empty() || have.digest() == iv.digest {
        return iv.version.clone();
    }
    let host: String = site
        .split("://")
        .last()
        .unwrap_or(site)
        .split('/')
        .next()
        .unwrap_or("")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' })
        .collect();
    format!("{}@{host}", iv.version)
}

/// Brings a version from a published site into the vault: its manifest, then every object the
/// vault lacks (a pack instead when there is one from a version the vault has, and it is smaller).
pub fn fetch(vault: &Vault, base: &str, version: Option<&str>, progress: Progress) -> Result<Manifest> {
    let index = fetch_index(base)?;
    let version = version.unwrap_or(&index.current).to_string();
    if !index.versions.iter().any(|v| v.version == version) {
        return Err(format!("{base} does not have version {version}"));
    }
    let mut s = String::new();
    get(&url_join(base, &format!("versions/{version}.json")))?.read_to_string(&mut s).map_err(err("manifest"))?;
    let mut m: Manifest = serde_json::from_str(&s).map_err(err("manifest"))?;
    m.check()?;
    if let Some(iv) = index.versions.iter().find(|v| v.version == version) {
        if !iv.digest.is_empty() && m.digest() != iv.digest {
            return Err(format!("{base}: version {version}'s manifest is not the one its index describes"));
        }
        m.version = local_name(vault, iv, base);
    }
    let mut need: BTreeMap<String, Entry> = BTreeMap::new();
    for e in &m.files {
        if !vault.has(e) {
            need.entry(e.sha256.clone()).or_insert_with(|| e.clone());
        }
    }
    // a difference-only site: the base version's files are the player's to bring
    if let Some(iv) = index.versions.iter().find(|v| v.version == version && !v.base.is_empty()) {
        let mut s = String::new();
        get(&url_join(base, &format!("versions/{}.json", iv.base)))?.read_to_string(&mut s).map_err(err("manifest"))?;
        let bm: Manifest = serde_json::from_str(&s).map_err(err("manifest"))?;
        let from_base: BTreeSet<&str> = bm.files.iter().map(|e| e.sha256.as_str()).collect();
        let lacking = need.keys().filter(|k| from_base.contains(k.as_str())).count();
        if lacking > 0 && iv.base.starts_with("installer-") {
            return Err(format!(
                "{lacking} files a fresh install of FINAL FANTASY XI has are missing or changed here, and the server does not \
                 host them (it hosts only what the official installer does not give). Repair or reinstall the game, then try again."
            ));
        }
        if lacking > 0 {
            return Err(format!(
                "The server publishes only what version {version} changes from version {}, and {lacking} files of {} are not \
                 here. Back up an install of version {} first (Launcher → Game files).",
                iv.base, iv.base, iv.base
            ));
        }
    }
    let need_bytes: u64 = need.values().map(|e| e.size).sum();
    if need.is_empty() {
        vault.save(&m)?;
        return Ok(m);
    }
    // a pack from a version this vault already has
    let have: BTreeSet<String> = vault.versions()?.into_iter().map(|m| m.version).collect();
    if let Some(p) = index.packs.iter().find(|p| p.to == version && have.contains(&p.from) && p.size < need_bytes) {
        unpack(vault, get(&url_join(base, &p.path))?, progress)?;
        if m.files.iter().all(|e| vault.has(e)) {
            vault.save(&m)?;
            return Ok(m);
        }
    }
    let done = AtomicU64::new(0);
    let pool = rayon::ThreadPoolBuilder::new().num_threads(8).build().map_err(err("threads"))?;
    pool.install(|| {
        need.par_iter().try_for_each(|(sha, e)| -> Result<()> {
            if vault.has(e) {
                return Ok(());
            }
            let url = url_join(base, &format!("objects/{}/{sha}.zst", &sha[..2]));
            let z = zstd::Decoder::new(get(&url)?).map_err(err(&url))?;
            vault.put_reader(z, sha)?;
            progress("fetch", done.fetch_add(e.size, Ordering::Relaxed) + e.size, need_bytes);
            Ok(())
        })
    })?;
    vault.save(&m)?;
    Ok(m)
}

/// Serves a published site over HTTP (GET and HEAD, no listings): a server operator's "one more
/// port". Blocks.
pub fn serve(site: &Path, addr: &str) -> Result<()> {
    let server = std::sync::Arc::new(tiny_http::Server::http(addr).map_err(err(addr))?);
    let site = site.canonicalize().map_err(err(site.display()))?;
    eprintln!("serving {} on http://{addr}/", site.display());
    // a player downloads 8 files at a time; so do many players
    let workers: Vec<_> = (0..32)
        .map(|_| {
            let (server, site) = (server.clone(), site.clone());
            std::thread::spawn(move || serve_requests(&server, &site))
        })
        .collect();
    for w in workers {
        let _ = w.join();
    }
    Ok(())
}

fn serve_requests(server: &tiny_http::Server, site: &Path) {
    for req in server.incoming_requests() {
        let url = req.url().split('?').next().unwrap_or("/").trim_start_matches('/').to_string();
        // '/'-separated plain names only: on Windows '\\' and "C:" are path syntax too
        let ok = !url.is_empty() && !url.split('/').any(|c| c.is_empty() || c == "." || c == ".." || c.contains(['\\', ':']));
        let path = site.join(&url);
        let resp = match (ok, File::open(&path)) {
            (true, Ok(f)) if path.is_file() => {
                let len = f.metadata().map(|m| m.len()).ok();
                let ctype = if url.ends_with(".json") { "application/json" } else { "application/octet-stream" };
                let h = tiny_http::Header::from_bytes("Content-Type", ctype).unwrap();
                req.respond(tiny_http::Response::new(200.into(), vec![h], f, len.map(|l| l as usize), None))
            }
            _ => req.respond(tiny_http::Response::from_string("not found").with_status_code(404)),
        };
        if let Err(e) = resp {
            eprintln!("{url}: {e}");
        }
    }
}

/// Bytes as the player reads them.
pub fn human(n: u64) -> String {
    let n = n as f64;
    if n >= 1e9 { format!("{:.2} GB", n / 1e9) } else if n >= 1e6 { format!("{:.1} MB", n / 1e6) } else if n >= 1e3 { format!("{:.0} KB", n / 1e3) } else { format!("{n} B") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iv(version: &str) -> IndexVersion {
        IndexVersion { version: version.into(), build: String::new(), files: 0, bytes: 0, base: String::new(), digest: String::new() }
    }

    #[test]
    fn paths_stay_inside_the_game_folder() {
        for ok in ["FFXiMain.dll", "ROM/0/0.DAT", "sound2/win/music/data/music001.bgw"] {
            assert!(plain_path(ok), "{ok}");
        }
        for bad in ["", "/etc/passwd", "../x", "ROM/../../x", "ROM//0.DAT", "./a", "ROM\\..\\x", "C:x", "a\0b"] {
            assert!(!plain_path(bad), "{bad:?}");
        }
    }

    #[test]
    fn a_manifest_with_a_bad_path_is_refused() {
        let e = |p: &str| Entry { path: p.into(), size: 1, sha256: "a".repeat(64) };
        let mut m = Manifest { format: FORMAT.into(), version: "v".into(), build: String::new(), ffximain_sha256: String::new(), ffxi_sha256: String::new(), created: 0, files: vec![e("ROM/0/0.DAT")] };
        assert!(m.check().is_ok());
        m.files.push(e("../evil.dll"));
        assert!(m.check().is_err());
    }

    #[test]
    fn the_version_the_server_names() {
        let index = |current: &str, versions: &[&str]| Index { format: FORMAT.into(), current: current.into(), versions: versions.iter().map(|v| iv(v)).collect(), packs: vec![] };
        // the customised version the site hands out, for the month CLIENT_VER names
        assert_eq!(pick_version(&index("30260904_1-custom.2", &["30260904_1", "30260904_1-custom.2"]), "30260904_1").as_deref(), Some("30260904_1-custom.2"));
        // the site hands out another month: the very name
        assert_eq!(pick_version(&index("30261001_0", &["30260805_0", "30261001_0"]), "30260805_0").as_deref(), Some("30260805_0"));
        // only the month matches: the newest of it
        assert_eq!(pick_version(&index("30261001_0", &["30260903_0", "30260904_1", "30261001_0"]), "30260900_0").as_deref(), Some("30260904_1"));
        assert_eq!(pick_version(&index("30261001_0", &["30261001_0"]), "30260805_0"), None);
    }

    #[test]
    fn a_digest_is_the_files_not_the_name() {
        let e = |p: &str, h: char| Entry { path: p.into(), size: 1, sha256: h.to_string().repeat(64) };
        let m = |v: &str, files: Vec<Entry>| Manifest { format: FORMAT.into(), version: v.into(), build: String::new(), ffximain_sha256: String::new(), ffxi_sha256: String::new(), created: 0, files };
        let a = m("x-custom.1", vec![e("a", '1'), e("b", '2')]);
        assert_eq!(a.digest(), m("other", vec![e("b", '2'), e("a", '1')]).digest());
        assert_ne!(a.digest(), m("x-custom.1", vec![e("a", '1'), e("b", '3')]).digest());
    }

    #[test]
    fn programs_are_told_from_data() {
        assert!(is_program("FFXiMain.dll") && is_program("Tools/polboot.EXE"));
        assert!(!is_program("ROM/0/0.DAT") && !is_program("patch.cfg"));
        assert!(!known_program(&"0".repeat(64)));
    }
}

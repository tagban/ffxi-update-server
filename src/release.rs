//! The server operator's double-click tool (xi-vault with no arguments, or `xi-vault release`).
//!
//! After PlayOnline updates the game on the operator's PC, it reads the install, asks the update
//! server what it hands out now, and when the install is newer, writes an update bundle of only the
//! files the server lacks. It publishes that into the server's site folder when it can reach it (on
//! this PC, or a Windows share such as \\\\VM\\xi-vault-site); or uploads it over SSH and has a
//! Linux server's xi-vault take it in (`xi-vault apply`); else it says how.
//!
//! Its settings are xi-release.json beside it: the game folder, the site folder or the server and
//! its SSH login.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use xi_vault::*;

#[derive(Serialize, Deserialize, Default)]
struct Settings {
    #[serde(default)]
    game: String,
    #[serde(default)]
    server: String,
    /// the server's site folder, here or on a share: published into directly (then `server` is not needed)
    #[serde(default)]
    site: String,
    /// the SSH login for the server ("root@update.example.com"); "" to upload by hand; absent: not asked yet
    #[serde(default)]
    upload: Option<String>,
    #[serde(default = "default_site")]
    remote_site: String,
    #[serde(default = "default_xi_vault")]
    remote_xi_vault: String,
}

fn default_site() -> String {
    "/srv/xi-vault/site".into()
}

fn default_xi_vault() -> String {
    "xi-vault".into()
}

pub struct Args {
    pub game: Option<PathBuf>,
    pub server: Option<String>,
    pub out: Option<PathBuf>,
    pub upload: Option<String>,
    pub site: Option<PathBuf>,
    /// publish under this name (a customised version: the server's own DATs over a retail one)
    pub name: Option<String>,
    pub current: bool,
}

/// The next free name for a customised version of `retail`: 30260904_1-custom.1, .2, ...
fn custom_name(index: &Index, retail: &str) -> String {
    let base = retail.split('-').next().unwrap_or(retail);
    let n = index
        .versions
        .iter()
        .filter_map(|v| v.version.strip_prefix(&format!("{base}-custom.")).and_then(|n| n.parse::<u32>().ok()))
        .max()
        .unwrap_or(0);
    format!("{base}-custom.{}", n + 1)
}

/// Where the site is read from: its folder, or its address.
enum Source {
    Dir(PathBuf),
    Url(String),
}

impl Source {
    fn index(&self) -> Result<Index> {
        match self {
            Source::Url(u) => fetch_index(u).map_err(|e| format!("could not reach the update server: {e}")),
            Source::Dir(d) => {
                let p = d.join("index.json");
                let i: Index = serde_json::from_str(&std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?)
                    .map_err(|e| format!("{}: {e}", p.display()))?;
                if i.format != FORMAT {
                    return Err(format!("{}: not an xi-vault site", d.display()));
                }
                Ok(i)
            }
        }
    }
    fn manifest(&self, version: &str) -> Result<Manifest> {
        match self {
            Source::Url(u) => fetch_manifest(u, version),
            Source::Dir(d) => {
                let p = d.join("versions").join(format!("{version}.json"));
                serde_json::from_str(&std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?).map_err(|e| format!("{}: {e}", p.display()))
            }
        }
    }
}

/// Beside the program (where a double-clicked tool keeps its things), else here.
pub(crate) fn home() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_else(|| PathBuf::from("."))
}

pub(crate) fn ask(q: &str) -> String {
    print!("{q} ");
    let _ = std::io::stdout().flush();
    let mut s = String::new();
    let _ = std::io::stdin().lock().read_line(&mut s);
    // a folder dragged into a Windows console comes quoted
    s.trim().trim_matches('"').trim().to_string()
}

pub(crate) fn yes_no(q: &str, default: bool) -> bool {
    let a = ask(&format!("{q} [{}]", if default { "Y/n" } else { "y/N" })).to_ascii_lowercase();
    if a.is_empty() { default } else { a.starts_with('y') }
}

pub(crate) fn is_game(p: &Path) -> bool {
    p.join("FFXiMain.dll").is_file()
}

/// Where PlayOnline put the game: beside this program, the registry, the usual folders.
pub(crate) fn find_game() -> Option<PathBuf> {
    let mut tries: Vec<PathBuf> = home().ancestors().map(Path::to_path_buf).collect();
    if cfg!(windows) {
        for region in ["PlayOnlineUS", "PlayOnlineEU", "PlayOnline"] {
            let key = format!(r"HKLM\SOFTWARE\WOW6432Node\{region}\InstallFolder");
            if let Ok(o) = Command::new("reg").args(["query", &key, "/v", "0001"]).output() {
                let text = String::from_utf8_lossy(&o.stdout).into_owned();
                if let Some(line) = text.lines().find(|l| l.contains("REG_SZ")) {
                    if let Some(p) = line.split("REG_SZ").nth(1) {
                        tries.push(PathBuf::from(p.trim()));
                    }
                }
            }
        }
        for pf in [r"C:\Program Files (x86)", r"C:\Program Files"] {
            tries.push(Path::new(pf).join(r"PlayOnline\SquareEnix\FINAL FANTASY XI"));
        }
    }
    tries.into_iter().find(|p| is_game(p))
}

/// "play.example.com" -> http://play.example.com:54080; an address with a scheme stays as it is.
pub(crate) fn server_url(s: &str) -> String {
    let s = s.trim().trim_end_matches('/');
    if s.contains("://") { s.to_string() } else if s.contains(':') { format!("http://{s}") } else { format!("http://{s}:{DEFAULT_PORT}") }
}

/// "30260805_0" as numbers, for which is newer.
pub(crate) fn version_key(v: &str) -> Option<(u64, u64)> {
    let (d, n) = v.split_once('_')?;
    Some((d.parse().ok()?, n.parse().ok()?))
}

pub(crate) fn rule() {
    println!("{}", "-".repeat(72));
}

pub fn release(a: Args, interactive: bool, p: Progress) -> Result<()> {
    let settings_path = home().join("xi-release.json");
    let mut s: Settings = std::fs::read_to_string(&settings_path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    if s.remote_site.is_empty() {
        s.remote_site = default_site();
    }
    if s.remote_xi_vault.is_empty() {
        s.remote_xi_vault = default_xi_vault();
    }
    println!("FINAL FANTASY XI: publish a game update to your server's players");
    rule();

    // the game
    let mut game = a.game.clone().or_else(|| Some(PathBuf::from(&s.game)).filter(|p| is_game(p))).or_else(find_game);
    while !game.as_deref().is_some_and(is_game) {
        if !interactive {
            return Err("give the FINAL FANTASY XI folder (--game)".into());
        }
        if let Some(g) = &game {
            println!("{} has no FFXiMain.dll.", g.display());
        }
        let g = ask("Where is your FINAL FANTASY XI folder? (drag it here, then press Enter)\n>");
        if g.is_empty() {
            return Err("no game folder".into());
        }
        game = Some(PathBuf::from(g));
    }
    let game = game.unwrap();
    s.game = game.to_string_lossy().into_owned();

    // where updates go
    if let Some(d) = &a.site {
        s.site = d.to_string_lossy().into_owned();
    }
    if let Some(u) = &a.upload {
        s.upload = Some(u.clone());
    }
    if interactive && s.site.is_empty() && s.upload.is_none() {
        println!("Where does your update server keep its files?");
        println!("  - its site folder: on this PC, or a share like \\\\VM\\xi-vault-site");
        println!("  - or the SSH login of a Linux server (like root@update.example.com)");
        let w = ask("Type or drag it here, or press Enter to copy updates there yourself:\n>");
        if w.contains('@') && !w.contains('\\') {
            s.upload = Some(w);
        } else if !w.is_empty() {
            s.site = w;
        } else {
            s.upload = Some(String::new());
        }
    }
    let site = Some(PathBuf::from(&s.site)).filter(|_| !s.site.is_empty());
    if let Some(d) = &site {
        if !d.join("index.json").is_file() {
            return Err(format!("{} is not an update server's site folder (no index.json); set it up with install-server.ps1 or install-server.sh", d.display()));
        }
    }

    // the server, when the site is not a folder here
    let mut server = a.server.clone().unwrap_or_else(|| s.server.clone());
    if server.is_empty() && site.is_none() {
        if !interactive {
            return Err("give the update server (--server)".into());
        }
        server = ask("Your update server (for example play.example.com):\n>");
        if server.is_empty() {
            return Err("no server".into());
        }
    }
    s.server = server;
    if interactive || a.game.is_some() || a.server.is_some() || a.site.is_some() {
        let _ = std::fs::write(&settings_path, serde_json::to_string_pretty(&s).unwrap());
    }
    let src = match &site {
        Some(d) => Source::Dir(d.clone()),
        None => Source::Url(server_url(&s.server)),
    };
    println!("\nGame:   {}", game.display());
    match &src {
        Source::Dir(d) => println!("Site:   {}", d.display()),
        Source::Url(u) => println!("Server: {u}"),
    }

    // what the server hands out
    let index = src.index()?;
    let current = index.current.clone();
    let (cm, bm, base) = if current.is_empty() {
        // a new site: the whole game goes in
        (None, None, String::new())
    } else {
        let entry = index.versions.iter().find(|v| v.version == current).ok_or(format!("the server's index names {current} but does not list it"))?;
        let bm = if entry.base.is_empty() { None } else { Some(src.manifest(&entry.base)?) };
        (Some(src.manifest(&current)?), bm, entry.base.clone())
    };
    println!("The server hands out: {}\n", if current.is_empty() { "nothing yet" } else { &current });

    // the install, quickly by patch.cfg, then every file
    if let (Some(mine), Some(theirs)) = (patched_version(&game).as_deref().and_then(version_key), version_key(&current)) {
        if mine < theirs {
            println!("\nYour game is {}, older than the server's {current}. Update it with PlayOnline first.", patched_version(&game).unwrap());
            return Ok(());
        }
    }
    let mut m = hash_install(&game, None, "reading your game files", p)?;
    println!("Your game is:        {}", m.version);
    rule();

    let empty = Manifest { format: FORMAT.into(), version: String::new(), build: String::new(), ffximain_sha256: String::new(), ffxi_sha256: String::new(), created: 0, files: Vec::new() };
    let cm = cm.unwrap_or(empty);
    let files = |m: &Manifest| m.files.iter().map(|e| (e.path.clone(), e.sha256.clone())).collect::<BTreeSet<_>>();
    if files(&m) == files(&cm) {
        println!("The server already hands out exactly this game. Nothing to do.");
        return Ok(());
    }
    // a name given: a customised version (the server's own DATs over a retail version)
    if let Some(n) = &a.name {
        m.version = n.trim().to_string();
    } else if index.versions.iter().any(|v| v.version == m.version) {
        // published under this name with other files: customised files here (or there)
        let pm = if m.version == current { cm.clone() } else { src.manifest(&m.version)? };
        if files(&pm) != files(&m) {
            let d = diff(&pm, &m);
            println!("Your game is {}, but {} files differ from the server's {}:", m.version, d.added.len() + d.changed.len() + d.removed.len(), m.version);
            for e in d.added.iter().chain(&d.changed).chain(&d.removed).take(10) {
                println!("  {}", e.path);
            }
            println!("If these are your server's own files (custom DATs), publish them as a customised version:");
            println!("players get them in place of the official ones.");
            let suggested = custom_name(&index, &m.version);
            if !interactive {
                return Err(format!("publish them under their own name: --name {suggested}"));
            }
            let n = ask(&format!("Name it, or press Enter for {suggested} (n to stop):\n>"));
            if n.eq_ignore_ascii_case("n") {
                return Ok(());
            }
            m.version = if n.is_empty() { suggested } else { n };
        }
    }
    if m.version.len() > 64 || m.version.is_empty() || !m.version.chars().all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c)) {
        return Err(format!("version name {:?}: letters, digits, _ - . only", m.version));
    }
    if a.name.is_some() && index.versions.iter().any(|v| v.version == m.version) {
        return Err(format!("the server has a version named {} already; pick another name", m.version));
    }
    let d = diff(&cm, &m);
    if index.versions.iter().any(|v| v.version == m.version) {
        println!("The server has {} already, but hands out {current}.", m.version);
        match &site {
            Some(d) if interactive && yes_no(&format!("Hand out {} now?", m.version), false) => {
                set_current(d, &m.version)?;
                println!("Done: your server hands out {} now. Set CLIENT_VER to match.", m.version);
            }
            Some(d) => println!("To hand it out: xi-vault current \"{}\" {}", d.display(), m.version),
            None => println!("To hand it out, on the server: {} current {} {}", s.remote_xi_vault, s.remote_site, m.version),
        }
        return Ok(());
    }
    println!("{} -> {}:", if current.is_empty() { "(nothing)" } else { &current }, m.version);
    println!("  {} new files, {} changed, {} removed", d.added.len(), d.changed.len(), d.removed.len());
    let mut folders: std::collections::BTreeMap<&str, (usize, u64)> = Default::default();
    for e in &d.new_objects {
        let top = e.path.split('/').next().filter(|_| e.path.contains('/')).unwrap_or("(game folder)");
        let f = folders.entry(top).or_default();
        f.0 += 1;
        f.1 += e.size;
    }
    for (k, (n, b)) in &folders {
        println!("    {k:<14} {n:>6} files  {:>10}", human(*b));
    }
    let foreign: Vec<&str> = m.files.iter().filter(|e| is_program(&e.path) && !known_program(&e.sha256)).map(|e| e.path.as_str()).collect();
    if !foreign.is_empty() {
        rule();
        println!("  NOTE: program files that are not a known Square Enix build: {}", foreign.join(", "));
        println!("  Players' updaters refuse programs they do not know (data can be customised, code not).");
        println!("  If this is a new official update, a launcher release lists it first.");
    }
    let dlls_changed = m.ffximain_sha256 != cm.ffximain_sha256 || m.ffxi_sha256 != cm.ffxi_sha256;
    let unknown = m.build.is_empty();
    if dlls_changed {
        println!("\n  The game's program changed (FFXiMain.dll / FFXi.dll).");
    }
    if unknown {
        rule();
        println!("  NOTE: the launcher does not know this FFXiMain.dll yet. Players on Mac and Linux");
        println!("  cannot play this version until a launcher update adds it. Upload it now if you like,");
        println!("  but do not hand it out (or change the server's CLIENT_VER) before that update.");
    }
    rule();

    let mut hosted: BTreeSet<String> = cm.files.iter().map(|e| e.sha256.clone()).collect();
    if let Some(b) = &bm {
        hosted.extend(b.files.iter().map(|e| e.sha256.clone()));
    }
    let ask_hand_out = |unknown: bool| {
        if interactive {
            println!("\nHanding it out means players' launchers download it on their next Play.");
            println!("Set CLIENT_VER in LandSandBoat's login.lua to {} at the same time.", m.version);
            if unknown {
                println!("(Not before the launcher knows this FFXiMain.dll: see the note above.)");
            }
            yes_no("Hand it out to players now?", false)
        } else {
            a.current
        }
    };

    // straight into the site folder
    if let Some(d) = &site {
        let hand_out = current.is_empty() || ask_hand_out(unknown);
        let (h, index) = publish_install(d, &game, &m, &hosted, &base, &current, hand_out, p)?;
        rule();
        println!("Published {} into {}: {} files, {}.", m.version, d.display(), h.objects, human(h.bytes));
        if index.current == m.version {
            println!("Your server hands out {} now. Players get it on their next Play.", m.version);
        } else {
            println!("It hands out {} still. To hand the new one out: xi-vault current \"{}\" {}", index.current, d.display(), m.version);
        }
        return Ok(());
    }

    // the bundle, for a server elsewhere
    let name = format!("ffxi-update-{}.tar", m.version);
    let out = a.out.clone().unwrap_or_else(home).join(&name);
    let h = make_bundle(&game, &m, &hosted, &base, &current, &out, p)?;
    let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    println!("Made {}\n  {} files, {}; the update file is {}", out.display(), h.objects, human(h.bytes), human(size));

    // to the server
    let login = s.upload.clone().unwrap_or_default();
    let apply = |current: bool| format!("{} apply {} <file>{}", s.remote_xi_vault, s.remote_site, if current { " --current" } else { "" });
    if login.is_empty() {
        println!("\nTo publish it: copy it to your server, then there run\n  {}", apply(false));
        println!("and to hand it out to players, add --current (and set CLIENT_VER in LandSandBoat's login.lua to {}).", m.version);
        return Ok(());
    }
    if interactive && !yes_no(&format!("\nUpload it to {login} now?"), true) {
        println!("Not uploaded. To publish it by hand: copy it there and run\n  {}", apply(false));
        return Ok(());
    }
    let hand_out = ask_hand_out(unknown);
    let remote = format!("/tmp/{name}");
    println!("> scp {} {login}:{remote}", out.display());
    let ok = Command::new("scp").arg(&out).arg(format!("{login}:{remote}")).status().map(|s| s.success()).unwrap_or(false);
    if !ok {
        return Err(format!("the upload to {login} failed (can you sign in with: ssh {login}?)"));
    }
    let cmd = format!(
        "{} apply '{}' '{remote}'{}; rc=$?; rm -f '{remote}'; exit $rc",
        s.remote_xi_vault,
        s.remote_site.replace('\'', ""),
        if hand_out { " --current" } else { "" }
    );
    println!("> ssh {login} {cmd}");
    let ok = Command::new("ssh").arg(&login).arg(&cmd).status().map(|s| s.success()).unwrap_or(false);
    if !ok {
        return Err(format!("the server did not take the update in (above); the bundle is still at {}", out.display()));
    }
    rule();
    if hand_out {
        println!("Done: your server hands out {} now. Players get it on their next Play.", m.version);
    } else {
        println!("Done: {} is on your server. To hand it out there: {} current {} {}", m.version, s.remote_xi_vault, s.remote_site, m.version);
    }
    Ok(())
}

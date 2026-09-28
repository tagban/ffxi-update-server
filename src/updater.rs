//! The player's updater (a copy of xi-vault named ffxi-updater, double-clicked; or `xi-vault update`).
//!
//! A server operator hands it to their players with ffxi-updater.json beside it, naming the server.
//! It asks the server's login server which client version it wants (LOGIN_VERSION_INFO) and where
//! it publishes it, and brings the player's own FINAL FANTASY XI folder to exactly that version, up
//! or down: for players who run the game from their install (xiloader, Ashita, Windower) rather than
//! the launcher, which keeps versions beside the install instead. Each file it replaces is kept in a
//! backup store first, so going back to a version this PC had never needs a site to host it.
//!
//! ffxi-updater.json: { "server": "play.example.com", "update_url": "", "game": "" }
//!   server      the game server (its login server; ":port" when not 54231)
//!   update_url  where its versions are, when the server does not say (else found as the launcher does)
//!   game        the FINAL FANTASY XI folder (else found, or asked, and remembered)

use crate::release::{ask, find_game, home, is_game, rule, version_key, yes_no};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use xi_vault::*;

#[derive(Serialize, Deserialize, Default)]
struct Settings {
    #[serde(default)]
    server: String,
    #[serde(default)]
    update_url: String,
    #[serde(default)]
    game: String,
}

pub struct Args {
    pub game: Option<PathBuf>,
    pub server: Option<String>,
    pub site: Option<String>,
    pub store: Option<PathBuf>,
}

/// Where the replaced files are kept: the user's data folder.
fn store_dir() -> PathBuf {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
    };
    base.unwrap_or_else(home).join("ffxi-updater")
}

/// Whether this can write in the game folder (Program Files needs an administrator on Windows).
fn writable(game: &Path) -> bool {
    let probe = game.join(".ffxi-updater-probe");
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// Runs this program again as an administrator (Windows asks the player), with the same arguments.
#[cfg(windows)]
fn run_elevated() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let args: Vec<String> = std::env::args().skip(1).map(|a| format!("'{}'", a.replace('\'', "''"))).collect();
    let list = if args.is_empty() { String::new() } else { format!(" -ArgumentList {}", args.join(",")) };
    let cmd = format!("Start-Process -FilePath '{}' -Verb RunAs{list}", exe.display().to_string().replace('\'', "''"));
    std::process::Command::new("powershell").args(["-NoProfile", "-Command", &cmd]).status().map(|s| s.success()).unwrap_or(false)
}

#[cfg(not(windows))]
fn run_elevated() -> bool {
    false
}

pub fn update(a: Args, interactive: bool, p: Progress) -> Result<()> {
    let settings_path = home().join("ffxi-updater.json");
    let mut s: Settings = std::fs::read_to_string(&settings_path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    if let Some(v) = &a.server {
        s.server = v.clone();
    }
    if let Some(v) = &a.site {
        s.update_url = v.clone();
    }
    if s.server.trim().is_empty() {
        if !interactive {
            return Err("give the game server (--server)".into());
        }
        s.server = ask("Which game server is this for? (for example play.example.com)\n>");
        if s.server.is_empty() {
            return Err("no server".into());
        }
    }
    println!("FINAL FANTASY XI updater for {}", s.server.trim());
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
    let _ = std::fs::write(&settings_path, serde_json::to_string_pretty(&s).unwrap());
    println!("Game: {}", game.display());

    // what the server wants, and where it is
    let info = server_info(s.server.trim(), std::time::Duration::from_secs(5)).ok();
    let named = info.as_ref().map(|i| i.update_url.trim().to_string()).filter(|u| !u.is_empty());
    let (url, index) = match named.or_else(|| Some(s.update_url.trim().to_string()).filter(|u| !u.is_empty())) {
        Some(u) => {
            let u = crate::release::server_url(&u);
            let index = fetch_index(&u).map_err(|e| format!("the update server ({u}) did not answer: {e}"))?;
            (u, index)
        }
        None => first_site(&site_candidates(s.server.trim())).ok_or(format!(
            "{} does not say where its game versions are, and none were found. Ask its operator for the address \
             (put it in ffxi-updater.json as \"update_url\").",
            s.server.trim()
        ))?,
    };
    let want = match info.as_ref().filter(|i| !i.client_ver.trim().is_empty()) {
        Some(i) => pick_version(&index, i.client_ver.trim())
            .ok_or(format!("{} wants version {}, which its update server does not have.", s.server.trim(), i.client_ver.trim()))?,
        None => index.current.clone(),
    };
    if want.is_empty() {
        return Err("the update server has no version to give".into());
    }
    println!("The server wants version {want}{}", if info.is_some() { "" } else { " (its update server says so)" });
    let m = fetch_manifest(&url, &want)?;

    // the install as it is
    println!();
    let have = hash_install(&game, None, "checking your game files", p)?;
    let now: std::collections::BTreeMap<&str, &str> = have.files.iter().map(|e| (e.path.as_str(), e.sha256.as_str())).collect();
    let todo: Vec<&Entry> = m.files.iter().filter(|e| now.get(e.path.as_str()) != Some(&e.sha256.as_str())).collect();
    rule();
    if todo.is_empty() {
        println!("Your game is version {want} already. Nothing to do.");
        return Ok(());
    }
    let bytes: u64 = todo.iter().map(|e| e.size).sum();
    let direction = match (version_key(&have.version), version_key(&want)) {
        (Some(h), Some(w)) if w < h => "back to",
        (Some(h), Some(w)) if w == h => "to exactly",
        _ => "to",
    };
    println!("Your game is {}. This changes it {direction} {want}:", have.version);
    println!("  {} files, {}", todo.len(), human(bytes));
    let store = Vault::open(a.store.clone().unwrap_or_else(store_dir))?;
    println!("  (each file it replaces is kept in {}, so you can come back)", store.root.display());

    if !writable(&game) {
        if interactive && cfg!(windows) {
            println!("\nChanging files in {} needs an administrator. Windows will ask.", game.display());
            if run_elevated() {
                return Ok(()); // the administrator's window carries on
            }
        }
        return Err(format!("cannot change files in {} (run this as an administrator)", game.display()));
    }
    if interactive && !yes_no("\nUpdate your game now? Close FINAL FANTASY XI and PlayOnline first.", true) {
        return Ok(());
    }
    let r = update_install(&game, &have, &m, &store, Some(&url), p)?;
    rule();
    println!(
        "Done: your game is version {want}. {} files written ({}), {} downloaded.",
        r.written,
        human(r.bytes),
        r.downloaded
    );
    if r.extra > 0 {
        println!("({} files that {want} does not use were left where they are.)", r.extra);
    }
    Ok(())
}

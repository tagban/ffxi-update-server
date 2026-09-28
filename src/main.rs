//! xi-vault: FINAL FANTASY XI client versions, whole (lib.rs).
//!
//!   xi-vault snapshot <FINAL FANTASY XI folder> [--name <version>]   record the install as a version
//!   xi-vault list                                                    the versions in the vault
//!   xi-vault diff <from> <to> [--files]                              what changed between two
//!   xi-vault pack <from> <to> <out.tar.zst> [--level n]              only what <to> adds, as one file
//!   xi-vault unpack <pack.tar.zst>                                   take a pack into the vault
//!   xi-vault verify <folder> <version> [--full] [--repair]           an install against a version
//!   xi-vault materialize <version> <out folder>                      a version as an install of its own
//!   xi-vault publish <out site> --current <v> <version>... [--packs] [--since v] static files
//!   xi-vault serve <site> [--listen 0.0.0.0:54080]                   serve them over HTTP
//!   xi-vault fetch <url> [--version v]                               a version from a site into the vault
//!   xi-vault release [--game <folder>] [--site <folder> | --server <url> [--upload <ssh login>]] [--current]
//!                                                                    an updated install to a server (release.rs)
//!   xi-vault update [--game <folder>] [--server <server>] [--site <url>]  an install to the server's version, in place
//!   xi-vault server-info <server[:port]>                               a LandSandBoat server's CLIENT_VER, VER_LOCK, UPDATE_URL
//!   xi-vault apply <site> <bundle.tar> [--current]                   take a release's bundle into a site
//!   xi-vault                                                         (no arguments: release, asking; named
//!                                                                    ffxi-updater: update, asking)
//!
//! The vault is --vault <dir>, else XI_VAULT, else ./xi-vault.

use clap::{Parser, Subcommand};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;
use xi_vault::*;

mod release;
mod updater;

#[derive(Parser)]
#[command(name = "xi-vault", about = "FINAL FANTASY XI client versions: snapshots, diffs, delta packs, a file server")]
struct Cli {
    /// The vault folder (else XI_VAULT, else ./xi-vault)
    #[arg(long, global = true)]
    vault: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Record an install as a version: every file hashed and stored once
    Snapshot { game: PathBuf, #[arg(long)] name: Option<String> },
    /// The versions in the vault
    List,
    /// Which version an install is
    Identify { game: PathBuf },
    /// What changed between two versions
    Diff { from: String, to: String, #[arg(long)] files: bool, #[arg(long)] json: bool },
    /// Only what <to> adds over <from>, as one zstd-compressed file
    Pack { from: String, to: String, out: PathBuf, #[arg(long, default_value_t = 19)] level: i32 },
    /// Take a pack into the vault
    Unpack { pack: PathBuf },
    /// Check an install against a version; --repair puts back what is missing or wrong
    Verify { game: PathBuf, version: String, #[arg(long)] full: bool, #[arg(long)] repair: bool },
    /// Put a version together as an install of its own
    Materialize { version: String, out: PathBuf },
    /// Static files for a server to hand out: index.json, manifests, objects, packs. --since <v>:
    /// only what the versions add to <v> (players bring the rest from their own install)
    Publish { out: PathBuf, #[arg(long)] current: String, versions: Vec<String>, #[arg(long)] packs: bool, #[arg(long)] since: Option<String> },
    /// Which version a published site's server wants (a rollback, or back): one it publishes already
    Current { site: PathBuf, version: String },
    /// Serve a published site over HTTP
    Serve { site: PathBuf, #[arg(long, default_value = "0.0.0.0:54080")] listen: String },
    /// Bring a version from a published site into the vault (default: the one it wants)
    Fetch { url: String, #[arg(long)] version: Option<String> },
    /// After PlayOnline updates the game: measure it against what the server hands out, make an
    /// update bundle of what the server lacks, and upload it over SSH (the double-click tool)
    Release {
        #[arg(long)]
        game: Option<PathBuf>,
        /// the update server ("play.example.com", or a full address)
        #[arg(long)]
        server: Option<String>,
        /// where the bundle goes (default: beside this program)
        #[arg(long)]
        out: Option<PathBuf>,
        /// the SSH login to upload with ("root@update.example.com"; "" not to)
        #[arg(long)]
        upload: Option<String>,
        /// the server's site folder, here or on a share: published into directly
        #[arg(long)]
        site: Option<PathBuf>,
        /// publish under this name: a customised version (the server's own DATs), e.g. 30260904_1-custom.1
        #[arg(long)]
        name: Option<String>,
        /// hand it out to players once it is on the server
        #[arg(long)]
        current: bool,
    },
    /// A player's own install to the version a server wants, up or down, in place (the updater)
    Update {
        #[arg(long)]
        game: Option<PathBuf>,
        /// the game server ("play.example.com", or with ":port" for its login server)
        #[arg(long)]
        server: Option<String>,
        /// where its versions are, when the server does not say
        #[arg(long)]
        site: Option<String>,
        /// where replaced files are kept (default: the user's data folder, ffxi-updater)
        #[arg(long)]
        store: Option<PathBuf>,
    },
    /// Which client version a LandSandBoat server wants, and where it publishes it (its login server)
    ServerInfo { server: String },
    /// Take an update bundle (from release) into a published site; --current: hand it out
    Apply { site: PathBuf, bundle: PathBuf, #[arg(long)] current: bool },
}

/// A one-line progress readout on stderr, at most ten times a second.
fn progress() -> impl Fn(&str, u64, u64) + Sync {
    let last = Mutex::new(Instant::now());
    move |what, done, total| {
        let mut l = last.lock().unwrap();
        if l.elapsed().as_millis() < 100 && done < total {
            return;
        }
        *l = Instant::now();
        let pct = if total > 0 { done as f64 * 100.0 / total as f64 } else { 100.0 };
        eprint!("\r{what}: {pct:5.1}%  {} of {}   ", human(done), human(total));
        if done >= total {
            eprintln!();
        }
        let _ = std::io::stderr().flush();
    }
}

fn summary(label: &str, list: &[Entry]) {
    let bytes: u64 = list.iter().map(|e| e.size).sum();
    println!("  {label:<10} {:>6} files  {:>10}", list.len(), human(bytes));
}

/// Sizes by top-level folder (ROM, ROM2, sound, ...).
fn by_folder(list: &[Entry]) {
    let mut m: std::collections::BTreeMap<&str, (usize, u64)> = Default::default();
    for e in list {
        let top = e.path.split('/').next().filter(|_| e.path.contains('/')).unwrap_or("(top)");
        let s = m.entry(top).or_default();
        s.0 += 1;
        s.1 += e.size;
    }
    for (k, (n, b)) in m {
        println!("    {k:<12} {n:>6} files  {:>10}", human(b));
    }
}

fn main() {
    let cli = Cli::parse();
    // no arguments: the double-clicked tool, which asks, and waits before its window closes
    let interactive = cli.cmd.is_none();
    let r = run(cli);
    if let Err(e) = &r {
        eprintln!("\nxi-vault: {e}");
    }
    if interactive {
        print!("\nPress Enter to close.");
        let _ = std::io::stdout().flush();
        let _ = std::io::stdin().read_line(&mut String::new());
    }
    if r.is_err() {
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let root = cli.vault.or_else(|| std::env::var_os("XI_VAULT").map(PathBuf::from)).unwrap_or_else(|| PathBuf::from("xi-vault"));
    let vault = || Vault::open(&root);
    let p = progress();
    let t = Instant::now();
    let Some(cmd) = cli.cmd else {
        // double-clicked: the updater when named so (what an operator hands players), else the publisher
        let name = std::env::current_exe().ok().and_then(|e| e.file_stem().map(|n| n.to_string_lossy().to_lowercase())).unwrap_or_default();
        if name.contains("updater") {
            let a = updater::Args { game: None, server: None, site: None, store: None };
            return updater::update(a, true, &p);
        }
        let a = release::Args { game: None, server: None, out: None, upload: None, site: None, name: None, current: false };
        return release::release(a, true, &p);
    };
    match cmd {
        Cmd::Snapshot { game, name } => {
            let v = vault()?;
            let m = v.snapshot(&game, name.as_deref(), &p)?;
            println!(
                "version {} (build {}): {} files, {} in {:.1}s\n  FFXiMain.dll {}\n  FFXi.dll     {}",
                m.version,
                if m.build.is_empty() { "unknown to the launcher" } else { &m.build },
                m.files.len(),
                human(m.bytes()),
                t.elapsed().as_secs_f64(),
                m.ffximain_sha256,
                m.ffxi_sha256
            );
        }
        Cmd::List => {
            for m in vault()?.versions()? {
                println!("{:<14} build {:<12} {:>6} files  {:>10}", m.version, if m.build.is_empty() { "?" } else { &m.build }, m.files.len(), human(m.bytes()));
            }
        }
        Cmd::Identify { game } => match identify(&vault()?, &game)? {
            Some(m) => println!("{}: version {} ({:.1}s)", game.display(), m.version, t.elapsed().as_secs_f64()),
            None => println!("{}: no version in the vault fits", game.display()),
        },
        Cmd::Diff { from, to, files, json } => {
            let v = vault()?;
            let d = diff(&v.load(&from)?, &v.load(&to)?);
            if json {
                println!("{}", serde_json::to_string_pretty(&d).unwrap());
                return Ok(());
            }
            println!("{from} -> {to}");
            summary("added", &d.added);
            summary("changed", &d.changed);
            summary("removed", &d.removed);
            println!("  to download: {} new objects, {}", d.new_objects.len(), human(d.new_bytes()));
            by_folder(&d.new_objects);
            if files {
                for (tag, l) in [("+", &d.added), ("~", &d.changed), ("-", &d.removed)] {
                    for e in l.iter() {
                        println!("{tag} {:<40} {:>10}", e.path, human(e.size));
                    }
                }
            }
        }
        Cmd::Pack { from, to, out, level } => {
            let (raw, packed) = pack(&vault()?, &from, &to, &out, level, &p)?;
            println!(
                "{}: {} of new files in {} ({:.0}%), {:.1}s",
                out.display(),
                human(raw),
                human(packed),
                if raw > 0 { packed as f64 * 100.0 / raw as f64 } else { 0.0 },
                t.elapsed().as_secs_f64()
            );
        }
        Cmd::Unpack { pack } => {
            let f = std::fs::File::open(&pack).map_err(|e| format!("{}: {e}", pack.display()))?;
            let m = unpack(&vault()?, f, &p)?;
            println!("version {}: in the vault", m.version);
        }
        Cmd::Verify { game, version, full, repair: fix } => {
            let v = vault()?;
            let m = v.load(&version)?;
            let c = verify(&game, &m, full, &p)?;
            summary("missing", &c.missing);
            summary("wrong", &c.wrong);
            println!("  extra      {:>6} files (not part of {version}; left alone)", c.extra.len());
            for e in c.missing.iter().chain(&c.wrong).take(50) {
                println!("    {}", e.path);
            }
            if fix && !(c.missing.is_empty() && c.wrong.is_empty()) {
                println!("repaired {} files", repair(&game, &v, &c)?);
            }
        }
        Cmd::Materialize { version, out } => {
            let m = vault()?.materialize(&version, &out, &p)?;
            println!("{}: version {}, {} files", out.display(), m.version, m.files.len());
        }
        Cmd::Publish { out, current, versions, packs, since } => {
            let versions = if versions.is_empty() { vec![current.clone()] } else { versions };
            let index = publish(&vault()?, &versions, &current, &out, packs, since.as_deref(), &p)?;
            println!("{}: {} versions, {} packs; the server wants {}", out.display(), index.versions.len(), index.packs.len(), index.current);
        }
        Cmd::Current { site, version } => {
            let index = set_current(&site, &version)?;
            let all: Vec<&str> = index.versions.iter().map(|v| v.version.as_str()).collect();
            println!("{}: the server wants {} (published: {})", site.display(), index.current, all.join(", "));
        }
        Cmd::Serve { site, listen } => serve(&site, &listen)?,
        Cmd::Fetch { url, version } => {
            let m = fetch(&vault()?, &url, version.as_deref(), &p)?;
            println!("version {}: in the vault ({} files)", m.version, m.files.len());
        }
        Cmd::Release { game, server, out, upload, site, name, current } => {
            release::release(release::Args { game, server, out, upload, site, name, current }, false, &p)?;
        }
        Cmd::Update { game, server, site, store } => {
            updater::update(updater::Args { game, server, site, store }, false, &p)?;
        }
        Cmd::ServerInfo { server } => {
            let i = server_info(&server, std::time::Duration::from_secs(5))?;
            println!("{}", serde_json::to_string_pretty(&i).unwrap());
        }
        Cmd::Apply { site, bundle, current } => {
            let f = std::fs::File::open(&bundle).map_err(|e| format!("{}: {e}", bundle.display()))?;
            let (h, index) = apply_bundle(&site, std::io::BufReader::new(f), current, &p)?;
            println!(
                "{}: version {} published ({} new files{}); the server hands out {}",
                site.display(),
                h.version,
                h.objects,
                if h.base.is_empty() { String::new() } else { format!(", players bring the rest from {}", h.base) },
                index.current
            );
        }
    }
    Ok(())
}

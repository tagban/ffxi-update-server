# xi-vault: game versions for servers and players

Setting up a server: [SERVER-OPERATORS.md](SERVER-OPERATORS.md). This is the reference.

`FFXiMain.dll` decides how the DAT files are read, so a **version** is the DLLs and the DATs together.
`xi-vault` records an install as one version (a manifest of every file's path, size and SHA-256) over a
content-addressed store, and never mixes the files of two versions. The launcher uses the same library
to back up the player's install, repair it, and bring the version a server wants.

It holds Square Enix's files. Nothing here ships them: a vault is made from your own install, and what
a server publishes is its operator's choice.

```
cargo build --release          # target/release/xi-vault
export XI_VAULT=/path/to/vault # or --vault <dir>; keep it on the same drive as the install (APFS: no extra space)
```

## Capturing an update

```
xi-vault snapshot "<...>/FINAL FANTASY XI"    # before updating: e.g. version 30260805_0
# ... update the game ...
xi-vault snapshot "<...>/FINAL FANTASY XI"    # after: e.g. 30260903_0
xi-vault diff 30260805_0 30260903_0 [--files] # what changed, by folder
xi-vault pack 30260805_0 30260903_0 30260805_0..30260903_0.tar.zst   # only what's new, compressed
```

The version name is the newest one PlayOnline's `patch.cfg` names (the install's real client version,
also for updates that change only DATs), else the one `known-builds.json` gives its build, else
`unknown-<hash>` (give one with `--name`). `USER/`, `TEMP/` and `SYS/` are the player's and are skipped.

## Publishing an update from the PC that has it (the double-click tool)

The game updates on a Windows PC (PlayOnline); the site is on the server. `ffxi-update-publisher.exe`
(this program, built for Windows; with no arguments it asks what it needs) does the rest:

1. finds the game (beside it, the registry, the usual folders, or asks) and the server (asks once);
2. reads the version the server hands out (`index.json`) and the install's version (`patch.cfg`);
   stops when the server has it already, or the install is older, or is that version with other files;
3. hashes the install, shows what changed, and warns when `FFXiMain.dll` is a build the launcher
   does not know yet (players cannot play it until a launcher update adds it: do not hand it out);
4. writes `ffxi-update-<version>.tar` beside it: the manifest and only the files the site lacks;
5. with an SSH login (asked once; Windows has `ssh`/`scp`), uploads it and runs on the server
   `xi-vault apply /srv/xi-vault/site <file> [--current]`, asking whether to hand it out now.

Its answers are kept in `xi-release.json` beside it (`game`, `server`, `upload`, `remote_site`,
`remote_xi_vault`). Without SSH, copy the file to the server and run `xi-vault apply` there.
`apply` checks every file against its hash and lists the version only once the site has all it
needs (or its base does); `--current` also hands it out. Set `CLIENT_VER` to match.
The same from a shell: `xi-vault release --game <folder> --server play.example.com [--upload root@update.example.com] [--current]`.

## Serving versions (a server operator)

Put the client your server wants in a vault (`snapshot` its folder), then publish and serve it:

```
xi-vault publish site --current 30260903_0 30260903_0 --since 30260805_0   # only what changed since 30260805_0
xi-vault serve site                                                         # http://0.0.0.0:54080/
```

- `--since <version>` hosts only the files that are not in that version, a few hundred MB instead
  of 15 GB. Players bring the rest from their own install: the launcher backs it up first and checks
  it is that version, and says so plainly when it is not. Leave it out to host every file.
- `--current` is the version your lobby wants: LandSandBoat's `CLIENT_VER` (`settings/default/login.lua`).
  A client older than it is refused with lobby error 331, which sends the launcher here.
- `--packs` also writes one-file deltas between the versions listed (a player with the older one
  downloads a single compressed file).
- When a player gives no address, the launcher looks on the game server itself at port **54080**,
  then at **`update.<server>`** (port 54080, or HTTPS), then `updates.<server>`: host the versions
  anywhere (a machine at home) and point `update.<server>` at it with a DNS record, and players
  configure nothing. `install-server.sh` sets a Linux machine up. Open it in the firewall. Any other address works too
  (players put it in the account's **Game updates address**), including a CDN or `https://`.

`site/` is plain static files, so any web server can serve it instead of `xi-vault serve`:

| file | what |
| --- | --- |
| `index.json` | the versions, the one the server wants (`current`), the packs |
| `versions/<version>.json` | each version's manifest (and the `--since` base's) |
| `objects/ab/<sha256>.zst` | every hosted file, by content, zstd-compressed |
| `packs/<from>..<to>.tar.zst` | a delta: only what `<to>` adds, one download |

As a service (`/etc/systemd/system/xi-vault.service`):

```
[Unit]
Description=FINAL FANTASY XI client versions for the launcher
After=network-online.target

[Service]
ExecStart=/usr/local/bin/xi-vault serve /srv/xi-vault/site --listen 0.0.0.0:54080
User=xi-vault
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

What the launcher does on Play: reads `index.json`; if the player lacks `current`, downloads only the
files they do not have (checking every one against its SHA-256), puts that version together beside
their install (theirs is never changed), makes the game for it, and plays.

**Safety.** The launcher only ever compiles a version whose `FFXiMain.dll` and `FFXi.dll` match a
build in `known-builds.json`; it refuses anything else. A site that is compromised, or a connection
that is tampered with, cannot get the launcher to run code it does not already know. A new client
version needs its metadata in the launcher first (`.claude/skills/game-version-update`).

## Rolling back

A site keeps every version it has published: publishing a newer one adds it and makes it `current`,
and the older ones stay, manifests and files. To take the server back:

```
xi-vault current site 30260805_0     # the server wants 30260805_0 again
```

and set LandSandBoat's `CLIENT_VER` back to match (with `VER_LOCK = 2`, a newer client is still let
in; with `1`, only the exact version). On their next Play, players' launchers switch that server back:
to their own install when it is that version, else to the copy put together before (nothing is
downloaded), else they download what they lack of it. `xi-vault current site <newer>` goes forward
again the same way. Leave a version out of the site only once no server will go back to it:
republishing without it keeps it listed until you remove its entry from `index.json`.

## Several servers, several versions

Each account plays the version its server wants. The launcher keeps each version the player has
been sent put together beside their install (`<vault>/installs/<version>/`, clones of the vault's
files, so a second version costs only what differs), and the game made for each build of
`FFXiMain.dll` side by side. Switching accounts switches versions; nothing is downloaded or made
twice. A version is recognised by its files, not only its DLLs, so two versions that differ in their
DATs alone are told apart.

## Other commands

```
xi-vault list                                  # versions in the vault
xi-vault identify <folder>                     # which version an install is
xi-vault current <site> <version>              # which version a published site's server wants
xi-vault verify <folder> <version> [--full] [--repair]
xi-vault materialize <version> <out folder>    # a version as an install of its own
xi-vault fetch <url> [--version v]             # from a published site into the vault
xi-vault unpack <pack.tar.zst>
```

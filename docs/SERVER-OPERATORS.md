# Running an update server

For the operator of a LandSandBoat (or other private) server who wants their players on the exact
client version the server needs, on any platform, with as little of their effort as possible.

An **update server** hosts versions of the game client. Your **game server** says which version it
wants. Players' **launchers** (ffxi-native, on macOS, Linux and Windows) and the **updater** (for
players who start the game with xiloader, Ashita or Windower) bring each player to that version,
upgrading or downgrading, and check every file by its SHA-256.

- [What a version is](#what-a-version-is): what has to be published besides the DAT files
- [Setting up an update server](#setting-up-an-update-server): Windows or Linux, one script
- [Publishing a new version](#publishing-a-new-version): after PlayOnline updates the game
- [Telling players which version to use](#telling-players-which-version-to-use): LandSandBoat's settings
- [Custom DATs](#custom-dats): your server's own files in place of the official ones
- [Rolling back](#rolling-back): to an older version
- [Players without the launcher](#players-without-the-launcher): the updater
- [Reference](#reference): commands, files, ports
- [Troubleshooting](#troubleshooting)

The program behind all of it is `xi-vault` (`ffxi-update-publisher.exe` and `ffxi-updater.exe` on
Windows are the same program). The [releases](../../../releases) have it for Windows, Linux and macOS.
Launchers: [ffxi-native](https://github.com/tagban/ffxi-native) does all of this on Play. Writing
your own: [PROTOCOL.md](PROTOCOL.md).

## What a version is

`FFXiMain.dll` decides how the DAT files are read. A version is therefore **the whole
`FINAL FANTASY XI` folder**, published and installed together. Never mix the DLLs of one version with
the DATs of another. Community "monthly update" archives often carry only the DATs and rely on
PlayOnline to fix up the rest afterwards; a player without PlayOnline ends up with a mismatched set.

| What | Where | Why it has to match |
| --- | --- | --- |
| `FFXiMain.dll`, `FFXi.dll`, `FFXiResource.dll` | top level | the game itself: rebuilt by Square Enix with (nearly) every monthly version |
| `FFXiVersions.dll`, `polboot.exe`, `xinputdll.dll`, `imeuidll.dll`, `ImeUiDll2.dll` | top level | the client's helpers |
| `VTABLE.DAT`, `FTABLE.DAT` | top level | the index of every DAT: which file holds what, and in which ROM folder |
| DAT files | `ROM/` to `ROM9/` | the game's data: zones, models, text, menus, music tables |
| sound | `sound/` to `sound9/` | music and effects |
| movies | `mov/` | cutscenes |
| tools | `Tools/`, `ToolsUS/`, `ToolsEU/` | the config and check tools |
| PlayOnline's records | `patch.cfg`, `patch2.cfg`, `patch.txt`, `patch.ver`, `patch.sin`, `patch.rst`, `file.txt` | which version each file came from; `xi-vault` names a version by `patch.cfg` |
| cursors and small files | `*.ani`, `*.cur`, `config.sys` | part of the install |

**Not** part of a version, and never published: `USER/` (each character's macros, key bindings and
settings), `TEMP/`, and `SYS/` (logs and state the game writes). Nor is the `PlayOnlineViewer`
folder beside the game, or the Windows registry. Players keep all of these.

A version is named by the newest version in its `patch.cfg`, e.g. `30260904_1`. That is the name to
use for `CLIENT_VER` too. LandSandBoat compares only the first six characters, the year and month
(`302609`).

**On hosting:** these are Square Enix's files. `xi-vault` publishes only what a version adds to a
version your players already have (`--since`), so a monthly update costs about 100 to 300 MB, and a
new player's full install never comes from you unless you choose to publish one.

## Setting up an update server

Any always-on machine works: a Windows or Linux PC or VM at home, or a small cloud server. Its
upload speed is what your players download at. You need one port open: **TCP 54080**.

### Windows

1. Download `ffxi-update-publisher.exe`, and `install-server.ps1` and `install-server.cmd` (from
   `scripts/`), into one
folder, e.g. `C:\xi-vault-kit`. Add a seed (`site-seed-<version>.zip`) if you have one (see
   [Starting from a version your players have](#starting-from-a-version-your-players-have)).
2. Double-click **`install-server.cmd`**. Windows asks for administrator rights; allow it.
3. A window stays open and ends with:
   ```
   xi-vault serves C:\xi-vault\site on port 54080 (it hands out: ...)
     this machine: http://192.168.1.50:54080/index.json
   ```

It installed `C:\xi-vault\xi-vault.exe`, the site folder `C:\xi-vault\site`, a scheduled task
(`xi-vault`) that serves the site from startup and restarts it if it stops, and a Windows Firewall
rule for port 54080. Running it again updates the program and keeps everything published.

### Linux

```
sudo sh scripts/install-server.sh ./xi-vault
```

It installs `/opt/xi-vault/xi-vault` (and `/usr/local/bin/xi-vault`), the site folder
`/srv/xi-vault/site`, a sandboxed systemd service (`xi-vault`) on port 54080, and opens the port in
ufw or firewalld when one is on.

### Reaching it from the internet

1. On your router, forward **TCP 54080** to the update server's local address.
2. Give it a name in your DNS: **`update.<your server's name>`**, e.g. `update.example.com`, pointing at
   your home address (a dynamic DNS service keeps it current if that address changes). The launcher
   and the updater look there by themselves. Or set `UPDATE_URL` (below) to any address.
3. From outside your network, `http://update.<your server>:54080/index.json` should show your versions.

Many home routers cannot reach their own public address from inside the house. If your own PCs
cannot open that address but outside players can, use the server's local address at home
(`http://192.168.1.50:54080` in the launcher's account, **Game updates address**).

### Starting from a version your players have

**New players.** When the publisher runs on an install PlayOnline updated, it reads `patch.cfg` to
tell which files are still exactly as Square Enix's installer left them (version `30210706_0`, the
2021 installer) and hosts everything else, once: about 1.6 GB (400 MB compressed) for 2026-09. So a
player with nothing but a fresh official install gets the current version from your server alone,
and a player on a recent version still downloads only what changed. Running it again on a version
the server already has offers to add this, if an older publisher did not.

A new site is empty. Seed it with the version your game server wants now, as a list of files your
players already have (no game files, a few MB), and every later update publishes only what it
changes:

```
xi-vault publish seed --current 30260805_0 30260805_0 --since 30260805_0
```

(from a PC with that version in its vault: `xi-vault snapshot "<FINAL FANTASY XI>"` records it). Zip
the `seed` folder's contents as `site-seed-30260805_0.zip` beside the installer, which uses it for a
new site only. Without a seed, the first version you publish is hosted whole (about 15 GB).

## Publishing a new version

When Square Enix releases an update:

1. On the PC with PlayOnline, update the game as usual. It can be the update server itself.
2. Double-click **`ffxi-update-publisher.exe`**. The first time, it asks:
   - the `FINAL FANTASY XI` folder, if it does not find it;
   - where the update server keeps its files: `C:\xi-vault\site` on the update server itself, a
     share such as `\\VM\xi-vault-site` (`install-server.ps1 -Share` makes one), or the SSH login
     of a Linux update server (`root@update.example.com`).
3. It reads the game (a few minutes), compares it with what the server has, and shows what changed.
4. **"Hand it out to players now?"** Yes makes it the version players are sent. Answer **No** if
   the game's code changed in a way the launcher does not support yet (it tells you), or if you want
   to change `CLIENT_VER` at the same moment yourself.
5. It publishes only the new files, checking each one, and says `Published <version>`.

Its answers are kept in `xi-release.json` beside it. From a shell:
`xi-vault release --game "<FINAL FANTASY XI>" --site <site folder> [--current]`.

**New game code.** The launcher runs the game by translating `FFXiMain.dll` for each platform, and it
only does so for DLL builds it knows (`known-builds.json`). When an update brings a DLL it does not
know, the publisher says so. Publish, but do not hand it out until a launcher release supports it
(open an issue with the version). Players who use the updater and play on Windows are not affected.

## Telling players which version to use

Your game server is the authority. In LandSandBoat's `settings/login.lua` (copy the setting from
`settings/default/login.lua`):

```lua
CLIENT_VER = '30260904_1',                       -- the version players must have
VER_LOCK   = 2,                                  -- 1: exactly it; 2: it or newer (by year and month)
UPDATE_URL = 'http://update.example.com:54080',  -- where you publish it
```

The launcher and the updater ask your login server (port 54231) for these three before anything
else, and bring each player to exactly `CLIENT_VER`, up or down, from `UPDATE_URL`.
`UPDATE_URL` and the request that reads it (`LOGIN_VERSION_INFO`) come with a LandSandBoat change
that is not merged upstream yet; see [lsb/](../lsb/). Without it, players' launchers use the version
your update server hands out (`xi-vault current`, below), found at `update.<server>` or port 54080
of the game server, so keep the two the same.

To move everyone to a new version: publish it, then set `CLIENT_VER` to it and restart the login
server (`xi_connect`). Players who press Play (or run the updater) are brought to it.

## Custom DATs

A private server can hand out its own DATs (and sounds, and other data) in place of the official
ones: new items, zones, text. Players get them like any update, and the launcher and the updater
keep them apart from another server's.

1. Put your files over a copy of the official version they build on (e.g. `30260904_1`).
2. Run the publisher on that copy. It sees files that differ from the official version and offers to
   publish them as a customised version, `30260904_1-custom.1` (or a name you type; from a shell,
   `--name`). Only your changed files are hosted.
3. Hand it out. Keep `CLIENT_VER` at the official version (`30260904_1`): it is what the game reports.
   Launchers pick the customised version the site hands out for that month (PROTOCOL.md, section 4).

Each change to your files is a new customised version (`-custom.2`, ...). Rolling back to an earlier
one, or to the official version, works like any rollback.

**Only data.** Program files (`.dll`, `.exe`) must be Square Enix's own builds: the updater refuses
a version with any other program file, and the ffxi-native launcher runs only `FFXiMain.dll` builds
it knows. The publisher warns you when a program file is not an official one.

## Rolling back

An update site keeps every version it has published. To go back:

1. Set `CLIENT_VER` to the older version and restart `xi_connect`.
2. On the update server: `xi-vault current <site> <older version>`.

Players go back on their next Play (the launcher keeps each version beside the install and switches,
usually downloading nothing), or with the updater (it restores the replaced files from its own backup
of what that PC had, and downloads only what it lacks). `xi-vault current <site> <newer version>`
goes forward again. With `VER_LOCK = 2`, players on a newer client can still log in, so set
`VER_LOCK = 1` when the older version is required.

## Players without the launcher

For players who start the game from their own install on Windows (xiloader, Ashita, Windower), hand
out **`ffxi-updater.exe`** (the same program, named so) with an **`ffxi-updater.json`** beside it:

```json
{ "server": "play.example.com" }
```

(`"play.example.com:54231"` if your login server is on another port; `"update_url"` when your server does not
answer the version request). The player double-clicks it. It finds their game, asks your server
which version it wants, shows what it will change, asks for administrator rights if the game is in
Program Files, and changes the install, up or down. Every file it replaces is kept in
`%LOCALAPPDATA%\ffxi-updater`, so going back to a version that PC had never needs your server.
Close the game and PlayOnline first. PlayOnline itself would update the install to Square Enix's
latest again: players on your server start the game with their loader, not PlayOnline.

## Reference

| Command | What |
| --- | --- |
| `xi-vault release` | publish an updated install (the publisher; double-click, or with `--game`, `--site`/`--server`, `--current`) |
| `xi-vault update` | bring an install to the server's version (the updater; `--game`, `--server`, `--site`) |
| `xi-vault serve <site> [--listen 0.0.0.0:54080]` | serve a site |
| `xi-vault current <site> <version>` | which version a site hands out (rolling back, or forward) |
| `xi-vault server-info <server>` | what a game server says: `CLIENT_VER`, `VER_LOCK`, `UPDATE_URL` |
| `xi-vault apply <site> <bundle.tar> [--current]` | take in an update made elsewhere (the publisher's SSH upload does this) |
| `xi-vault snapshot`, `diff`, `publish`, `fetch`, `verify`, `list` | the vault: see [REFERENCE.md](REFERENCE.md) |

A site is plain static files (`index.json`, `versions/<version>.json`, `objects/ab/<sha256>.zst`), so
any web server or CDN can serve a copy of it too.

| Port | What |
| --- | --- |
| 54080 TCP | the update server (xi-vault serve) |
| 54231 TCP | LandSandBoat's login server: the version request, and signing in |

## Troubleshooting

- **"Run this in an administrator PowerShell"** (an older `install-server.ps1`): double-click
  `install-server.cmd` instead; it asks for the rights itself.
- **Windows SmartScreen** stops `ffxi-update-publisher.exe`: **More info**, then **Run anyway**.
- **Players cannot reach `update.<server>`** but you can locally: the router's port forward, or the
  DNS record. Outside your network, open `http://update.<server>:54080/index.json`.
- **You cannot reach it from home** but players can: see the router note under
  [Reaching it from the internet](#reaching-it-from-the-internet).
- **"The server wants version X, which its game updates address does not publish"**: publish X, or
  set `CLIENT_VER` to a version the site has (`xi-vault list` on a vault, or the site's `index.json`).
- **"files ... are neither here nor in the backups"** (the updater, going back): that version's
  files are not on the update server and this PC never had them. Publish that version whole on the
  server (`xi-vault publish` without `--since`), or have the player reinstall.

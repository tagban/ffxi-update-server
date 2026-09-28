# The update protocol

How a launcher or updater learns which FINAL FANTASY XI client version a private server wants, and
brings a player to it, from any update server. Everything here is plain HTTP(S), JSON and zstd; the
`xi-vault` crate in this repository is one implementation (the ffxi-native launcher and the updater
use it), and this document is enough to write another.

Format version: **`xi-vault/1`**. Terms: a **version** is a whole `FINAL FANTASY XI` folder (see
[What a version is](SERVER-OPERATORS.md#what-a-version-is)); a **site** is an update server's files;
the **game server** is the private server (LandSandBoat) the player plays on.

1. [Asking the game server](#1-asking-the-game-server)
2. [Finding the site](#2-finding-the-site)
3. [The site](#3-the-site)
4. [Choosing the version](#4-choosing-the-version)
5. [Getting a version](#5-getting-a-version)
6. [Putting it in place](#6-putting-it-in-place)
7. [Signing in with the right loader](#7-signing-in-with-the-right-loader)
8. [Security](#8-security)

## 1. Asking the game server

The game server is the authority on the version. A LandSandBoat login server with the
`LOGIN_VERSION_INFO` request ([lsb/](../lsb/)) answers it on its auth port, **TCP 54231**, over TLS
(1.2 or 1.3; self-signed certificates, as xiloader accepts), without an account:

```
→ {"command":64,"version":[2,2,0]}
← {"client_ver":"30260904_1","ver_lock":2,"update_url":"http://update.example.com:54080","loader_version":[2,2,0],"profile_port":51220}
```

| Field | Meaning |
| --- | --- |
| `client_ver` | LandSandBoat's `CLIENT_VER`: the retail client version the server expects the game to report |
| `ver_lock` | `0` any version, `1` exactly `client_ver`, `2` `client_ver` or newer. LandSandBoat compares only the first six characters (year and month, `302609`) |
| `update_url` | the site that publishes the versions; empty when the server names none |
| `loader_version` | the xiloader protocol its login server speaks: `[major, minor, patch]`, major.minor must match ([section 7](#7-signing-in-with-the-right-loader)) |
| `profile_port` | its PlayOnline profile server (`xi_profile`, loader 2.2): where polcore's friend list and messages go |

Send the request in one write; read until the reply parses as JSON (it may be followed by NUL
padding) or the connection closes. Give up after a couple of seconds: a login server without the
request does not answer it, and the connection closes or times out. Then fall back to section 2 and
use the version the site hands out.

## 2. Finding the site

In order, the first that answers:

1. `update_url` from the game server (section 1);
2. an address the player gave (a setting);
3. `http://<server>:54080`, then `http://update.<server>:54080`, `https://update.<server>`,
   `http://updates.<server>:54080`, `https://updates.<server>`, all asked at once with a short
   timeout (1.5 s), the first in this order that serves an `index.json` of format `xi-vault/1`.

`<server>` is the game server's host name. Skip the `update.` names for an IP address. Remember what
was found, and look again when a remembered address stops answering.

## 3. The site

Static files, served by `xi-vault serve` or any web server or CDN:

```
index.json                    the versions, and the one handed out
versions/<version>.json       each version's manifest
objects/<ab>/<sha256>.zst     each file of a version, by content, zstd-compressed
packs/<from>..<to>.tar.zst    optional: a delta in one download
```

### index.json

```json
{
  "format": "xi-vault/1",
  "current": "30260904_1",
  "versions": [
    { "version": "30260805_0", "build": "2026-08-22", "files": 65046, "bytes": 14892300155,
      "base": "30260805_0", "digest": "9c1f…" },
    { "version": "30260904_1", "build": "2026-09-03", "files": 65053, "bytes": 15046064100,
      "base": "30260805_0", "digest": "41d0…" }
  ],
  "packs": []
}
```

| Field | Meaning |
| --- | --- |
| `current` | the version the site hands out (used when the game server does not say, section 4) |
| `version` | its name: the newest version in its `patch.cfg`, or an operator's name for a customised version (`30260904_1-custom.1`); letters, digits, `_ - .` |
| `build` | the ffxi-native launcher's label for its `FFXiMain.dll` (`known-builds.json`); empty when unknown |
| `files`, `bytes` | its file count and total size |
| `base` | optional: the site hosts only the files this version does **not** share with `base`; the player brings the rest from their own install of `base`. `base` equal to the version itself: the site hosts none of its files |
| `digest` | optional: the SHA-256 of the manifest's sorted `path NUL sha256 LF` lines. What the version is, whatever it is called |

### versions/&lt;version&gt;.json (a manifest)

```json
{
  "format": "xi-vault/1",
  "version": "30260904_1",
  "build": "2026-09-03",
  "ffximain_sha256": "f2245d1c…",
  "ffxi_sha256": "9053d410…",
  "created": 1790600000,
  "files": [
    { "path": "FFXi.dll", "size": 93251, "sha256": "…" },
    { "path": "ROM/0/0.DAT", "size": 1234, "sha256": "…" }
  ]
}
```

`files` is sorted by path. A `path` is relative to the `FINAL FANTASY XI` folder, `/`-separated, in
the case the install has. It never includes `USER/`, `TEMP/` or `SYS/` (the player's) or
`.DS_Store`, `Thumbs.db`, `desktop.ini`. `sha256` is lowercase hex.

### objects

`objects/<first two hex digits>/<sha256>.zst`: a zstd frame of the file's bytes. The SHA-256 of the
decompressed bytes is the name. One object serves every path and every version with that content.

### packs (optional)

`packs/<from>..<to>.tar.zst`, listed in `index.json` `packs` with its `size` and `sha256`: a
zstd-compressed tar whose first entry, `xipack.json`, is `{format, from, to, manifest, objects}` (the
whole manifest of `to`, and the entries it adds), followed by `objects/<sha256>` entries (raw bytes).
Use it when the player has `from` and it is smaller than the objects they would fetch one by one.

## 4. Choosing the version

With `client_ver` from the game server (it is a retail version; the site may hand out a customised
one of it):

1. the site's `current`, when its first six characters equal `client_ver`'s;
2. else the version named exactly `client_ver`;
3. else the newest (by name) of the versions whose first six characters equal `client_ver`'s;
4. else stop: the site does not publish what the server wants. Say so.

Without `client_ver`: the site's `current`. Bring the player to exactly that version, whether it is
newer or older than what they have. The server decides, including rollbacks.

## 5. Getting a version

1. Fetch `versions/<version>.json`. Check it (section 8) and, when `index.json` gives a `digest`,
   that it matches.
2. For each file, the content is needed unless the player already has it: in their install (any
   path with that SHA-256) or in a local store of objects (from an earlier version).
3. If the version has a `base`, the files it shares with `base` are not on the site. They must come
   from the player's side; if they are missing there, stop and say the player needs a whole install
   of `base` first (back it up, or reinstall).
4. Download each remaining object, decompress it, and check its SHA-256 before using it. Eight at a
   time is plenty.

Hash the player's install once per run (a 15 GB install takes under a minute on an SSD). Hashing by
size and date is not enough: many DATs keep their size across versions.

## 6. Putting it in place

Two ways, both in `xi-vault`:

- **Beside the install** (the launcher, `Vault::materialize`): each version is its own folder of
  copies (clones on APFS, so free), and each server's account plays from the folder of its version.
  The player's install is never changed. Link the copy's `USER` folder to the install's, so macros
  and settings are the same on every version (`share_player_dirs`).
- **In place** (the updater, `update_install`): the install becomes the version. Before replacing a
  file, keep it in the local store, so going back never needs the site. Write each file to a
  temporary name, check its SHA-256, then rename over the old one. Leave files the version does not
  have where they are (unused), and never touch `USER`, `TEMP` or `SYS`.

**Keep servers apart.** Two servers can publish different customised versions under the same name.
When a local version of that name exists with another `digest`, keep the new one as
`<version>@<site host>` (`local_name`).

## 7. Signing in with the right loader

LandSandBoat's login server accepts only the xiloader protocol whose major.minor it was built for
(`loader_version`, section 1). Sign in with that variant:

| Loader | Servers | What differs |
| --- | --- | --- |
| 2.1 | LandSandBoat until mid-2026 | the loader answers polcore's profile connection itself (port 51220, locally) |
| 2.2 | current LandSandBoat | TLS 1.3 required on the auth port; polcore's PlayOnline connections (profile 51220, IRC) are relayed to the server's `xi_profile` over TLS, each opened with the account id and session hash |

The auth request (`command` 16) and the login data connection (54230) are the same in both.
Without `loader_version`, try the newest variant you speak. A server that wants another refuses
with `{"error_message":"Your xiloader is too old.\nPlease update to version '2.1.x'. …"}`; parse the
version after `version '` and sign in once more with that variant.

## 8. Security

- **Check everything from a site.** A manifest's paths must be plain relative paths: no empty, `.` or
  `..` component, no `\`, `:` or NUL, at most 260 characters (`plain_path`). Refuse a manifest that
  has another. Check each object's SHA-256 after decompressing. A site, or anything between it and
  the player, is untrusted.
- **Data may be customised; code may not.** A private server may hand out its own DATs, sounds and
  other data in place of the official ones. Program files (`.dll`, `.exe` and the like) must be ones
  Square Enix shipped: their SHA-256 must be listed in `known-builds.json` (`known_program`). The
  updater refuses a version with any other program file. A launcher that runs the game natively
  (ffxi-native) only runs `FFXiMain.dll` builds it knows.
- **The game server's answers** travel over TLS without certificate checks, as xiloader signs in.
  What they carry is public and only picks among versions, which are then checked as above.
- **Never mix** a version's files with another's (section 2 of the operator guide): `FFXiMain.dll`
  decides how the DATs are read.

## Implementations

- `xi-vault` (this repository): the site, the publisher, the updater, and the library below both.
  `server_info`, `site_candidates`/`first_site`, `pick_version`, `fetch`, `local_name`,
  `materialize`, `share_player_dirs`, `update_install`, `Manifest::check`, `known_program`.
- [ffxi-native](https://github.com/tagban/ffxi-native): a launcher that runs FINAL FANTASY XI natively
  on macOS, Linux and Windows; it does all of the above on Play.

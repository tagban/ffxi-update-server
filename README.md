# ffxi-update-server

Get every player on a private FINAL FANTASY XI server onto the exact client version the server needs,
automatically, on Windows, macOS and Linux: upgrades, rollbacks, and the server's own custom DATs.

- **Update server**: hosts client versions as plain static files. Only what each version changes
  is hosted (about 100 to 500 MB a month).
- **Publisher**: after PlayOnline updates the game on your PC, double-click it. It publishes what
  changed, checking every file.
- **Updater**: for players who start the game with xiloader, Ashita or Windower. It brings their own
  install to the server's version, up or down, and keeps a backup of every file it replaces.
- **LandSandBoat patch**: the login server tells launchers which version it wants, where to get it,
  and which xiloader protocol it speaks.
- **A documented protocol**, so any launcher can do the same.

One program does all of it: `xi-vault` (on Windows also named `ffxi-update-publisher.exe` and
`ffxi-updater.exe`; the name decides what a double-click does).

## Documentation

| | |
| --- | --- |
| [Running an update server](docs/SERVER-OPERATORS.md) | for server operators: set up (Windows or Linux), publish, point players at it, roll back, custom DATs |
| [The update protocol](docs/PROTOCOL.md) | for launcher authors: how to ask a server, find its site, choose and fetch a version, check it |
| [LandSandBoat patch](lsb/README.md) | `LOGIN_VERSION_INFO`: the version request on the login server |
| [xi-vault reference](docs/REFERENCE.md) | every command, the site layout, the vault |

## Quick start (server operator, Windows)

1. From the [releases](../../releases), put `ffxi-update-publisher.exe`, `install-server.ps1` and
   `install-server.cmd` in one folder. Double-click `install-server.cmd`.
2. Forward TCP 54080 to that PC, and point `update.<your server>` at it in DNS.
3. After each game update, double-click `ffxi-update-publisher.exe` and give it the site folder
   `C:\xi-vault\site`.
4. In LandSandBoat's `settings/login.lua`: `CLIENT_VER` to the new version, and (with the patch)
   `UPDATE_URL` to your update server.

Linux: `sudo sh scripts/install-server.sh ./xi-vault`. Details in the
[operator guide](docs/SERVER-OPERATORS.md).

## Launchers that use it

- [ffxi-native](https://github.com/tagban/ffxi-native): runs FINAL FANTASY XI natively on macOS,
  Linux and Windows. It asks the server, fetches the version, and plays it, all on Play.

Adding it to another launcher: [docs/PROTOCOL.md](docs/PROTOCOL.md), or use the `xi-vault` Rust
crate directly (`xi_vault::server_info`, `pick_version`, `fetch`, `update_install`, ...).

## Building

```
cargo build --release          # target/release/xi-vault
```

Rust 1.75 or newer. No other dependencies.

## Game files

This repository holds no Square Enix files, only code, and hashes of the official program files
(`known-builds.json`) so the updater can tell official programs from anything else. What an update
server hosts is its operator's choice: by default only the difference between versions your
players already have.

## License

MIT. See [LICENSE](LICENSE).

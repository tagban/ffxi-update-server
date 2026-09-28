# LandSandBoat: the client version request

`login-version-info.patch` adds to LandSandBoat's login server (`xi_connect`, the auth port 54231)
a request anyone can make without an account, `LOGIN_VERSION_INFO` (command `0x40`), answered with
the server's `CLIENT_VER`, `VER_LOCK`, the xiloader protocol its login server speaks
(`loader_version`, so a launcher signs in the matching way), and a new setting, `UPDATE_URL`: where
the server publishes that client version. The launcher and the updater ask it first, so the game server decides which
version its players are brought to, up or down ([docs/SERVER-OPERATORS.md](../docs/SERVER-OPERATORS.md), [docs/PROTOCOL.md](../docs/PROTOCOL.md)).

```
cd <LandSandBoat>
git apply <ffxi-update-server>/lsb/login-version-info.patch
```

then set `UPDATE_URL` in `settings/login.lua` and rebuild `xi_connect`.

The request, over TLS as xiloader connects (self-signed certificates are fine):

```
{"command":64,"version":[2,1,2]}
```

The reply:

```
{"client_ver":"30260904_1","loader_version":[2,2,0],"update_url":"http://update.example.com:54080","ver_lock":2}
```

A LandSandBoat without the patch does not answer it (the connection closes or times out); the
launcher then finds the update server at `update.<server>` or port 54080, and uses the version it
hands out. `xi-vault server-info <server>` shows what a server answers.

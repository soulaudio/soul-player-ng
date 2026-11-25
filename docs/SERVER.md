# Soul Server

`soul-server` serves a music library to Soul Player over the network. It indexes your
folders, hands the client the same `.soulidx` the desktop app builds locally, and streams
**the original file bytes**: no transcoding, no re-encoding, so a remote track is as
bit-perfect as a local one. The client decodes it with exactly the same pipeline.

It is a single binary plus one TOML file. It never writes to your music folders.

## Build

The server is in the root workspace, on **stable** Rust, so no nightly is needed:

```sh
cargo build --release -p soul-server
# → target/release/soul-server
```

On Linux the build needs `pkg-config`, `libasound2-dev`, `libdbus-1-dev`, and
`libssl-dev` (see [DEVELOP.md](DEVELOP.md); the server itself doesn't open an audio
device, but it shares a workspace with crates that do).

## Configure

Copy [`soul-server.example.toml`](../soul-server.example.toml) and edit it:

```toml
bind = "127.0.0.1:8080"
roots = ["/srv/music"]
db = "/var/lib/soul-server/users.json"
# public_url = "https://music.example.com"
```

| Key | Meaning |
|---|---|
| `bind` | Address to listen on. `127.0.0.1` is local-only, which is what you want when a reverse proxy sits in front. `0.0.0.0` accepts connections from the network. |
| `roots` | Library directories to index and serve. Read-only. |
| `db` | Users, hashed credentials, and refresh tokens (a JSON file written atomically). The JWT signing key, `jwt_key.der`, is created next to it. |
| `public_url` | The base URL clients reach the server on, when that differs from `bind`. See [Behind a reverse proxy](#behind-a-reverse-proxy). |

## Add a user

There is no self-registration and no default account: an empty `db` means nobody can log
in. Create accounts from the CLI:

```sh
soul-server --config soul-server.toml user add alice
# Password for alice: ‹prompted, not echoed›
```

`--password` takes it non-interactively, at the usual cost of landing in your shell
history and the process table. Passwords are stored as Argon2id hashes; refresh tokens
are stored only as SHA-256 digests, so the `db` file cannot be replayed as a credential
if it leaks. It is still the server's identity, so keep it readable only by the service
user.

## Run

```sh
soul-server --config soul-server.toml serve
```

The library is indexed at startup, so first boot over a large collection takes a while.

### As a systemd service

```ini
# /etc/systemd/system/soul-server.service
[Unit]
Description=Soul Audio remote source server
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=/usr/local/bin/soul-server --config /etc/soul-server.toml serve
DynamicUser=yes
StateDirectory=soul-server
# Needs read access to your library; everything else stays locked down.
ReadOnlyPaths=/srv/music
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
NoNewPrivileges=yes
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

With `DynamicUser=yes` and `StateDirectory=soul-server`, point `db` at
`/var/lib/soul-server/users.json`, which systemd creates and owns for you.

```sh
sudo systemctl enable --now soul-server
```

Run `user add` as the same user that owns the state directory, or the server won't be
able to read what you wrote.

## Putting it on the network

**`soul-server` speaks plain HTTP and does not terminate TLS.** Exposing it directly to
the internet would put passwords and media in the clear. Three sane deployments:

- **LAN only.** `bind = "0.0.0.0:8080"`, no proxy. Fine inside a trusted network.
- **Over a VPN** (WireGuard, Tailscale). Bind to the VPN interface. No proxy needed,
  nothing exposed.
- **Public, behind a TLS reverse proxy.** The setup below.

### Behind a reverse proxy

The client discovers the login endpoints from
`GET /.well-known/openid-configuration`, and the server builds every URL in that
document from its issuer. Unset, the issuer is `http://{bind}`, your *internal* address,
which the client's browser cannot reach. So behind a proxy, `public_url` is **required**:

```toml
bind = "127.0.0.1:8080"
public_url = "https://music.example.com"
```

Then proxy the whole origin, not a subpath. `/.well-known/`, `/authorize`, `/token`,
`/revoke`, `/jwks`, and `/v0/` all have to land on the server at the same paths:

```nginx
server {
    listen 443 ssl;
    server_name music.example.com;
    # ssl_certificate … (certbot, or whatever issues your cert)

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_set_header Host $host;
        # Streaming: don't buffer whole tracks, and let long reads finish.
        proxy_buffering off;
        proxy_read_timeout 300s;
    }
}
```

Caddy needs no more than `music.example.com { reverse_proxy 127.0.0.1:8080 }`.

Changing `public_url` later invalidates tokens already issued (the issuer is stamped
into each one and checked on every request), so every client logs in again. Pick the
hostname before you hand the server to anyone.

## Connect from the player

In Soul Player: **Settings → Sources → Add → Remote**, enter the server URL
(`https://music.example.com`), and log in. The login opens your **system browser**:
the app never sees your password. The refresh token is kept in the OS keychain.

The remote library then merges into the normal Albums / Artists / Tracks views alongside
your local folders.

## The wire protocol

Useful if you want to write another client. `/v0/*` requires
`Authorization: Bearer <access token>`; the auth endpoints are unauthenticated.

| Method & path | What it does |
|---|---|
| `GET /v0/ping` | Health, server version, library version/etag. Unauthenticated. |
| `GET /v0/index` | The library `.soulidx`. Sends `ETag`; honours `If-None-Match` with `304`. |
| `GET /v0/track/{id}` | The original file bytes. Supports `Range` (`206`), `Content-Type` from the codec, no transcode. |
| `GET /v0/art/{album_id}` | Cover-art bytes; `404` when the album has none. |
| `GET /.well-known/openid-configuration` | OIDC discovery: where the endpoints below live. |
| `GET /jwks` | The JSON Web Key Set for verifying access tokens. |
| `GET`/`POST /authorize` | Authorization Code + PKCE login (RFC 8252 loopback redirect). |
| `POST /token` | Code → tokens, and refresh-token rotation. |
| `POST /revoke` | Revoke a refresh token. |

The index is the same `.soulidx` the desktop bakes, so a server and its clients must run the
same index format version. Since the multi-disc release the format is **v8**; a v7 client reading
a v8 server (or the reverse) sees an empty remote library until both are updated.

Access tokens are EdDSA (Ed25519) JWTs, audience `soul-player`, scope `library`, valid
one hour. Refresh tokens rotate on every use: redeeming one invalidates it, so a leaked
token is good for at most a single use before the legitimate client's next refresh kills
it.

The server is hexagonal: the domain speaks only its own types, and the HTTP protocol is
one adapter in `crates/soul-server/src/proto/`. A second protocol (OpenSubsonic, say)
would slot in beside it without touching the domain.

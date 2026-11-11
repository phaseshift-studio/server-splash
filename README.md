# server-splash

**A live dashboard of everything running on your server — generated from what is
actually there, not from a config file you have to keep up to date.**

![server-splash dashboard](screenshot.png)

Point it at a Linux box and it produces a single page listing every service it can
find, with the right name, a sensible icon, and a status light that reflects reality.
Add a service and it appears on the next refresh. No HTML, no service list, no
bookkeeping.

---

## Features

**Finds things you forgot you were running.** It reads the host's listening sockets,
systemd units, Docker containers and `nvidia-smi`, then merges them by port. A service
that is neither a systemd unit nor a published container port still shows up, because
it is listening.

**Names them properly.** ~80 services are recognised by container name, systemd unit,
process, image or the page's own `<title>` — Apache gets a feather, PostgreSQL an
elephant, Home Assistant a house. Anything still unknown borrows its own favicon. Only
genuinely anonymous services fall back to a generic globe.

**Live status, not a build-time snapshot.** Every card re-checks itself from your
browser, so a service that goes down goes red without regenerating anything. The state
at generation time is baked in too, so the page is still useful with JavaScript off.

**GPU panels.** VRAM, utilisation, temperature and fan for each card, filled in from
`nvidia-smi` at generation time and refreshed live from [Glances][glances] if you run it.

**Machine panel.** CPU usage, load average and RAM used, beside the GPU cards. Sampled
from `/proc` when the page is generated, so it is there without JavaScript, and
refreshed live from Glances when one is configured.

**System stats.** `64 GB RAM · 12 TB across 4 disks · 24 cores · eno1 1 Gb/s` under the
title, from `/proc`, `lsblk` and `/sys`.

**A speed test.** One button measures your browser's link to the server — the number
that actually matters for a LAN dashboard.

**Resync.** A button re-probes the host for services that started since the page was
generated. (A browser cannot enumerate ports, so it explores a list of ~140 common
ones; anything else needs a refresh, which the container does on a timer.) When `splash`
is the one serving the page it also asks which *machines* it can see right now, so a
peer that appeared after the page was built shows up immediately instead of waiting for
the next regeneration.

**Curation that survives regeneration.** Rename, re-icon, regroup or hide anything in
`overrides.toml`. The generated HTML is never the source of truth, so regenerating
never loses your edits.

**Never destroys what it replaces.** Before deploying over an existing page it copies
what it is about to overwrite, and prints the restore command.

**No LLM required.** Discovery is deterministic and offline. If you happen to run
Ollama it will suggest names for the handful of services the catalog does not know —
strictly optional, and it may not overwrite a curated name or icon.

---

## Install

### Docker (recommended)

```bash
git clone https://github.com/phaseshift-studio/server-splash
cd server-splash
docker compose up -d
```

Then open **http://localhost:6420**.

That is the whole thing — the container discovers the host, regenerates every 15
minutes, and serves the page itself. No web server to configure, no document root to
find, no file ownership to get right.

> **The one thing to know:** `network_mode: host` and `pid: host` are required, and
> `docker-compose.yml` already sets them. Discovery reads *the host's* listening
> sockets; inside a private network namespace the container would only ever see itself.

**What it can see from inside a container**

| | |
| --- | --- |
| Listening ports | yes — host network namespace |
| Process names | yes — host PID namespace |
| Docker containers | yes — via the mounted socket |
| systemd unit names | yes — via the mounted D-Bus socket |
| Its own name | yes — via `uts: host`, so links point at the machine, not the container id |
| Distribution name | yes — via the mounted `/etc/os-release` |
| GPUs | only with the `deploy:` block in the compose file uncommented |

To run the published image rather than building locally, delete the `build:` line from
`docker-compose.yml`:

```yaml
image: ghcr.io/phaseshift-studio/server-splash:latest
```

### From source (Rust)

Needs a Rust toolchain (edition 2021) and nothing else:

```bash
cargo build --release
sudo install -m0755 target/release/splash /usr/local/bin/
splash --serve
```

Or generate once and exit:

```bash
splash --auto --hostname "$(hostname).local" --deploy /var/www/html
```

[glances]: https://github.com/nicolargo/glances

---

## Configuration

Everything is optional. Command-line flags beat environment variables, which beat the
config file, which beats the defaults.

### Environment variables

These exist so a container needs no config file — see `docker-compose.yml`.

| Variable | Default | What it does |
| --- | --- | --- |
| `SPLASH_HOSTNAME` | this machine's hostname | How the dashboard is reached. A bare name gets `.local` appended. |
| `SPLASH_PORT` | `6420` | Serve the page here. `0` generates once and exits. |
| `SPLASH_INTERVAL` | `900` | Seconds between rediscoveries. `0` never refreshes. |
| `SPLASH_DEPLOY` | – | Also copy the finished page here, e.g. `/var/www/html`. |
| `SPLASH_GLANCES` | – | Glances API base, e.g. `http://ginger.local:61208`. |
| `SPLASH_EXCLUDE_PORTS` | – | Ports to leave off the page, e.g. `"8080,443"`. |
| `SPLASH_TITLE` | the hostname | Page heading. |
| `SPLASH_SUBTITLE` | from the hardware | Line under the heading. |
| `SPLASH_AGENT_URL` | `http://127.0.0.1:11434` | OpenAI-compatible endpoint for optional naming. |
| `SPLASH_NO_AGENT` | `false` | Never call an LLM. |
| `SPLASH_INCLUDE_LOOPBACK` | `false` | Also list services bound to `127.0.0.1` only. |
| `SPLASH_INCLUDE_EPHEMERAL` | `false` | Also list ports 32768–60999 (usually test noise). |
| `SPLASH_SPEEDTEST_BYTES` | `8388608` | Speed-test payload size; `0` removes the button. |
| `SPLASH_INLINE_ICONS` | `true` | Embed icons in the page rather than writing `icons/`. |
| `SPLASH_MODULES` | – | Sub-dashboards to build: `"Ollama Dashboard"`, `"systemd"`, `"cron"`. |
| `SPLASH_OVERRIDES` | `~/.config/server-splash/overrides.toml` | Curation rules. |
| `SPLASH_PEERS` | `~/.config/server-splash/peers.toml` | Other splash machines to link. |
| `SPLASH_LAN` | `true` | Find and announce dashboards over mDNS. |
| `SPLASH_LAN_ANNOUNCE` | `true` | Announce this dashboard over mDNS (only while serving). |
| `SPLASH_LAN_LISTEN` | `2` | Seconds a one-shot run listens for peers. `0` skips the wait. |
| `SPLASH_PEER_TTL` | `900` | Forget a peer not announced for this many seconds. |
| `SPLASH_FEDERATION` | `true` | Learn peers from the peers in `peers.toml`. |
| `SPLASH_PUBLISH_PEERS` | `true` | Serve `/peers.json` so others can learn from you. |
| `SPLASH_PEER_CACHE_DAYS` | `30` | How long the page remembers a machine; `0` disables. |
| `SPLASH_OUTPUT_DIR` | `./server-splash` | Where the page is written. |
| `SPLASH_BACKUP_DIR` | `~/.local/share/server-splash/backups` | Pre-deploy backups. |

> **`SPLASH_GLANCES` is fetched by your browser, not by the server.** `localhost` there
> means *your* machine, so the GPU and machine panels would silently stay at their
> generation-time values. Use the address you reach the dashboard on. The tool warns
> when it sees a loopback URL.

### Config file

`~/.config/server-splash/splash-config.toml`, or pass `--config <file>`. Same settings,
TOML spelling:

```toml
hostname = "ginger.local"
port = 6420
interval = 900
deploy_path = "/var/www/html"
glances_api_base = "http://ginger.local:61208"
exclude_ports = [80]
title = "Home Server"

# Other dashboards: the manual list, and the two ways to fill it in.
peers_file = "/etc/server-splash/peers.toml"
lan = true               # mDNS discovery (announce + browse)
federation = true        # learn from the peers in peers.toml
publish_peers = true     # serve /peers.json -- false on a public host
peer_cache_days = 30     # remember addresses this long, for a run that finds nothing
```

### Curating the result

Discovery gets the ports right; it cannot know your taste. Generate a starter file:

```bash
splash --scaffold
```

That writes one rule per discovered service. Edit freely — the first match that
supplies a field wins, and unset fields fall back to the built-in catalog:

```toml
[[service]]
port = 8080
name = "Zigbee2MQTT"
desc = "Home automation Zigbee bridge"
icon = "🏠"

# …or match on the container / unit / process name.
[[service]]
match = "ollama"
icon = "🦙"

# Hide something without deleting anything.
[[service]]
port = 445
hidden = true
```

`icon` may be an emoji, an image path (`icons/my-logo.png`) or a URL.

### Linking other machines

One page describes one host. `peers.toml` turns a rack of them into something you can
walk around with the mouse instead of the address bar: every entry becomes a card in a
**Machines** section at the top of the page, and clicking one loads *that* machine's
dashboard, which links to the rest in turn.

```bash
splash --scaffold-peers     # write a documented starter file, then edit it
```

```toml
# ~/.config/server-splash/peers.toml
[[peer]]
name  = "ginger"
url   = "ginger.local:6420"
desc  = "Dual RTX 3090 — media + GPUs"
icon  = "🫚"
group = "LAN"

[[peer]]
name  = "basil"
url   = "https://basil.example.com"     # WAN: a full https URL, no port needed
desc  = "Colo VPS — always on"
group = "WAN"

[[peer]]
name   = "old-thinkpad"
url    = "192.168.1.50:6420"
hidden = true                           # kept in the file, off the page
```

| Field | What it does |
| --- | --- |
| `name` | Card title. Required — a peer without one is skipped with a warning. |
| `url` | Where its splash page lives. Required. See the forms below. |
| `desc` | One line under the name. Defaults to the URL's host. |
| `icon` | Emoji, an image path, or an absolute URL. Defaults to 🖥️. |
| `group` | Small tag on the card, e.g. `LAN` / `WAN`. Purely presentational. |
| `hidden` | Keep the entry, leave it off the page. |
| `probe` | Set `false` to drop the live status light for a slow or remote host. |

`url` accepts a full address — the only way to reach a host behind a reverse proxy or
on another network — or a bare host, which gets `http://` and the same `.local` suffix
as service links:

| Written | Becomes |
| --- | --- |
| `ginger` | `http://ginger.local` |
| `ginger:6420` | `http://ginger.local:6420` |
| `192.168.1.50:6420` | `http://192.168.1.50:6420` |
| `https://basil.example.com` | unchanged |

The status light is the browser asking the other machine whether it answers, so the
page tells you a box is down before you click it. Two consequences worth knowing:

- A page served over **https** cannot probe an **http** peer — the browser blocks
  mixed content, so the light stays red even though the link works by hand. Give WAN
  peers an `https://` URL.
- Peers are read at generation time, like everything else, so an edit takes effect on
  the next refresh (`--serve` regenerates on its interval).

`--peers <FILE>` and `SPLASH_PEERS` override the path, so a whole fleet can share one
list from a mounted file or a config repo.

### Finding the other machines automatically

Writing the list by hand is fine for a rack you rarely touch. For the rest, two
mechanisms fill the **Machines** row in without anybody typing a hostname, and they
cover different halves of the problem.

**On one network segment: mDNS.** While serving, the dashboard registers
`_splash._tcp.local` and browses for the same type — the mechanism printers, Chromecast
and Home Assistant have used for years. Every dashboard it hears becomes a card tagged
`discovered`, within a couple of seconds of starting. Nothing to configure and nothing
to maintain; a machine that shuts down disappears when it says goodbye, or after
`SPLASH_PEER_TTL` if the cable was pulled.

It has to happen in the `splash` process, not the page: browsers have no mDNS API and
cannot multicast, which is why a static page can never find another page. So a
long-running `--serve` gets live discovery, and a one-shot `--auto` listens for
`SPLASH_LAN_LISTEN` seconds (2, settable to 0) before generating.

Two limits worth knowing: mDNS does not cross a router, a VLAN boundary or most VPNs
(that is what federation is for), and the container already needs `network_mode: host`
for socket discovery — the same requirement makes mDNS work.

**Across networks: federation.** Any dashboard can be asked for its peer list at
`/peers.json`, which carries the machines its owner wrote into `peers.toml`. So the
machines you already configured become seeds: each refresh asks them what they know,
and whatever comes back is shown tagged `via ginger.local`. Over plain HTTPS, so it
works for a colo box, another subnet, or a VPN peer.

Federation is deliberately one hop each way:

- only machines **in `peers.toml`** are fetched from — something discovered over mDNS
  is never used as a seed, so learning cannot chase its own tail;
- only machines **in `peers.toml`** are republished — so ginger teaches you about
  basil, but nothing you merely heard about leaves your machine.

What a dashboard publishes and what is therefore visible:

| URL | Contents |
| --- | --- |
| `/peers.json` | The machines in your `peers.toml`, nothing else. |
| `/.well-known/splash.json` | Its identity: name, title, version, build. |

Neither file lists a service or a port. That is the same rule that keeps
`dashboard.json` out of every deploy: an announcement should not be an inventory.

> **If this page is on the public internet**, `/peers.json` names your other machines.
> Set `advertise = false` on a peer you would rather not publish, or
> `publish_peers = false` to serve no list at all. Discovery is read-only and
> unauthenticated in both directions: anyone who can reach the page can read the list,
> and anyone can claim a `_splash._tcp.local` name on your LAN.

Turn the automation off with `--no-lan` (no mDNS at all), `--no-announce` (find others,
stay invisible yourself), `--no-federation` (don't ask peers for theirs), or
`publish_peers = false`. Run `splash --auto --no-lan` in a network you don't control.

Spell a peer by hostname rather than by IP where you can (`ginger.local:6420`): the
mDNS announcement uses the hostname, so the two collapse into a single card. A peer
written as an IP is matched against the addresses the machine announced, so it usually
collapses too — but the hostname is exact.

**Every generation also retries the machines it remembers.** The page carries its own
memory, in a comment at the end of itself — no cache file, no cache directory, nothing to
clean up:

```html
<!-- CACHE {"peers":[{"name":"bravo.local","url":"http://bravo.local:6422","last_seen":1794366784}]} -->
```

The next run reads that back out of the page it is about to replace, then asks those
addresses whether they are still there:

```
remembered 1 machine(s) from what this page kept
```

A remembered machine is shown with its age (`last seen 3h ago`) and a status light that says
whether it answered just now — and it doubles as a federation seed, so one machine that came
back can pull the rest of the list in with it. That is what keeps the page useful on a
network where multicast is filtered, or on a morning when every configured peer is asleep.

The **Resync with server** button asks the same question on demand: `/_splash/peers`
returns the cards the server can see at that moment, so a machine that appeared since the
page was built shows up without a restart or a refresh. (That endpoint exists only when
`splash` serves the page; deployed to nginx or Apache the button falls back to what the
page was built with.)

Three properties keep this from becoming a source of fiction:

- a remembered entry never displaces a machine discovered live — the live spelling wins;
- it is **never republished** in `/peers.json`, so remembering a machine cannot put it into
  your published topology;
- entries expire after `SPLASH_PEER_CACHE_DAYS` (30), and a machine that answers keeps its
  timestamp fresh.

Because the memory *is* the page, regenerating into a different output directory starts from
nothing — unless `--deploy` is set, in which case the last deployed page is read instead.
**Deleting the page forgets the machines**, which is the opt-out; `peer_cache_days = 0`
turns the memory off entirely. Note that the comment is part of the HTML: anyone who can
read the page can read the last-seen addresses in it.

---

## Deployment

### Serving itself (default)

`splash --serve` regenerates on an interval and serves the result. This is what the
container runs, and nothing else is needed.

### Into an existing web server

If Apache or nginx already owns port 80, generate into its document root instead:

```bash
splash --auto --hostname ginger.local --deploy /var/www/html
```

`splash` backs up everything it overwrites and tells you how to undo it:

```
Deploying to: /var/www/html
  kept index.html
  backed up 1 existing file(s) to /root/.local/share/server-splash/backups/2026-10-16_155733
  restore with: cp -r /root/.local/share/server-splash/backups/2026-10-16_155733/. /var/www/html
```

For an archive of the whole site — not just the file being replaced — there is
`scripts/backup-site.sh`, useful before a first deploy or when several pages might be
affected:

```bash
scripts/backup-site.sh --host http://ginger.local --out backups
```

It records the HTTP status, size, SHA-256 and `Last-Modified` of each path, then
re-fetches everything and compares, so a truncated backup is caught immediately rather
than when it is needed.

### On a schedule

Ship the included unit files to regenerate every 15 minutes:

```bash
sudo cp deploy/splash.service deploy/splash.timer /etc/systemd/system/
sudo systemctl edit splash.service      # set your paths
sudo systemctl enable --now splash.timer
```

Or just use cron:

```cron
*/15 * * * * /usr/local/bin/splash --auto --deploy /var/www/html >/dev/null 2>&1
```

### Sub-dashboards

Three ship with the tool. Each generates its own page under the output directory and gets
a card on the main dashboard:

| Module | What it is | Backend |
| --- | --- | --- |
| `Ollama Dashboard` | Models, chat and VRAM metrics for a local Ollama | talks to Ollama directly (default `:11434`) |
| `systemd` | Browse, start and stop systemd units | `backend.py` on `:9870` |
| `cron` | Browse and edit cron jobs | `backend.py` on `:9871` |

The two management panels are a static page plus a small Python service, which is
written next to the page when the module is generated:

```bash
SPLASH_MODULES="systemd,cron" splash --auto --output /var/www/html
python3 /var/www/html/systemd/backend.py      # serves the REST API the page calls
```

The backends use only the Python standard library — no packages to install. They shell
out to `systemctl` and `crontab`, so **they must run on the host, not in the container**:
the image has no systemd. In a container the generated pages still work, but those two
panels will report that their backend is unreachable.

---

## How it works

Four independent sources, merged by port:

1. **Listening TCP sockets** (`ss -ltnp`, falling back to `/proc/net/tcp*`) — the ground
   truth for "something is serving here right now".
2. **systemd units** — a human description for a port's process.
3. **Docker containers** — a name and image for published ports.
4. **`nvidia-smi`** — GPU inventory, used for the page subtitle.

Each port is then asked what it is over HTTP, which classifies it and picks up the
page's `<title>`. Loopback-only services are listed but not linked, and the dynamic port
range is skipped because it is mostly short-lived test sockets.

Deeper detail — the icon precedence ladder, what the resync button can and cannot find,
authoring a sub-dashboard module — is in [docs/DESIGN.md](docs/DESIGN.md).

---

## Development

```bash
cargo test                        # 150+ tests, no network or daemons required
cargo clippy --all-targets
cargo run -- --auto --no-agent --output /tmp/preview
```

The parsers are pure functions over captured command output, so the suite runs anywhere
— including on a machine with no Docker, no systemd and no GPU.

## Licence

MIT — see [LICENSE](LICENSE).

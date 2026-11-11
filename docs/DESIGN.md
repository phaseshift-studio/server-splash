# How server-splash works

The README covers what it does and how to run it. This is the detail behind the
decisions — useful when changing the tool, or when the generated page says something
surprising.

## Discovery

Four independent sources, merged by port. Each is optional; a missing one degrades the
page rather than failing the run.

| Source | Command | Contributes |
| --- | --- | --- |
| Listening sockets | `ss -ltnp`, falling back to `ss -ltn`, then `/proc/net/tcp*` | *That a service exists at all*, its port, and whether it is reachable off-box |
| systemd | `systemctl list-units --type=service --state=running` (system and user) | A human description for the process holding the port |
| Docker | `docker ps --format '{{json .}}'` | Container name and image for published ports |
| NVIDIA | `nvidia-smi --query-gpu=... --format=csv` | GPUs for the subtitle and the baked-in GPU cards |

Merging rules worth knowing:

- **A routable bind wins over a loopback bind** of the same port. `0.0.0.0:80` and
  `[::]:80` are one service, and `127.0.0.1:631` is flagged `local_only` so the page
  does not offer a link that only works on the host.
- **The kernel's ephemeral range is skipped** unless `--include-ephemeral`. The range
  is read from `/proc/sys/net/ipv4/ip_local_port_range` (normally `32768 60999`) rather
  than assumed, because "everything above 32767" is not the same thing. That mistake
  silently dropped Glances, which listens on 61208 — just above the range, and exactly
  the kind of long-running service this page exists to surface. Test containers, which
  really do land inside it, are still filtered.
- **`docker-proxy` and `containerd-shim` are not identities.** Docker forwards every
  published port through a process called `docker-proxy`; treating that as the
  service's identity labelled every container on the host "Docker" — including Dockge.
  They are filtered out of `identity()`, `id()` and rule matching. See
  `is_runtime_plumbing`.
- **A systemd unit is attached to a process strictly.** `sshd` runs `ssh.service` and
  `mysqld` runs `mysql.service`, so name matching allows an exact match or a single
  `d` suffix — not substring containment. The loose version let `docker-proxy` match
  `docker.service`, dragging that unit's description ("Docker Application Container
  Engine") into the evidence for every published container port. Dockge, nginx and
  four other services on one real host were all named "Docker" as a result. Port
  forwarders are excluded from unit attachment entirely.
- **Interfaces are only shown if they are physical.** A Docker bridge reports a
  plausible 10 Gb/s, which outranked and hid the machine's real 1 Gb/s NIC. The test is
  the presence of `/sys/class/net/<name>/device`.

### HTTP probing

Each surviving port gets a short request to `127.0.0.1:<port>/`, first `http` then
`https`. That answers three questions at once: whether it speaks HTTP at all, its
scheme, and — from the response — its `<title>`, which is often the best available
name for a service nothing recognises. The same response body is scanned for
`<link rel="icon">`.

Ports that never answer are still listed, as `tcp` services with no link.

## Peer discovery

The page describes one host, so linking to another host means learning its address. Three
sources feed the **Machines** row, and each only contributes machines the previous one did
not already name:

1. **`peers.toml`** — authoritative, works across any network, and the only source whose
   entries are ever republished.
2. **mDNS** — `_splash._tcp.local`, announced and browsed by the daemon.
3. **Federation** — `GET /peers.json` from the machines in `peers.toml`.

### Why discovery cannot happen in the page

A browser has no mDNS API and cannot send multicast, so a static page can never find a
sibling page. Discovery therefore runs in the `splash` process and is baked into the next
generation. That shapes the modes: `--serve` is long-lived, so it announces and browses
continuously; a one-shot `--auto` listens for `lan_listen_secs` and announces nothing,
because a service name that outlives the process by milliseconds points other dashboards
at a dead record.

The first paint of `--serve` deliberately skips both the listen and the federation pull.
The page is useful in about a second, and the worker refreshes two seconds later with the
machines it found.

### Two rules that bound the flood

- **Seeds are configured, not discovered.** A machine heard over mDNS is never fetched
  from, so learning cannot chase its own tail around the network.
- **Only configured machines are republished.** What federation brings in is shown and
  stops there. That bounds propagation to one hop — ginger teaches you about basil, and
  basil learns nothing about carol through you — and keeps a LAN's machine names off a WAN
  host by accident.

`/peers.json` and `/.well-known/splash.json` list machines and identity (name, title,
version, build). Neither lists a service or a port, for the same reason `dashboard.json` is
never deployed: an announcement should not be an inventory. Both are written as files, so
they exist whether the page is served by this binary or by nginx.

### Remembered addresses

Discovery is only as good as the network, so the page remembers what it found **in itself**:
a single `<!-- CACHE {…} -->` comment at the end of the document. The next generation reads
it out of the page it is about to replace and tries those addresses.

Putting the memory in the page rather than in a file is a deliberate simplification: there is
no cache directory to create, no permissions to get right, no state file to exclude from a
deploy, and no cleanup. The artefact and its memory are one object, and it travels with the
page — a machine that generates into a temporary directory and copies the result still
remembers, because the deployed page is read as a fallback when the output page has no
memory yet. Deleting the page is the way to forget.

Consequences worth stating:

- the memory is read every run and **tried**: remembered addresses are fetched like
  configured ones, so each proves it is there and may hand back more of the fleet.
- both directions are bounded as before — remembered addresses are seeds, and only
  `peers.toml` entries are republished, so a machine cannot enter the published topology by
  being remembered.
- entries carry `last_seen`; a machine that answers is refreshed, one that does not keeps
  its age and expires after `peer_cache_days`.
- `--` inside the JSON is written as `-\u002d`. A JSON string is the only place two hyphens
  can occur, and an unescaped `-->` in a peer description would end the comment early and
  dump the memory into the document as visible text.
- the comment is public HTML: it lists the machines this dashboard has seen. That is the
  same information the Machines row shows, plus timestamps.

### Failure modes

- mDNS is layer 2. It does not cross a router, a VLAN boundary, or most VPNs — that is
  what federation is for. Networks that filter multicast are covered by federation too, as
  long as one reachable peer is listed.
- The link is written as `http://<hostname>:<port>` and resolved by the *browser*, so the
  viewer needs mDNS name resolution (Windows 10+, macOS, systemd-resolved with nss-mdns).
  The same requirement already applies to every service link on the page.
- A machine's mDNS answer carries every address it has, including `docker0` and Tailscale
  ranges, so the hostname is preferred over the resolved IP: a link to the wrong address
  fails in a way nobody can debug from the card.
- Our own announcement comes back through the browse, so results are filtered against
  `(hostname, port)`. Without that the page offers a card linking to itself.
- Deduplication is by hostname and by the addresses a machine announced, so an entry
  written as an IP and the same machine announcing a hostname collapse into one card.

## Naming

Resolved in this order; first answer wins:

1. **Your `overrides.toml`** — a deliberate choice, never overridden.
2. **The built-in catalog** — ~80 services matched by container name, systemd unit,
   process, image, or the page's own title. Matchers are sorted by length so a specific
   rule always beats a generic one it merely contains.
3. **The page's own `<title>`** — a service that calls itself "metatron — PhaseShift
   Studio" has identified itself.
4. **A prettified identity** — the container or process name, tidied.
5. **`Service on :PORT`** — genuinely unknown.

Only steps 4 and 5 are treated as guesses. Steps 1–3 set `name_specific`, which is what
stops an LLM from "correcting" `metatron` to `Metatron` or renaming every container
`Docker`.

## Icons

Same shape, but icons have one more rung, because the fallback is the thing users
notice:

1. **Your `overrides.toml`**
2. **A recognised service** — the catalog's emoji
3. **The service's own icon** — its `<link rel="icon">`, else `/favicon.ico`, verified
   by magic bytes (a 200 HTML error page is rejected) and *embedded into the page*
4. **A guess from the port number** — weaker than a real favicon, because a port only
   suggests what might be there
5. **The per-protocol default** — the globe, the lock, the plug

The LLM enrichment pass may only set an icon where the answer would otherwise be the
generic default, and a generic suggestion (`🌐`, `🔌`, `🛠️`, `⚙️`) is discarded.

**Icons are embedded as data URIs by default** (`inline_icons = true`), preferring the
2x asset. This is not tidiness: a relative `icons/…` path only resolves when the page is
served from the exact directory it was written to, so the same page in a preview pane, a
subdirectory or `file://` shows a broken-image box. One deployment target also refused
to serve a subresource (`403` on a stylesheet) for reasons that had nothing to do with
the page.

## Liveness

Status is established twice:

- **At generation time**, by the same HTTP probe used for classification. The result is
  baked into the card's class, so the page is meaningful with JavaScript disabled.
- **In the browser**, by a `no-cors` fetch per card every 30 seconds, plus a manual
  button. `no-cors` is deliberate: the question is only whether the port answers, and
  most of these services send no CORS headers.

Non-HTTP services (`ssh`, `vnc`, `mqtt`, raw `tcp`) cannot be checked from a browser, so
they stay neutral rather than being reported as down.

## The page

- **Stats chips** — memory from `/proc/meminfo`, storage from `lsblk -bdno` (decimal
  units, so a 4 TB disk reads `4.0 TB`), cores from `available_parallelism`, link speed
  from `/sys/class/net/*/speed`. Memory is binary, because that is how RAM is sold and
  reported everywhere else.
- **Machine cards** — CPU usage, load averages and memory from `/proc`, in the same
  tiles as the GPU panel. CPU usage comes from two `/proc/stat` reads 150 ms apart:
  a single read is an average over the machine's whole uptime and never moves. Memory
  "used" is `MemTotal - MemAvailable`, so reclaimable page cache does not read as a
  leak. Values are baked in at generation time and refreshed in place from Glances
  every 3 seconds when one is configured; unlike the GPU cards, a failed poll leaves
  the build-time numbers rather than blanking the tiles.
- **GPU cards** — from `nvidia-smi` at generation time, refreshed from the Glances API
  every 3 seconds when one is configured. The URL is fetched by the *browser*, so
  `localhost` there means the viewer's machine; the tool warns when it sees one.
- **Speed test** — downloads a generated `speedtest.bin` three times and keeps the best.
  The body is fully consumed before stopping the clock, because `fetch` resolves on
  headers and timing that measures latency, not throughput. The payload is
  incompressible so a proxy cannot serve a smaller copy and inflate the result, and it
  is only rewritten when missing or the wrong size so a 15-minute timer does not push
  megabytes through the disk every quarter hour.
- **Resync** — re-checks existing cards, then probes ~140 candidate ports for services
  that appeared since generation. **A browser cannot enumerate a remote host's listening
  sockets**; it can only try ports somebody listed. It will find a service on a common
  port and will not find one somewhere else — that needs a regeneration, which the
  container does on a timer.
- **Footer** — hostname, IP, generation time, and the *build stamp*.

## The build stamp

`build.rs` records the compile time, surfaced in `--version`, in `dashboard.json` and in
the page footer. This exists because a stale binary is otherwise invisible: `--version`
reported the same string for every build, so "I deployed and nothing changed" had no
obvious cause. The footer answers the question from a browser. `scripts/install-remote.sh`
additionally refuses to ship a binary older than `src/`.

## Curation

`overrides.toml` is a list of rules. Each may match on `port` or on `match` (a
case-insensitive substring of container, unit, process, image or title), and may supply
any of `name`, `desc`, `icon`, `protocol`, `group`, `base_path`, `web_probe_url`,
`host_override`, `hidden`.

Rules are evaluated in file order, then the built-ins. A rule that matches but leaves a
field unset does not block a later rule from supplying it. `--scaffold` writes a starter
file from live discovery.

## Backups

Before deploying into a directory, every file about to be overwritten is copied to
`<backup_dir>/<timestamp>/`, mirroring the tree, and the restore command is printed.
Backups never land in a web root, where they would be publicly readable. Debug
artefacts (`dashboard.json`, `splash-state.json`, `agent-response.json`) are never
deployed at all — an earlier version copied the whole output directory and published raw
`systemctl` and `docker ps` output.

## Running in a container

Discovery reads the *host*, so the container needs the host's namespaces:

- `network_mode: host` — otherwise `ss` sees only the container itself.
- `pid: host` — so `ss -p` can resolve a port to a process name.
- `uts: host` — so `hostname` reports the machine rather than the container id. Without
  it the page titles itself `f3a9c1b2d4e5` and every link on it is unresolvable; the
  tool warns when the resolved hostname looks like a container id.
- `/var/run/docker.sock` — container names and images.
- `/run/dbus/system_bus_socket` — systemd unit descriptions.

Two things do not come along for free, and are handled explicitly:

Compose does not run shell commands, so `${SPLASH_HOSTNAME:-$(hostname).local}` is
passed through *literally* — the variable would have been set to the string
`$(hostname).local`. `uts: host` is what makes deriving the name work instead.

- **`/etc/os-release` describes the image, not the machine.** Without a mount, a
  dashboard about a Mint host announced "Debian GNU/Linux 12 (bookworm)" — the
  container's own base image. `compose` mounts the host's file at
  `/host/etc/os-release`, which `collect_distro` prefers; `SPLASH_OS_RELEASE` overrides
  the location. Kernel, memory and listening sockets need no such treatment: those are
  already the host's.
- **GPUs** are invisible unless the compose `deploy:` block is uncommented. The live
  GPU panel only needs Glances, which is reached over the network and needs nothing.

## Choosing a port

The default is 6420, not 8080. 8080 is one of the most contended ports on a Linux box
— nginx, Tomcat and a great many application images all reach for it — and a default
that collides on the first run is the worst possible introduction.

Colliding anyway is handled rather than hoped against. Before binding, `serve` tests the
port and, if it is taken, says so and names the owner, because discovery already knows
what is listening:

```
port 8080 is already in use by nginx
       (docker container my-stack)
       choose another port:  SPLASH_PORT=6420   (or --port 6420)
```

The suggestion is a port that is neither in the discovery results nor bindable by us, so
it is a real offer rather than a guess. Without this, Docker reports "address already in
use" and crash-loops, which tells a user nothing.

## Serving

`--serve` runs the generation loop and a small static file server in one process.
Serving paths are resolved component-wise and refused if they traverse upward, so
`/../../etc/passwd` is a 404. `index.html` is written to a temporary file and renamed,
so a reader sees either the old page or the new one, never half of either. `/healthz`
answers for the container health check, which is `splash --healthcheck` — the binary
testing its own port, so the image needs no curl or wget.

## Sub-dashboard modules

A module is a directory under `src/modules/` containing:

```
my-module/
  module.json      — { name, description, default_port, icon, url_prefix }
  mod.rs           — module_info() returning DashboardModule with a generate fn
  generate.rs      — HTML generation logic
  template.html    — HTML template with <!-- PLACEHOLDER --> markers
  style.css        — stylesheet copied to output
```

Register it in `src/modules/mod.rs` → `all_modules()`. Selected modules get a card in
the page's module slot and their output directory is deployed with the page. Select them
with `SPLASH_MODULES` or interactively in the wizard.

## Testing

The parsers are pure functions over captured command output, so the suite exercises
`ss`, `docker ps`, `systemctl`, `lsblk` and `nvidia-smi` behaviour without any of those
daemons present. The generator is tested by rendering a fixture of the real target
host's inventory, which is also written to `target/preview/ginger.html` for eyeballing.

Running with no terminal (a container, a pipe, cron) falls back to a single generation
rather than launching the interactive wizard, which would hang with nothing to prompt
on.

Two rules the tests are built around:

- A test that pins real observed data (the target host's ports, the Dockge/docker-proxy
  bug, the Docker bridge speed) is worth more than one that pins invented data.
- Anything a mistake could silently break — `metatron` casing, the footer link, the
  icon ladder, the build stamp — gets a test, because the failure mode is a page that
  looks fine and is subtly wrong.

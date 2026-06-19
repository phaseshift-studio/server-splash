#!/usr/bin/env python3
"""Probe HTTP endpoints for selected services, display status table, wait for Enter."""
import sys
import json
import os
import urllib.request

def _c(code, text):
    return "\033[{}m{}\033[0m".format(code, text)

YELLOW = lambda t: _c(33, t)
GREEN  = lambda t: _c(32, t)
RED    = lambda t: _c(31, t)
CYAN   = lambda t: _c(36, t)
BOLD_GREEN = lambda t: _c("1;32", t)
BOLD_RED   = lambda t: _c("1;31", t)

def probe_url(url, timeout=3):
    """Return True if URL responds with 2xx/3xx."""
    try:
        req = urllib.request.Request(url, method="GET")
        resp = urllib.request.urlopen(req, timeout=timeout)
        return 100 <= resp.status < 400
    except Exception:
        return False

def build_url(svc):
    """Build an HTTP URL to probe for a service."""
    wp = svc.get("web_probe_url")
    if wp and wp.lower() != "none":
        return wp
    port = svc.get("port")
    if not port:
        return None
    protocol = (svc.get("protocol") or "http").lower()
    if protocol in ("ssh", "vnc", "mqtt", "ws"):
        return None
    host = svc.get("host_override") or "localhost"
    bp = svc.get("base_path") or ""
    return "http://{}:{}{}".format(host, port, bp)

def trunc(s, width):
    s = str(s)
    if len(s) > width:
        return s[:width - 3] + "..."
    return s.ljust(width)

def main():
    """Read services JSON from stdin, probe endpoints, print table to stderr, output results JSON to stdout."""
    raw = sys.stdin.read()
    try:
        services = json.loads(raw)
    except json.JSONDecodeError as e:
        print("ERROR: {}".format(e), file=sys.stderr)
        sys.exit(1)

    if not isinstance(services, list):
        print("ERROR: Expected JSON array", file=sys.stderr)
        sys.exit(1)

    # Probe each service - show progress on stderr
    eprint = lambda *a, **kw: print(*a, file=sys.stderr, **kw)
    
    eprint()
    eprint(CYAN("  Probing {} service endpoints...").format(len(services)))
    eprint()

    results = []
    for svc in services:
        name = svc.get("name", "?")
        desc = svc.get("desc", "")
        port = svc.get("port") or "daemon"
        url = build_url(svc)

        alive = False
        endpoint_str = "n/a"
        if url:
            sys.stdout.write("." if not hasattr(sys.stdout, 'isatty') else " ")
            alive = probe_url(url)
            endpoint_str = url
        
        results.append({
            "name": name,
            "desc": desc,
            "port": port,
            "endpoint": endpoint_str,
            "alive": alive,
        })

    print("", file=sys.stdout)  # newline after dots

    # Column widths for table
    w_status = 10
    w_name   = max(12, max(len(r["name"]) for r in results), default=0 if not results else 0)
    w_port   = 10
    w_ep     = max(20, min(55, max(len(r["endpoint"]) for r in results) if results else 20))
    w_desc   = 40

    # If no results, provide defaults for widths
    if not results:
        w_name = 12
        w_ep = 20

    def make_sep():
        return YELLOW("+{}+{}+{}+{}+{}+".format(
            "-" * (w_status + 2),
            "-" * (w_name + 2),
            "-" * (w_port + 2),
            "-" * (w_ep + 2),
            "-" * (w_desc + 2),
        ))

    # Print table header
    eprint()
    eprint(make_sep())
    eprint("{} | {} | {} | {} | {}".format(
        "STATUS".rjust(w_status),
        CYAN("NAME").ljust(w_name),
        "PORT".rjust(w_port),
        "ENDPOINT".ljust(w_ep),
        "DESC".ljust(w_desc),
    ))
    eprint(make_sep())

    # Print each row
    for r in results:
        if r["alive"]:
            status = "\u2714 {}".format(BOLD_GREEN("alive"))
        else:
            status = "\u2718 {}".format(BOLD_RED("down"))

        eprint("{} | {} | {} | {} | {}".format(
            CYAN(status).rjust(w_status),
            trunc(r["name"], w_name),
            str(r["port"]).rjust(w_port),
            trunc(r["endpoint"], w_ep),
            trunc(r["desc"], w_desc),
        ))

    eprint(make_sep())

    alive_count = sum(1 for r in results if r["alive"])
    total = len(results)
    eprint("  {}/{} services reachable".format(alive_count, total))
    eprint()

    # Wait for user to press Enter (read from /dev/tty which is the real terminal)
    try:
        tty = open("/dev/tty")
        prompt_txt = "\r{}".format(CYAN("  Press [Enter] to generate dashboard..."))
        tty.write(prompt_txt)
        tty.flush()
        tty.readline()
        tty.close()
    except (OSError, IOError):
        # Fallback: skip wait if no TTY available (piped input)
        pass

    # Output JSON results to stdout for Rust
    print(json.dumps(results))

if __name__ == "__main__":
    main()

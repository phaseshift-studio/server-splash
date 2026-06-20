#!/usr/bin/env python3
"""Cron dashboard backend — serves REST API for crontab management."""
import json
import subprocess
import sys
from http.server import HTTPServer, BaseHTTPRequestHandler
from urllib.parse import urlparse

PORT = <!-- PORT -->


def parse_cron_line(line):
    """Parse a crontab line into structured fields. Returns None for comments/blank lines."""
    line = line.strip()
    if not line or line.startswith("#"):
        return None

    # Check if it's a commented-out cron entry (suspended)
    enabled = True
    if line.startswith("# "):
        # Might be a commented-out cron entry
        maybe = line[2:].strip()
        parts_check = maybe.split(None, 5)
        if len(parts_check) >= 6:
            # Verify it looks like cron fields
            if parts_check[0] in ("*",) or parts_check[0].isdigit() or "/" in parts_check[0] or "," in parts_check[0]:
                enabled = False
                line = maybe

    parts = line.split(None, 5)
    if len(parts) < 6:
        return None

    return {
        "minute": parts[0],
        "hour": parts[1],
        "day_of_month": parts[2],
        "month": parts[3],
        "day_of_week": parts[4],
        "command": parts[5],
        "enabled": enabled,
    }


def format_cron_line(entry):
    """Format a cron entry dict back into a crontab line."""
    prefix = "" if entry.get("enabled", True) else "# "
    return "{}{} {} {} {} {} {}".format(
        prefix,
        entry["minute"], entry["hour"], entry["day_of_month"],
        entry["month"], entry["day_of_week"], entry["command"]
    )


def get_user_crons():
    """Get user crontab entries."""
    try:
        r = subprocess.run(
            ["crontab", "-l"],
            capture_output=True, text=True, timeout=5
        )
        if r.returncode != 0:
            return []
        entries = []
        for line in r.stdout.strip().split("\n"):
            entry = parse_cron_line(line)
            if entry:
                entries.append(entry)
        return entries
    except Exception:
        return []


def get_system_crons():
    """Get system crontab entries from /etc/crontab."""
    try:
        with open("/etc/crontab") as f:
            content = f.read()
        entries = []
        for line in content.strip().split("\n"):
            # Skip environment variable assignments in /etc/crontab
            if line.strip() and not line.strip().startswith("#"):
                # Skip SHELL=, PATH=, etc.
                if "=" in line.split(None, 1)[0] if line.split(None, 1) else False:
                    continue
            entry = parse_cron_line(line)
            if entry:
                entries.append(entry)
        return entries
    except FileNotFoundError:
        return []
    except Exception:
        return []


def write_user_crontab(content):
    """Write content to user crontab via crontab -."""
    try:
        r = subprocess.run(
            ["crontab", "-"],
            input=content, capture_output=True, text=True, timeout=10
        )
        return r.stdout + r.stderr, r.returncode
    except Exception as e:
        return str(e), 1


def write_system_crontab(content):
    """Write content to /etc/crontab."""
    try:
        with open("/etc/crontab", "w") as f:
            f.write(content)
        return "Written to /etc/crontab", 0
    except PermissionError:
        return "Permission denied — run backend with sudo", 1
    except Exception as e:
        return str(e), 1


class Handler(BaseHTTPRequestHandler):
    def log_message(self, format, *args):
        pass

    def _send_json(self, data, status=200):
        body = json.dumps(data).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_OPTIONS(self):
        self.send_response(204)
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
        self.send_header("Access-Control-Allow-Headers", "Content-Type")
        self.end_headers()

    def _strip_prefix(self, path):
        """Strip a single leading module prefix so paths work both directly and behind a proxy.
        
        E.g. /cron/api/user -> /api/user
             /api/user      -> /api/user
        """
        parts = path.strip("/").split("/", 1)
        if len(parts) > 1 and not parts[0].startswith("api"):
            return f"/{parts[1]}"
        return path

    def do_GET(self):
        path = self._strip_prefix(urlparse(self.path).path)
        if path == "/api/user":
            self._send_json(get_user_crons())
        elif path == "/api/system":
            self._send_json(get_system_crons())
        else:
            self._send_json({"error": "not found"}, 404)

    def do_POST(self):
        path = self._strip_prefix(urlparse(self.path).path)
        length = int(self.headers.get("Content-Length", 0))
        body = json.loads(self.rfile.read(length)) if length else {}

        if path == "/api/user/write":
            content = body.get("content", "")
            if not content:
                self._send_json({"error": "content required"}, 400)
                return
            out, code = write_user_crontab(content)
            self._send_json({
                "output": out,
                "exit_code": code,
                "command": "crontab -"
            })

        elif path == "/api/system/write":
            content = body.get("content", "")
            if not content:
                self._send_json({"error": "content required"}, 400)
                return
            out, code = write_system_crontab(content)
            self._send_json({
                "output": out,
                "exit_code": code,
                "command": "write /etc/crontab"
            })

        else:
            self._send_json({"error": "not found"}, 404)


if __name__ == "__main__":
    print(f"cron backend listening on :{PORT}")
    HTTPServer(("0.0.0.0", PORT), Handler).serve_forever()

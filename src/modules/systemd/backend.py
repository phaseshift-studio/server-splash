#!/usr/bin/env python3
"""Systemd dashboard backend — serves a REST API for systemctl management."""
import json
import subprocess
import sys
from http.server import HTTPServer, BaseHTTPRequestHandler
from urllib.parse import urlparse

PORT = <!-- PORT -->


def run_systemctl(args, timeout=15):
    """Run a systemctl command and return (output, exit_code)."""
    try:
        r = subprocess.run(
            ["systemctl"] + args,
            capture_output=True, text=True, timeout=timeout
        )
        return r.stdout + r.stderr, r.returncode
    except subprocess.TimeoutExpired:
        return "Command timed out", 124
    except FileNotFoundError:
        return "systemctl not found", 127
    except Exception as e:
        return str(e), 1


def list_units():
    """Return JSON array of all systemd units with status info."""
    try:
        r = subprocess.run(
            ["systemctl", "list-units", "--all", "--no-legend", "--no-pager",
             "--output=json"],
            capture_output=True, text=True, timeout=10
        )
        if r.returncode != 0:
            # fallback: parse plain output
            return list_units_fallback()
        units = []
        for u in json.loads(r.stdout):
            units.append({
                "name": u.get("unit", ""),
                "loaded": u.get("load", ""),
                "active": u.get("active", ""),
                "sub": u.get("sub", ""),
                "enabled": "unknown",
                "unit_file": "unknown",
            })
        # Enrich with enabled/unit_file status from list-unit-files
        enrich_enabled(units)
        return units
    except (json.JSONDecodeError, Exception):
        return list_units_fallback()


def list_units_fallback():
    """Fallback: parse systemctl list-units --all plain output."""
    try:
        r = subprocess.run(
            ["systemctl", "list-units", "--all", "--no-legend", "--no-pager"],
            capture_output=True, text=True, timeout=10
        )
        units = []
        for line in r.stdout.strip().split("\n"):
            if not line.strip():
                continue
            parts = line.split(None, 4)
            if len(parts) >= 4:
                units.append({
                    "name": parts[0].strip(),
                    "loaded": parts[1].strip(),
                    "active": parts[2].strip(),
                    "sub": parts[3].strip(),
                    "enabled": "unknown",
                    "unit_file": "unknown",
                })
        enrich_enabled(units)
        return units
    except Exception as e:
        return []


def enrich_enabled(units):
    """Add enabled/unit_file fields from systemctl list-unit-files."""
    try:
        r = subprocess.run(
            ["systemctl", "list-unit-files", "--no-legend", "--no-pager"],
            capture_output=True, text=True, timeout=10
        )
        file_map = {}
        for line in r.stdout.strip().split("\n"):
            parts = line.split(None, 1)
            if len(parts) >= 2:
                name = parts[0].strip()
                state = parts[1].strip().split()[0]
                file_map[name] = state
        for u in units:
            if u["name"] in file_map:
                u["enabled"] = file_map[u["name"]]
                u["unit_file"] = file_map[u["name"]]
    except Exception:
        pass


def get_unit_file_content(name):
    """Read the unit file contents for a service."""
    try:
        r = subprocess.run(
            ["systemctl", "cat", name],
            capture_output=True, text=True, timeout=5
        )
        return r.stdout, r.returncode
    except Exception as e:
        return str(e), 1


class Handler(BaseHTTPRequestHandler):
    def log_message(self, format, *args):
        pass  # suppress default logging

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
        
        E.g. /systemd/api/services -> /api/services
             /api/services         -> /api/services
        """
        parts = path.strip("/").split("/", 1)
        if len(parts) > 1 and not parts[0].startswith("api"):
            return f"/{parts[1]}"
        return path

    def do_GET(self):
        path = self._strip_prefix(urlparse(self.path).path)
        if path == "/api/services":
            self._send_json(list_units())
        else:
            self._send_json({"error": "not found"}, 404)

    def do_POST(self):
        path = self._strip_prefix(urlparse(self.path).path)
        length = int(self.headers.get("Content-Length", 0))
        body = json.loads(self.rfile.read(length)) if length else {}

        if path == "/api/action":
            name = body.get("name", "")
            action = body.get("action", "")
            valid = {"start", "stop", "restart", "enable", "disable"}
            if action not in valid:
                self._send_json({"error": f"invalid action: {action}"}, 400)
                return
            cmd = ["sudo", "systemctl", action, name] if action in ("enable", "disable") else ["systemctl", action, name]
            out, code = run_systemctl(cmd[1:] if action in ("start", "stop", "restart") else cmd[1:])
            # For enable/disable, prefix with sudo
            if action in ("enable", "disable"):
                out2, code = run_systemctl([action, name])
            self._send_json({"output": out, "exit_code": code, "command": " ".join(cmd)})

        elif path == "/api/edit":
            name = body.get("name", "")
            content = body.get("content", "")
            if not name or not content:
                self._send_json({"error": "name and content required"}, 400)
                return
            try:
                path = f"/etc/systemd/system/{name}"
                with open(path, "w") as f:
                    f.write(content)
                r = subprocess.run(
                    ["sudo", "systemctl", "daemon-reload"],
                    capture_output=True, text=True, timeout=10
                )
                self._send_json({
                    "output": f"Written to {path}\\ndaemon-reload: {r.stdout}{r.stderr}",
                    "exit_code": r.returncode,
                    "command": f"write /etc/systemd/system/{name} && systemctl daemon-reload"
                })
            except PermissionError:
                self._send_json({"error": "Permission denied — run backend with sudo"}, 403)
            except Exception as e:
                self._send_json({"error": str(e)}, 500)

        elif path == "/api/cat":
            name = body.get("name", "")
            if not name:
                self._send_json({"error": "name required"}, 400)
                return
            content, code = get_unit_file_content(name)
            self._send_json({"content": content, "exit_code": code, "name": name})

        else:
            self._send_json({"error": "not found"}, 404)


if __name__ == "__main__":
    print(f"systemd backend listening on :{PORT}")
    HTTPServer(("0.0.0.0", PORT), Handler).serve_forever()

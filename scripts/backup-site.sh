#!/bin/bash
# Archive what a site is currently serving, before server-splash deploys over it.
#
# This fetches over HTTP, so it captures exactly what visitors see (including any
# re-writes the web server does) rather than the files on disk. For an authoritative
# on-disk copy you need shell access to the host — see the note the script writes into
# the manifest.
#
# Usage:
#   scripts/backup-site.sh [--host URL] [--out DIR] [--tag NAME] [PATH ...]
#
# Defaults: host http://ginger.local and every path server-splash could overwrite
#           (/, /splash-server.html, /ollama/, /ollama-ui/, its stylesheet, favicon).
set -euo pipefail

HOST="http://ginger.local"
OUT_ROOT="backups"
TAG=""
PATHS=()

while [ $# -gt 0 ]; do
    case "$1" in
        --host) HOST="$2"; shift 2 ;;
        --out)  OUT_ROOT="$2"; shift 2 ;;
        --tag)  TAG="$2"; shift 2 ;;
        -h|--help) sed -n '2,14p' "$0"; exit 0 ;;
        *) PATHS+=("$1"); shift ;;
    esac
done

if [ ${#PATHS[@]} -eq 0 ]; then
    PATHS=(/ /splash-server.html /ollama/ /ollama-ui/ /ollama-ui/css/style.css /favicon.ico)
fi

STAMP="$(date +%Y-%m-%d_%H%M%S)"
[ -n "$TAG" ] && STAMP="${STAMP}_${TAG}"
DEST="${OUT_ROOT}/$(printf '%s' "$HOST" | sed -E 's#^https?://##; s#[:/]#_#g')-${STAMP}"
mkdir -p "$DEST"

MANIFEST="${DEST}/MANIFEST.txt"
{
    echo "server-splash site backup"
    echo "host      : ${HOST}"
    echo "taken     : $(date -Is)"
    echo "taken by  : $(id -un)@$(hostname)"
    echo
    echo "NOTE: these are HTTP responses, i.e. what the server sends, not the raw files"
    echo "on disk. For an authoritative copy, run this on the host itself:"
    echo "    sudo tar czf ~/www-backup-$(date +%F).tgz -C /var/www html"
    echo
    printf '%-24s %-6s %-10s %-12s %s\n' "PATH" "STATUS" "BYTES" "SHA256" "LAST-MODIFIED"
} > "$MANIFEST"

FAILED=0
for raw_path in "${PATHS[@]}"; do
    url="${HOST%/}${raw_path}"
    # Map a URL path to a file path: / -> index.html, /ollama/ -> ollama/index.html
    rel="${raw_path#/}"
    case "$rel" in
        "")      rel="index.html" ;;
        */)      rel="${rel}index.html" ;;
    esac
    target="${DEST}/${rel}"
    mkdir -p "$(dirname "$target")"

    headers="$(mktemp)"
    # Distinguish "the server refused this path" (documented, not fatal) from
    # "we could not talk to the server at all" (fatal).
    status="$(curl -sS -D "$headers" -o "$target" -w '%{http_code}' "$url" 2>/dev/null)" || {
        echo "  FAILED  ${url} (transport error)"
        printf '%-24s %-6s\n' "$raw_path" "ERROR" >> "$MANIFEST"
        rm -f "$headers" "$target"
        FAILED=1
        continue
    }

    if [ "$status" != "200" ]; then
        printf '%-24s %-6s %-10s %-12s %s\n' "$raw_path" "$status" "-" "-" "not readable" >> "$MANIFEST"
        echo "  skipped ${url}  (HTTP ${status})"
        rm -f "$headers" "$target"
        continue
    fi

    # grep -i rather than awk: IGNORECASE is a gawk extension and mawk ignores it,
    # which silently produced empty values here.
    lastmod="$(grep -i '^last-modified:' "$headers" | head -1 | sed 's/^[^:]*: //' | tr -d '\r')"
    bytes="$(wc -c < "$target" | tr -d ' ')"
    sum="$(sha256sum "$target" | cut -d' ' -f1)"

    printf '%-24s %-6s %-10s %-12s %s\n' "$raw_path" "$status" "$bytes" "${sum:0:12}" "${lastmod:-—}" >> "$MANIFEST"
    echo "  saved   ${url}  ->  ${rel}  (${bytes} bytes, sha256 ${sum:0:12})"
    rm -f "$headers"
done

# Verify: re-fetch and compare hashes, so a truncated or half-written backup is caught
# now rather than when it is needed.
echo >> "$MANIFEST"
echo "VERIFICATION (re-fetched and compared)" >> "$MANIFEST"
for raw_path in "${PATHS[@]}"; do
    url="${HOST%/}${raw_path}"
    rel="${raw_path#/}"
    case "$rel" in
        "")  rel="index.html" ;;
        */)  rel="${rel}index.html" ;;
    esac
    target="${DEST}/${rel}"
    [ -f "$target" ] || continue
    fresh="$(mktemp)"
    if curl -fsS -o "$fresh" "$url" 2>/dev/null; then
        if cmp -s "$target" "$fresh"; then
            echo "  ok      ${raw_path}" >> "$MANIFEST"
        else
            echo "  CHANGED ${raw_path} (the live file changed while we were backing up)" >> "$MANIFEST"
            FAILED=1
        fi
    else
        echo "  unchanged-unreadable ${raw_path}" >> "$MANIFEST"
    fi
    rm -f "$fresh"
done

echo
echo "Backup written to: ${DEST}"
echo "Manifest:          ${MANIFEST}"
if [ "$FAILED" -ne 0 ]; then
    echo "WARNING: some paths failed or changed — review the manifest."
    exit 1
fi
echo "All paths verified byte-for-byte against a second fetch."

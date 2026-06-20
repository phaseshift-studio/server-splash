#!/bin/bash
set -euo pipefail

SRC="./server-splash"
DST="${1:-/var/www/html}"

if [ ! -d "$SRC" ]; then
    echo "Error: source directory '$SRC' not found. Run 'splash' first."
    exit 1
fi

if [ ! -d "$DST" ]; then
    echo "Error: destination '$DST' does not exist or is not a directory."
    exit 1
fi

cp -rf "$SRC"/* "$DST"
echo "Deployed $SRC -> $DST"

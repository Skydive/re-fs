#!/usr/bin/env bash
# Compare `re-fs ls` against tools/paths.py on a local mirror (default: ./mirror).
set -euo pipefail
cd "$(dirname "$0")/.."
m=${1:-mirror}
for nb in "" --notebooks; do
  diff <(python3 -I tools/paths.py "$m" $nb | LC_ALL=C sort) \
       <(cargo run -q -p re-fs -- ls --mirror "$m" -R --paths --no-views $([ -z "$nb" ] && echo --hide-notebooks) | grep -v '^\.trash' | LC_ALL=C sort)
done
echo "tree matches (${m})"

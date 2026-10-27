#!/usr/bin/env bash
# End-to-end: the same Odoo pages served by the Rust engine and by the Python
# engine (two servers on two copies of the same database), timed with hyperfine.
#
#   benches/hf_http.sh <rust base url> <python base url> [paths...]
#
# Start the servers first, e.g. (workers=0, warm caches by the warmup runs):
#   odoo-bin -c ~/.cache/qweb-rs-pg/odoo_rs.conf -d qw_web --db-filter=^qw_web$
#   ODOO_QWEB_ENGINE=python odoo-bin -c ~/.cache/qweb-rs-pg/odoo_py.conf -d qw_web_py --db-filter=^qw_web_py$
set -euo pipefail
RS=$1; PY=$2; shift 2
PATHS=("$@")
[ ${#PATHS[@]} -eq 0 ] && PATHS=(/ /contactus /website/info /web/login)
for p in "${PATHS[@]}"; do
    hyperfine -N -w 20 -r 200 \
        -n "python $p" "curl -sf -o /dev/null $PY$p" \
        -n "rust $p" "curl -sf -o /dev/null $RS$p"
done

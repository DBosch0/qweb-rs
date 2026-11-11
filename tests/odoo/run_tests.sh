#!/usr/bin/env bash
# Run Odoo test classes against a database of the private test cluster.
#
#   tests/odoo/run_tests.sh <db> <test-tags> [log-name]
#
# Env: ODOO (odoo source, default $ODOO_PATH; required), PGDIR (cluster
# dir, default ~/.cache/qweb-rs-pg), CONF (odoo config, default $PGDIR/odoo.conf),
# ODOO_QWEB_ENGINE=python for the reference.
set -u
DB=$1; TAGS=$2; NAME=${3:-$DB}
ODOO=${ODOO:-${ODOO_PATH:-}}
[ -n "$ODOO" ] || { echo "set ODOO (or ODOO_PATH) to the Odoo source tree" >&2; exit 1; }
PGDIR=${PGDIR:-$HOME/.cache/qweb-rs-pg}
PY=${PY:-$(cd "$(dirname "$0")/../.." && pwd)/.venv/bin/python}
LOG=$PGDIR/$NAME.log
CONF=${CONF:-$PGDIR/odoo.conf}
cd "$ODOO" || exit 1
"$PY" odoo-bin -c "$CONF" -d "$DB" --test-tags "$TAGS" --stop-after-init --log-level=test > "$LOG" 2>&1
grep -E "(FAIL|ERROR): " "$LOG" | sed -E 's/.*(FAIL|ERROR): /\1: /' | sort -u
grep -E "tests when loading|post-tests in|tests [0-9.]+s" "$LOG" | sed -E 's/^[0-9-]+ [0-9:,]+ [0-9]+ //'
echo "log: $LOG"

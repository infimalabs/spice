#!/usr/bin/env sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)

if [ -f "$repo_root/spice/__main__.py" ] && [ -f "$repo_root/spice/cli/entry.py" ] && [ -f "$repo_root/spice/agent/wrap.py" ]; then
    if [ ! -x "$repo_root/.venv/bin/python" ]; then
        echo "spice.sh: local spice checkout requires $repo_root/.venv/bin/python" >&2
        exit 127
    fi
    export PYTHONPATH="$repo_root${PYTHONPATH:+:$PYTHONPATH}"
    if ! probe=$("$repo_root/.venv/bin/python" -c 'import spice.cli.entry, spice.agent.wrap' 2>&1); then
        echo "spice.sh: the local spice checkout at $repo_root cannot import; repair the file named below (look for conflict markers), or run the installed spice entrypoint until the checkout is fixed" >&2
        printf '%s\n' "$probe" >&2
        exit 127
    fi
    exec "$repo_root/.venv/bin/python" -m spice agent run -- "$@"
fi

exec spice agent run -- "$@"

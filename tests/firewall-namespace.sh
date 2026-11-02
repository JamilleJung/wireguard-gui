#!/usr/bin/env bash
set -euo pipefail

if [ "$(uname -s)" != Linux ]; then
    echo "Firewall integration requires Linux network namespaces." >&2
    exit 2
fi

# Build as the ordinary user. Only the already-built test process receives
# privilege, inside its own network namespace. The Rust test independently
# refuses the host namespace before issuing any firewall command.
artifacts="$(mktemp)"
trap 'rm -f "$artifacts"' EXIT
cargo test --locked --no-run --bin wg-helper --message-format=json > "$artifacts"
helper_test="$(python3 - "$artifacts" <<'PY'
import json
import sys
from pathlib import Path

executables = []
for line in Path(sys.argv[1]).read_text().splitlines():
    item = json.loads(line)
    if (item.get("reason") == "compiler-artifact"
            and item.get("target", {}).get("name") == "wg-helper"
            and item.get("executable")):
        executables.append(item["executable"])
if len(executables) != 1:
    raise SystemExit("Expected exactly one helper test executable")
print(executables[0])
PY
)"
test -x "$helper_test"
sudo unshare --net -- "$helper_test" --ignored --exact \
    tests::firewall_batches_in_isolated_network_namespace --nocapture

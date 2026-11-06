#!/usr/bin/env bash
# lenovo1's nightly jobs: every executable in jobs/ beside this script, in name
# order. One failing does not stop the rest; the run fails if any did, so the
# Schedule tab shows it. Installed by `just deploy-nightly`.
set -u
dir="$(cd "$(dirname "$0")" && pwd)/jobs"
failed=()
for job in "$dir"/*; do
    [ -f "$job" ] && [ -x "$job" ] || continue
    name="$(basename "$job")"
    echo ">>> $name"
    if ! "$job"; then
        echo ">>> $name failed"
        failed+=("$name")
    fi
done
if [ ${#failed[@]} -gt 0 ]; then
    echo ">>> failed: ${failed[*]}"
    exit 1
fi
echo ">>> all done"

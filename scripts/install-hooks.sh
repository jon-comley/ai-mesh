#!/bin/bash
set -e

REPO_ROOT="$(git rev-parse --show-toplevel)"
HOOKS_SRC="${REPO_ROOT}/scripts/hooks"
HOOKS_DST="${REPO_ROOT}/.git/hooks"

for hook in "${HOOKS_SRC}"/*; do
    name="$(basename "${hook}")"
    ln -sf "${hook}" "${HOOKS_DST}/${name}"
    chmod +x "${hook}"
    echo "Installed ${name} hook"
done

echo "All hooks installed."

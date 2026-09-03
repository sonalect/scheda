#!/usr/bin/env bash
# Drop preinstalled GitHub-hosted toolchains that the debian:13 job does not
# use. Each directory is a bind mount under /mnt/drop (not the runner root:
# mounting / into the container can change host file ownership).
set -euo pipefail

if [[ ! -d /mnt/drop ]]; then
  echo "expected host toolchains at /mnt/drop (container volumes)" >&2
  exit 1
fi

df -h /

shopt -s nullglob
for path in /mnt/drop/*; do
  if [[ -d "${path}" ]]; then
    du -sh "${path}" || true
    find "${path}" -mindepth 1 -maxdepth 1 -exec rm -rf {} +
  fi
done

df -h /

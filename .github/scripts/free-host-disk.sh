#!/usr/bin/env bash
# Drop preinstalled GitHub-hosted toolchains that the debian:13 job does not
# use. The job container bind-mounts the runner root at /host.
set -euo pipefail

if [[ ! -d /host/usr ]]; then
  echo "expected the runner root at /host (container volumes: - /:/host)" >&2
  exit 1
fi

df -h /host

# Paths relative to the runner root. Skip anything missing so a newer image
# that dropped a toolchain does not fail the job.
dirs=(
  /usr/local/lib/android
  /usr/share/dotnet
  /opt/ghc
  /usr/local/.ghcup
  /usr/share/swift
  /usr/local/share/powershell
  /opt/hostedtoolcache
  /usr/lib/jvm
  /opt/az
  /usr/share/miniconda
  /usr/local/share/boost
  /opt/microsoft
  /usr/share/codeql
  /usr/local/graalvm
  /usr/share/sbt
  /opt/pipx
)

for rel in "${dirs[@]}"; do
  path="/host${rel}"
  if [[ -e "${path}" ]]; then
    du -sh "${path}" || true
    rm -rf "${path}" || true
  fi
done

df -h /host

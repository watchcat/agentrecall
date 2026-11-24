#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
promtool=${PROMTOOL:-promtool}

if ! command -v "$promtool" >/dev/null 2>&1; then
  echo "promtool is required (set PROMTOOL to its executable path if needed)" >&2
  exit 127
fi

"$promtool" check rules "$repo_root/config/prometheus/agentrecall-alerts.yml"
"$promtool" test rules "$repo_root/config/prometheus/agentrecall-alerts.test.yml"

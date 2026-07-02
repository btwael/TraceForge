#!/usr/bin/env bash
set -euo pipefail

cargo build -p examples --bins --release "$@"

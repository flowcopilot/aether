#!/bin/sh
# Build the documentation site into target/docs-site.
set -eu

cd "$(dirname "$0")/.."
npm --prefix docs ci
npm --prefix docs run build

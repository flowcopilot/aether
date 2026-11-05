#!/bin/sh
# Build the documentation site into target/docs-site.
set -eu

cd "$(dirname "$0")/.."
mdbook build docs

# Agents read raw Markdown. Publish the agent onboarding page as-is.
cp server/priv/static/docs.md target/docs-site/agent.md
rm -f target/docs-site/book.toml

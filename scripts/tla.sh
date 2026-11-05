#!/bin/sh
# Model-checks every TLA+ spec in specs/ with TLC. Each `specs/<Name>.cfg`
# checks `specs/<Name>.tla`. Pass spec names to check only those, for
# example `sh scripts/tla.sh ExecutionLease`.
set -eu

version="v1.7.4"
sha256="936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88"
root="$(cd "$(dirname "$0")/.." && pwd)"
jar="$root/target/tla/tla2tools-$version.jar"

if [ ! -f "$jar" ]; then
  mkdir -p "$(dirname "$jar")"
  curl -sSfL -o "$jar.part" \
    "https://github.com/tlaplus/tlaplus/releases/download/$version/tla2tools.jar"
  echo "$sha256  $jar.part" | shasum -a 256 -c - >/dev/null
  mv "$jar.part" "$jar"
fi

if [ "$#" -eq 0 ]; then
  set -- $(cd "$root/specs" && ls *.cfg | sed 's/\.cfg$//')
fi

states="$root/target/tla/states"
for spec in "$@"; do
  echo "== $spec"
  (cd "$root/specs" && java -XX:+UseParallelGC -cp "$jar" tlc2.TLC \
    -workers auto -deadlock -cleanup -metadir "$states/$spec" \
    -config "$spec.cfg" "$spec.tla")
done

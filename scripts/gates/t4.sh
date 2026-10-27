#!/usr/bin/env bash
# T4 gate: every orbital-core test, ratcheted against scripts/gates/known-failures/tests.txt.
set -u
cd "$(dirname "$0")/../.."
ORB=${ORB:-orb}
CARGO=${CARGO:-cargo}
G=scripts/gates
mkdir -p $G/.observed $G/known-failures
touch $G/known-failures/tests.txt
: > $G/.observed/cargo-test.log
for m in Cargo.toml crates/base/Cargo.toml crates/eval/Cargo.toml crates/runtime/Cargo.toml crates/std/Cargo.toml; do
  echo "     Crate $m" >> $G/.observed/cargo-test.log
  $CARGO test --manifest-path "$PWD/$m" --no-fail-fast >> $G/.observed/cargo-test.log 2>&1
done
awk '
  /^     Crate / { crate = $2; next }
  /^     Running / { bin = crate ":" $2; next }
  /^   Doc-tests / { bin = crate ":doc:" $2; next }
  /^test .* \.\.\. FAILED$/ { name = $0; sub(/^test /, "", name); sub(/ \.\.\. FAILED$/, "", name); print bin "#" name; next }
  /^error: could not compile/ { print "COMPILE#" $0 }
' $G/.observed/cargo-test.log | LC_ALL=C sort -u > $G/.observed/tests.txt
if ! grep -q "^test result:" $G/.observed/cargo-test.log; then
  echo "t4 tests: no test binary ran"
  exit 2
fi
echo "t4 tests: $(grep -c '^test result:' $G/.observed/cargo-test.log) test binaries ran"
$ORB gate $G/known-failures/tests.txt $G/.observed/tests.txt

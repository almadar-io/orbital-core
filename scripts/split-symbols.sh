#!/usr/bin/env bash
# split-symbols.sh <binary> <symbols-out-stem>
# Move a release binary's debug info into a separate symbol file and strip the
# binary in place. The symbol file ships as a release asset so crashes stay
# symbolicable without bloating what users download.
#   Linux:   <stem>.debug (objcopy; the binary keeps a .gnu_debuglink to it)
#   macOS:   <stem>.dSYM.tar.gz (dsymutil)
#   Windows: <stem>.pdb (the linker already wrote it beside the .exe)
set -euo pipefail
bin="$1"; stem="$2"
mkdir -p "$(dirname "$stem")"
case "$(uname -s)" in
  Linux)
    objcopy --only-keep-debug "$bin" "$stem.debug"
    objcopy --strip-debug --strip-unneeded --add-gnu-debuglink="$stem.debug" "$bin"
    ;;
  Darwin)
    dsymutil "$bin" -o "$stem.dSYM"
    strip -S -x "$bin"
    tar -czf "$stem.dSYM.tar.gz" -C "$(dirname "$stem")" "$(basename "$stem").dSYM"
    rm -rf "$stem.dSYM"
    ;;
  MINGW*|MSYS*|CYGWIN*)
    pdb="${bin%.exe}.pdb"
    [ -f "$pdb" ] || pdb="$(dirname "$bin")/$(basename "${bin%.exe}" | tr '-' '_').pdb"
    cp "$pdb" "$stem.pdb"
    ;;
  *) echo "split-symbols: unsupported platform $(uname -s)" >&2; exit 1 ;;
esac
ls -la "$bin" "$stem"*

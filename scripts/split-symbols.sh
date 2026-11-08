#!/usr/bin/env bash
# split-symbols.sh <binary> <symbols-out-stem>
# Move a release binary's debug info into a separate symbol file and strip the
# binary in place. The symbol file ships as a release asset so crashes stay
# symbolicable without bloating what users download.
#   Linux:   <stem>.debug (objcopy; the binary keeps a .gnu_debuglink to it)
#   macOS:   <stem>.dSYM.tar.gz (dsymutil)
#   Windows: <stem>.pdb (the linker already wrote it beside the .exe or .dll)
set -euo pipefail
bin="$1"; stem="$2"
mkdir -p "$(dirname "$stem")"
# The binary's own format picks the tools, so a host can split what it cross-built.
objcopy_tool() {
  for t in objcopy llvm-objcopy /opt/homebrew/opt/llvm/bin/llvm-objcopy; do
    command -v "$t" >/dev/null 2>&1 && { echo "$t"; return; }
  done
  echo "split-symbols: no objcopy or llvm-objcopy for an ELF binary" >&2; exit 1
}
case "$(od -An -tx1 -N4 "$bin" | tr -d ' \n')" in
  7f454c46)
    objcopy="$(objcopy_tool)"
    "$objcopy" --only-keep-debug "$bin" "$stem.debug"
    "$objcopy" --strip-debug --strip-unneeded --add-gnu-debuglink="$stem.debug" "$bin"
    ;;
  cffaedfe|cefaedfe|cafebabe)
    dsymutil "$bin" -o "$stem.dSYM"
    strip -S -x "$bin"
    tar -czf "$stem.dSYM.tar.gz" -C "$(dirname "$stem")" "$(basename "$stem").dSYM"
    rm -rf "$stem.dSYM"
    ;;
  4d5a*)
    pdb="${bin%.*}.pdb"
    [ -f "$pdb" ] || pdb="$(dirname "$bin")/$(basename "${bin%.*}" | tr '-' '_').pdb"
    cp "$pdb" "$stem.pdb"
    ;;
  *) echo "split-symbols: $bin is not an ELF, Mach-O or PE binary" >&2; exit 1 ;;
esac
ls -la "$bin" "$stem"*

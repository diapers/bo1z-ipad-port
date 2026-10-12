#!/usr/bin/env bash
# Probe the WSL environment for iOS cross-compilation capability.
echo "DISTRO : $(. /etc/os-release; echo "$PRETTY_NAME")"
echo "KERNEL : $(uname -r)"
echo "ARCH   : $(uname -m)"
echo "CPUS   : $(nproc)"
echo "MEM    : $(free -g | awk '/^Mem:/{print $2" GB total, "$7" GB available"}')"
echo
echo "--- host tools ---"
for t in rustc cargo clang clang++ ldid ldid2 git curl unzip python3 cmake ninja make swiftc xcrun; do
  printf '%-10s %s\n' "$t" "$(command -v "$t" 2>/dev/null || echo MISSING)"
done
echo
echo "--- disk ---"
df -h / | tail -1
echo
echo "--- can we reach the internet (rust static + theos) ---"
for u in https://static.rust-lang.org https://github.com https://raw.githubusercontent.com; do
  code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 12 "$u" 2>/dev/null)
  printf '%-45s %s\n' "$u" "$code"
done

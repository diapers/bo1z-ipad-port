#!/usr/bin/env bash
# Inspect the Ubuntu distro for pre-existing USER data before any removal.
echo "=== /home/mirac (top level, sizes) ==="
ls -la /home/mirac 2>/dev/null
echo
echo "=== disk usage of user dirs ==="
du -sh /home/mirac 2>/dev/null
du -sh /root 2>/dev/null
echo
echo "=== anything that looks like a project (git repos / node / python) ==="
find /home/mirac /root -maxdepth 3 \( -name '.git' -o -name 'package.json' -o -name 'Cargo.toml' -o -name 'requirements.txt' -o -name '*.py' -o -name '*.ipynb' \) 2>/dev/null | head -40
echo
echo "=== most recently modified files under /home/mirac (top 20) ==="
find /home/mirac -type f -not -path '*/.cache/*' -not -path '*/.local/share/*' -printf '%TY-%Tm-%Td %TH:%TM  %10s  %p\n' 2>/dev/null | sort -r | head -20
echo
echo "=== was rust actually installed by my aborted script? ==="
ls -la /root/.cargo /home/mirac/.cargo 2>/dev/null || echo "no cargo dir in /root or /home/mirac"
echo
echo "=== did my apt install complete? (build-essential/clang present?) ==="
for t in clang cmake ninja unzip; do printf '%-8s %s\n' "$t" "$(command -v $t 2>/dev/null || echo MISSING)"; done
echo
echo "=== ext4 vhdx size on the Windows side will be reported separately ==="

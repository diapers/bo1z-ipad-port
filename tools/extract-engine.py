#!/usr/bin/env python3
"""Extract the AOT-compiled engine (host/src/aot.rs: section __TEXT,__bo1z_engine) from a
built BO1Z.ipa, as a .cwasm the next build can embed again (BO1Z_CWASM).

The engine wasm is pinned by hash and vel.gg now serves a newer build, so CI cannot fetch
and precompile it any more; every IPA it built already carries the pinned engine compiled
for aarch64-apple-ios. Valid only while host/Cargo.lock keeps the same wasmtime and
rt::engine_config is unchanged (wasmtime checks both when loading).

Usage: extract-engine.py <BO1Z.ipa> <out.cwasm>
"""
import struct
import sys
import zipfile

MH_MAGIC_64 = 0xFEEDFACF
LC_SEGMENT_64 = 0x19


def section(macho: bytes, segname: bytes, sectname: bytes) -> bytes:
    magic, _cpu, _sub, _ftype, ncmds, _size, _flags, _res = struct.unpack_from("<IiiIIIII", macho, 0)
    if magic != MH_MAGIC_64:
        raise SystemExit("not a thin 64-bit Mach-O")
    off = 32
    for _ in range(ncmds):
        cmd, cmdsize = struct.unpack_from("<II", macho, off)
        if cmd == LC_SEGMENT_64:
            nsects = struct.unpack_from("<I", macho, off + 64)[0]
            for i in range(nsects):
                s = off + 72 + i * 80
                sname = macho[s:s + 16].rstrip(b"\0")
                seg = macho[s + 16:s + 32].rstrip(b"\0")
                if seg == segname and sname == sectname:
                    _addr, size, offset = struct.unpack_from("<QQI", macho, s + 32)
                    return macho[offset:offset + size]
        off += cmdsize
    raise SystemExit(f"section {segname.decode()},{sectname.decode()} not found")


def main() -> None:
    ipa, out = sys.argv[1], sys.argv[2]
    with zipfile.ZipFile(ipa) as z:
        names = [n for n in z.namelist() if n.startswith("Payload/") and n.count("/") == 2 and n.endswith("/bo1z_host")]
        if not names:
            # the executable is named after the bundle (CFBundleExecutable)
            apps = {n.split("/")[1] for n in z.namelist() if n.startswith("Payload/") and n.split("/")[1].endswith(".app")}
            names = [f"Payload/{a}/{a[:-4]}" for a in apps if f"Payload/{a}/{a[:-4]}" in z.namelist()]
        if not names:
            raise SystemExit("no app executable in the IPA")
        macho = z.read(names[0])
    data = section(macho, b"__TEXT", b"__bo1z_engine")
    if data[:4] != b"\x7fELF":
        raise SystemExit("section does not hold a wasmtime artifact (no ELF header)")
    with open(out, "wb") as f:
        f.write(data)
    print(f"{names[0]}: engine {len(data)} bytes -> {out}")


if __name__ == "__main__":
    main()

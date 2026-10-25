"""从 MinGW 的 .a 导入库生成 MSVC 格式的 .lib 导入库。

两遍处理:
  1. 解析所有 .a, 按 DLL 名(不区分大小写)归并符号。多个 .a 映射同一个 DLL 时合并。
  2. 每个 DLL 生成一个 .def, 调 lib.exe 生成 MSVC .lib。

用法:
  python gen_msvc_libs.py <mingw_lib_dir> <out_dir>
"""
import os
import re
import struct
import subprocess
import sys


def parse_ar(data):
    """返回 (symbol_names, members)。"""
    assert data[:8] == b"!<arch>\n", "not an ar archive"
    off = 8
    members = []
    while off + 60 <= len(data):
        hdr = data[off : off + 60]
        if hdr[58:60] != b"`\n":
            break
        name = hdr[0:16].decode(errors="replace").strip()
        size = int(hdr[48:58].decode().strip())
        body = data[off + 60 : off + 60 + size]
        members.append((name, body))
        off += 60 + size + (size % 2)
    symbols = []
    for name, body in members:
        if name == "/":
            count = struct.unpack(">I", body[:4])[0]
            names_blob = body[4 + count * 4 :]
            for s in names_blob.split(b"\x00"):
                if s:
                    symbols.append(s.decode(errors="replace"))
    return symbols, members


def find_dll_name(members):
    for name, body in members:
        if name in ("/", "//"):
            continue
        for m in re.finditer(rb"[A-Za-z0-9_\-]{2,40}\.(?:dll|drv|sys|ocx)", body):
            return m.group(0).decode()
    return None


def lib_filename_to_dll(path):
    base = os.path.basename(path)
    if base.startswith("lib"):
        base = base[3:]
    if base.endswith(".a"):
        base = base[:-2]
    return base + ".dll"


SYM_RE = re.compile(r"[A-Za-z_][A-Za-z0-9_@?$.]{0,200}")


def bad_symbol(s):
    # _load_config_used 会让 link.exe 报 LNK1329
    return s.startswith("_load_config")


def main():
    src_dir = sys.argv[1]
    out_dir = sys.argv[2]
    defs_dir = os.path.join(out_dir, "_defs")
    os.makedirs(defs_dir, exist_ok=True)

    lib_exe = os.environ.get(
        "TBHUB_LIB_EXE",
        r"C:\Program Files\Microsoft Visual Studio\18\Community\VC\Tools\MSVC\14.44.35207\bin\HostX64\x64\lib.exe",
    )

    # 第一遍: 按 DLL 名归并符号
    dll_symbols = {}
    parse_fail = []
    for f in sorted(os.listdir(src_dir)):
        if not f.endswith(".a"):
            continue
        path = os.path.join(src_dir, f)
        try:
            data = open(path, "rb").read()
            symbols, members = parse_ar(data)
        except Exception as e:
            parse_fail.append((f, str(e)))
            continue
        dll = find_dll_name(members) or lib_filename_to_dll(path)
        key = dll.lower()
        syms = dll_symbols.setdefault(key, {"dll": dll, "syms": set(), "src": []})
        syms["src"].append(f)
        for s in symbols:
            if s.startswith("__imp_"):
                continue
            if bad_symbol(s):
                continue
            if SYM_RE.fullmatch(s):
                syms["syms"].add(s)

    # 第二遍: 生成 def + lib
    ok, fail, empty = 0, 0, 0
    for key, info in sorted(dll_symbols.items()):
        dll = info["dll"]
        syms = sorted(info["syms"])
        if not syms:
            empty += 1
            print("NO SYMBOLS", info["src"], dll)
            continue
        def_name = os.path.splitext(dll)[0] + ".def"
        def_path = os.path.join(defs_dir, def_name)
        with open(def_path, "w", encoding="ascii") as fp:
            fp.write("LIBRARY %s\nEXPORTS\n" % dll)
            for s in syms:
                fp.write("  %s\n" % s)
        out_lib = os.path.join(out_dir, os.path.splitext(dll)[0] + ".lib")
        r = subprocess.run(
            [lib_exe, "/NOLOGO", "/DEF:%s" % def_path, "/OUT:%s" % out_lib, "/MACHINE:X64"],
            capture_output=True,
        )
        if r.returncode == 0 and os.path.exists(out_lib):
            ok += 1
        else:
            print("LIB FAIL", dll, r.returncode, r.stderr[:200])
            fail += 1
    for f, e in parse_fail:
        print("PARSE FAIL", f, e)
    print("done ok=%d fail=%d empty=%d" % (ok, fail, empty))


if __name__ == "__main__":
    main()

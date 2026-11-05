#!/usr/bin/env bash
# Verify a set of release artifacts: the same checks run in the Release
# workflow before publishing, and usable on a downloaded release.
#
# Usage:
#   scripts/verify-release.sh --tag vX.Y.Z [options] DIR
#   scripts/verify-release.sh --tag vX.Y.Z --download [DIR]   # fetch with gh first
#
# Options:
#   --tag vX.Y.Z      release tag the artifacts must belong to (required)
#   --pubkey FILE     minisign public key (default: minisign.pub committed next
#                     to this script; a key shipped inside DIR is never trusted)
#   --project NAME    wireguard-tui or wireguard-gui (default: Cargo.toml name)
#   --repo OWNER/NAME GitHub repository for --download (default: JamilleJung/NAME)
#   --download        download the release assets for --tag into DIR with gh
#   --strict          fail instead of warn when a tool (upx, dpkg-deb) is
#                     missing or tar entries are not owned by root (CI uses this)
#   --run             also run each binary the host can execute through
#                     tests/release-regression.sh (needs the checkout's tests/)
#
# Checks:
#   * SHA256SUMS.minisig verifies against the trusted key; the trusted comment
#     names this project and version
#   * SHA256SUMS is well formed, lists every artifact exactly once, and every
#     listed file matches; all mandatory artifacts are present
#   * each tarball contains exactly the expected files, with the expected modes,
#     no links, no path tricks, and (strict) root ownership
#   * each binary is UPX-packed, has the right CPU architecture, is static (TUI:
#     no ELF interpreter, no needed libraries) or uses the expected glibc
#     loader without RPATH/RUNPATH (GUI), and its embedded cargo-auditable
#     dependency data names this package at the tag's version
#   * the .deb has the expected name/version/architecture and files
#
# Exit status: 0 verified, 1 a check failed, 2 usage or missing tools.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tag=""
pubkey=""
project=""
repo=""
download=0
strict=0
run_bins=0
dir=""

usage() { sed -n '2,33p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

while [ $# -gt 0 ]; do
    case "$1" in
        --tag) tag="${2:?}"; shift 2 ;;
        --pubkey) pubkey="${2:?}"; shift 2 ;;
        --project) project="${2:?}"; shift 2 ;;
        --repo) repo="${2:?}"; shift 2 ;;
        --download) download=1; shift ;;
        --strict) strict=1; shift ;;
        --run) run_bins=1; shift ;;
        -h|--help) usage; exit 0 ;;
        -*) echo "verify-release: unknown option $1" >&2; usage >&2; exit 2 ;;
        *) [ -z "$dir" ] || { echo "verify-release: one DIR only" >&2; exit 2; }; dir="$1"; shift ;;
    esac
done

printf '%s\n' "$tag" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+$' \
    || { echo "verify-release: --tag vMAJOR.MINOR.PATCH is required" >&2; exit 2; }
if [ -z "$project" ] && [ -r "$here/Cargo.toml" ]; then
    project="$(awk '/^\[/{p=($0=="[package]");next} p&&$1=="name"{gsub(/"/,"",$3);print $3;exit}' "$here/Cargo.toml")"
fi
case "$project" in
    wireguard-tui|wireguard-gui) ;;
    *) echo "verify-release: --project must be wireguard-tui or wireguard-gui" >&2; exit 2 ;;
esac
[ -n "$pubkey" ] || pubkey="$here/minisign.pub"
[ -r "$pubkey" ] || { echo "verify-release: public key not readable: $pubkey" >&2; exit 2; }

for tool in python3 minisign; do
    command -v "$tool" >/dev/null || { echo "verify-release: $tool is required" >&2; exit 2; }
done

if [ "$download" = 1 ]; then
    command -v gh >/dev/null || { echo "verify-release: --download needs the gh CLI" >&2; exit 2; }
    [ -n "$dir" ] || dir="$(mktemp -d)"
    mkdir -p "$dir"
    gh release download "$tag" --repo "${repo:-JamilleJung/$project}" --dir "$dir" --clobber
fi
[ -n "$dir" ] && [ -d "$dir" ] || { echo "verify-release: artifact directory required" >&2; usage >&2; exit 2; }

export VR_DIR="$dir" VR_TAG="$tag" VR_PUBKEY="$pubkey" VR_PROJECT="$project"
export VR_STRICT="$strict" VR_RUN="$run_bins" VR_HERE="$here"
exec python3 -I - <<'PY'
import hashlib
import io
import json
import os
import re
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile
import zlib

DIR = os.environ["VR_DIR"]
TAG = os.environ["VR_TAG"]
VERSION = TAG[1:]
PUBKEY = os.environ["VR_PUBKEY"]
PROJECT = os.environ["VR_PROJECT"]
STRICT = os.environ["VR_STRICT"] == "1"
RUN = os.environ["VR_RUN"] == "1"
HERE = os.environ["VR_HERE"]

APP = {"wireguard-tui": "wg-tui", "wireguard-gui": "wireguard-gui"}[PROJECT]
TARBALL_ARCHES = {
    "wireguard-tui": ["x86_64", "i686", "aarch64", "armv7"],
    "wireguard-gui": ["x86_64", "aarch64"],
}[PROJECT]
STATIC = PROJECT == "wireguard-tui"
DEB = f"{PROJECT}_{VERSION}-1_amd64.deb"
APPIMAGE = f"{PROJECT}-{VERSION}-x86_64.AppImage"
REQUIRED = [f"{PROJECT}-{VERSION}-{a}-linux.tar.gz" for a in TARBALL_ARCHES] + [DEB]
OPTIONAL = [APPIMAGE] if PROJECT == "wireguard-gui" else []
# ELF e_machine and the glibc loader expected for dynamically linked builds.
ARCH = {
    "x86_64": (2, 62, "/lib64/ld-linux-x86-64.so.2"),
    "i686": (1, 3, "/lib/ld-linux.so.2"),
    "aarch64": (2, 183, "/lib/ld-linux-aarch64.so.1"),
    "armv7": (1, 40, "/lib/ld-linux-armhf.so.3"),
}
LIBC_FAMILY = {"libc.so.6", "libm.so.6", "libgcc_s.so.1", "libpthread.so.0", "libdl.so.2",
               "librt.so.1", "ld-linux-x86-64.so.2", "ld-linux-aarch64.so.1"}
HOST_ARCH = {"x86_64": "x86_64", "amd64": "x86_64", "aarch64": "aarch64", "arm64": "aarch64",
             "i686": "i686", "armv7l": "armv7"}.get(os.uname().machine)

failures = []
warnings = []


def ok(msg):
    print(f"ok    {msg}")


def fail(msg):
    failures.append(msg)
    print(f"FAIL  {msg}")


def warn(msg):
    if STRICT:
        fail(msg + " (strict)")
    else:
        warnings.append(msg)
        print(f"warn  {msg}")


def have(tool):
    return shutil.which(tool) is not None


def path(name):
    return os.path.join(DIR, name)


# ------------------------------------------------------------ signature
def check_signature():
    sums, sig = path("SHA256SUMS"), path("SHA256SUMS.minisig")
    for f in (sums, sig):
        if not os.path.isfile(f):
            fail(f"{os.path.basename(f)} is missing")
            return False
    proc = subprocess.run(["minisign", "-V", "-m", sums, "-x", sig, "-p", PUBKEY],
                          capture_output=True, text=True)
    if proc.returncode != 0:
        fail(f"minisign signature does not verify against {PUBKEY}: {proc.stderr.strip()}")
        return False
    ok(f"SHA256SUMS signature verifies against {os.path.relpath(PUBKEY, HERE) if PUBKEY.startswith(HERE) else PUBKEY}")
    comment = re.search(r"Trusted comment: (.*)", proc.stdout + proc.stderr)
    expected = f"{PROJECT} {VERSION}"
    if comment and comment.group(1).strip() == expected:
        ok(f"trusted comment is '{expected}'")
    else:
        fail(f"trusted comment {comment.group(1).strip() if comment else None!r} != {expected!r}")
    shipped = path("minisign.pub")
    if os.path.isfile(shipped):
        with open(shipped, "rb") as a, open(PUBKEY, "rb") as b:
            if a.read().strip() == b.read().strip():
                ok("minisign.pub shipped with the release equals the trusted key")
            else:
                fail("minisign.pub shipped with the release differs from the trusted key")
    return True


# ------------------------------------------------------------ checksums
def sha256(file):
    h = hashlib.sha256()
    with open(file, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def check_sums():
    listed = {}
    try:
        lines = open(path("SHA256SUMS"), encoding="ascii").read().splitlines()
    except (OSError, UnicodeDecodeError) as e:
        fail(f"SHA256SUMS unreadable: {e}")
        return {}
    for n, line in enumerate(lines, 1):
        m = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9][A-Za-z0-9._+-]*)", line)
        if not m:
            fail(f"SHA256SUMS line {n} is malformed: {line!r}")
            continue
        digest, name = m.groups()
        if name in listed:
            fail(f"SHA256SUMS lists {name} twice")
        listed[name] = digest
    for name, digest in listed.items():
        if not os.path.isfile(path(name)):
            # A partial download may legitimately skip other architectures.
            if name in REQUIRED:
                fail(f"{name} is listed but missing")
            else:
                warn(f"{name} is listed but not present (partial download?)")
            continue
        if sha256(path(name)) == digest:
            ok(f"sha256 {name}")
        else:
            fail(f"sha256 mismatch for {name}")
    for name in REQUIRED:
        if name not in listed:
            fail(f"mandatory artifact {name} is not in SHA256SUMS")
    artifacts = [f for f in os.listdir(DIR)
                 if f.endswith((".tar.gz", ".deb", ".AppImage")) and os.path.isfile(path(f))]
    for f in sorted(artifacts):
        if f not in listed:
            fail(f"{f} is present but not covered by SHA256SUMS")
    unexpected = set(listed) - set(REQUIRED) - set(OPTIONAL)
    for f in sorted(unexpected):
        fail(f"SHA256SUMS lists an unexpected artifact {f}")
    return listed


# ------------------------------------------------------------ ELF
def elf_info(data):
    if data[:4] != b"\x7fELF":
        raise ValueError("not an ELF file")
    cls, endian = data[4], data[5]
    if endian != 1:
        raise ValueError("not little-endian")
    e = "<"
    machine = struct.unpack_from(e + "H", data, 18)[0]
    if cls == 2:
        phoff, shoff = struct.unpack_from(e + "QQ", data, 32)
        phentsize, phnum, shentsize, shnum, shstrndx = struct.unpack_from(e + "HHHHH", data, 54)
    else:
        phoff, shoff = struct.unpack_from(e + "II", data, 28)
        phentsize, phnum, shentsize, shnum, shstrndx = struct.unpack_from(e + "HHHHH", data, 42)
    segs = []
    for i in range(phnum):
        o = phoff + i * phentsize
        if cls == 2:
            p_type, _flags, p_offset, p_vaddr, _pa, p_filesz = struct.unpack_from(e + "IIQQQQ", data, o)
        else:
            p_type, p_offset, p_vaddr, _pa, p_filesz = struct.unpack_from(e + "IIIII", data, o)
        segs.append((p_type, p_offset, p_vaddr, p_filesz))
    interp = None
    dyn = []
    for p_type, off, _va, size in segs:
        if p_type == 3:
            interp = data[off:off + size].rstrip(b"\0").decode(errors="replace")
        if p_type == 2:
            step = 16 if cls == 2 else 8
            fmt = e + ("qQ" if cls == 2 else "iI")
            for o in range(off, off + size, step):
                tag, val = struct.unpack_from(fmt, data, o)
                if tag == 0:
                    break
                dyn.append((tag, val))

    def vaddr_to_off(va):
        for p_type, off, seg_va, size in segs:
            if p_type == 1 and seg_va <= va < seg_va + size:
                return off + va - seg_va
        return None

    strtab = next((v for t, v in dyn if t == 5), None)
    strtab_off = vaddr_to_off(strtab) if strtab is not None else None

    def dynstr(o):
        if strtab_off is None:
            return f"<str@{o}>"
        end = data.index(b"\0", strtab_off + o)
        return data[strtab_off + o:end].decode(errors="replace")

    needed = [dynstr(v) for t, v in dyn if t == 1]
    rpath = [dynstr(v) for t, v in dyn if t in (15, 29)]
    sections = {}
    if shoff and shnum and shstrndx < shnum:
        hdr = []
        for i in range(shnum):
            o = shoff + i * shentsize
            if cls == 2:
                name, _t, _f, _a, off, size = struct.unpack_from(e + "IIQQQQ", data, o)
            else:
                name, _t, _f, _a, off, size = struct.unpack_from(e + "IIIIII", data, o)
            hdr.append((name, off, size))
        _n, str_off, _s = hdr[shstrndx]
        for name, off, size in hdr:
            end = data.index(b"\0", str_off + name)
            sections[data[str_off + name:end].decode(errors="replace")] = data[off:off + size]
    return {"class": cls, "machine": machine, "interp": interp, "needed": needed,
            "rpath": rpath, "sections": sections}


def check_binary(label, raw, arch):
    """raw: bytes of the shipped (UPX-packed) binary."""
    with tempfile.TemporaryDirectory() as tmp:
        packed = os.path.join(tmp, "packed")
        with open(packed, "wb") as fh:
            fh.write(raw)
        data = raw
        if have("upx"):
            if subprocess.run(["upx", "-t", "-q", packed], capture_output=True).returncode == 0:
                ok(f"{label}: UPX-packed and the packed image tests OK")
                unpacked = os.path.join(tmp, "unpacked")
                proc = subprocess.run(["upx", "-d", "-q", "-o", unpacked, packed], capture_output=True, text=True)
                if proc.returncode != 0:
                    fail(f"{label}: upx -d failed: {proc.stderr.strip()}")
                    return
                data = open(unpacked, "rb").read()
            else:
                fail(f"{label}: not a valid UPX-packed binary")
        else:
            warn(f"{label}: upx not installed; inspecting the packed file only")
        try:
            info = elf_info(data)
        except (ValueError, struct.error) as e:
            fail(f"{label}: {e}")
            return
        want_class, want_machine, loader = ARCH[arch]
        if (info["class"], info["machine"]) == (want_class, want_machine):
            ok(f"{label}: ELF architecture is {arch}")
        else:
            fail(f"{label}: ELF class/machine {info['class']}/{info['machine']} is not {arch}")
        if data is raw:
            return  # packed image: linkage/SBOM are only visible after upx -d
        if STATIC:
            if info["interp"] is None and not info["needed"]:
                ok(f"{label}: statically linked (no interpreter, no needed libraries)")
            else:
                fail(f"{label}: not static: interpreter={info['interp']} needed={info['needed']}")
        else:
            if info["interp"] == loader:
                ok(f"{label}: glibc loader {loader}; needs {', '.join(info['needed']) or 'nothing'}")
            else:
                fail(f"{label}: interpreter {info['interp']!r}, expected {loader}")
            if info["rpath"]:
                fail(f"{label}: has RPATH/RUNPATH {info['rpath']}")
            if label.endswith("wg-helper"):
                # The privileged helper may only depend on the C runtime.
                extra = [lib for lib in info["needed"] if lib not in LIBC_FAMILY]
                if extra:
                    fail(f"{label}: privileged helper links non-libc libraries {extra}")
                else:
                    ok(f"{label}: privileged helper links only the C runtime")
        sbom = info["sections"].get(".dep-v0")
        if sbom is None:
            fail(f"{label}: no cargo-auditable .dep-v0 section")
            return
        try:
            deps = json.loads(zlib.decompress(sbom))
        except (zlib.error, ValueError) as e:
            fail(f"{label}: unreadable dependency data: {e}")
            return
        roots = [p for p in deps.get("packages", []) if p.get("root")]
        if len(roots) == 1 and roots[0].get("name") == PROJECT and roots[0].get("version") == VERSION:
            ok(f"{label}: embedded package is {PROJECT} {VERSION}")
        else:
            fail(f"{label}: embedded root package {[(p.get('name'), p.get('version')) for p in roots]} "
                 f"!= {PROJECT} {VERSION}")


# ------------------------------------------------------------ tarballs
def expected_members(arch):
    top = f"{PROJECT}-{VERSION}-{arch}-linux"
    files = {
        f"{top}/{APP}": 0o755,
        f"{top}/wg-helper": 0o755,
        f"{top}/install.sh": 0o755,
        f"{top}/README.md": 0o644,
        f"{top}/LICENSE": 0o644,
        f"{top}/packaging/49-{PROJECT}.rules": 0o644,
        f"{top}/packaging/{PROJECT}.desktop": 0o644,
        f"{top}/packaging/{PROJECT}.svg": 0o644,
    }
    dirs = {top: 0o755, f"{top}/packaging": 0o755}
    return top, files, dirs


def check_tarball(name, arch):
    top, files, dirs = expected_members(arch)
    try:
        tar = tarfile.open(path(name), "r:gz")
        members = tar.getmembers()
    except (OSError, tarfile.TarError) as e:
        fail(f"{name}: unreadable: {e}")
        return
    seen = {}
    owners = set()
    good = True
    for m in members:
        n = m.name.rstrip("/")
        if n.startswith("./"):
            n = n[2:]
        if n.startswith("/") or ".." in n.split("/"):
            fail(f"{name}: unsafe path {m.name!r}")
            good = False
            continue
        if n in seen:
            fail(f"{name}: duplicate entry {n}")
            good = False
        seen[n] = m
        if not (m.isfile() or m.isdir()):
            fail(f"{name}: {n} is not a regular file or directory")
            good = False
        if m.uid != 0 or m.gid != 0:
            owners.add(f"{m.uid}:{m.gid}")
    if owners:
        warn(f"{name}: entries are owned by {', '.join(sorted(owners))}, not 0:0 (root)")
    want = set(files) | set(dirs)
    for n in sorted(set(seen) - want):
        fail(f"{name}: unexpected entry {n}")
        good = False
    for n in sorted(want - set(seen)):
        fail(f"{name}: missing entry {n}")
        good = False
    for n, mode in {**files, **dirs}.items():
        m = seen.get(n)
        if m is None:
            continue
        if (n in dirs) != m.isdir():
            fail(f"{name}: {n} has the wrong type")
            good = False
        elif m.mode & 0o7777 != mode:
            fail(f"{name}: {n} mode {m.mode & 0o7777:04o}, expected {mode:04o}")
            good = False
    if good:
        ok(f"{name}: exactly the expected {len(want)} entries, modes and types")
    with tempfile.TemporaryDirectory() as tmp:
        for binary in (APP, "wg-helper"):
            m = seen.get(f"{top}/{binary}")
            if m is None or not m.isfile():
                continue
            raw = tar.extractfile(m).read()
            check_binary(f"{name}:{binary}", raw, arch)
            if RUN:
                with open(os.path.join(tmp, binary), "wb") as fh:
                    fh.write(raw)
                os.chmod(os.path.join(tmp, binary), 0o755)
        if RUN:
            run_regression(name, arch, tmp)


def run_regression(name, arch, tmp):
    script = os.path.join(HERE, "tests", "release-regression.sh")
    if not os.path.isfile(script):
        warn(f"{name}: --run needs {script}")
        return
    qemu = {"armv7": "arm", "i686": "i386"}.get(arch, arch)
    runnable = sys.platform.startswith("linux") and (
        arch == HOST_ARCH
        or (HOST_ARCH == "x86_64" and arch == "i686")
        or os.path.exists(f"/proc/sys/fs/binfmt_misc/qemu-{qemu}")
    )
    if runnable and not STATIC and arch != HOST_ARCH:
        # A foreign dynamically linked build needs that architecture's loader
        # and libraries, not just QEMU; the release workflow's smoke job runs
        # it inside a matching container instead.
        print(f"skip  {name}: dynamically linked {arch} binaries need a {arch} userland")
        return
    if not runnable:
        print(f"skip  {name}: this host cannot execute {arch} Linux binaries")
        return
    proc = subprocess.run(["bash", script, "--version", VERSION,
                           os.path.join(tmp, APP), os.path.join(tmp, "wg-helper")],
                          capture_output=True, text=True)
    if proc.returncode == 0:
        ok(f"{name}: release regression contract holds ({proc.stdout.strip().splitlines()[-1]})")
    else:
        detail = "\n".join((proc.stdout + proc.stderr).strip().splitlines()[-15:])
        fail(f"{name}: release regression failed (exit {proc.returncode}):\n{detail}")


# ------------------------------------------------------------ .deb
def check_deb():
    if not os.path.isfile(path(DEB)):
        return
    if not have("dpkg-deb"):
        warn(f"{DEB}: dpkg-deb not installed; package metadata not checked")
        return
    fields = subprocess.run(["dpkg-deb", "-f", path(DEB), "Package", "Version", "Architecture"],
                            capture_output=True, text=True)
    got = dict(line.split(": ", 1) for line in fields.stdout.splitlines() if ": " in line)
    want = {"Package": PROJECT, "Version": f"{VERSION}-1", "Architecture": "amd64"}
    if fields.returncode == 0 and got == want:
        ok(f"{DEB}: control fields {want}")
    else:
        fail(f"{DEB}: control fields {got} != {want}")
    listing = subprocess.run(["dpkg-deb", "-c", path(DEB)], capture_output=True, text=True).stdout
    entries = {}
    for line in listing.splitlines():
        parts = line.split(None, 5)
        if len(parts) == 6:
            entries[parts[5].split(" -> ")[0].lstrip(".")] = parts[0]
    for p, mode in {f"/usr/bin/{APP}": "-rwxr-xr-x",
                    f"/usr/lib/{PROJECT}/wg-helper": "-rwxr-xr-x",
                    f"/etc/polkit-1/rules.d/49-{PROJECT}.rules": "-rw-r--r--"}.items():
        if entries.get(p) == mode:
            ok(f"{DEB}: {p} {mode}")
        else:
            fail(f"{DEB}: {p} is {entries.get(p)}, expected {mode}")
    with tempfile.TemporaryDirectory() as tmp:
        if subprocess.run(["dpkg-deb", "-x", path(DEB), tmp]).returncode == 0:
            for rel in (f"usr/bin/{APP}", f"usr/lib/{PROJECT}/wg-helper"):
                f = os.path.join(tmp, rel)
                if os.path.isfile(f):
                    check_binary(f"{DEB}:{rel}", open(f, "rb").read(), "x86_64")


def check_appimage():
    if not os.path.isfile(path(APPIMAGE)):
        if PROJECT == "wireguard-gui":
            print(f"skip  {APPIMAGE}: not part of this release (optional)")
        return
    with open(path(APPIMAGE), "rb") as fh:
        head = fh.read(64)
    if len(head) == 64 and head[:4] == b"\x7fELF" and head[4] == 2 \
            and struct.unpack_from("<H", head, 18)[0] == 62:
        ok(f"{APPIMAGE}: x86_64 ELF runtime")
    else:
        fail(f"{APPIMAGE}: not an x86_64 ELF AppImage")


print(f"Verifying {PROJECT} {TAG} in {DIR}")
if check_signature():
    check_sums()
for arch in TARBALL_ARCHES:
    name = f"{PROJECT}-{VERSION}-{arch}-linux.tar.gz"
    if os.path.isfile(path(name)):
        check_tarball(name, arch)
check_deb()
check_appimage()

print()
if warnings:
    print(f"verify-release: {len(warnings)} warning(s)")
if failures:
    print(f"verify-release: {len(failures)} check(s) FAILED for {PROJECT} {TAG}", file=sys.stderr)
    sys.exit(1)
print(f"verify-release: {PROJECT} {TAG} verified")
PY

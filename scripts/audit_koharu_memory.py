#!/usr/bin/env python3
from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import pwd
import re
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

PAGE_SIZE = os.sysconf("SC_PAGE_SIZE") if os.name == "posix" else 4096
TARGETS = (
    ("detection", "models--mayocream--koharu-layout-rfdetr-seg-2xl-1152", "aed55fdb8ca953c6bec33cf6ed6dd52a9b72bfa2", "model.safetensors"),
    ("baberu", "models--genshiai-daichi--baberu-ocr", "d9cc13153e9a1cd8fdfa3b7b1cc329da2020aeae", "model.safetensors"),
    ("lama", "models--mayocream--lama-manga", "f91c85b26913b3e83f9877867b4c336da3675238", "lama-manga.safetensors"),
)
IMAGE_SUFFIXES = {".png", ".jpg", ".jpeg", ".webp", ".bmp", ".gif", ".tif", ".tiff"}


def command(args: list[str]) -> str:
    return subprocess.check_output(args, text=True, errors="replace")


def read_text(path: Path) -> str:
    try:
        return path.read_text(errors="replace")
    except OSError as exc:
        return f"<unreadable: {exc}>\n"


def write_text(path: Path, content: str) -> None:
    path.write_text(content, encoding="utf-8")


def mebibytes(kib: int) -> str:
    return f"{kib / 1024:,.1f} MiB"


def find_pid(explicit: int | None) -> int:
    if explicit:
        return explicit
    best: tuple[int, int] | None = None
    for status in Path("/proc").glob("[0-9]*/status"):
        text = read_text(status)
        fields = dict(re.findall(r"^([A-Za-z]+):\s+(.+)$", text, re.M))
        if fields.get("Name") != "koharu":
            continue
        rss = int(fields.get("VmRSS", "0").split()[0])
        pid = int(status.parent.name)
        if best is None or rss > best[0]:
            best = (rss, pid)
    if best is None:
        raise SystemExit("koharu process not found; pass --pid")
    return best[1]


def status_fields(pid: int) -> dict[str, str]:
    text = read_text(Path(f"/proc/{pid}/status"))
    return dict(re.findall(r"^([A-Za-z]+):\s+(.+)$", text, re.M))


def integer_kib(fields: dict[str, str], key: str) -> int:
    raw = fields.get(key, "0").split()
    return int(raw[0]) if raw and raw[0].isdigit() else 0


def rollup(pid: int) -> dict[str, int]:
    values: dict[str, int] = {}
    for line in read_text(Path(f"/proc/{pid}/smaps_rollup")).splitlines():
        if ":" not in line:
            continue
        key, raw = line.split(":", 1)
        if not key.replace("_", "").isalpha():
            continue
        number = raw.strip().split()
        if number and number[0].isdigit():
            values[key] = int(number[0])
    return values


def swap_by_mapping(pid: int) -> list[dict[str, int | str]]:
    rows: list[dict[str, int | str]] = []
    current: dict[str, int | str] | None = None
    header = re.compile(r"^[0-9a-f]+-[0-9a-f]+\s")
    for line in read_text(Path(f"/proc/{pid}/smaps")).splitlines():
        if header.match(line):
            if current and int(current["swap_kib"]):
                rows.append(current)
            path = line.split(None, 5)[-1] if len(line.split()) >= 6 else "[anonymous]"
            current = {"path": path, "swap_kib": 0, "rss_kib": 0, "size_kib": 0}
            continue
        if current is None or ":" not in line:
            continue
        key, raw = line.split(":", 1)
        number = raw.strip().split()
        if not number or not number[0].isdigit():
            continue
        if key == "Swap":
            current["swap_kib"] = int(current["swap_kib"]) + int(number[0])
        elif key == "Rss":
            current["rss_kib"] = int(current["rss_kib"]) + int(number[0])
        elif key == "Size":
            current["size_kib"] = int(current["size_kib"]) + int(number[0])
    if current and int(current["swap_kib"]):
        rows.append(current)
    return sorted(rows, key=lambda item: int(item["swap_kib"]), reverse=True)


def map_files(pid: int) -> list[tuple[str, int]]:
    sizes: dict[str, int] = defaultdict(int)
    for line in read_text(Path(f"/proc/{pid}/maps")).splitlines():
        parts = line.split()
        if len(parts) < 6:
            continue
        start, end = parts[0].split("-")
        sizes[parts[-1]] += int(end, 16) - int(start, 16)
    return sorted(sizes.items(), key=lambda item: item[1], reverse=True)


def category(path: str) -> str:
    lower = path.lower()
    if lower.endswith(".safetensors"):
        return "model_weights"
    if ".so" in lower or "/Koharu/packages/" in lower:
        return "runtime_libraries"
    if Path(lower).suffix in IMAGE_SUFFIXES:
        return "project_images"
    if path == "[heap]" or path.startswith("[anon"):
        return "anonymous"
    return "other"


def cgroup_path(pid: int) -> str:
    for line in read_text(Path(f"/proc/{pid}/cgroup")).splitlines():
        if line.startswith("0::"):
            return line.removeprefix("0::")
    return ""


def cgroup_memory(path: str) -> dict[str, int]:
    root = Path("/sys/fs/cgroup") / path.lstrip("/")
    values = {}
    for name in ("memory.current", "memory.swap.current", "memory.peak", "memory.swap.peak"):
        file = root / name
        if file.exists():
            raw = read_text(file).strip()
            if raw.isdigit():
                values[name] = int(raw)
    return values


def cache_roots() -> list[Path]:
    roots = [Path("/home/koharu/.cache/koharu/packages/hugging-face/hub")]
    roots.extend(Path("/home").glob("*/.cache/koharu/packages/hugging-face/hub"))
    roots.extend(Path("/").glob("*/.cache/koharu/packages/hugging-face/hub"))
    return [root for root in roots if root.is_dir()]


def model_files() -> list[dict[str, int | str]]:
    found = []
    for label, repository, revision, filename in TARGETS:
        for root in cache_roots():
            path = root / repository / "snapshots" / revision / filename
            if not path.is_file():
                continue
            stat = path.stat()
            found.append({"model": label, "path": str(path), "bytes": stat.st_size, "inode": stat.st_ino})
    return found


def open_files(pid: int, models: list[dict[str, int | str]]) -> list[str]:
    inodes = {int(item["inode"]) for item in models}
    rows = []
    for fd in Path(f"/proc/{pid}/fd").glob("*"):
        try:
            stat = fd.stat()
            if stat.st_ino in inodes:
                rows.append(f"{fd.name}: {os.readlink(fd)} inode={stat.st_ino}")
        except OSError:
            continue
    return rows


def children(pid: int) -> list[dict[str, str]]:
    try:
        text = command(["ps", "-eo", "pid,ppid,user,rss,vsz,nlwp,stat,args", "--no-headers"])
    except (OSError, subprocess.CalledProcessError):
        return []
    rows = []
    for line in text.splitlines():
        parts = line.split(None, 7)
        if len(parts) == 8 and parts[1] == str(pid):
            rows.append(dict(zip(("pid", "ppid", "user", "rss_kib", "vsz_kib", "threads", "stat", "args"), parts, strict=True)))
    return rows


def main() -> None:
    parser = argparse.ArgumentParser(description="Read-only koharu memory attribution audit.")
    parser.add_argument("--pid", type=int)
    parser.add_argument("--output", type=Path, default=Path("/home/qinzhu") / f"koharu-memory-audit-{dt.datetime.now():%Y%m%d-%H%M%S}")
    args = parser.parse_args()
    if hasattr(os, "geteuid") and os.geteuid() != 0:
        print("warning: not root; smaps and file descriptors may be incomplete", file=sys.stderr)

    pid = find_pid(args.pid)
    fields = status_fields(pid)
    totals = rollup(pid)
    maps = map_files(pid)
    groups: dict[str, int] = defaultdict(int)
    for path, size in maps:
        groups[category(path)] += size
    models = model_files()
    swapped = swap_by_mapping(pid)
    group = cgroup_path(pid)
    uid = os.stat(f"/proc/{pid}").st_uid
    report = {
        "time": dt.datetime.now(dt.timezone.utc).astimezone().isoformat(),
        "pid": pid,
        "user": pwd.getpwuid(uid).pw_name,
        "threads": integer_kib(fields, "Threads"),
        "cgroup": group,
        "cgroup_memory": cgroup_memory(group),
        "status_kib": {key: integer_kib(fields, key) for key in ("VmRSS", "VmSwap", "RssAnon", "RssFile", "RssShmem")},
        "rollup_kib": totals,
        "map_groups_bytes": dict(groups),
        "model_files": models,
        "swap_by_mapping": swapped[:100],
        "open_model_files": open_files(pid, models),
        "children": children(pid),
    }
    args.output.mkdir(parents=True, exist_ok=True)
    write_text(args.output / "report.json", json.dumps(report, indent=2) + "\n")
    write_text(args.output / "status.txt", read_text(Path(f"/proc/{pid}/status")))
    write_text(args.output / "smaps_rollup.txt", read_text(Path(f"/proc/{pid}/smaps_rollup")))
    write_text(args.output / "maps.txt", "".join(f"{size}\t{path}\n" for path, size in maps))
    lines = [
        f"pid={pid} user={report['user']} threads={report['threads']}",
        f"PSS={mebibytes(totals.get('Pss', 0))} SwapPss={mebibytes(totals.get('SwapPss', 0))}",
        "mapped file groups:",
        *(f"  {name}={size / 1024**2:,.1f} MiB" for name, size in sorted(groups.items(), key=lambda item: item[1], reverse=True)),
        "target model files:",
        *(f"  {item['model']}={int(item['bytes']) / 1024**2:,.1f} MiB {item['path']}" for item in models),
        "open target model files:",
        *(f"  {row}" for row in report["open_model_files"] or ["<none>"]),
    ]
    write_text(args.output / "summary.txt", "\n".join(lines) + "\n")
    print("\n".join(lines))
    print(f"output={args.output}")


if __name__ == "__main__":
    main()

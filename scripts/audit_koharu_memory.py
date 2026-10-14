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
# Resident anonymous memory may wobble by a few MiB across a task; only a
# sustained increase is the leak this audit is looking for.
RSS_ANON_SLACK_KIB = 4096


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


def user_name(uid: int) -> str:
    """The owner of the audited process.

    An audit run from a container may not share the host's user database, so an
    unmapped uid is reported as its number rather than failing the run.
    """
    try:
        return pwd.getpwuid(uid).pw_name
    except KeyError:
        return str(uid)


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


ADDRESS_RANGE = re.compile(r"^[0-9a-fA-F]+-[0-9a-fA-F]+\s")


def mapping_lines(pid: int) -> list[str]:
    """Raw /proc/PID/maps lines, excluding any error placeholder.

    `read_text` reports an unreadable file as a single prose line, which is
    not a mapping and must never reach the address-range parsers.
    """
    return [
        line
        for line in read_text(Path(f"/proc/{pid}/maps")).splitlines()
        if ADDRESS_RANGE.match(line)
    ]


def map_files(pid: int) -> list[tuple[str, int]]:
    sizes: dict[str, int] = defaultdict(int)
    for line in mapping_lines(pid):
        parts = line.split()
        if len(parts) < 6:
            continue
        start, end = parts[0].split("-")
        sizes[parts[-1]] += int(end, 16) - int(start, 16)
    return sorted(sizes.items(), key=lambda item: item[1], reverse=True)


def anonymous_mappings(pid: int) -> dict[str, int]:
    """Size of each anonymous mapping the process holds.

    The task subprocess boundary is validated by these not growing: the model
    runtimes keep their allocations under `[anonymous]`, `[heap]` and `memfd`
    even after every Rust owner is dropped.
    """
    sizes: dict[str, int] = defaultdict(int)
    for line in mapping_lines(pid):
        parts = line.split()
        start, end = parts[0].split("-")
        path = parts[-1] if len(parts) >= 6 else "[anonymous]"
        if path in ("[heap]", "[stack]") or path.startswith("[anon") or path.startswith("/memfd:") or path.startswith("anon_inode:"):
            sizes[path] += int(end, 16) - int(start, 16)
    return dict(sizes)


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


def diff(before: dict, after: dict) -> dict[str, object]:
    """Compare two audit reports the way the acceptance criteria read them.

    The task subprocess boundary is the fix: once the child exits, the parent
    must not keep the anonymous mappings or swap pages the run allocated.
    """
    grown = {
        name: size - before["anonymous_mappings_bytes"].get(name, 0)
        for name, size in after["anonymous_mappings_bytes"].items()
        if size > before["anonymous_mappings_bytes"].get(name, 0)
    }
    return {
        "memory_current_delta": after["memory_current"] - before["memory_current"],
        "memory_swap_current_delta": after["memory_swap_current"] - before["memory_swap_current"],
        "swap_kib_delta": after["swap_kib"] - before["swap_kib"],
        "rss_anon_delta": after["status_kib"]["RssAnon"] - before["status_kib"]["RssAnon"],
        "heap_delta": after["anonymous_mappings_bytes"].get("[heap]", 0)
        - before["anonymous_mappings_bytes"].get("[heap]", 0),
        "growing_anonymous_mappings_bytes": grown,
        "task_children": after.get("task_children", []),
        "open_model_files": after.get("open_model_files", []),
    }


def verdict(report: dict[str, object]) -> list[str]:
    """One line per acceptance criterion that did not hold.

    Anonymous mappings reserve address space up front, so their size alone is
    not a leak: an arena that grows its reservation stays flat in RSS. What has
    to hold is that resident anonymous memory, the heap, and swap do not grow,
    and that the task left neither a subprocess nor an open model file behind.
    """
    failures = []
    if report["memory_swap_current_delta"] > 0:
        failures.append(
            f"memory.swap.current grew by {report['memory_swap_current_delta']} bytes"
        )
    if report["swap_kib_delta"] > 0:
        failures.append(f"VmSwap grew by {report['swap_kib_delta']} KiB")
    if report["rss_anon_delta"] > RSS_ANON_SLACK_KIB:
        failures.append(f"RssAnon grew by {report['rss_anon_delta']} KiB")
    if report["heap_delta"] > 0:
        failures.append(f"[heap] grew by {report['heap_delta']} bytes")
    if report["task_children"]:
        failures.append("pipeline-task subprocess is still running")
    if report["open_model_files"]:
        failures.append("parent still holds model files open")
    return failures


def main() -> None:
    parser = argparse.ArgumentParser(description="Read-only koharu memory attribution audit.")
    parser.add_argument("--pid", type=int)
    parser.add_argument(
        "--compare",
        type=Path,
        help="a report.json taken before the task; prints the acceptance verdict",
    )
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
        "user": user_name(uid),
        "threads": integer_kib(fields, "Threads"),
        "cgroup": group,
        "cgroup_memory": cgroup_memory(group),
        "status_kib": {key: integer_kib(fields, key) for key in ("VmRSS", "VmSwap", "RssAnon", "RssFile", "RssShmem")},
        "rollup_kib": totals,
        "map_groups_bytes": dict(groups),
        "anonymous_mappings_bytes": anonymous_mappings(pid),
        "memory_current": cgroup_memory(group).get("memory.current", 0),
        "memory_swap_current": cgroup_memory(group).get("memory.swap.current", 0),
        "swap_kib": integer_kib(fields, "VmSwap"),
        "task_children": [row for row in children(pid) if "pipeline-task" in row.get("args", "")],
        "model_files": models,
        "swap_by_mapping": swapped[:100],
        "open_model_files": open_files(pid, models),
        "children": children(pid),
    }
    failures: list[str] = []
    if args.compare:
        before = json.loads(args.compare.read_text(encoding="utf-8"))
        comparison = diff(before, report)
        failures = verdict(comparison)
        report["comparison"] = comparison
        report["verdict"] = failures
    args.output.mkdir(parents=True, exist_ok=True)
    write_text(args.output / "report.json", json.dumps(report, indent=2) + "\n")
    write_text(args.output / "status.txt", read_text(Path(f"/proc/{pid}/status")))
    write_text(args.output / "smaps_rollup.txt", read_text(Path(f"/proc/{pid}/smaps_rollup")))
    write_text(args.output / "maps.txt", "".join(f"{size}\t{path}\n" for path, size in maps))
    if args.compare:
        acceptance = ["acceptance:", *([f"  FAIL {row}" for row in failures] or ["  all criteria held"])]
    else:
        acceptance = []
    lines = [
        f"pid={pid} user={report['user']} threads={report['threads']}",
        f"PSS={mebibytes(totals.get('Pss', 0))} SwapPss={mebibytes(totals.get('SwapPss', 0))}",
        "mapped file groups:",
        *(f"  {name}={size / 1024**2:,.1f} MiB" for name, size in sorted(groups.items(), key=lambda item: item[1], reverse=True)),
        *acceptance,
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

"""Find crates in Cargo.lock that were published too recently to trust yet.

Cargo has no setting to refuse freshly published crates, so this does the check by hand
using the publish times in the crates.io sparse index. Run it after anything that changes
Cargo.lock (cargo add, cargo update) and BEFORE building, since building runs the
crates' build scripts.

Usage: python3 scripts/lock_ages.py [path/to/Cargo.lock] [minimum age in days]

Defaults to ./Cargo.lock and 7 days. For each too-new crate it prints the newest older
version in the same semver-compatible range as a ready-to-run `cargo update --precise` line.
"""
import datetime
import json
import re
import sys
import urllib.request
from concurrent.futures import ThreadPoolExecutor

lock_path = sys.argv[1] if len(sys.argv) > 1 else "Cargo.lock"
minimum_days = int(sys.argv[2]) if len(sys.argv) > 2 else 7
cutoff = (datetime.date.today() - datetime.timedelta(days=minimum_days)).isoformat()

packages = []
for block in open(lock_path).read().split("[[package]]")[1:]:
    name = re.search(r'^name = "([^"]+)"', block, re.M).group(1)
    version = re.search(r'^version = "([^"]+)"', block, re.M).group(1)
    if 'source = "registry+https://github.com/rust-lang/crates.io-index"' in block:
        packages.append((name, version))
    else:
        print(f"SKIPPED (not from crates.io): {name} {version}")


def index_path(name):
    n = name.lower()
    if len(n) <= 2:
        return f"{len(n)}/{n}"
    if len(n) == 3:
        return f"3/{n[0]}/{n}"
    return f"{n[:2]}/{n[2:4]}/{n}"


def numeric(version):
    return tuple(int(x) for x in re.match(r"(\d+)\.(\d+)\.(\d+)", version).groups())


def compatible_range(version):
    major, minor, patch = numeric(version)
    if major != 0:
        return (major,)
    if minor != 0:
        return (major, minor)
    return (major, minor, patch)


def check(package):
    name, version = package
    request = urllib.request.Request(
        f"https://index.crates.io/{index_path(name)}", headers={"User-Agent": "lock-age-check"}
    )
    lines = urllib.request.urlopen(request, timeout=30).read().splitlines()
    rows = [json.loads(line) for line in lines]
    mine = next((r for r in rows if r["vers"] == version), None)
    if mine is None or not mine.get("pubtime"):
        return f"UNKNOWN publish time: {name} {version}"
    if mine["pubtime"][:10] < cutoff:
        return None
    older = [
        r for r in rows
        if not r.get("yanked") and r.get("pubtime") and r["pubtime"][:10] < cutoff
        and "-" not in r["vers"].split("+")[0]
        and compatible_range(r["vers"]) == compatible_range(version)
    ]
    if not older:
        return f"TOO NEW, no older compatible version: {name} {version} ({mine['pubtime'][:10]})"
    best = max(older, key=lambda r: numeric(r["vers"]))
    return (
        f"TOO NEW: {name} {version} ({mine['pubtime'][:10]})\n"
        f"  cargo update -p {name}@{version} --precise {best['vers']}   # {best['pubtime'][:10]}"
    )


with ThreadPoolExecutor(max_workers=8) as pool:
    results = [r for r in pool.map(check, packages) if r]
print("\n".join(results) if results else "all clear")
print(f"checked {len(packages)} crates: published before {cutoff} passes")
# A failing exit code lets build scripts stop when something is too new.
sys.exit(1 if results else 0)

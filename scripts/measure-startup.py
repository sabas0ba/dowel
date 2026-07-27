#!/usr/bin/env python3
"""起動時間の計測。

常駐しない構成（ADR-0002）では起動時間が毎回課金される。予算は無操作時 10ms 以下
（docs/20-architecture.md 5.4）であり、実装言語と依存の選択を縛る制約でもあるため、
継続して測る。

計測対象は `examples/hello` の複製。例そのものを汚さないため `.work/` へ複製する。

CI の実行機は揺れるので、この計測は失敗の判定には使わない（`verify.sh` では advisory）。
明らかな退行（依存を増やして起動が桁で遅くなる等）だけを拾えるよう、
緩い上限だけ設けてある。
"""

import argparse
import json
import os
import shutil
import statistics
import subprocess
import sys
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BINARY = os.path.join(REPO, "target", "release", "dowel")
# 桁違いの退行だけを拾う上限。予算（10ms）そのものではない。
CEILING_MS = 100.0


def stage_project(work):
    src = os.path.join(REPO, "examples", "hello")
    dst = os.path.join(work, "hello")
    if os.path.exists(dst):
        shutil.rmtree(dst)
    shutil.copytree(dst=dst, src=src, ignore=shutil.ignore_patterns(".dowel"))
    return os.path.join(dst, "app")


def bench(cwd, args, n):
    samples = []
    for _ in range(n):
        start = time.perf_counter()
        subprocess.run(
            [BINARY] + args,
            cwd=cwd,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        )
        samples.append((time.perf_counter() - start) * 1000)
    return {
        "args": args,
        "runs": n,
        "min_ms": min(samples),
        "median_ms": statistics.median(samples),
        "max_ms": max(samples),
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--runs", type=int, default=20)
    args = ap.parse_args()

    if not os.path.exists(BINARY):
        print(f"{BINARY} がない。先に `cargo build --release` を実行する", file=sys.stderr)
        return 1

    work = os.path.join(REPO, ".work", "startup")
    os.makedirs(work, exist_ok=True)
    cwd = stage_project(work)

    measurements = [
        bench(cwd, ["--version"], args.runs),
        bench(cwd, ["check"], args.runs),
        bench(cwd, ["graph", "--format=json"], args.runs),
    ]

    size = os.path.getsize(BINARY)
    result = {
        "binary_bytes": size,
        "budget_ms": 10.0,
        "ceiling_ms": CEILING_MS,
        "measurements": measurements,
    }
    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
    with open(args.out, "w", encoding="utf-8") as f:
        json.dump(result, f, ensure_ascii=False, indent=2)
        f.write("\n")

    print(f"バイナリ {size / 1024:.0f}KB")
    for m in measurements:
        print(
            "  dowel {:<22} 最小 {:6.2f}ms  中央 {:6.2f}ms".format(
                " ".join(m["args"]), m["min_ms"], m["median_ms"]
            )
        )

    worst = max(m["median_ms"] for m in measurements)
    if worst > CEILING_MS:
        print(
            f"中央値 {worst:.2f}ms が上限 {CEILING_MS:.0f}ms を超えている",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

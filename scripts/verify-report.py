#!/usr/bin/env python3
"""検証結果の集計。

`verify.sh` が書いた TSV から、人間向けの要約（Markdown）と機械可読な JSON を作る。
Markdown は GitHub Actions のジョブ要約へそのまま流せる形にしてある。

終了状態は「必須（gating）の段階が1つでも失敗したか」で決める。
参考（advisory）の段階は記録に残すが全体を落とさない。
"""

import argparse
import json
import os
import sys

STATE_MARK = {"ok": "✅", "failed": "❌", "warn": "⚠️", "skipped": "—"}


def read_records(path):
    rows = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.rstrip("\n")
            if not line:
                continue
            name, state, ms, passed, failed, gating = line.split("\t")
            rows.append(
                {
                    "name": name,
                    "state": state,
                    "duration_ms": int(ms),
                    "passed": int(passed),
                    "failed": int(failed),
                    "gating": gating == "gating",
                }
            )
    return rows


def read_startup(out_dir):
    path = os.path.join(out_dir, "startup.json")
    if not os.path.exists(path):
        return None
    try:
        with open(path, encoding="utf-8") as f:
            return json.load(f)
    except (json.JSONDecodeError, OSError):
        return None


def render_markdown(rows, startup, ok):
    total_passed = sum(r["passed"] for r in rows)
    total_failed = sum(r["failed"] for r in rows)
    total_ms = sum(r["duration_ms"] for r in rows)

    out = []
    out.append("# Verification results")
    out.append("")
    out.append(
        "**{}** - {} tests passed, {} failed, {:.1f}s total".format(
            "PASSED" if ok else "FAILED", total_passed, total_failed, total_ms / 1000
        )
    )
    out.append("")
    out.append("| | Step | Result | Passed | Failed | Time |")
    out.append("|---|---|---|---:|---:|---:|")
    for r in rows:
        note = "" if r["gating"] else " (advisory)"
        out.append(
            "| {} | `{}`{} | {} | {} | {} | {}ms |".format(
                STATE_MARK.get(r["state"], "?"),
                r["name"],
                note,
                r["state"],
                r["passed"],
                r["failed"],
                r["duration_ms"],
            )
        )
    out.append("")

    if startup and startup.get("measurements"):
        out.append("## Startup time")
        out.append("")
        out.append(
            "The budget is 10ms or less when idle (docs/20-architecture.md 5.4). "
            "CI runners are noisy, so this step is advisory and does not fail the run."
        )
        out.append("")
        out.append("| Command | Min | Median |")
        out.append("|---|---:|---:|")
        for m in startup["measurements"]:
            out.append(
                "| `dowel {}` | {:.2f}ms | {:.2f}ms |".format(
                    " ".join(m["args"]), m["min_ms"], m["median_ms"]
                )
            )
        out.append("")

    failures = [r for r in rows if r["state"] == "failed"]
    if failures:
        out.append("## Failed steps")
        out.append("")
        for r in failures:
            out.append("- `{}` - output in `logs/{}.log`".format(r["name"], r["name"]))
        out.append("")

    return "\n".join(out) + "\n"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--records", required=True)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    rows = read_records(args.records)
    startup = read_startup(args.out)
    ok = not any(r["state"] == "failed" for r in rows)

    summary = render_markdown(rows, startup, ok)
    with open(os.path.join(args.out, "summary.md"), "w", encoding="utf-8") as f:
        f.write(summary)

    with open(os.path.join(args.out, "results.json"), "w", encoding="utf-8") as f:
        json.dump(
            {
                "ok": ok,
                "steps": rows,
                "totals": {
                    "passed": sum(r["passed"] for r in rows),
                    "failed": sum(r["failed"] for r in rows),
                    "duration_ms": sum(r["duration_ms"] for r in rows),
                },
                "startup": startup,
            },
            f,
            ensure_ascii=False,
            indent=2,
        )
        f.write("\n")

    sys.stdout.write(summary)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())

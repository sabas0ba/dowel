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
import re
import sys

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


# libtest の1件の結果行。`test 名前 ... ok` の形。要約行（`test result:`）は
# `result:` が名前の位置に来ないため一致しない。
TEST_LINE = re.compile(r"^test (.+?) \.\.\. (ok|FAILED|ignored)(?:,.*)?$")


def read_tests(out_dir, name):
    """段階のログから個々のテスト名と結果を、実行順のまま拾う。"""
    path = os.path.join(out_dir, "logs", "{}.log".format(name))
    if not os.path.exists(path):
        return []
    tests = []
    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            m = TEST_LINE.match(line.rstrip("\n"))
            if m:
                tests.append({"name": m.group(1), "result": m.group(2)})
    return tests


def read_startup(out_dir):
    path = os.path.join(out_dir, "startup.json")
    if not os.path.exists(path):
        return None
    try:
        with open(path, encoding="utf-8") as f:
            return json.load(f)
    except (json.JSONDecodeError, OSError):
        return None


def render_tests_by_step(rows, out_dir):
    """段階ごとのテスト名の一覧。折りたたみで、何を検査したかを一目で追える。"""
    out = []
    for r in rows:
        tests = read_tests(out_dir, r["name"])
        if not tests:
            # fmt や release-build のように、個々のテストを持たない段階は出さない。
            continue
        failed = [t for t in tests if t["result"] == "FAILED"]
        summary = "<code>{}</code> — {} tests".format(r["name"], len(tests))
        if failed:
            summary += ", {} FAILED".format(len(failed))
        out.append("<details>")
        out.append("<summary>{}</summary>".format(summary))
        out.append("")
        # 失敗を先頭に出す。折りたたみを開いた理由は大抵それである。
        for t in failed:
            out.append("- FAILED: `{}`".format(t["name"]))
        for t in tests:
            if t["result"] == "FAILED":
                continue
            note = " (ignored)" if t["result"] == "ignored" else ""
            out.append("- `{}`{}".format(t["name"], note))
        out.append("")
        out.append("</details>")
    if not out:
        return []
    return ["## Tests by step", ""] + out + [""]


def render_markdown(rows, startup, ok, out_dir):
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
    out.append("| Step | Result | Passed | Failed | Time |")
    out.append("|---|---|---:|---:|---:|")
    for r in rows:
        note = "" if r["gating"] else " (advisory)"
        # 状態は `Result` 列の語がそのまま示す。記号の列は情報を増やさない。
        out.append(
            "| `{}`{} | {} | {} | {} | {}ms |".format(
                r["name"],
                note,
                r["state"].upper() if r["state"] == "failed" else r["state"],
                r["passed"],
                r["failed"],
                r["duration_ms"],
            )
        )
    out.append("")
    out.extend(render_tests_by_step(rows, out_dir))

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

    summary = render_markdown(rows, startup, ok, args.out)
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

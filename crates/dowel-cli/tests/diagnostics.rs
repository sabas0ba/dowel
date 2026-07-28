//! 診断の網羅検査。安定コードを持つ診断が、実際に CLI から出ることを確かめる。
//!
//! 単体テストは診断が生成されることを検査する。しかし利用者に到達するまでには
//! 評価・検証・整形・出力の経路があり、途中で破棄されても単体テストは成功する。
//! 本ファイルは `dowel` を実際に起動し、`--message-format=json` への出力を検査する。
//!
//! 併せて網羅も追跡する。診断を追加して事例を追加しなかった場合、検証が失敗する。
//! 設計は [`docs/51-testing.md`](../../../docs/51-testing.md) にある。

mod common;

use common::{repo_root, Project};
use std::collections::BTreeSet;

/// 1つの診断を出させる最小の入力。
struct Case {
    /// 期待する安定コード
    code: &'static str,
    /// なぜこの入力でその診断が出るのか。読み手が入力を復元できるようにする
    why: &'static str,
    /// 基準プロジェクトに上書きするファイル
    files: &'static [(&'static str, &'static str)],
    /// `app` ディレクトリで起動する引数
    args: &'static [&'static str],
}

const CHECK: &[&str] = &["check", "--message-format=json"];
const BUILD: &[&str] = &["build", "--message-format=json"];

/// 診断を出さない最小のプロジェクト。各事例はこの上に必要なファイルのみを上書きする。
fn base(p: &Project) {
    p.write("app/dowel.toml", "[package]\nname    = \"app\"\nversion = \"0.1.0\"\n");
    p.write("app/dowel.build", "[bin.app]\nsources = glob(\"src/*.c\")\n");
    p.write("app/src/main.c", "int main(void) { return 0; }\n");
}

/// 併合の衝突を起こす最小の構成。
///
/// 2つのパッケージが要る。1パッケージでは同じプロパティへ2つの値が
/// 到達する経路がなく、`error_on_conflict` と `must_equal` は発現しない。
const TWO_PACKAGES: &[(&str, &str)] = &[
    (
        "app/dowel.toml",
        "[package]\nname    = \"app\"\nversion = \"0.1.0\"\n\n[[dependencies]]\nname = \"lib\"\npath = \"../lib\"\n",
    ),
    ("lib/dowel.toml", "[package]\nname    = \"lib\"\nversion = \"0.1.0\"\n"),
    ("lib/src/lib.c", "int lib_answer(void) { return 42; }\n"),
];

const TWO_PACKAGES_WITH_CONFLICTING_DEFINES: &[(&str, &str)] = &[
    TWO_PACKAGES[0],
    TWO_PACKAGES[1],
    TWO_PACKAGES[2],
    (
        "lib/dowel.build",
        "[lib.lib]\nsources = glob(\"src/*.c\")\n\n[lib.lib.public]\ndefines = { SHARED_LIMIT = 64 }\n",
    ),
    (
        "app/dowel.build",
        "[bin.app]\nsources = glob(\"src/*.c\")\n\n[bin.app.private]\ndeps    = [dep(\"lib\")]\ndefines = { SHARED_LIMIT = 128 }\n",
    ),
];

const TWO_PACKAGES_WITH_CONFLICTING_ABI: &[(&str, &str)] = &[
    TWO_PACKAGES[0],
    TWO_PACKAGES[1],
    TWO_PACKAGES[2],
    (
        "lib/dowel.build",
        "[lib.lib]\nsources = glob(\"src/*.c\")\n\n[lib.lib.public]\nabi = \"x86_64-linux-musl\"\n",
    ),
    (
        "app/dowel.build",
        "[bin.app]\nsources = glob(\"src/*.c\")\n\n[bin.app.private]\ndeps = [dep(\"lib\")]\nabi  = \"x86_64-linux-gnu\"\n",
    ),
];

const CASES: &[Case] = &[
    // --- 構文 -----------------------------------------------------------
    Case {
        code: "unterminated-string",
        why: "the string literal has no closing quote",
        files: &[("app/dowel.build", "[bin.app]\nsources = glob(\"src/*.c)\n")],
        args: CHECK,
    },
    Case {
        code: "unterminated-comment",
        why: "the block comment is never closed",
        files: &[("app/dowel.build", "/* open\n[bin.app]\nsources = glob(\"src/*.c\")\n")],
        args: CHECK,
    },
    Case {
        code: "expected-token",
        why: "the call has no closing parenthesis",
        files: &[("app/dowel.build", "[bin.app]\nsources = glob(\"src/*.c\"\n")],
        args: CHECK,
    },
    Case {
        code: "unknown-char",
        why: "`$` is not part of the lexical grammar",
        files: &[("app/dowel.build", "[bin.app]\nsources = glob(\"src/*.c\")\n$\n")],
        args: CHECK,
    },
    // --- マニフェストの厳密性 --------------------------------------------
    Case {
        code: "expression-in-strict-toml",
        why: "`dowel.toml` must stay readable by a plain TOML parser",
        files: &[(
            "app/dowel.toml",
            "[package]\nname    = \"app\"\nversion = \"0.1.0\"\nedition = match cfg.opt { debug => \"2026\", release => \"2026\" }\n",
        )],
        args: CHECK,
    },
    // --- パッケージ -------------------------------------------------------
    Case {
        code: "missing-manifest",
        why: "the directory has no `dowel.toml`",
        files: &[],
        args: CHECK,
    },
    Case {
        code: "missing-build",
        why: "the package has a manifest but no `dowel.build`",
        files: &[],
        args: CHECK,
    },
    Case {
        code: "missing-table",
        why: "`[package]` is required in `dowel.toml`",
        files: &[("app/dowel.toml", "# no package table\n")],
        args: CHECK,
    },
    Case {
        code: "missing-field",
        why: "`[package]` has no `name`",
        files: &[("app/dowel.toml", "[package]\nversion = \"0.1.0\"\n")],
        args: CHECK,
    },
    Case {
        code: "duplicate-table",
        why: "the same table header appears twice",
        files: &[(
            "app/dowel.build",
            "[bin.app]\nsources = glob(\"src/*.c\")\n\n[bin.app]\nsources = glob(\"src/*.c\")\n",
        )],
        args: CHECK,
    },
    Case {
        code: "duplicate-key",
        why: "the same key is set twice in one table",
        files: &[(
            "app/dowel.build",
            "[bin.app]\nsources = glob(\"src/*.c\")\nsources = glob(\"src/*.c\")\n",
        )],
        args: CHECK,
    },
    Case {
        code: "duplicate-property",
        why: "the same property arrives from two blocks of the same target",
        files: &[(
            "app/dowel.build",
            "[bin.app]\nsources = glob(\"src/*.c\")\nprivate.flags = [\"-O0\"]\n\n[bin.app.private]\nflags = [\"-O1\"]\n",
        )],
        args: CHECK,
    },
    Case {
        code: "toplevel-entry",
        why: "a key sits outside any table header",
        files: &[("app/dowel.build", "stray = 1\n\n[bin.app]\nsources = glob(\"src/*.c\")\n")],
        args: CHECK,
    },
    Case {
        code: "too-deep-table",
        why: "`[kind.name.block.more]` has no meaning",
        files: &[(
            "app/dowel.build",
            "[bin.app]\nsources = glob(\"src/*.c\")\n\n[bin.app.public.extra]\nincludes = []\n",
        )],
        args: CHECK,
    },
    Case {
        code: "missing-target-name",
        why: "`[bin]` names no target",
        files: &[("app/dowel.build", "[bin]\nsources = glob(\"src/*.c\")\n")],
        args: CHECK,
    },
    Case {
        code: "unknown-kind",
        why: "`exe` is not a table kind",
        files: &[("app/dowel.build", "[exe.app]\nsources = glob(\"src/*.c\")\n")],
        args: CHECK,
    },
    Case {
        code: "unimplemented-kind",
        why: "`bench` is a recognized kind that is not implemented yet",
        files: &[(
            "app/dowel.build",
            "[bin.app]\nsources = glob(\"src/*.c\")\n\n[bench.b]\nsources = glob(\"src/*.c\")\n",
        )],
        args: CHECK,
    },
    Case {
        code: "unknown-block",
        why: "only `public` and `private` exist",
        files: &[(
            "app/dowel.build",
            "[bin.app]\nsources = glob(\"src/*.c\")\n\n[bin.app.internal]\nflags = [\"-O0\"]\n",
        )],
        args: CHECK,
    },
    Case {
        code: "unknown-property",
        why: "`sourcess` is not a property",
        files: &[("app/dowel.build", "[bin.app]\nsourcess = glob(\"src/*.c\")\n")],
        args: CHECK,
    },
    Case {
        code: "type-mismatch",
        why: "`sources` is a list of paths, not a string",
        files: &[("app/dowel.build", "[bin.app]\nsources = \"src/main.c\"\n")],
        args: CHECK,
    },
    // --- 式 ---------------------------------------------------------------
    Case {
        code: "unknown-function",
        why: "`files(...)` does not exist",
        files: &[("app/dowel.build", "[bin.app]\nsources = files(\"src/*.c\")\n")],
        args: CHECK,
    },
    Case {
        code: "unknown-namespace",
        why: "`conf` is not a namespace",
        files: &[(
            "app/dowel.build",
            "[bin.app]\nsources = glob(\"src/*.c\")\n\n[bin.app.private]\nflags = [\"-O0\"] when conf.opt\n",
        )],
        args: CHECK,
    },
    Case {
        code: "unknown-cfg-key",
        why: "`cfg.mode` is not in the configuration vocabulary",
        files: &[(
            "app/dowel.build",
            "[bin.app]\nsources = glob(\"src/*.c\")\n\n[bin.app.private]\nflags = match cfg.mode { _ => [] }\n",
        )],
        args: CHECK,
    },
    Case {
        code: "unknown-pattern",
        why: "`fast` is not a value of `cfg.opt`",
        files: &[(
            "app/dowel.build",
            "[bin.app]\nsources = glob(\"src/*.c\")\n\n[bin.app.private]\nflags = match cfg.opt { debug => [], release => [], fast => [] }\n",
        )],
        args: CHECK,
    },
    Case {
        code: "non-exhaustive-match",
        why: "`cfg.opt` has a closed domain and `release` is not covered",
        files: &[(
            "app/dowel.build",
            "[bin.app]\nsources = glob(\"src/*.c\")\n\n[bin.app.private]\nflags = match cfg.opt { debug => [] }\n",
        )],
        args: CHECK,
    },
    // --- 依存 -------------------------------------------------------------
    Case {
        code: "incomplete-dependency",
        why: "the dependency names no source",
        files: &[(
            "app/dowel.toml",
            "[package]\nname    = \"app\"\nversion = \"0.1.0\"\n\n[[dependencies]]\nname = \"libfoo\"\n",
        )],
        args: CHECK,
    },
    Case {
        code: "unsupported-dependency",
        why: "fetching from a registry is not implemented",
        files: &[(
            "app/dowel.toml",
            "[package]\nname    = \"app\"\nversion = \"0.1.0\"\n\n[[dependencies]]\nname = \"libfoo\"\nversion = \"1\"\n",
        )],
        args: CHECK,
    },
    Case {
        code: "undeclared-dependency",
        why: "`dep(\"libfoo\")` is used without declaring it in `dowel.toml`",
        files: &[(
            "app/dowel.build",
            "[bin.app]\nsources = glob(\"src/*.c\")\n\n[bin.app.private]\ndeps = [dep(\"libfoo\")]\n",
        )],
        args: CHECK,
    },
    Case {
        code: "inactive-dependency",
        why: "the reference is not gated on the feature that enables the optional dependency",
        files: &[
            (
                "app/dowel.toml",
                "[package]\nname    = \"app\"\nversion = \"0.1.0\"\n\n[features]\ndefault = []\nlibfoo  = []\n\n[[dependencies]]\nname     = \"libfoo\"\npath     = \"../libfoo\"\noptional = true\n",
            ),
            ("libfoo/dowel.toml", "[package]\nname    = \"libfoo\"\nversion = \"0.1.0\"\n"),
            ("libfoo/dowel.build", "[lib.libfoo]\nsources = glob(\"src/*.c\")\n"),
            ("libfoo/src/foo.c", "int foo(void) { return 1; }\n"),
            (
                "app/dowel.build",
                "[bin.app]\nsources = glob(\"src/*.c\")\n\n[bin.app.private]\ndeps = [dep(\"libfoo\")]\n",
            ),
        ],
        args: CHECK,
    },
    Case {
        code: "unknown-target",
        why: "`target(\"nope\")` names no target in this package",
        files: &[(
            "app/dowel.build",
            "[bin.app]\nsources = glob(\"src/*.c\")\n\n[bin.app.private]\ndeps = [target(\"nope\")]\n",
        )],
        args: CHECK,
    },
    Case {
        code: "invalid-dependency",
        why: "`deps` takes references, not strings",
        files: &[(
            "app/dowel.build",
            "[bin.app]\nsources = glob(\"src/*.c\")\n\n[bin.app.private]\ndeps = [dir(\"src\")]\n",
        )],
        args: CHECK,
    },
    // --- ランナー ---------------------------------------------------------
    Case {
        code: "missing-runner",
        why: "the target triple is not the host and no runner is declared",
        files: &[("app/dowel.build", "[test.t]\nsources = glob(\"src/*.c\")\n")],
        args: &["test", "--target=riscv64gc-unknown-linux-gnu", "--message-format=json"],
    },
    Case {
        code: "missing-field",
        why: "a runner must say what to launch",
        files: &[(
            "app/dowel.build",
            "[bin.app]\nsources = glob(\"src/*.c\")\n\n[runner.riscv64gc-unknown-linux-gnu]\nargs = [\"-L\", \"/sysroot\"]\n",
        )],
        args: CHECK,
    },
    Case {
        code: "incomplete-runner",
        why: "`transfer` needs a destination, which only `remote_dir` provides",
        files: &[(
            "app/dowel.build",
            "[bin.app]\nsources = glob(\"src/*.c\")\n\n[runner.riscv64gc-unknown-linux-gnu]\ncommand = \"ssh\"\ntransfer = [\"scp\"]\n",
        )],
        args: CHECK,
    },
    // --- 併合 -------------------------------------------------------------
    Case {
        code: "merge-conflict",
        why: "`defines` merges with error_on_conflict and two packages set the same key",
        files: TWO_PACKAGES_WITH_CONFLICTING_DEFINES,
        args: CHECK,
    },
    Case {
        code: "abi-mismatch",
        why: "`abi` merges with must_equal and the dependency declares a different one",
        files: TWO_PACKAGES_WITH_CONFLICTING_ABI,
        args: CHECK,
    },
    // --- ビルド計画 -------------------------------------------------------
    Case {
        code: "no-sources",
        why: "the target declares no `sources`",
        files: &[("app/dowel.build", "[bin.app]\n")],
        args: BUILD,
    },
    Case {
        code: "empty-glob",
        why: "the pattern matches no file",
        files: &[("app/dowel.build", "[bin.app]\nsources = glob(\"nowhere/*.c\")\n")],
        args: BUILD,
    },
    Case {
        code: "invalid-source",
        why: "a directory cannot be compiled",
        files: &[("app/dowel.build", "[bin.app]\nsources = [dir(\"src\")]\n")],
        args: BUILD,
    },
    Case {
        code: "unresolved-path",
        why: "the declared source does not exist",
        files: &[("app/dowel.build", "[bin.app]\nsources = [file(\"src/absent.c\")]\n")],
        args: BUILD,
    },
];

/// 事例を組み立てて起動し、出た診断コードを集める。
fn codes_of(case: &Case) -> Vec<String> {
    let p = Project::new(&format!("diag-{}", case.code));
    base(&p);
    if case.code == "missing-manifest" {
        std::fs::remove_file(p.path("app/dowel.toml")).expect("cannot remove the manifest");
    }
    if case.code == "missing-build" {
        std::fs::remove_file(p.path("app/dowel.build")).expect("cannot remove the build file");
    }
    for (rel, text) in case.files {
        p.write(rel, text);
    }
    let r = p.run("app", case.args);
    r.stdout
        .lines()
        .filter_map(|l| l.split("\"code\":\"").nth(1))
        .filter_map(|rest| rest.split('"').next())
        .map(|s| s.to_string())
        .collect()
}

#[test]
fn every_case_produces_the_diagnostic_it_claims() {
    // 事例をひとまとめに走らせ、食い違いを全て並べて報告する。
    // 1件ずつ落ちると、直すたびに次の1件が現れて反復が遅くなる。
    let mut failures = Vec::new();
    for case in CASES {
        let got = codes_of(case);
        if !got.iter().any(|c| c == case.code) {
            failures.push(format!(
                "  {}: expected `{}` ({}), got [{}]",
                case.code,
                case.code,
                case.why,
                got.join(", ")
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} case(s) did not emit their code:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// 事例を組み立てて起動し、実行結果をそのまま返す。
fn run_case(case: &Case, args: &[&str]) -> common::Run {
    let p = Project::new(&format!("diag-{}-{}", case.code, args[0]));
    base(&p);
    for (rel, text) in case.files {
        p.write(rel, text);
    }
    p.run("app", args)
}

#[test]
fn a_diagnostic_that_spans_two_files_names_both_in_the_human_rendering() {
    // 機械可読形式は2つのラベルを持つが、描画が主ラベルのファイルしか
    // 出さないと、利用者は衝突の相手を探すことになる。
    for case in CASES.iter().filter(|c| c.code == "merge-conflict" || c.code == "abi-mismatch") {
        let json = run_case(case, CHECK);
        let line = json
            .stdout
            .lines()
            .find(|l| l.contains(&format!("\"code\":\"{}\"", case.code)))
            .unwrap_or_else(|| panic!("no `{}` in\n{json}", case.code));
        assert!(
            line.matches("\"primary\":").count() >= 2,
            "`{}` should carry two labels: {line}",
            case.code
        );

        let human = run_case(case, &["check"]);
        assert!(human.stderr.contains("lib/dowel.build"), "`{}`\n{human}", case.code);
        assert!(human.stderr.contains("app/dowel.build"), "`{}`\n{human}", case.code);
    }
}

#[test]
fn the_case_table_has_no_duplicates() {
    // 同一コードに対する複数の事例は許容する。発生経路が異なる診断は
    // 経路ごとに検査する必要がある（`missing-field` はパッケージとランナーの
    // 双方から出る）。重複とみなすのは主張が同一の場合に限る。
    let mut seen = BTreeSet::new();
    for case in CASES {
        assert!(
            seen.insert((case.code, case.why)),
            "`{}` has two cases making the same claim: {}",
            case.code,
            case.why
        );
    }
}

/// 事例表に載っているコードの一覧。
fn covered_codes() -> Vec<&'static str> {
    CASES.iter().map(|c| c.code).collect()
}

// ---------------------------------------------------------------------------
// 網羅の追跡。**機能に対しテストが存在するか**を機械的に見る。
//
// テストは足し忘れる。落ちない限り足し忘れは表に出ないため、
// 「足し忘れたら落ちる」機構をここに置く。診断コードを対象にするのは、
// 利用者に見える表面の中で唯一、安定識別子を持ち機械的に数え上げられるためである。
//
// ここが見るのは「事例表にコードが載っているか」だけであり、その事例が
// 意味のある入力かどうかは見ない（それは `why` を人が読んで判断する）。
// 網羅の**下限**を守る仕掛けであって、品質そのものの証明ではない。
// ---------------------------------------------------------------------------

/// まだ CLI 経由の事例を持てない診断と、その理由。
///
/// 空にするのが目標である。増やすときは理由を書く。
/// 理由を書けないものは、事例を書けるということである。
const UNCOVERED: &[(&str, &str)] = &[
    ("toolchain-mismatch", "`[toolchain]` selection is Phase 5; the warning has no real trigger"),
    ("unimplemented-path-base", "`sysroot` paths are Phase 4"),
    ("empty-dependency", "`[[dependencies]]` with no entry at all is rejected by the parser first"),
    ("unreadable-build", "requires a `dowel.build` that exists but cannot be read (permissions)"),
    (
        "dependency-cycle",
        "covered by the model integration tests, which can build the cycle directly",
    ),
];

/// ソースに現れる安定コードを集める。
///
/// 走査でしか集められないのは、コードが登録簿ではなく呼び出し位置に
/// 書かれているためである。登録簿を作れば走査は要らなくなるが、
/// 診断を書くたびに2箇所を触ることになる。
fn declared_codes() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let crates = repo_root().join("crates");
    for entry in walk(&crates) {
        let Ok(text) = std::fs::read_to_string(&entry) else { continue };
        // 単体テスト内の作り物を数えない。`Diagnostic::error("e", ...)` のような
        // 骨組みが本物のコードとして混ざる。
        let text = match text.find("#[cfg(test)]") {
            Some(i) => text[..i].to_string(),
            None => text,
        };
        // `Diagnostic::error("code"` と、改行を挟んだ同じ形。
        for (i, _) in text
            .match_indices("Diagnostic::error(")
            .chain(text.match_indices("Diagnostic::warning("))
        {
            let rest = &text[i..];
            let Some(open) = rest.find('(') else { continue };
            if let Some(code) = first_string_literal(&rest[open + 1..]) {
                out.insert(code);
            }
        }
    }
    out
}

/// 先頭の空白と改行を読み飛ばし、最初の文字列リテラルの中身を返す。
/// 直後がリテラルでなければ（変数を渡している場合）`None`。
fn first_string_literal(s: &str) -> Option<String> {
    let s = s.trim_start();
    let rest = s.strip_prefix('"')?;
    let end = rest.find('"')?;
    let code = &rest[..end];
    // 安定コードは小文字とハイフンだけで書く。
    if !code.is_empty() && code.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
        Some(code.to_string())
    } else {
        None
    }
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            // テスト自身は走査しない。事例表に書いたコードを
            // 「宣言されている」と数えてしまうため。
            if p.file_name().is_some_and(|n| n == "tests" || n == "target") {
                continue;
            }
            out.extend(walk(&p));
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
    out
}

#[test]
fn every_diagnostic_code_has_a_case_or_a_documented_reason() {
    let declared = declared_codes();
    assert!(
        declared.len() > 20,
        "the scan found only {} codes; it is probably broken",
        declared.len()
    );

    let covered: BTreeSet<String> = covered_codes().into_iter().map(|s| s.to_string()).collect();
    let excused: BTreeSet<String> = UNCOVERED.iter().map(|(c, _)| c.to_string()).collect();

    let missing: Vec<&String> =
        declared.iter().filter(|c| !covered.contains(*c) && !excused.contains(*c)).collect();
    assert!(
        missing.is_empty(),
        "these diagnostics have no test case. add one to crates/dowel-cli/tests/diagnostics.rs, \
         or add it to UNCOVERED with a reason:\n  {}",
        missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n  ")
    );
}

#[test]
fn the_uncovered_list_has_no_stale_entries() {
    // 直したのに免除が残っていると、次に壊れても気づけない。
    let declared = declared_codes();
    let covered: BTreeSet<String> = covered_codes().into_iter().map(|s| s.to_string()).collect();
    for (code, _) in UNCOVERED {
        assert!(
            declared.contains(*code),
            "`{code}` is excused but no longer exists; remove it from UNCOVERED"
        );
        assert!(!covered.contains(*code), "`{code}` now has a case; remove it from UNCOVERED");
    }
}

#[test]
fn the_case_table_only_names_codes_that_exist() {
    // 綴りを誤った事例は、何も検査せずに成功する。
    let declared = declared_codes();
    for code in covered_codes() {
        assert!(declared.contains(code), "`{code}` is in the case table but no code emits it");
    }
}

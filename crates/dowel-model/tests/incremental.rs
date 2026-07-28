//! 読み直しの増分性。
//!
//! 結果の正しさだけでは不十分である。増分性の要件は何を再計算しなかったかにあり、
//! これはクエリエンジンの計数でしか観測できない。
//!
//! 数の内訳（`crates/dowel-model/src/query.rs`）:
//!
//! - ファイル1つにつき導出クエリは2つ（`Parsed` と `Evaluated`）
//! - パッケージ1つにつきファイルは2つ（`dowel.toml` と `dowel.build`）

mod common;

use common::Scratch;
use dowel_eval::Config;
use dowel_model::{graph, interface, Session};

/// 2パッケージ（= 4ファイル = 8個の導出クエリ）。
fn workspace() -> Scratch {
    let s = Scratch::new("incremental");
    s.write("libfoo/dowel.toml", "[package]\nname = \"libfoo\"\nversion = \"0.1.0\"\n");
    s.write("libfoo/dowel.build", "[lib.foo]\nsources = glob(\"src/*.c\")\n");
    s.write("libfoo/src/foo.c", "int foo(void) { return 1; }\n");
    s.write(
        "app/dowel.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [[dependencies]]\nname = \"libfoo\"\npath = \"../libfoo\"\n",
    );
    s.write("app/dowel.build", "[bin.app]\nsources = glob(\"src/*.c\")\n");
    s.write("app/src/main.c", "int main(void) { return 0; }\n");
    s
}

/// 併合に入力を持つワークスペース。
///
/// [`workspace`] の `libfoo` は `public` も `private` も空である。併合の入力が
/// 空のままでは、要約が変わらないことを検査しても何も確かめたことにならない。
fn workspace_with_propagation() -> Scratch {
    let s = workspace();
    s.write(
        "libfoo/dowel.build",
        "[lib.foo]\nsources = glob(\"src/*.c\")\n\n\
         [lib.foo.public]\nincludes = [dir(\"include\")]\ndefines = { FOO = 1 }\n",
    );
    s.write(
        "app/dowel.build",
        "[bin.app]\nsources = glob(\"src/*.c\")\n\n\
         [bin.app.private]\ndeps = [dep(\"libfoo\")]\nflags = [\"-Wall\"]\n",
    );
    s
}

/// 読み込みの後に走る段。依存を解決し、ターゲット単位の派生まで問い合わせる。
///
/// `Session::load` はファイル単位のクエリしか触らない。併合は構成が決まって
/// から行うため、`graph::build` を挟んでここで問い合わせる。
fn derive(sess: &Session) {
    let cfg = Config::host_default();
    let (g, _) = graph::build(sess, &cfg);
    interface::prepare(sess, &g, &cfg);
    for t in &sess.targets {
        sess.compile_env_of(t.id);
    }
}

#[test]
fn a_comment_only_edit_does_not_reach_the_merge() {
    // early cutoff（docs/20-architecture.md 3節）が要求するのはこの性質である。
    // 評価結果はスパンがずれるため必ず変わるが、併合の入力は変わらない。
    let s = workspace_with_propagation();
    let mut sess = Session::load(&s.path("app"));
    derive(&sess);

    // 先頭にコメントを1行足す。宣言の中身は同じで、スパンは全て動く。
    s.write(
        "libfoo/dowel.build",
        "# what this library provides\n[lib.foo]\nsources = glob(\"src/*.c\")\n\n\
         [lib.foo.public]\nincludes = [dir(\"include\")]\ndefines = { FOO = 1 }\n",
    );
    sess.reload();
    derive(&sess);

    let stats = sess.query_stats();
    // 触ったファイルの `Parsed` と `Evaluated` だけ。派生の併合は走っていない。
    assert_eq!(stats.computed, 2, "the merge ran again: {stats:?}");
    assert_eq!(stats.cut_off, 0, "{stats:?}");
}

#[test]
fn changing_a_declared_value_reaches_the_merge() {
    // 対になる検査。要約が変われば併合は走り直す。
    // これが無いと、上の検査は「そもそも併合を問い合わせていない」でも通る。
    let s = workspace_with_propagation();
    let mut sess = Session::load(&s.path("app"));
    derive(&sess);

    s.write(
        "libfoo/dowel.build",
        "[lib.foo]\nsources = glob(\"src/*.c\")\n\n\
         [lib.foo.public]\nincludes = [dir(\"include\")]\ndefines = { FOO = 2 }\n",
    );
    sess.reload();
    derive(&sess);

    let stats = sess.query_stats();
    // `Parsed` と `Evaluated` に加え、libfoo の `interface` と `compile_env`、
    // それを取り込む app の `compile_env`。
    assert!(stats.computed >= 5, "the merge did not run: {stats:?}");
}

#[test]
fn the_first_load_computes_every_query() {
    let s = workspace();
    let sess = Session::load(&s.path("app"));
    assert!(!sess.has_errors(), "{:?}", sess.diagnostics);

    let stats = sess.query_stats();
    // 4ファイル × 2クエリ。
    assert_eq!(stats.computed, 8, "{stats:?}");
    assert_eq!(stats.hit, 0, "{stats:?}");
}

#[test]
fn reloading_an_untouched_workspace_parses_nothing_again() {
    let s = workspace();
    let mut sess = Session::load(&s.path("app"));
    sess.reload();

    // `computed` と `cut_off` はどちらも「計算手続きを走らせた」件数である。
    // 双方が 0 であることが、字句解析も構文解析も評価も一度も走らなかったこと
    // そのものを意味する。
    let stats = sess.query_stats();
    assert_eq!(stats.computed, 0, "something was recomputed: {stats:?}");
    assert_eq!(stats.cut_off, 0, "{stats:?}");

    // 空回りしていないこと。ファイル4つ分の評価クエリは問い合わせている。
    // `hit`（同じ版で2度目）と `verified`（依存を辿って変化なしと確認）の
    // 内訳は、読み込みが「入力を置く」と「問い合わせる」を交互に行うことに依る。
    // 最後に読んだファイル以外は版が進んだ後に問い合わされるため `verified` 側に落ちる。
    assert!(stats.hit + stats.verified >= 4, "nothing was queried at all: {stats:?}");

    // 再利用しても組み上がるモデルは同じ。
    assert_eq!(sess.packages.len(), 2);
    assert_eq!(sess.targets.len(), 2);
}

#[test]
fn rewriting_a_file_with_the_same_bytes_invalidates_nothing() {
    let s = workspace();
    let mut sess = Session::load(&s.path("app"));

    // 内容は同じで書き直す。更新時刻は変わるが、判定は内容で行う。
    let text = std::fs::read_to_string(s.path("libfoo/dowel.build")).unwrap();
    s.write("libfoo/dowel.build", &text);

    sess.reload();
    assert_eq!(sess.query_stats().computed, 0, "{:?}", sess.query_stats());
}

#[test]
fn editing_one_file_recomputes_only_that_file() {
    let s = workspace();
    let mut sess = Session::load(&s.path("app"));

    s.write(
        "libfoo/dowel.build",
        "[lib.foo]\nsources = glob(\"src/*.c\")\n\n[lib.foo.private]\nflags = [\"-O2\"]\n",
    );
    sess.reload();

    let stats = sess.query_stats();
    // 触ったファイルの `Parsed` と `Evaluated` だけ。
    assert_eq!(stats.computed, 2, "{stats:?}");
    // 残り3ファイルは依存を辿って「変わっていない」と確認しただけ。
    assert_eq!(stats.verified, 6, "{stats:?}");
    assert!(!sess.has_errors(), "{:?}", sess.diagnostics);
}

#[test]
fn an_edit_is_actually_reflected_in_the_model() {
    let s = workspace();
    let mut sess = Session::load(&s.path("app"));
    assert_eq!(sess.targets.len(), 2);

    s.write(
        "libfoo/dowel.build",
        "[lib.foo]\nsources = glob(\"src/*.c\")\n\n[lib.bar]\nsources = glob(\"src/*.c\")\n",
    );
    sess.reload();

    assert_eq!(sess.targets.len(), 3);
    assert!(sess.find_target("libfoo:bar").is_ok(), "the new target is missing");
}

#[test]
fn diagnostics_survive_a_memo_hit() {
    // 診断を値の一部にしていないと、2回目の読み込みで消える。
    let s = workspace();
    s.write("libfoo/dowel.build", "[lib.foo]\nsources = glob(\"src/*.c\")\nnosuchprop = 1\n");

    let mut sess = Session::load(&s.path("app"));
    let first = sess.diagnostics.len();
    assert!(sess.has_errors());

    sess.reload();
    assert_eq!(sess.query_stats().computed, 0, "{:?}", sess.query_stats());
    assert_eq!(sess.diagnostics.len(), first, "diagnostics were lost on reuse");
    assert!(sess.has_errors(), "the error disappeared after reloading");
}

#[test]
fn a_syntax_error_is_reported_again_after_an_untouched_reload() {
    let s = workspace();
    // 閉じていない文字列。構文解析の段で診断が出る。
    s.write("libfoo/dowel.build", "[lib.foo]\nsources = glob(\"src/*.c\n");

    let mut sess = Session::load(&s.path("app"));
    let first: Vec<String> = sess.diagnostics.iter().map(|d| d.code.to_string()).collect();
    assert!(!first.is_empty());

    sess.reload();
    let again: Vec<String> = sess.diagnostics.iter().map(|d| d.code.to_string()).collect();
    assert_eq!(again, first);
}

#[test]
fn fixing_a_file_clears_its_diagnostics() {
    let s = workspace();
    s.write("libfoo/dowel.build", "[lib.foo]\nsources = glob(\"src/*.c\")\nnosuchprop = 1\n");
    let mut sess = Session::load(&s.path("app"));
    assert!(sess.has_errors());

    s.write("libfoo/dowel.build", "[lib.foo]\nsources = glob(\"src/*.c\")\n");
    sess.reload();
    assert!(!sess.has_errors(), "{:?}", sess.diagnostics);
}

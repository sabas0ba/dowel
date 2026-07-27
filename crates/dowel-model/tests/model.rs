//! パッケージ読み込みからインタフェース併合までの検証。

mod common;

use common::Scratch;
use dowel_eval::Config;
use dowel_model::{graph, interface, Session};

/// 依存するライブラリと、それを使う実行ファイルの2パッケージ構成。
fn two_packages() -> Scratch {
    let s = Scratch::new("two-packages");
    s.write(
        "libfoo/dowel.toml",
        r#"
[package]
name    = "libfoo"
version = "0.1.0"
"#,
    );
    s.write(
        "libfoo/dowel.build",
        r#"
[lib.foo]
sources = glob("src/*.c")

[lib.foo.public]
includes = [dir("include")]
defines  = { FOO_API = 1 }

[lib.foo.private]
includes = [dir("src")]
flags    = ["-Wall"]
"#,
    );
    s.write("libfoo/include/foo.h", "int foo(void);\n");
    s.write("libfoo/src/foo.c", "int foo(void) { return 1; }\n");

    s.write(
        "app/dowel.toml",
        r#"
[package]
name    = "app"
version = "0.1.0"

[[dependencies]]
name = "libfoo"
path = "../libfoo"
"#,
    );
    s.write(
        "app/dowel.build",
        r#"
[bin.app]
sources = glob("src/*.c")

[bin.app.private]
deps  = [dep("libfoo")]
flags = ["-O0"]
"#,
    );
    s.write("app/src/main.c", "int main(void) { return 0; }\n");
    s
}

fn load(s: &Scratch, rel: &str) -> Session {
    let sess = Session::load(&s.path(rel));
    assert!(
        !sess.has_errors(),
        "診断: {:#?}",
        sess.diagnostics.iter().map(|d| (d.code, d.message.clone())).collect::<Vec<_>>()
    );
    sess
}

fn codes(sess: &Session) -> Vec<&str> {
    sess.diagnostics.iter().map(|d| d.code).collect()
}

#[test]
fn パス依存を辿ってパッケージを読み込む() {
    let s = two_packages();
    let sess = load(&s, "app");
    assert_eq!(sess.packages.len(), 2);
    assert_eq!(sess.targets.len(), 2);
    assert!(sess.find_target("app:app").is_ok());
    assert!(sess.find_target("libfoo:foo").is_ok());
    // 名前だけでも一意なら引ける。
    assert!(sess.find_target("foo").is_ok());
}

#[test]
fn 公開プロパティは依存元へ伝播し非公開は伝播しない() {
    let s = two_packages();
    let sess = load(&s, "app");
    let cfg = Config::host_default();
    let (g, gd) = graph::build(&sess, &cfg);
    assert!(gd.is_empty(), "{gd:#?}");
    let (ifaces, id) = interface::compute(&sess, &g, &cfg);
    assert!(id.is_empty(), "{id:#?}");

    let app = sess.find_target("app:app").unwrap();
    let mut diags = Vec::new();
    let env = interface::compile_env(&sess, &g, &ifaces, app, &cfg, &mut diags);
    assert!(diags.is_empty(), "{diags:#?}");

    let includes = env.get("includes").expect("includes が伝播していない");
    let shown: Vec<String> = includes.as_list().unwrap().iter().map(|v| v.display()).collect();
    assert!(shown.contains(&"include".to_string()), "public.includes が伝播していない: {shown:?}");
    assert!(!shown.contains(&"src".to_string()), "private.includes が伝播してはならない");

    // public.defines も伝播する。
    let defines = env.get("defines").expect("defines が伝播していない");
    assert!(defines.as_map().unwrap().contains_key("FOO_API"));

    // libfoo の private.flags は app に効かない。
    let flags = env.get("flags").unwrap();
    let shown: Vec<String> = flags.as_list().unwrap().iter().map(|v| v.display()).collect();
    assert_eq!(shown, vec!["\"-O0\""], "private.flags が伝播してはならない");
}

#[test]
fn 伝播した値の来歴が辿れる() {
    let s = two_packages();
    let sess = load(&s, "app");
    let cfg = Config::host_default();
    let (g, _) = graph::build(&sess, &cfg);
    let (ifaces, _) = interface::compute(&sess, &g, &cfg);
    let app = sess.find_target("app:app").unwrap();

    let e = dowel_model::why::explain(&sess, &g, &ifaces, app, "includes", &cfg).unwrap();
    let text = dowel_model::why::render_text(&e);
    assert!(text.contains("include"), "{text}");
    assert!(text.contains("dir(...)"), "{text}");
    assert!(text.contains("includes of libfoo:foo"), "{text}");
    // 位置が付いていること。
    assert!(text.contains("dowel.build:"), "{text}");
}

#[test]
fn 未宣言の依存を診断し候補を出す() {
    let s = Scratch::new("undeclared");
    s.write("dowel.toml", "[package]\nname = \"p\"\nversion = \"0\"\n");
    s.write(
        "dowel.build",
        "[bin.a]\nsources = glob(\"*.c\")\n\n[bin.a.private]\ndeps = [dep(\"libfoo\")]\n",
    );
    let sess = Session::load(&s.root);
    let cfg = Config::host_default();
    let (_, gd) = graph::build(&sess, &cfg);
    assert_eq!(gd.iter().map(|d| d.code).collect::<Vec<_>>(), vec!["undeclared-dependency"]);
}

#[test]
fn 未知のプロパティに候補を出す() {
    let s = Scratch::new("unknown-prop");
    s.write("dowel.toml", "[package]\nname = \"p\"\nversion = \"0\"\n");
    s.write("dowel.build", "[lib.a]\nsources = []\n\n[lib.a.public]\ninclude = [dir(\"x\")]\n");
    let sess = Session::load(&s.root);
    assert_eq!(codes(&sess), vec!["unknown-property"]);
    assert_eq!(sess.diagnostics[0].suggestions[0].replacement, "includes");
}

#[test]
fn パスを文字列で書くと型で落ちて助言が出る() {
    let s = Scratch::new("path-type");
    s.write("dowel.toml", "[package]\nname = \"p\"\nversion = \"0\"\n");
    s.write("dowel.build", "[lib.a]\nsources = []\n\n[lib.a.public]\nincludes = [\"include\"]\n");
    let sess = Session::load(&s.root);
    assert_eq!(codes(&sess), vec!["type-mismatch"]);
    assert!(
        sess.diagnostics[0].notes.iter().any(|n| n.contains("dir(")),
        "{:?}",
        sess.diagnostics[0].notes
    );
}

#[test]
fn 依存の閉路を検出する() {
    let s = Scratch::new("cycle");
    s.write("dowel.toml", "[package]\nname = \"p\"\nversion = \"0\"\n");
    s.write(
        "dowel.build",
        r#"
[lib.a]
sources = []
[lib.a.private]
deps = [target("b")]

[lib.b]
sources = []
[lib.b.private]
deps = [target("a")]
"#,
    );
    let sess = Session::load(&s.root);
    let cfg = Config::host_default();
    let (_, gd) = graph::build(&sess, &cfg);
    assert!(gd.iter().any(|d| d.code == "dependency-cycle"), "{gd:#?}");
}

#[test]
fn abi_ラベルの不一致を失敗させる() {
    let s = Scratch::new("abi");
    s.write("dowel.toml", "[package]\nname = \"p\"\nversion = \"0\"\n");
    s.write(
        "dowel.build",
        r#"
[lib.a]
sources = []
[lib.a.public]
abi = "gnu11-cxx11abi1"

[lib.b]
sources = []
[lib.b.public]
abi = "gnu11-cxx11abi0"

[bin.app]
sources = []
[bin.app.private]
deps = [target("a"), target("b")]
"#,
    );
    let sess = Session::load(&s.root);
    let cfg = Config::host_default();
    let (g, _) = graph::build(&sess, &cfg);
    let (ifaces, _) = interface::compute(&sess, &g, &cfg);
    let app = sess.find_target("app").unwrap();
    let mut diags = Vec::new();
    interface::compile_env(&sess, &g, &ifaces, app, &cfg, &mut diags);
    assert!(diags.iter().any(|d| d.code == "abi-mismatch"), "{diags:#?}");
}

#[test]
fn 機能フラグで依存の辺が現れる() {
    let s = Scratch::new("feature-dep");
    s.write("libz/dowel.toml", "[package]\nname = \"libz\"\nversion = \"0\"\n");
    s.write("libz/dowel.build", "[lib.z]\nsources = []\n");
    s.write(
        "dowel.toml",
        r#"
[package]
name    = "p"
version = "0"

[[dependencies]]
name     = "libz"
path     = "libz"
optional = true

[features]
zlib = ["libz"]
"#,
    );
    s.write(
        "dowel.build",
        "[bin.a]\nsources = []\n\n[bin.a.private]\ndeps = [dep(\"libz\") when feature.zlib]\n",
    );
    let sess = Session::load(&s.root);
    assert!(!sess.has_errors(), "{:?}", codes(&sess));

    let mut cfg = Config::host_default();
    let a = sess.find_target("a").unwrap();

    let (g, _) = graph::build(&sess, &cfg);
    assert_eq!(g.deps_of(a).len(), 0, "機能が無効なら辺は現れない");

    cfg.features.insert("zlib".into());
    let (g, _) = graph::build(&sess, &cfg);
    assert_eq!(g.deps_of(a).len(), 1, "機能が有効なら辺が現れる");
}

#[test]
fn 取得を要する依存は未実装として診断する() {
    let s = Scratch::new("registry-dep");
    s.write(
        "dowel.toml",
        "[package]\nname = \"p\"\nversion = \"0\"\n\n[[dependencies]]\nname = \"zlib\"\nversion = \"1.3\"\n",
    );
    s.write("dowel.build", "[bin.a]\nsources = []\n");
    let sess = Session::load(&s.root);
    assert!(codes(&sess).contains(&"unsupported-dependency"), "{:?}", codes(&sess));
}

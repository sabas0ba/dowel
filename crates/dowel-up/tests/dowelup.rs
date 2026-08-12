//! dowelup の e2e。上流のフィクスチャ（ローカルの git リポジトリ）に対して
//! 解決・取得・固定・切り替えの全経路を通す。
//!
//! フィクスチャの各コミットは異なる文字列を印字する小さな cargo プロジェクト
//! であり、どの版が起動したかを stdout で判別できる。ネットワークには
//! 触れない。上流はパスで渡し、依存を持たないためビルドもオフラインで済む。

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("dowelup-{name}"));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("cannot create the scratch directory");
    root
}

struct Run {
    status: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Run {
    fn from(out: Output) -> Run {
        Run {
            status: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn ok(self) -> Run {
        assert_eq!(
            self.status,
            Some(0),
            "expected success\nstdout:\n{}\nstderr:\n{}",
            self.stdout,
            self.stderr
        );
        self
    }

    fn err(self) -> Run {
        assert_ne!(self.status, Some(0), "expected failure\nstdout:\n{}", self.stdout);
        self
    }
}

fn dowelup(home: &Path, cwd: &Path, args: &[&str]) -> Run {
    let out = Command::new(env!("CARGO_BIN_EXE_dowelup"))
        .args(args)
        .current_dir(cwd)
        .env("DOWELUP_HOME", home)
        .env_remove("DOWELUP_UPSTREAM")
        .output()
        .expect("cannot start dowelup");
    Run::from(out)
}

/// shim（`dowel` の名を持つリンク）越しに起動する。
fn dowel(shim: &Path, home: &Path, cwd: &Path, args: &[&str]) -> Run {
    let out = Command::new(shim)
        .args(args)
        .current_dir(cwd)
        .env("DOWELUP_HOME", home)
        .output()
        .expect("cannot start the shim");
    Run::from(out)
}

fn git(dir: &Path, args: &[&str], date: Option<&str>) {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        // 実行環境の git 設定に依存しない。署名や hook の設定が混ざると
        // フィクスチャの作成自体が環境依存になる。
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid");
    if let Some(d) = date {
        cmd.env("GIT_AUTHOR_DATE", d).env("GIT_COMMITTER_DATE", d);
    }
    let out = cmd.output().expect("cannot start git");
    assert!(out.status.success(), "git {args:?} failed:\n{}", String::from_utf8_lossy(&out.stderr));
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().expect("cannot start git");
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

struct Upstream {
    url: String,
    c1: String,
    c2: String,
    c3: String,
}

/// 印字する文字列だけが異なる3つのコミットを持つ上流を作る。
///
/// - `c1` — "one"。タグ `v0.1.0`。committer date は 2026-01-01
/// - `c2` — "two"。`main` の先端。2026-02-01
/// - `c3` — "three"。ブランチ `feature` の先端。2026-03-01
fn upstream(root: &Path) -> Upstream {
    let dir = root.join("upstream");
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "--quiet", "-b", "main"], None);
    // 空の [workspace] で自立させる。フィクスチャの checkout は本リポジトリの
    // target/ の下に置かれるため、これが無いと cargo が外側のワークスペースを
    // 見つけてしまう。
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"dowel\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let main_rs = |word: &str| format!("fn main() {{ println!(\"{word}\"); }}\n");
    std::fs::write(dir.join("src/main.rs"), main_rs("one")).unwrap();
    // ロックを持たせ、--locked の経路を通す。
    let lock = Command::new("cargo")
        .args(["generate-lockfile", "--offline"])
        .current_dir(&dir)
        .output()
        .expect("cannot start cargo");
    assert!(
        lock.status.success(),
        "cargo generate-lockfile failed:\n{}",
        String::from_utf8_lossy(&lock.stderr)
    );
    git(&dir, &["add", "."], None);
    git(&dir, &["commit", "--quiet", "-m", "one"], Some("2026-01-01T12:00:00Z"));
    git(&dir, &["tag", "v0.1.0"], None);
    let c1 = git_out(&dir, &["rev-parse", "HEAD"]);
    std::fs::write(dir.join("src/main.rs"), main_rs("two")).unwrap();
    git(&dir, &["commit", "--quiet", "-am", "two"], Some("2026-02-01T12:00:00Z"));
    let c2 = git_out(&dir, &["rev-parse", "HEAD"]);
    git(&dir, &["checkout", "--quiet", "-b", "feature"], None);
    std::fs::write(dir.join("src/main.rs"), main_rs("three")).unwrap();
    git(&dir, &["commit", "--quiet", "-am", "three"], Some("2026-03-01T12:00:00Z"));
    let c3 = git_out(&dir, &["rev-parse", "HEAD"]);
    git(&dir, &["checkout", "--quiet", "main"], None);
    Upstream { url: dir.to_string_lossy().into_owned(), c1, c2, c3 }
}

#[test]
fn pins_a_release_and_dispatches_through_the_shim() {
    let root = scratch("pin");
    let up = upstream(&root);
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();

    // タグからの解決。pin は解決済みの sha を書く。
    let r = dowelup(&home, &project, &["--upstream", &up.url, "pin", "0.1.0"]).ok();
    assert_eq!(r.stdout.trim(), up.c1);
    let pin = std::fs::read_to_string(project.join(".dowel-version")).unwrap();
    assert!(pin.contains(&up.c1), "the pin file does not contain the hash:\n{pin}");

    // stable は同じコミットに解決され、取得は再利用される。
    let r = dowelup(&home, &project, &["--upstream", &up.url, "install", "stable"]).ok();
    assert_eq!(r.stdout.trim(), up.c1);
    assert!(r.stderr.contains("already installed"), "stderr:\n{}", r.stderr);

    // shim 越しに pin の版が動く。
    let bindir = root.join("bin");
    let r = dowelup(&home, &project, &["shim", bindir.to_str().unwrap()]).ok();
    let shim = PathBuf::from(r.stdout.trim());
    assert_eq!(shim, bindir.join("dowel"));
    let r = dowel(&shim, &home, &project, &[]).ok();
    assert_eq!(r.stdout, "one\n");

    // which は pin の版を指す。
    let r = dowelup(&home, &project, &["which"]).ok();
    assert!(r.stdout.contains(&up.c1), "stdout:\n{}", r.stdout);

    // 手書きの名前は解決しない。固定は sha のみ（ADR-0012）。
    std::fs::write(project.join(".dowel-version"), "nightly\n").unwrap();
    let r = dowel(&shim, &home, &project, &[]).err();
    assert!(r.stderr.contains("dowelup pin nightly"), "stderr:\n{}", r.stderr);
}

#[test]
fn every_specifier_used_to_install_can_select_that_version() {
    // issue #39。`stable` とそれが指すタグが同じコミットに解決されるのは
    // 通常の状態であり、どの指定子で入れても、その指定子で選べること。
    let root = scratch("respec");
    let up = upstream(&root);
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();

    // 3つの指定子が同じコミットに解決される。2つ目以降は実体を再利用する。
    for spec in ["0.1.0", "stable", "tag:v0.1.0"] {
        let r = dowelup(&home, &project, &["--upstream", &up.url, "install", spec]).ok();
        assert_eq!(r.stdout.trim(), up.c1, "`{spec}` resolved to a different commit");
    }

    // どの指定子でも選べる。`run` は `+<指定子>` と同じ照合を使う。
    for spec in ["0.1.0", "stable", "tag:v0.1.0"] {
        let r = dowelup(&home, &project, &["run", spec]).ok();
        assert_eq!(r.stdout, "one\n", "`{spec}` cannot select the installed version");
    }

    // 一覧は全ての指定子を持つ。
    let r = dowelup(&home, &project, &["list"]).ok();
    let line = r.stdout.lines().find(|l| l.contains(&up.c1)).expect("c1 is not listed");
    for spec in ["0.1.0", "stable", "tag:v0.1.0"] {
        assert!(line.contains(spec), "`{spec}` is missing from the list line: {line}");
    }
}

#[test]
fn resolves_moving_references_and_switches_between_them() {
    let root = scratch("switch");
    let up = upstream(&root);
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();

    // 既定ブランチの先端、日付、ブランチのそれぞれが別のコミットに解決される。
    let r = dowelup(&home, &project, &["--upstream", &up.url, "install", "nightly"]).ok();
    assert_eq!(r.stdout.trim(), up.c2);
    let r =
        dowelup(&home, &project, &["--upstream", &up.url, "install", "nightly-2026-01-15"]).ok();
    assert_eq!(r.stdout.trim(), up.c1);
    let r = dowelup(&home, &project, &["--upstream", &up.url, "install", "branch:feature"]).ok();
    assert_eq!(r.stdout.trim(), up.c3);

    // 既定を設定すると一覧に印が付く。
    let r = dowelup(&home, &project, &["--upstream", &up.url, "default", "nightly"]).ok();
    assert_eq!(r.stdout.trim(), up.c2);
    let r = dowelup(&home, &project, &["list"]).ok();
    assert!(r.stdout.contains(&format!("* {}", up.c2)), "stdout:\n{}", r.stdout);
    assert!(r.stdout.contains(&up.c1) && r.stdout.contains(&up.c3), "stdout:\n{}", r.stdout);

    // pin の無い場所では既定が動く。
    let bindir = root.join("bin");
    let r = dowelup(&home, &project, &["shim", bindir.to_str().unwrap()]).ok();
    let shim = PathBuf::from(r.stdout.trim());
    let r = dowel(&shim, &home, &project, &[]).ok();
    assert_eq!(r.stdout, "two\n");

    // 先頭の +指定子は選択より優先され、sha の接頭辞でも選べる。
    let r = dowel(&shim, &home, &project, &["+branch:feature"]).ok();
    assert_eq!(r.stdout, "three\n");
    let plus = format!("+{}", &up.c1[..10]);
    let r = dowel(&shim, &home, &project, &[plus.as_str()]).ok();
    assert_eq!(r.stdout, "one\n");

    // run は選択を経ずに起動する。
    let r = dowelup(&home, &project, &["run", "branch:feature"]).ok();
    assert_eq!(r.stdout, "three\n");

    // uninstall で消え、以後は選べない。
    dowelup(&home, &project, &["uninstall", "branch:feature"]).ok();
    let r = dowelup(&home, &project, &["list"]).ok();
    assert!(!r.stdout.contains(&up.c3), "stdout:\n{}", r.stdout);
    let r = dowel(&shim, &home, &project, &["+branch:feature"]).err();
    assert!(r.stderr.contains("no installed version"), "stderr:\n{}", r.stderr);
}

/// 上流のディレクトリに、release タグ向けの資産を置く（ADR-0036）。
///
/// dowelup が探す場所は `<upstream>/releases/download/<tag>/` である。
/// 上流をローカルの木にしてあるので、その下に同じ形で置けば取得の経路を
/// そのまま通せる——URL の組み立てと資産の命名が食い違えば、ここで落ちる。
fn publish_asset(upstream_dir: &Path, tag: &str, word: &str) -> String {
    let triple = {
        let arch = std::env::consts::ARCH;
        let os = match std::env::consts::OS {
            "linux" => "unknown-linux-gnu",
            "macos" => "apple-darwin",
            other => other,
        };
        format!("{arch}-{os}")
    };
    let dir = upstream_dir.join("releases/download").join(tag);
    std::fs::create_dir_all(&dir).unwrap();

    // 中身は「印字するだけ」の実行ファイル。ソースから組んだものと
    // 区別が付くよう、別の語を印字させる。
    let stage = upstream_dir.parent().unwrap().join("stage");
    std::fs::create_dir_all(&stage).unwrap();
    let script = stage.join("dowel");
    std::fs::write(&script, format!("#!/bin/sh\necho {word}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let asset = dir.join(format!("dowel-{tag}-{triple}.tar.gz"));
    let tar = Command::new("tar")
        .arg("-czf")
        .arg(&asset)
        .arg("-C")
        .arg(&stage)
        .arg("dowel")
        .output()
        .expect("cannot start tar");
    assert!(tar.status.success(), "tar failed: {}", String::from_utf8_lossy(&tar.stderr));

    let sum = Command::new("sha256sum").arg(&asset).output().expect("cannot start sha256sum");
    assert!(sum.status.success());
    let hex = String::from_utf8_lossy(&sum.stdout).split_whitespace().next().unwrap().to_string();
    std::fs::write(format!("{}.sha256", asset.display()), format!("{hex}  {}\n", asset.display()))
        .unwrap();
    hex
}

#[test]
fn a_release_specifier_takes_the_published_asset() {
    // 事前ビルドが在れば Rust ツールチェーンは要らない（ADR-0036）。
    // 資産が印字する語で、ソースから組んだのではないことを見る。
    let root = scratch("prebuilt");
    let up = upstream(&root);
    publish_asset(Path::new(&up.url), "v0.1.0", "prebuilt");
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();

    let r = dowelup(&home, &project, &["--upstream", &up.url, "install", "0.1.0"]).ok();
    assert_eq!(r.stdout.trim(), up.c1);
    assert!(r.stderr.contains("from a release asset"), "stderr:\n{}", r.stderr);
    assert!(r.stderr.contains("verified by sha256"), "stderr:\n{}", r.stderr);

    // 入ったのは資産の中身である。ソースの版は "one" を印字する。
    let r = dowelup(&home, &project, &["run", &up.c1]).ok();
    assert_eq!(r.stdout.trim(), "prebuilt");
}

#[test]
fn from_source_ignores_the_published_asset() {
    // 事前ビルドが在っても、求められればソースから組む。信頼の根が違う。
    let root = scratch("prebuilt-from-source");
    let up = upstream(&root);
    publish_asset(Path::new(&up.url), "v0.1.0", "prebuilt");
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();

    let r = dowelup(&home, &project, &["--upstream", &up.url, "--from-source", "install", "0.1.0"])
        .ok();
    assert!(r.stderr.contains("built from source"), "stderr:\n{}", r.stderr);
    let r = dowelup(&home, &project, &["run", &up.c1]).ok();
    assert_eq!(r.stdout.trim(), "one");
}

#[test]
fn a_corrupted_asset_is_refused_and_the_source_build_takes_over() {
    // ハッシュが合わなければ使わない。それでも入るのは、落ちるのではなく
    // ソースへ落ちるためである——資産が壊れていることは、その版が組めない
    // ことを意味しない。
    let root = scratch("prebuilt-corrupt");
    let up = upstream(&root);
    publish_asset(Path::new(&up.url), "v0.1.0", "prebuilt");
    // 書庫だけを差し替える。隣の `.sha256` は元のままなので合わなくなる。
    let triple = format!(
        "{}-{}",
        std::env::consts::ARCH,
        match std::env::consts::OS {
            "linux" => "unknown-linux-gnu",
            "macos" => "apple-darwin",
            other => other,
        }
    );
    let asset = Path::new(&up.url)
        .join("releases/download/v0.1.0")
        .join(format!("dowel-v0.1.0-{triple}.tar.gz"));
    std::fs::write(&asset, b"not the archive you published").unwrap();

    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let r = dowelup(&home, &project, &["--upstream", &up.url, "install", "0.1.0"]).ok();
    assert!(r.stderr.contains("does not match its checksum"), "stderr:\n{}", r.stderr);
    assert!(r.stderr.contains("building from source"), "stderr:\n{}", r.stderr);
    let r = dowelup(&home, &project, &["run", &up.c1]).ok();
    assert_eq!(r.stdout.trim(), "one");
}

#[test]
fn a_specifier_without_a_release_builds_from_source() {
    // `nightly` はタグを経由しない。事前ビルドを探しに行かない。
    let root = scratch("prebuilt-nightly");
    let up = upstream(&root);
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();

    let r = dowelup(&home, &project, &["--upstream", &up.url, "install", "nightly"]).ok();
    assert!(r.stderr.contains("does not name a release"), "stderr:\n{}", r.stderr);
    assert!(r.stderr.contains("built from source"), "stderr:\n{}", r.stderr);
}

#[test]
fn the_record_says_which_way_each_version_arrived() {
    // ADR-0036 は2つの経路の違いを信用の根に置いている。どちらであるかが
    // 残らなければ、目の前のバイナリについて「そのコミットから組まれた」と
    // 言ってよいのか「公開者を信用している」だけなのかが決まらない
    // （issue #146）。
    //
    // 退避があるため、これは意図せず起きる。資産が無ければ黙って組む側へ
    // 回るので、「取ったつもりが組んでいた」は普通に起きる。
    let root = scratch("arrival");
    let up = upstream(&root);
    publish_asset(Path::new(&up.url), "v0.1.0", "prebuilt");
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();

    dowelup(&home, &project, &["--upstream", &up.url, "install", "0.1.0"]).ok();
    let origin = std::fs::read_to_string(home.join("versions").join(&up.c1).join("origin"))
        .expect("no origin record");
    assert!(origin.contains("from=asset"), "{origin}");
    // 検めた digest も残す。後から突き合わせられる。
    assert!(origin.lines().any(|l| l.starts_with("asset_sha256=")), "{origin}");

    // 「何が入っているか」を尋ねる道具に、「どういう資格で入っているか」が出る。
    let r = dowelup(&home, &project, &["list"]).ok();
    assert!(r.stdout.contains("[asset]"), "stdout:\n{}", r.stdout);

    // 同じ sha を別の指定子で引き当てても、実体は入れ替わらない。経路の
    // 記録も入れ替わってはならない。
    dowelup(&home, &project, &["--upstream", &up.url, "install", "stable"]).ok();
    let origin =
        std::fs::read_to_string(home.join("versions").join(&up.c1).join("origin")).unwrap();
    assert!(origin.contains("from=asset"), "{origin}");
    assert_eq!(origin.matches("from=").count(), 1, "the path is not accumulated:\n{origin}");
}

#[test]
fn a_version_built_from_source_is_recorded_as_such() {
    // 対になる検査。これが無いと、上は「常に asset と書く」でも通る。
    let root = scratch("arrival-source");
    let up = upstream(&root);
    publish_asset(Path::new(&up.url), "v0.1.0", "prebuilt");
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();

    dowelup(&home, &project, &["--upstream", &up.url, "--from-source", "install", "0.1.0"]).ok();
    let origin =
        std::fs::read_to_string(home.join("versions").join(&up.c1).join("origin")).unwrap();
    assert!(origin.contains("from=source"), "{origin}");
    // 資産の digest は、資産から来たときにだけ在る。
    assert!(!origin.contains("asset_sha256="), "{origin}");

    let r = dowelup(&home, &project, &["list"]).ok();
    assert!(r.stdout.contains("[source]"), "stdout:\n{}", r.stdout);
}

#[test]
fn a_failed_fetch_says_why_and_names_the_tool_that_ran() {
    // 取得の失敗はこの機能で最も原因が広く、しかも利用者の機械の側にある
    // 種類の失敗である——proxy、TLS、DNS、404、社内の遮断。理由の1行が
    // あれば当たりが付き、無ければ何も分からない（issue #145）。
    //
    // しかも失敗は静かに退避する。cargo の無い機械では、最後に残る言葉が
    // 「cargo が無い」になり、実際の問題を指さない。
    let root = scratch("fetch-why");
    let up = upstream(&root);
    // 資産は publish しない。release タグは在るので、取得は試みて失敗する。
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir_all(&project).unwrap();

    let r = dowelup(&home, &project, &["--upstream", &up.url, "install", "0.1.0"]).ok();
    assert!(r.stderr.contains("no usable release asset"), "stderr:\n{}", r.stderr);
    // 括弧の中が空でない。走らせた道具の名前と、その道具の言い分が入る。
    assert!(
        r.stderr.contains("curl failed:") || r.stderr.contains("cannot run curl"),
        "the reason names no tool that ran:\n{}",
        r.stderr
    );
    assert!(!r.stderr.contains("failed: )"), "the reason is empty:\n{}", r.stderr);
    // それでも入る。退避の判断そのものは変えていない。
    assert!(r.stderr.contains("built from source"), "stderr:\n{}", r.stderr);
}

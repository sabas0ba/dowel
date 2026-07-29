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

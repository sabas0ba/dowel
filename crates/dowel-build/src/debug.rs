//! デバッガの起動と、その構成の書き出し（[ADR-0024](../../../docs/adr/0024-debug-command.md)）。
//!
//! ビルド系は成果物を作った動作の入力を全て知っている。デバッガの構成を
//! 人に書かせて同期を頼むのではなく、こちらが**生成**できる
//! （docs/30-devexp.md 2節）。
//!
//! 決めていないことが1つある。**スタブの立て方は宣言させる。**
//! ランナーを stub 付きで起動する引数（qemu-user の `-g <port>`、板なら
//! ssh 越しの `gdbserver`）は互いに導出できず、推測すると「それらしく見えて
//! 固まるコマンド」ができる。

use crate::plan::Plan;
use dowel_eval::Config;
use dowel_model::{Session, TargetId};
use dowel_support::json::JsonWriter;
use dowel_support::{log_debug, log_info, Diagnostic};
use std::path::PathBuf;
use std::process::Command;

/// 1回のデバッグ起動に必要な全て。
///
/// 名前を `Session` にしないのは、`dowel_model::Session` が別のものだからである。
///
/// `--dap` はこれを書き出し、起動はこれをそのまま使う。2つの経路が同じ値を
/// 読むので、「エディタで開いた構成」と「`dowel debug` が起こす構成」が
/// 食い違わない。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    /// デバッグ対象の成果物
    pub program: PathBuf,
    /// 作業ディレクトリ。テストと同じくパッケージルート
    pub cwd: PathBuf,
    /// 起動するデバッガ。`[toolchain] debug`（既定 `gdb`）
    pub debugger: String,
    /// プログラムに渡す引数。事例を開き直すとき、その事例の引数が入る
    /// （`dowel test --debug-failed`）
    pub args: Vec<String>,
    /// プログラムに与える環境変数。同じく、事例のものが入る
    pub env: Vec<(String, String)>,
    /// クロスの場合の、スタブを立てるコマンドと接続先
    pub stub: Option<Stub>,
}

/// 別の機械（あるいはエミュレータ）でプログラムを保持する側。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stub {
    /// スタブ付きでランナーを起動するコマンド
    pub program: String,
    pub args: Vec<String>,
    /// デバッガが繋ぐ先。`localhost:1234` のような形
    pub connect: String,
}

/// 対象のデバッグ構成を組み立てる。
///
/// 起動もしないし書き出しもしない。決めるだけである——`--dap` と実起動で
/// 同じ判断を2度書かないため。
pub fn prepare(
    sess: &Session,
    plan: &Plan,
    cfg: &Config,
    tid: TargetId,
) -> Result<Launch, Diagnostic> {
    use dowel_eval::schema::TableKind;
    let target = sess.target(tid);
    // 書庫は起動できない。デバッガを書庫に向けるより、起動するものが無いと
    // 述べるほうがよい。
    if !matches!(target.kind, TableKind::Bin | TableKind::Test | TableKind::Bench) {
        return Err(Diagnostic::error(
            "not-debuggable",
            format!(
                "`{}` is a {} target; there is nothing to start",
                sess.label(tid),
                target.kind.name()
            ),
        )
        .at(target.site.file, target.site.span, "declared here")
        .note("`bin`, `test`, and `bench` targets produce something a debugger can run"));
    }
    let Some(program) = plan.artifacts.get(&tid).cloned() else {
        return Err(Diagnostic::error(
            "not-debuggable",
            format!("no artifact was planned for `{}`", sess.label(tid)),
        ));
    };

    let debugger = cfg.tool("debug").to_string();
    let cwd = sess.package(target.package).root.clone();

    // ホストと同じトリプルなら、そのまま起動できる。
    if cfg.targets_host() {
        return Ok(Launch {
            program,
            cwd,
            debugger,
            args: Vec::new(),
            env: Vec::new(),
            stub: None,
        });
    }

    // クロス。ランナーがスタブの立て方を述べていなければ断る。ホストの gdb を
    // 別アーキテクチャの実行ファイルに向けても、読めるのは記号までである。
    let runner = sess.runners.get(&cfg.target);
    let prop =
        |name: &str| runner.and_then(|r| r.prop(name)).and_then(|v| dowel_eval::specialize(v, cfg));
    let debug_args = prop("debug_args").map(|v| strings(&v)).unwrap_or_default();
    let connect = prop("debug_connect").and_then(|v| v.as_str().map(|s| s.to_string()));
    let (Some(connect), false) = (connect, debug_args.is_empty()) else {
        return Err(half_declared_stub(cfg, runner, &debug_args, prop("debug_connect")));
    };

    // ランナーの起動列に、スタブの引数を**前**から足す。
    //
    // 後ろに足せない。ランナーの `args` は末尾が「成果物を取るフラグ」で
    // あってよく（`-kernel`、ADR-0008 が勧める形）、その直後に別の
    // オプションを挿すと、フラグがそれを成果物として食う（issue #107）。
    // 前に置いて意味が変わる道具は、qemu-user の `-g` にも
    // qemu-system の `-gdb` にも無い——オプションの順序は、隣接の対を
    // 崩さない限り自由である。
    let launcher = crate::testing::Launcher::for_config(sess, cfg).0;
    let (stub_program, launch_args) = launcher.command(&program);
    let mut stub_args = debug_args;
    stub_args.extend(launch_args);
    Ok(Launch {
        program,
        cwd,
        debugger,
        args: Vec::new(),
        env: Vec::new(),
        stub: Some(Stub { program: stub_program, args: stub_args, connect }),
    })
}

/// スタブの宣言が揃っていないときの診断（issue #109）。
///
/// 「両方無い」と「片方だけ」を同じ文言にしない。半分書いた利用者に
/// 「宣言が無い」と言うと、書いてある側を見返させることになる——直すべき
/// 行は、**欠けている側**である。
fn half_declared_stub(
    cfg: &Config,
    runner: Option<&dowel_model::Runner>,
    debug_args: &[String],
    connect: Option<dowel_eval::Value>,
) -> Diagnostic {
    let target = &cfg.target;
    // 在る側の鍵を指す。表の見出しを指しても、どの行を直すのかは読めない。
    let site_of =
        |name: &str| runner.and_then(|r| r.prop(name)).and_then(|v| v.prov.nearest_site());
    let mut d = match (debug_args.is_empty(), connect.is_some()) {
        // ホスト側の起動列はあるが、繋ぎ先が無い。
        (false, false) => {
            let mut d = Diagnostic::error(
                "missing-debug-stub",
                format!("the debug stub for `{target}` has no attach address"),
            );
            if let Some(s) = site_of("debug_args") {
                d = d.at(s.file, s.span, "the host side is declared");
            }
            d.note("dowel does not parse the runner's flags, so it cannot read the address out (ADR-0024)")
                .note("add `debug_connect = \"localhost:<port>\"` next to it, naming the port those arguments open")
        }
        // 繋ぎ先はあるが、そこで待つものを誰も立てない。
        (true, true) => {
            let mut d = Diagnostic::error(
                "missing-debug-stub",
                format!("nothing hosts the program for `{target}`"),
            );
            if let Some(s) = site_of("debug_connect") {
                d = d.at(s.file, s.span, "the address to attach to is declared");
            }
            d.note("an address alone starts no stub: the debugger would wait for nothing")
                .note("add `debug_args = [...]`, the arguments that make this runner host the program behind a stub")
        }
        // どちらも無い。
        _ => {
            let mut d = Diagnostic::error(
                "missing-debug-stub",
                format!("no debug stub is declared for `{target}`"),
            )
            .note("debugging another machine's artifact needs something to host it and an address to attach to")
            .note("in `[runner.<triple>]`: `debug_args = [\"-g\", \"1234\"]`, `debug_connect = \"localhost:1234\"`")
            .note("they are written separately because dowel does not parse the runner's flags (ADR-0024)");
            if let Some(r) = runner {
                d = d.at(r.site.file, r.site.span, "this runner declares no stub");
            }
            d
        }
    };
    // 位置が1つも付かなかった場合（ランナー自体が無い）は、せめて表を指す。
    if runner.is_none() {
        d = d.note(format!("no `[runner.{target}]` is declared at all"));
    }
    d
}

fn strings(v: &dowel_eval::Value) -> Vec<String> {
    match &v.data {
        dowel_eval::Data::List(items) => {
            items.iter().filter_map(|i| i.as_str().map(|s| s.to_string())).collect()
        }
        _ => Vec::new(),
    }
}

/// DAP の起動構成。エディタがこれを読むと、同じ環境が再現される。
///
/// 名前は `cppdbg`（VS Code の C/C++ 拡張）のものに合わせる。DAP そのものは
/// 起動要求の中身を規定しておらず、実装ごとの取り決めになっている。
pub fn dap(s: &Launch) -> String {
    let mut w = JsonWriter::pretty();
    w.begin_object();
    w.field_str("type", "cppdbg");
    w.field_str("request", "launch");
    w.field_str("name", &format!("dowel: {}", file_name(&s.program)));
    w.field_str("program", &s.program.display().to_string());
    w.field_str("cwd", &s.cwd.display().to_string());
    w.field_strs("args", s.args.iter().map(|a| a.as_str()));
    if !s.env.is_empty() {
        // `cppdbg` の形。`{name, value}` の列である。
        w.key("environment").begin_array();
        for (k, v) in &s.env {
            w.begin_object();
            w.field_str("name", k);
            w.field_str("value", v);
            w.end_object();
        }
        w.end_array();
    }
    w.field_str("MIMode", "gdb");
    w.field_str("miDebuggerPath", &s.debugger);
    if let Some(stub) = &s.stub {
        // 接続先と、それを立てるコマンド。エディタ側が起動できるように、
        // 組み立て済みの列をそのまま渡す。
        w.field_str("miDebuggerServerAddress", &stub.connect);
        w.field_str("debugServerPath", &stub.program);
        // プログラムの引数はスタブの側で渡す。デバッガは繋ぐだけであり、
        // 起動列を持つのはランナーである。
        w.field_strs("debugServerArgs", stub.args.iter().chain(s.args.iter()).map(|a| a.as_str()));
    }
    // 遠隔でないことを明示する。書かないと、拡張の既定に委ねることになる。
    w.field_bool("stopAtEntry", false);
    w.end_object();
    w.finish()
}

fn file_name(p: &std::path::Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

/// デバッガを起動する。
///
/// スタブがある場合は先に立て、終了時に落とす。デバッガは端末を引き継ぐ——
/// 対話するものなので、出力を捕まえると使えない。
pub fn run(s: &Launch) -> Result<(), String> {
    let mut stub_child = match &s.stub {
        None => None,
        Some(stub) => {
            log_info!("{} {}", stub.program, stub.args.join(" "));
            let mut cmd = Command::new(&stub.program);
            // プログラムの引数は成果物の後ろ。qemu も gdbserver も、
            // プログラム以降を被起動側の引数として渡す。
            cmd.args(&stub.args).args(&s.args).current_dir(&s.cwd);
            for (k, v) in &s.env {
                cmd.env(k, v);
            }
            let child = cmd
                .spawn()
                .map_err(|e| format!("cannot start the debug stub `{}`: {e}", stub.program))?;
            Some(child)
        }
    };

    let mut cmd = Command::new(&s.debugger);
    if let Some(stub) = &s.stub {
        cmd.arg("-ex").arg(format!("target remote {}", stub.connect));
    }
    if s.stub.is_none() {
        // ホストではデバッガが起動する側なので、引数も環境もこちらに与える。
        // 被起動側の環境はデバッガから引き継がれる。
        if !s.args.is_empty() {
            cmd.arg("--args");
        }
        for (k, v) in &s.env {
            cmd.env(k, v);
        }
    }
    cmd.arg(&s.program).args(if s.stub.is_none() { &s.args[..] } else { &[] });
    cmd.current_dir(&s.cwd);
    log_info!("{} {}", s.debugger, s.program.display());
    let status = cmd.status();

    // デバッガが終わったらスタブも落とす。残すと次回の起動が同じ港を
    // 掴めない。
    if let Some(child) = &mut stub_child {
        let _ = child.kill();
        let _ = child.wait();
        log_debug!("stopped the debug stub");
    }
    match status {
        Ok(_) => Ok(()),
        Err(e) => Err(format!("cannot start `{}`: {e}", s.debugger)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(stub: Option<Stub>) -> Launch {
        Launch {
            program: PathBuf::from("/b/bin/app"),
            cwd: PathBuf::from("/p"),
            debugger: "gdb".into(),
            args: Vec::new(),
            env: Vec::new(),
            stub,
        }
    }

    #[test]
    fn a_host_configuration_names_the_program_and_the_debugger() {
        let text = dap(&session(None));
        assert!(text.contains("\"program\": \"/b/bin/app\""), "{text}");
        assert!(text.contains("\"miDebuggerPath\": \"gdb\""), "{text}");
        assert!(text.contains("\"cwd\": \"/p\""), "{text}");
        // ホストでは繋ぎ先が無い。書くと、無い相手を待つ構成になる。
        assert!(!text.contains("miDebuggerServerAddress"), "{text}");
    }

    #[test]
    fn a_case_launch_carries_its_arguments_and_environment() {
        // `--debug-failed` で開き直すとき、事例の宣言がそのまま構成になる。
        let mut s = session(None);
        s.args = vec!["parse".into(), "--strict".into()];
        s.env = vec![("SUITE_MODE".into(), "strict".into())];
        let text = dap(&s);
        assert!(text.contains("\"parse\""), "{text}");
        assert!(text.contains("\"--strict\""), "{text}");
        assert!(text.contains("\"name\": \"SUITE_MODE\""), "{text}");
        assert!(text.contains("\"value\": \"strict\""), "{text}");
    }

    #[test]
    fn a_cross_case_passes_its_arguments_through_the_stub() {
        // 引数を受け取るのはスタブに包まれた側である。デバッガは繋ぐだけ。
        let mut s = session(Some(Stub {
            program: "qemu-riscv64".into(),
            args: vec!["-g".into(), "1234".into(), "/b/bin/app".into()],
            connect: "localhost:1234".into(),
        }));
        s.args = vec!["parse".into()];
        let text = dap(&s);
        let server = &text[text.find("debugServerArgs").unwrap()..];
        // 成果物の**後**に来る。qemu はプログラム以降を被起動側に渡す。
        let artifact = server.find("/b/bin/app").unwrap();
        let arg = server.find("\"parse\"").unwrap();
        assert!(artifact < arg, "{server}");
    }

    #[test]
    fn a_cross_configuration_carries_the_stub_and_where_to_attach() {
        let text = dap(&session(Some(Stub {
            program: "qemu-riscv64".into(),
            args: vec!["-g".into(), "1234".into(), "/b/bin/app".into()],
            connect: "localhost:1234".into(),
        })));
        assert!(text.contains("\"miDebuggerServerAddress\": \"localhost:1234\""), "{text}");
        assert!(text.contains("\"debugServerPath\": \"qemu-riscv64\""), "{text}");
        assert!(text.contains("\"-g\""), "{text}");
    }
}

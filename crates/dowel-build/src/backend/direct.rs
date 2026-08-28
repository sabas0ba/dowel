//! direct バックエンド。外部の生成器を使わずに、この処理系自身が走らせる。
//!
//! 外部の生成器が無い環境でも動き、何より「生成器の挙動に依存せず
//! ビルドグラフ自体が正しいか」を切り分けられる。ninja が居ない機械では
//! ここが既定になる（`backend::select`）ので、速さも役目のうちである
//! （[ADR-0056](../../../docs/adr/0056-direct-backend-parallelism.md)）。
//!
//! 最新性は素朴な mtime 比較で判定する。ヘッダ依存はコンパイラが書いた
//! depfile を読む。ここで作った機構は将来、内容アドレスによるアクション
//! キャッシュへ置き換わる（docs/20-architecture.md 8節）。

use crate::action::ActionKind;
use crate::backend::{Backend, BuildGraph, Step};
use crate::exec::{progress, CommandLog, Failure};
use crate::toolstyle::{Deps, SHOW_INCLUDES_PREFIX};
use dowel_support::{log_debug, log_trace};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Condvar, Mutex};

pub struct Direct;

impl Backend for Direct {
    fn name(&self) -> &'static str {
        "direct"
    }

    /// 書き出すものが無い。実行そのものがこのバックエンドの出力である。
    fn emit(&self, _g: &BuildGraph) -> Result<Vec<PathBuf>, Failure> {
        Ok(Vec::new())
    }

    fn run(&self, g: &BuildGraph, jobs: Option<usize>) -> Result<(), Failure> {
        let jobs = jobs.unwrap_or_else(default_jobs).max(1).min(g.steps.len().max(1));
        log_debug!("running {} steps with {jobs} job(s)", g.steps.len());
        let previous = CommandLog::load(&g.build_dir);
        let waits = dependencies(g);

        let mut remaining = vec![0usize; g.steps.len()];
        let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); g.steps.len()];
        for (i, on) in waits.iter().enumerate() {
            remaining[i] = on.len();
            for &d in on {
                dependents[d].push(i);
            }
        }
        // 種は `order()` の並び。1本で走らせたときの出力を、辺の張り方に
        // 依らず同じ順にするため
        let ready: VecDeque<usize> = g.order().into_iter().filter(|&i| remaining[i] == 0).collect();

        let shared = Mutex::new(State {
            ready,
            remaining,
            running: 0,
            done: vec![false; g.steps.len()],
            ran: 0,
            skipped: 0,
            failure: None,
        });
        let wake = Condvar::new();
        std::thread::scope(|scope| {
            for _ in 0..jobs {
                scope.spawn(|| worker(g, &previous, &dependents, &shared, &wake));
            }
        });

        let mut state = shared.into_inner().expect("the scheduler mutex is poisoned");
        if let Some(failure) = state.failure.take() {
            return Err(failure);
        }
        // 循環があれば、その中のステップは前提が揃わないまま残る。落として
        // しまうと「理由なく実行されないステップ」になるので、`order()` が
        // 末尾に並べたのと同じ扱いで、残りを順に走らせる。
        for i in g.order() {
            if state.done[i] {
                continue;
            }
            log_trace!("  running {} outside the schedule (its inputs never settled)", i);
            if execute(g, &previous, &g.steps[i])? {
                state.ran += 1;
                progress(&format!("[{}/{}] {}", state.ran, g.steps.len(), g.steps[i].description));
            } else {
                state.skipped += 1;
            }
        }
        log_debug!("ran {} steps, skipped {} already up to date", state.ran, state.skipped);
        Ok(())
    }
}

/// 走らせる本数の既定。
///
/// ninja に合わせて「その機械が同時に進められる数」を採る。翻訳は CPU を
/// 使い切るので、これ以上並べても待ち行列が伸びるだけである。読めなければ
/// 1本——並列にできない機械で当て推量に走るより、遅い方を選ぶ。
fn default_jobs() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}

/// 走らせる側の共有状態。
struct State {
    /// 前提の揃ったステップ
    ready: VecDeque<usize>,
    /// 各ステップの、まだ終わっていない前提の数
    remaining: Vec<usize>,
    /// いま走っているステップの数。0 になり `ready` も空なら、もう増えない
    running: usize,
    done: Vec<bool>,
    ran: usize,
    skipped: usize,
    /// 最初の失敗。以後は新たに走らせない
    failure: Option<Failure>,
}

fn worker(
    g: &BuildGraph,
    previous: &CommandLog,
    dependents: &[Vec<usize>],
    shared: &Mutex<State>,
    wake: &Condvar,
) {
    loop {
        let mut state = shared.lock().expect("the scheduler mutex is poisoned");
        // 走っているものが在る限り、`ready` は増えうる。両方尽きて初めて
        // 「もう何も来ない」と言える。
        while state.ready.is_empty() && state.running > 0 && state.failure.is_none() {
            state = wake.wait(state).expect("the scheduler mutex is poisoned");
        }
        if state.failure.is_some() {
            break;
        }
        let Some(i) = state.ready.pop_front() else { break };
        state.running += 1;
        drop(state);

        let result = execute(g, previous, &g.steps[i]);

        let mut state = shared.lock().expect("the scheduler mutex is poisoned");
        state.running -= 1;
        state.done[i] = true;
        match result {
            Ok(true) => {
                state.ran += 1;
                // 番号を振るのは終わったときである。始めたときに振ると、
                // 同時に走っている分だけ番号が飛び飛びに現れる。錠を持った
                // まま書くので、番号の順と行の順が食い違うこともない。
                progress(&format!("[{}/{}] {}", state.ran, g.steps.len(), g.steps[i].description));
            }
            Ok(false) => state.skipped += 1,
            Err(e) => {
                if state.failure.is_none() {
                    state.failure = Some(e);
                }
            }
        }
        if state.failure.is_none() {
            for &d in &dependents[i] {
                state.remaining[d] -= 1;
                if state.remaining[d] == 0 {
                    state.ready.push_back(d);
                }
            }
        }
        drop(state);
        // 待っている者を起こす。前提が減ったかもしれず、尽きたかもしれない。
        wake.notify_all();
    }
    // 抜ける前にもう一度起こす。失敗と枯渇はどちらも全員に伝える必要がある。
    wake.notify_all();
}

/// 1つのステップを、要るなら走らせる。戻り値は「走らせたか」。
///
/// 最新性の判定は走らせる直前に行う。前提が書き直した入力を見なければ
/// ならず、計画の時点で決めておくことはできない。
fn execute(g: &BuildGraph, previous: &CommandLog, step: &Step) -> Result<bool, Failure> {
    match crate::exec::staleness(step, previous) {
        None => {
            log_trace!("up to date: {}", step.description);
            return Ok(false);
        }
        Some(reason) => log_trace!("  stale: {}", reason.say()),
    }
    run_step(g, step)?;
    Ok(true)
}

/// 各ステップが待つべきステップ。
///
/// 宣言された辺（`deps`）と、ファイルの関係（入力を作るステップ）の**両方**を
/// 採る。逐次に走らせていた頃はどちらか片方で足りたが、同時に走らせる以上、
/// 片方にしか現れない順序は競合になる。`build-graph.json` を読み直した
/// グラフも同じ経路を通る——外から来た文書の `deps` が完全である保証は
/// 無い（docs/14-build-graph.md）。
pub fn dependencies(g: &BuildGraph) -> Vec<Vec<usize>> {
    let mut producer: HashMap<&Path, usize> = HashMap::new();
    for (i, s) in g.steps.iter().enumerate() {
        for out in &s.outputs {
            producer.insert(out.as_path(), i);
        }
    }
    g.steps
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let mut on: BTreeSet<usize> =
                s.deps.iter().copied().filter(|&d| d < g.steps.len() && d != i).collect();
            for input in &s.inputs {
                match producer.get(input.as_path()) {
                    Some(&p) if p != i => {
                        on.insert(p);
                    }
                    _ => {}
                }
            }
            on.into_iter().collect()
        })
        .collect()
}

fn run_step(g: &BuildGraph, step: &Step) -> Result<(), Failure> {
    for out in &step.outputs {
        if let Some(parent) = out.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    log_debug!("  {}", step.command_line());

    let mut cmd = Command::new(&step.program);
    cmd.args(&step.arguments);
    // 生成は自分の出力の置き場所で走る（ADR-0054）。上で親を作ってあるので
    // 在ることは保証されている。
    cmd.current_dir(step.cwd.as_deref().unwrap_or(&g.build_dir));
    let out = cmd.output().map_err(|e| {
        Failure::of(
            &step.description,
            step.command_line(),
            format!("{e} (cannot start `{}`)", step.program),
        )
    })?;
    if !out.status.success() {
        return Err(Failure {
            description: step.description.clone(),
            command: step.command_line(),
            status: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        });
    }
    // 宣言した出力が現れないまま成功したら、そこで落とす
    // （[ADR-0051](../../../docs/adr/0051-source-language-is-closed.md)）。
    // 通すと、失敗は次の段の言葉になる——結合器が、ビルドディレクトリの
    // 中のパスについて述べる。そのうえ現れない出力は常に古いままなので、
    // 増分ビルドが収束しない（issue #157、#112 と同じ形）。
    if let Some(missing) = step.outputs.iter().find(|o| !o.exists()) {
        return Err(Failure {
            description: step.description.clone(),
            command: step.command_line(),
            status: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).to_string(),
            // 道具自身の言葉を残す。「翻訳しないので入力を使わなかった」と
            // 言っていることが多く、それが最も説明になる。
            stderr: format!(
                "{}\nit exited 0 without writing {}",
                String::from_utf8_lossy(&out.stderr).trim_end(),
                missing.display()
            ),
        });
    }
    // MSVC はヘッダ依存の記録を書かない。標準出力に1行1件で並べるだけなので、
    // それを `.d` に畳むのは**実行した側**の仕事になる（ADR-0027）。畳んで
    // おけば、最新性を判定する側は様式を知らずに済む。
    if g.deps == Deps::ShowIncludes && step.kind == ActionKind::Compile {
        if let Some(d) = &step.depfile {
            let stdout = String::from_utf8_lossy(&out.stdout);
            write_depfile_from_show_includes(d, &step.outputs, &stdout)?;
        }
    }
    Ok(())
}

/// `/showIncludes` の出力を make 形式の `.d` に畳む。
///
/// 接頭辞に合う行が1つも無いことは、**依存が無いこと**ではない。地域化された
/// `cl` は別の文言を出す。`.d` を空で書くと「ヘッダに依存しない翻訳単位」と
/// 読まれ、ヘッダの変更が黙って見落とされる——書かずに残せば、次回は
/// 「記録が無い」として保守的に組み直される（[`is_up_to_date`]）。
fn write_depfile_from_show_includes(
    depfile: &Path,
    outputs: &[PathBuf],
    stdout: &str,
) -> Result<(), Failure> {
    let headers: Vec<&str> = stdout
        .lines()
        .filter_map(|l| l.trim_end().strip_prefix(SHOW_INCLUDES_PREFIX))
        .map(|rest| rest.trim())
        .filter(|rest| !rest.is_empty())
        .collect();
    if headers.is_empty() {
        log_debug!("  no `{SHOW_INCLUDES_PREFIX}` lines; leaving the dependency record unwritten");
        return Ok(());
    }
    let target = outputs.first().map(|p| p.display().to_string()).unwrap_or_default();
    // make 形式。空白を含む道はエスケープする——読む側（`read_depfile`）が
    // その形を期待している。
    let mut text = format!("{}:", target.replace(' ', "\\ "));
    for h in &headers {
        text.push_str(&format!(" \\\n  {}", h.replace(' ', "\\ ")));
    }
    text.push('\n');
    if let Some(parent) = depfile.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(depfile, text).map_err(|e| {
        Failure::of(
            "recording the header dependencies",
            depfile.display().to_string(),
            format!("{e} (cannot write `{}`)", depfile.display()),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(id: usize, inputs: &[&str], outputs: &[&str], deps: Vec<usize>) -> Step {
        Step {
            id,
            kind: ActionKind::Compile,
            target: "p:t".into(),
            description: format!("step {id}"),
            program: "true".into(),
            arguments: vec![],
            inputs: inputs.iter().map(PathBuf::from).collect(),
            outputs: outputs.iter().map(PathBuf::from).collect(),
            depfile: None,
            deps,
            cwd: None,
        }
    }

    fn graph_of(steps: Vec<Step>) -> BuildGraph {
        BuildGraph {
            build_dir: PathBuf::from("/b"),
            steps,
            artifacts: vec![],
            deps: Deps::Depfile,
            default_outputs: vec![],
            tool_stamps: vec![],
        }
    }

    #[test]
    fn a_step_waits_for_the_one_that_writes_its_input() {
        // 宣言された辺が無くても、入力を作る側は前提である。同時に走らせる
        // 以上、片方にしか現れない順序は競合になる（ADR-0056）。
        let g = graph_of(vec![
            step(0, &["/s/a.c"], &["/b/a.o"], vec![]),
            step(1, &["/b/a.o"], &["/b/app"], vec![]),
        ]);
        assert_eq!(dependencies(&g), vec![Vec::<usize>::new(), vec![0]]);
    }

    #[test]
    fn a_declared_edge_counts_even_when_no_file_connects_the_steps() {
        // 生成された頭部を読む翻訳のように、出力を読まない前提もある。
        let g = graph_of(vec![
            step(0, &[], &["/b/gen/x.h"], vec![]),
            step(1, &["/s/b.c"], &["/b/b.o"], vec![0]),
        ]);
        assert_eq!(dependencies(&g), vec![Vec::<usize>::new(), vec![0]]);
    }

    #[test]
    fn a_step_does_not_wait_for_itself_or_for_a_step_that_is_not_there() {
        // 自分の出力を読み直すステップと、範囲外を指す `deps`。どちらも
        // そのまま数えると前提が永久に減らず、走らせる側が止まる。
        let g = graph_of(vec![step(0, &["/b/a.o"], &["/b/a.o"], vec![0, 7])]);
        assert_eq!(dependencies(&g), vec![Vec::<usize>::new()]);
    }
}

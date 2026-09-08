//! 走らせずに、今の状態を述べる
//! （[ADR-0061](../../../docs/adr/0061-the-state-is-a-question.md)）。
//!
//! ビルド系に最も多く向けられる問いは2つある——「なぜ組み直したのか」と
//! 「なぜ速くならないのか」。どちらも今までは trace のログにしか答が無く、
//! しかも既定のバックエンドでは答そのものが無かった。ログは走らせた後の
//! 記録であり、問いは走らせる前に立つ。
//!
//! ここは**問い合わせ**である。段を1つも走らせず、ビルド木には何も書かない。
//! バックエンドも呼ばない。起動の段取りそのものは他の命令と同じで、評価の
//! 記録は `check` と同じように書かれ、道具の三つ組も要れば聞く——ビルド
//! ディレクトリの名前が構成から出る以上、聞かずには見に行く先が決まらない。
//!
//! 答は3段に分かれる。
//!
//! - 評価が何を使い回し、何を作り直したか（`dowel_query::Stats`）
//! - ビルド前の入力と別名の準備が何を変えるか
//! - どの段が走り、その理由は何か（`exec::staleness`）
//!
//! 判定は借りてくる。走らせる側と同じ関数を呼ぶので、ここが述べた理由で
//! 走り、走る理由がここに出る。

use crate::backend::BuildGraph;
use crate::exec::{CommandLog, Stale};
use crate::plan::Plan;
use dowel_model::Session;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// 走らせる前に分かること全部。
#[derive(Clone, Debug)]
pub struct Status {
    /// ビルドディレクトリ。道を相対で述べるための基点
    pub build_dir: PathBuf,
    /// パッケージの根。ビルド木の外の道はここからの相対で述べる
    pub source_root: PathBuf,
    /// マニフェストの評価が何をしたか
    pub evaluation: dowel_model::QueryStats,
    /// ビルド前に揃える、計画が生成した入力と共有ライブラリの別名
    pub preparations: Vec<PreparationStatus>,
    /// 段ごとの見立て。計画の順
    pub steps: Vec<StepStatus>,
}

/// ビルドを始める側が、どの段よりも前に揃えるもの。
#[derive(Clone, Debug)]
pub struct PreparationStatus {
    pub kind: PreparationKind,
    pub would_change: bool,
}

#[derive(Clone, Debug)]
pub enum PreparationKind {
    File(PathBuf),
    LinkAlias { path: PathBuf, target: PathBuf },
}

/// 1つの段の見立て。
#[derive(Clone, Debug)]
pub struct StepStatus {
    pub description: String,
    /// この段が属するターゲットの表示ラベル
    pub target: String,
    /// 走る理由。`None` は最新
    pub reason: Option<Stale>,
}

impl Status {
    /// 走ることになる段の数。
    pub fn would_run(&self) -> usize {
        self.steps.iter().filter(|s| s.reason.is_some()).count()
    }

    /// 書くか置き直すことになる準備の数。
    pub fn would_prepare(&self) -> usize {
        self.preparations.iter().filter(|p| p.would_change).count()
    }
}

/// 今の状態を読む。何も書かない。
pub fn of(sess: &Session, plan: &Plan) -> Status {
    let g = BuildGraph::of(sess, plan);
    let previous = CommandLog::load(&g.build_dir);

    // 道具の刻印は、走らせるならステップより先に書かれる（ADR-0055）。
    // 中身が変われば書き直され、それを入力に持つ段はそこで古くなる。
    // 述べる側は書かないので、書かれた後に見えるはずのものを先に数える。
    //
    // **在って中身が違うものだけ。** 刻印がまだ無いのは、その道具が変わった
    // のではなく、このビルド木で1度も組んでいないということである。それを
    // 「道具が変わった」と述べると、初回のビルドがすべて道具のせいになる。
    let rewritten: BTreeSet<&Path> = g
        .tool_stamps
        .iter()
        .filter(|(path, identity)| {
            path.exists()
                && std::fs::read_to_string(path).ok().as_deref() != Some(identity.as_str())
        })
        .map(|(path, _)| path.as_path())
        .collect();

    // 計画が生成する入力も、実行側は内容が違うときだけ書き直す。まだ無い
    // ものは通常の `InputMissing` / `NeverRun` に任せ、在って違うときだけ
    // 「計画が書き直す」と述べる。
    let prepared_rewrites: BTreeSet<&Path> = g
        .prepared_files
        .iter()
        .filter(|(path, contents)| {
            path.exists()
                && std::fs::read_to_string(path).ok().as_deref() != Some(contents.as_str())
        })
        .map(|(path, _)| path.as_path())
        .collect();

    let mut reasons: Vec<Option<Stale>> = g
        .steps
        .iter()
        .map(|step| {
            if let Some(stamp) = step.inputs.iter().find(|i| rewritten.contains(i.as_path())) {
                return Some(Stale::ToolChanged(stamp.clone()));
            }
            if let Some(input) =
                step.inputs.iter().find(|i| prepared_rewrites.contains(i.as_path()))
            {
                return Some(Stale::PreparedInputChanged(input.clone()));
            }
            crate::exec::staleness(step, &previous)
        })
        .collect();

    propagate(&g, &mut reasons);

    Status {
        build_dir: g.build_dir.clone(),
        source_root: sess.packages.first().map(|p| p.root.clone()).unwrap_or_default(),
        evaluation: sess.query_stats(),
        preparations: g
            .prepared_files
            .iter()
            .map(|(path, contents)| PreparationStatus {
                kind: PreparationKind::File(path.clone()),
                would_change: std::fs::read_to_string(path).ok().as_deref()
                    != Some(contents.as_str()),
            })
            .chain(g.link_aliases.iter().map(|(path, target)| PreparationStatus {
                kind: PreparationKind::LinkAlias { path: path.clone(), target: target.clone() },
                would_change: !crate::backend::link_alias_matches(path, target),
            }))
            .collect(),
        steps: g
            .steps
            .iter()
            .zip(reasons)
            .map(|(step, reason)| StepStatus {
                description: step.description.clone(),
                target: step.target.clone(),
                reason,
            })
            .collect(),
    }
}

/// 先に走る段が書き直す入力を、下流へ伝える。
///
/// 走らせる側はこれを見つける必要が無い——先の段が実際に書き、時刻が動くので、
/// 次の段は `InputNewer` として自分で気づく。走らせない側は、その1手を自分で
/// 進めなければ「最新」と述べてしまう。
///
/// 変化が無くなるまで回す。回数はグラフの深さで止まり、深さは段の数より
/// はるかに小さい——翻訳、書庫、リンクで3である。
fn propagate(g: &BuildGraph, reasons: &mut [Option<Stale>]) {
    // `deps` は順序だけを表す辺を含む。direct と ninja はそれを鮮度には
    // 使わない（ADR-0056）ので、ここも実際に読むファイルだけを辿る。
    let mut producer: HashMap<&Path, usize> = HashMap::new();
    for (i, step) in g.steps.iter().enumerate() {
        for output in &step.outputs {
            producer.insert(output.as_path(), i);
        }
    }
    loop {
        let mut moved = false;
        for i in 0..g.steps.len() {
            if reasons[i].is_some() {
                continue;
            }
            let Some(input) = g.steps[i].inputs.iter().find(|input| {
                producer.get(input.as_path()).is_some_and(|upstream| reasons[*upstream].is_some())
            }) else {
                continue;
            };
            reasons[i] = Some(Stale::InputRebuilt(input.clone()));
            moved = true;
        }
        if !moved {
            return;
        }
    }
}

/// 人が読む形。
pub fn render_text(s: &Status) -> String {
    let mut out = String::new();
    let e = &s.evaluation;
    // 使い回しは1種類ではない。依存を辿って確かめたものと、同じ版で2度目
    // 以降に聞かれて即答したものは別のことであり、足して1つの「reused」に
    // すると、どちらが効いているのかが読めなくなる。
    out.push_str(&format!(
        "evaluation  {} recomputed, {} unchanged after recomputing, \
         {} verified, {} answered again, {} skipped\n",
        e.computed, e.cut_off, e.verified, e.hit, e.skipped
    ));
    if !s.preparations.is_empty() {
        out.push_str(&format!(
            "preparation {} planned, {} would change\n",
            s.preparations.len(),
            s.would_prepare()
        ));
    }
    out.push_str(&format!("steps       {} planned, {} would run\n", s.steps.len(), s.would_run()));

    let mut preparing = s.preparations.iter().filter(|p| p.would_change).peekable();
    if preparing.peek().is_some() {
        out.push_str("\nwould prepare\n");
        for p in preparing {
            match &p.kind {
                PreparationKind::File(path) => {
                    out.push_str(&format!("  WRITE {}\n", short_path(s, path).display()));
                }
                PreparationKind::LinkAlias { path, target } => {
                    out.push_str(&format!(
                        "  SYMLINK {} -> {}\n",
                        short_path(s, path).display(),
                        target.display()
                    ));
                }
            }
        }
    }

    let width = s
        .steps
        .iter()
        .filter(|st| st.reason.is_some())
        .map(|st| st.description.chars().count())
        .max()
        .unwrap_or(0);
    let mut running = s.steps.iter().filter(|st| st.reason.is_some()).peekable();
    if running.peek().is_some() {
        out.push_str("\nwould run\n");
        for st in running {
            let reason = st.reason.as_ref().expect("filtered to the steps that would run");
            out.push_str(&format!(
                "  {:<width$}  {}\n",
                st.description,
                say(s, reason),
                width = width
            ));
        }
    }

    let mut fresh = s.steps.iter().filter(|st| st.reason.is_none()).peekable();
    if fresh.peek().is_some() {
        out.push_str("\nup to date\n");
        for st in fresh {
            out.push_str(&format!("  {}\n", st.description));
        }
    }
    if s.steps.is_empty() {
        out.push_str("\n(no steps)\n");
    }
    out
}

/// 機械が読む形。
pub fn render_json(s: &Status) -> String {
    let mut w = dowel_support::json::JsonWriter::new();
    w.begin_object();
    w.key("evaluation");
    w.begin_object()
        .field_u64("recomputed", s.evaluation.computed as u64)
        .field_u64("cut_off", s.evaluation.cut_off as u64)
        .field_u64("verified", s.evaluation.verified as u64)
        .field_u64("answered_again", s.evaluation.hit as u64)
        .field_u64("skipped", s.evaluation.skipped as u64)
        .end_object();
    w.key("steps");
    w.begin_array();
    for st in &s.steps {
        w.begin_object()
            .field_str("description", &st.description)
            .field_str("target", &st.target)
            .field_bool("would_run", st.reason.is_some());
        if let Some(reason) = &st.reason {
            w.field_str("reason", &say(s, reason));
        }
        w.end_object();
    }
    w.end_array();
    w.key("preparations");
    w.begin_array();
    for p in &s.preparations {
        w.begin_object();
        match &p.kind {
            PreparationKind::File(path) => {
                w.field_str("kind", "write");
                w.field_str("path", &short_path(s, path).display().to_string());
            }
            PreparationKind::LinkAlias { path, target } => {
                w.field_str("kind", "symlink");
                w.field_str("path", &short_path(s, path).display().to_string());
                w.field_str("target", &target.display().to_string());
            }
        }
        w.field_bool("would_change", p.would_change);
        w.end_object();
    }
    w.end_array();
    w.end_object();
    w.finish()
}

/// 理由を、読める道で述べる。
///
/// 絶対パスのままでは、どの段の話かより道の長さが目に入る。ビルド木の中は
/// ビルドディレクトリから、外はパッケージの根からの相対にする。
fn say(s: &Status, reason: &Stale) -> String {
    let Some(path) = reason.path() else { return reason.say() };
    let short = short_path(s, path);
    if short == path {
        return reason.say();
    }
    reason.say().replace(&path.display().to_string(), &short.display().to_string())
}

fn short_path<'a>(s: &'a Status, path: &'a Path) -> &'a Path {
    path.strip_prefix(&s.build_dir).or_else(|_| path.strip_prefix(&s.source_root)).unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::ActionKind;
    use crate::backend::Step;
    use crate::toolstyle::Deps;

    fn status(steps: Vec<StepStatus>) -> Status {
        Status {
            build_dir: PathBuf::from("/b"),
            source_root: PathBuf::from("/s"),
            evaluation: dowel_model::QueryStats::default(),
            preparations: Vec::new(),
            steps,
        }
    }

    fn step(description: &str, reason: Option<Stale>) -> StepStatus {
        StepStatus { description: description.into(), target: "p:t".into(), reason }
    }

    fn graph_step(id: usize, inputs: &[&str], outputs: &[&str], deps: Vec<usize>) -> Step {
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

    fn graph(steps: Vec<Step>) -> BuildGraph {
        BuildGraph {
            build_dir: PathBuf::from("/b"),
            steps,
            artifacts: vec![],
            default_outputs: vec![],
            deps: Deps::Depfile,
            tool_stamps: vec![],
            prepared_files: vec![],
            link_aliases: vec![],
        }
    }

    #[test]
    fn a_path_inside_the_build_tree_is_said_from_the_build_directory() {
        let s = status(Vec::new());
        assert_eq!(
            say(&s, &Stale::InputNewer(PathBuf::from("/b/obj/a.o"))),
            "obj/a.o is newer than the output"
        );
        assert_eq!(
            say(&s, &Stale::InputNewer(PathBuf::from("/s/src/a.c"))),
            "src/a.c is newer than the output"
        );
    }

    #[test]
    fn a_path_under_neither_root_is_said_whole() {
        // 系のヘッダはどちらの下にも無い。切り詰めれば別のファイルを指す。
        let s = status(Vec::new());
        assert_eq!(
            say(&s, &Stale::InputNewer(PathBuf::from("/usr/include/stdio.h"))),
            "/usr/include/stdio.h is newer than the output"
        );
    }

    #[test]
    fn a_reason_without_a_path_is_said_as_it_is() {
        let s = status(Vec::new());
        assert_eq!(say(&s, &Stale::CommandChanged), "the command changed since the last run");
    }

    #[test]
    fn the_two_lists_are_shown_separately_and_counted() {
        let s = status(vec![
            step("CC a.o", Some(Stale::InputNewer(PathBuf::from("/s/a.c")))),
            step("CC b.o", None),
        ]);
        assert_eq!(s.would_run(), 1);
        let text = render_text(&s);
        assert!(text.contains("steps       2 planned, 1 would run"), "{text}");
        assert!(text.contains("would run\n  CC a.o  a.c is newer than the output"), "{text}");
        assert!(text.contains("up to date\n  CC b.o"), "{text}");
    }

    #[test]
    fn nothing_planned_says_so_rather_than_showing_empty_lists() {
        let text = render_text(&status(Vec::new()));
        assert!(text.contains("(no steps)"), "{text}");
        // 見出しの側だけを見る。要約の行にも同じ語が入っている。
        assert!(!text.contains("\nwould run\n"), "{text}");
        assert!(!text.contains("\nup to date\n"), "{text}");
    }

    #[test]
    fn an_order_only_dependency_does_not_make_a_fresh_step_stale() {
        let g = graph(vec![
            graph_step(0, &["/s/a.c"], &["/b/a.o"], vec![]),
            graph_step(1, &["/s/b.c"], &["/b/b.o"], vec![0]),
        ]);
        let mut reasons = vec![Some(Stale::InputNewer(PathBuf::from("/s/a.c"))), None];
        propagate(&g, &mut reasons);
        assert!(reasons[1].is_none(), "an ordering edge was treated as freshness");
    }

    #[test]
    fn a_rewritten_input_does_make_its_reader_stale() {
        let g = graph(vec![
            graph_step(0, &["/s/a.c"], &["/b/a.o"], vec![]),
            graph_step(1, &["/b/a.o"], &["/b/app"], vec![]),
        ]);
        let mut reasons = vec![Some(Stale::InputNewer(PathBuf::from("/s/a.c"))), None];
        propagate(&g, &mut reasons);
        assert_eq!(reasons[1], Some(Stale::InputRebuilt(PathBuf::from("/b/a.o"))));
    }
}

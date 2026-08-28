//! 走らせずに、今の状態を述べる
//! （[ADR-0061](../../../docs/adr/0061-the-state-is-a-question.md)）。
//!
//! ビルド系に最も多く向けられる問いは2つある——「なぜ組み直したのか」と
//! 「なぜ速くならないのか」。どちらも今までは trace のログにしか答が無く、
//! しかも既定のバックエンドでは答そのものが無かった。ログは走らせた後の
//! 記録であり、問いは走らせる前に立つ。
//!
//! ここは**問い合わせ**である。何も書かず、何も起こさない。答は2段に分かれる。
//!
//! - 評価が何を使い回し、何を作り直したか（`dowel_query::Stats`）
//! - どの段が走り、その理由は何か（`exec::staleness`）
//!
//! 判定は借りてくる。走らせる側と同じ関数を呼ぶので、ここが述べた理由で
//! 走り、走る理由がここに出る。

use crate::backend::BuildGraph;
use crate::exec::{CommandLog, Stale};
use crate::plan::Plan;
use dowel_model::Session;
use std::collections::BTreeSet;
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
    /// 段ごとの見立て。計画の順
    pub steps: Vec<StepStatus>,
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
            std::fs::read_to_string(path).is_ok_and(|said| said != **identity)
        })
        .map(|(path, _)| path.as_path())
        .collect();

    let mut reasons: Vec<Option<Stale>> = g
        .steps
        .iter()
        .map(|step| match step.inputs.iter().find(|i| rewritten.contains(i.as_path())) {
            Some(stamp) => Some(Stale::ToolChanged(stamp.clone())),
            None => crate::exec::staleness(step, &previous),
        })
        .collect();

    propagate(&g, &mut reasons);

    Status {
        build_dir: g.build_dir.clone(),
        source_root: sess.packages.first().map(|p| p.root.clone()).unwrap_or_default(),
        evaluation: sess.query_stats(),
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
    let waits = crate::backend::direct::dependencies(g);
    loop {
        let mut moved = false;
        for i in 0..g.steps.len() {
            if reasons[i].is_some() {
                continue;
            }
            let Some(&upstream) = waits[i].iter().find(|&&w| reasons[w].is_some()) else {
                continue;
            };
            // 書き直される入力そのものを名指す。「先の段が走るから」だけでは、
            // どのファイルを経由して届いたのかが読み手に分からない。
            let through = g.steps[upstream]
                .outputs
                .iter()
                .find(|o| g.steps[i].inputs.contains(o))
                .or_else(|| g.steps[upstream].outputs.first());
            reasons[i] = Some(match through {
                Some(path) => Stale::InputRebuilt(path.clone()),
                // 出力を宣言しない段に待たされている。辿る道が無いので、
                // 先の段そのものを名指す。
                None => Stale::InputRebuiltBy(g.steps[upstream].description.clone()),
            });
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
    out.push_str(&format!(
        "evaluation  {} recomputed, {} unchanged after recomputing, {} reused, {} skipped\n",
        e.computed, e.cut_off, e.verified, e.skipped
    ));
    out.push_str(&format!("steps       {} planned, {} would run\n", s.steps.len(), s.would_run()));

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
        .field_u64("reused", s.evaluation.verified as u64)
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
    w.end_object();
    w.finish()
}

/// 理由を、読める道で述べる。
///
/// 絶対パスのままでは、どの段の話かより道の長さが目に入る。ビルド木の中は
/// ビルドディレクトリから、外はパッケージの根からの相対にする。
fn say(s: &Status, reason: &Stale) -> String {
    let Some(path) = reason.path() else { return reason.say() };
    let short = path
        .strip_prefix(&s.build_dir)
        .or_else(|_| path.strip_prefix(&s.source_root))
        .unwrap_or(path);
    if short == path {
        return reason.say();
    }
    reason.say().replace(&path.display().to_string(), &short.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(steps: Vec<StepStatus>) -> Status {
        Status {
            build_dir: PathBuf::from("/b"),
            source_root: PathBuf::from("/s"),
            evaluation: dowel_model::QueryStats::default(),
            steps,
        }
    }

    fn step(description: &str, reason: Option<Stale>) -> StepStatus {
        StepStatus { description: description.into(), target: "p:t".into(), reason }
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
}

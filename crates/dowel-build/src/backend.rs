//! 出力段のバックエンド層（[ADR-0018](../../../docs/adr/0018-backend-layer.md)）。
//!
//! 計画は「何を起動すべきか」までを決める。それを**誰が**走らせるかは別の
//! 関心であり、ここがその境界である。
//!
//! バックエンドが受け取るのは `BuildGraph` だけで、`Plan` は渡さない。渡せば
//! 渡した分だけ内部表現に触れ、境界が形骸化する。`BuildGraph` は独自形式
//! （`build-graph.json`）の中身そのものであり、書き出して読み直したものと
//! 区別が付かない。ゆえに「形式に何かが足りない」という事故は起こらない——
//! 足りなければ ninja も make も動かなくなる。

pub mod direct;
pub mod graph;
pub mod make;
pub mod ninja;

use crate::action::ActionKind;
use crate::exec::{CommandLog, Failure};
use crate::plan::Plan;
use dowel_model::Session;
use dowel_support::log_debug;
use std::path::PathBuf;

/// バックエンドが受け取るビルドグラフ。
///
/// `TargetId` も `Session` も持たない。ターゲットは表示ラベル——診断が使うのと
/// 同じ文字列——として現れる。この層から先に模型の識別子を持ち出さないため。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BuildGraph {
    pub build_dir: PathBuf,
    pub steps: Vec<Step>,
    /// ターゲットの表示ラベル → 最終成果物
    pub artifacts: Vec<(String, PathBuf)>,
    /// 対象を指定しなかったときに作るもの
    pub default_outputs: Vec<PathBuf>,
    /// ヘッダ依存の取り方（ADR-0027）。1回のビルドで様式は1つなので、
    /// ステップごとではなくグラフが持つ
    pub deps: crate::toolstyle::Deps,
    /// 道具の刻印と、その中身
    /// （[ADR-0055](../../../docs/adr/0055-tool-identity-in-freshness.md)）。
    /// ステップの入力に現れるので、走らせる前に書かれていなければならない
    pub tool_stamps: Vec<(PathBuf, String)>,
}

/// 1回のプロセス起動。
///
/// `Action` と別の型なのは、こちらが**読み直せる**側だからである。解析した
/// 文書には `TargetId` が無い。同じ型で兼ねると、片方の経路でだけ埋まる欄が
/// できる。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Step {
    pub id: usize,
    pub kind: ActionKind,
    /// このステップが属するターゲットの表示ラベル
    pub target: String,
    pub description: String,
    pub program: String,
    pub arguments: Vec<String>,
    pub inputs: Vec<PathBuf>,
    pub outputs: Vec<PathBuf>,
    /// コンパイラが書き出すヘッダ依存（make 形式）
    pub depfile: Option<PathBuf>,
    /// 先に終わっていなければならないステップの `id`
    pub deps: Vec<usize>,
    /// 起動するときの作業ディレクトリ。`None` はビルドディレクトリ（ADR-0054）
    pub cwd: Option<PathBuf>,
}

impl Step {
    pub fn command(&self) -> Vec<String> {
        let mut v = Vec::with_capacity(self.arguments.len() + 1);
        v.push(self.program.clone());
        v.extend(self.arguments.iter().cloned());
        v
    }

    pub fn command_line(&self) -> String {
        crate::action::command_line(&self.command(), self.cwd.as_deref())
    }
}

impl BuildGraph {
    /// 計画から作る。ここがラベルを解決する唯一の場所である。
    pub fn of(sess: &Session, plan: &Plan) -> BuildGraph {
        let steps = plan
            .actions
            .iter()
            .map(|a| Step {
                id: a.id.0,
                kind: a.kind,
                target: sess.label(a.target),
                description: a.description.clone(),
                program: a.program.clone(),
                arguments: a.args.clone(),
                inputs: a.inputs.clone(),
                outputs: a.outputs.clone(),
                depfile: a.depfile.clone(),
                deps: a.deps.iter().map(|d| d.0).collect(),
                cwd: a.cwd.clone(),
            })
            .collect();
        BuildGraph {
            build_dir: plan.build_dir.clone(),
            steps,
            artifacts: plan.artifacts.iter().map(|(t, p)| (sess.label(*t), p.clone())).collect(),
            default_outputs: plan.default_outputs(),
            deps: plan.deps,
            tool_stamps: plan.tool_stamps.clone(),
        }
    }

    /// 依存が先に来る順。
    ///
    /// 計画が作ったものは既にこの順に並んでいるが、読み込んだ文書はそうとは
    /// 限らない。整列は文書を信用しないためのものである。循環していれば
    /// 残りをそのままの順で返す——ここで黙って落とすと、実行されない
    /// ステップが理由なく消える。
    pub fn order(&self) -> Vec<usize> {
        let mut done = vec![false; self.steps.len()];
        let mut out = Vec::with_capacity(self.steps.len());
        let mut progressed = true;
        while progressed {
            progressed = false;
            for (i, s) in self.steps.iter().enumerate() {
                if done[i] {
                    continue;
                }
                if s.deps.iter().all(|d| self.steps.get(*d).is_none() || done[*d]) {
                    done[i] = true;
                    out.push(i);
                    progressed = true;
                }
            }
        }
        for (i, _) in self.steps.iter().enumerate() {
            if !done[i] {
                out.push(i);
            }
        }
        out
    }
}

/// ビルドグラフを受け取って走らせるもの。
///
/// 追加は `NAMES` に1行と、このトレイトの実装を1つ書くことである。
pub trait Backend {
    fn name(&self) -> &'static str;

    /// この環境で使えるか。既定の選択が見る。
    fn available(&self, probe: &mut crate::probe::Prober) -> bool {
        let _ = probe;
        true
    }

    /// 成果物を作るか。`graph` は書き出すだけで、走らせるのは受け取った側。
    fn builds(&self) -> bool {
        true
    }

    /// このバックエンドの入力を書き出す。書いたファイルを返す。
    fn emit(&self, g: &BuildGraph) -> Result<Vec<PathBuf>, Failure>;

    /// 書き出したものを走らせる。
    fn run(&self, g: &BuildGraph, jobs: Option<usize>) -> Result<(), Failure>;
}

/// 使えるバックエンドの名前。`find` と食い違わないことは試験で確かめる。
pub const NAMES: &[&str] = &["ninja", "direct", "make", "graph"];

pub fn find(name: &str) -> Option<Box<dyn Backend>> {
    match name {
        "ninja" => Some(Box::new(ninja::Ninja)),
        "direct" => Some(Box::new(direct::Direct)),
        "make" => Some(Box::new(make::Make)),
        "graph" => Some(Box::new(graph::Graph)),
        _ => None,
    }
}

/// 指定されたバックエンド。指定が無ければ既定を選ぶ。
pub fn select(
    requested: Option<&str>,
    probe: &mut crate::probe::Prober,
) -> Result<Box<dyn Backend>, String> {
    match requested {
        Some(name) => find(name).ok_or_else(|| {
            format!("`--backend` must be one of {} (got `{name}`)", NAMES.join(", "))
        }),
        // ninja が既定。無ければ direct へ落ちる。make へは落ちない——
        // 既定が環境によって別の生成器になると、同じ指示が別の失敗を出す。
        None => Ok(if ninja::Ninja.available(probe) {
            Box::new(ninja::Ninja)
        } else {
            log_debug!("ninja not found; falling back to the direct backend");
            Box::new(direct::Direct)
        }),
    }
}

/// バックエンドを走らせ、成功した実行を記録する。
///
/// 記録はこの層が持つ。どのバックエンドで作られたかに関わらず「今ある成果物は
/// どのコマンドの産物か」が一貫する。途中で失敗した場合は書かない——再生成
/// できたものまで最新扱いにすると、次の実行が古い成果物を残したまま成功する。
pub fn run(backend: &dyn Backend, g: &BuildGraph, jobs: Option<usize>) -> Result<(), Failure> {
    let _phase = dowel_support::log::Phase::start("execute");
    write_tool_stamps(g);
    let result = backend.run(g, jobs);
    if result.is_ok() && backend.builds() {
        // 前回の記録に**重ねて**書く。今回のグラフに無かった成果物は、この実行が
        // 触れていないだけで、依然として記録どおりのコマンドの産物である
        // （issue #69）。
        let mut log = CommandLog::load(&g.build_dir);
        log.absorb(&CommandLog::of(g));
        log.save(&g.build_dir);
    }
    result
}

/// 道具の刻印を書く（ADR-0055）。
///
/// **中身が変わったときだけ書く。** 書き直せば mtime が動き、その道具を
/// 使うすべてのアクションが組み直される——変わっていないのに書けば、
/// 毎回すべてが組み直される。
///
/// 走らせる前である必要がある。刻印はステップの入力であり、無ければ ninja は
/// 「作る規則が無い」と言う。
pub fn write_tool_stamps(g: &BuildGraph) {
    for (path, identity) in &g.tool_stamps {
        if std::fs::read_to_string(path).ok().as_deref() == Some(identity.as_str()) {
            continue;
        }
        if let Some(parent) = path.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                continue;
            }
        }
        if std::fs::write(path, identity).is_err() {
            log_debug!("cannot write the tool stamp {}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toolstyle::Deps;

    #[test]
    fn every_listed_backend_resolves_to_one_with_that_name() {
        for name in NAMES {
            let b = find(name).unwrap_or_else(|| panic!("`{name}` is listed but does not resolve"));
            assert_eq!(b.name(), *name);
        }
        assert!(find("cmake").is_none());
    }

    #[test]
    fn only_the_graph_backend_declines_to_build() {
        for name in NAMES {
            let b = find(name).unwrap();
            assert_eq!(b.builds(), *name != "graph", "`{name}` reports the wrong kind");
        }
    }

    #[test]
    fn a_stamp_is_rewritten_only_when_the_identity_changes() {
        // 変わっていないのに書き直すと mtime が動き、その道具を使うすべての
        // アクションが毎回組み直される——刻印を入れた目的の逆になる。
        let dir = std::env::temp_dir().join("dowel-tool-stamp-test");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("tools/cc-0.stamp");
        let graph = |identity: &str| BuildGraph {
            build_dir: dir.clone(),
            steps: vec![],
            artifacts: vec![],
            deps: Deps::Depfile,
            default_outputs: vec![],
            tool_stamps: vec![(path.clone(), identity.to_string())],
        };

        write_tool_stamps(&graph("cc:1:1"));
        let first = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "cc:1:1");

        // 同じ同一性では触らない。時刻の分解能に頼らず、書き込みの有無を
        // 中身の差し替えで見る。
        std::fs::write(&path, "cc:1:1").unwrap();
        let marker = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert!(marker >= first);
        write_tool_stamps(&graph("cc:1:1"));
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), marker);

        write_tool_stamps(&graph("cc:2:2"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "cc:2:2");
    }

    #[test]
    fn an_unknown_backend_names_the_ones_that_exist() {
        let mut probe =
            crate::probe::Prober::in_dir(std::env::temp_dir().join("dowel-backend-test-facts"));
        let e =
            select(Some("bazel"), &mut probe).err().expect("an unknown backend must be refused");
        assert!(e.contains("ninja"), "{e}");
        assert!(e.contains("bazel"), "{e}");
    }

    fn step(id: usize, deps: Vec<usize>) -> Step {
        Step {
            id,
            kind: ActionKind::Compile,
            target: "p:t".into(),
            description: format!("step {id}"),
            program: "cc".into(),
            arguments: vec![],
            inputs: vec![],
            outputs: vec![PathBuf::from(format!("{id}.o"))],
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
    fn ordering_puts_dependencies_first_even_when_the_document_does_not() {
        // 依存が後ろに書かれた文書。読み込んだ側は並べ直す。
        let g = graph_of(vec![step(0, vec![1]), step(1, vec![])]);
        assert_eq!(g.order(), vec![1, 0]);
    }

    #[test]
    fn a_cycle_still_lists_every_step() {
        let g = graph_of(vec![step(0, vec![1]), step(1, vec![0])]);
        let mut o = g.order();
        o.sort();
        assert_eq!(o, vec![0, 1]);
    }
}

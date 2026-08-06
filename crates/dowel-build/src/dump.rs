//! アクショングラフの書き出し。
//!
//! 依存グラフ（`dowel_model::dump`）が「何に依存しているか」を示すのに対し、
//! こちらは「実際に何を起動するか」を示す。ビルドが期待と違う挙動をしたとき、
//! 最初に見る場所になる。
//!
//! JSON はここで別に組み立てない。バックエンドが受け取るのと同じ文書
//! （`backend::graph`）を出す。アクショングラフの JSON 表現が2つあると、
//! 読む側と走る側が黙ってずれる（ADR-0018）。

use crate::action::ActionKind;
use crate::backend::BuildGraph;
use crate::plan::Plan;
use dowel_model::Session;
use std::path::Path;

pub fn text(sess: &Session, plan: &Plan) -> String {
    let mut out = String::new();
    out.push_str(&format!("build directory: {}\n\n", plan.build_dir.display()));
    for a in &plan.actions {
        out.push_str(&format!("[{}] {} ({})\n", a.id.0, a.description, sess.label(a.target)));
        for i in &a.inputs {
            out.push_str(&format!("  in  {}\n", rel(&plan.build_dir, i)));
        }
        for o in &a.outputs {
            out.push_str(&format!("  out {}\n", rel(&plan.build_dir, o)));
        }
        out.push_str(&format!("  $ {}\n\n", a.command_line()));
    }
    if plan.actions.is_empty() {
        out.push_str("(no actions)\n");
    }
    out
}

pub fn dot(sess: &Session, plan: &Plan) -> String {
    let mut out = String::from("digraph actions {\n  rankdir=LR;\n  node [shape=box];\n");
    for a in &plan.actions {
        let shape = match a.kind {
            ActionKind::Compile => "box",
            ActionKind::Archive => "folder",
            ActionKind::Link => "component",
            ActionKind::Transform => "note",
        };
        out.push_str(&format!(
            "  a{} [shape={shape},label=\"{}\\n{}\"];\n",
            a.id.0,
            a.description.replace('"', "'"),
            sess.label(a.target)
        ));
    }
    for a in &plan.actions {
        for d in &a.deps {
            out.push_str(&format!("  a{} -> a{};\n", d.0, a.id.0));
        }
    }
    out.push_str("}\n");
    out
}

pub fn json(sess: &Session, plan: &Plan) -> String {
    crate::backend::graph::render(&BuildGraph::of(sess, plan))
}

fn rel(base: &Path, p: &Path) -> String {
    p.strip_prefix(base).unwrap_or(p).display().to_string()
}

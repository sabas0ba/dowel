//! アクショングラフの書き出し。
//!
//! 依存グラフ（`dowel_model::dump`）が「何に依存しているか」を示すのに対し、
//! こちらは「実際に何を起動するか」を示す。ビルドが期待と違う挙動をしたとき、
//! 最初に見る場所になる。

use crate::action::ActionKind;
use crate::plan::Plan;
use dowel_model::Session;
use dowel_support::json::JsonWriter;
use std::path::Path;

pub fn text(sess: &Session, plan: &Plan) -> String {
    let mut out = String::new();
    out.push_str(&format!("ビルドディレクトリ: {}\n\n", plan.build_dir.display()));
    for a in &plan.actions {
        out.push_str(&format!("[{}] {} ({})\n", a.id.0, a.description, sess.label(a.target)));
        for i in &a.inputs {
            out.push_str(&format!("  入力 {}\n", rel(&plan.build_dir, i)));
        }
        for o in &a.outputs {
            out.push_str(&format!("  出力 {}\n", rel(&plan.build_dir, o)));
        }
        out.push_str(&format!("  $ {}\n\n", a.command_line()));
    }
    if plan.actions.is_empty() {
        out.push_str("（アクションがない）\n");
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
    let mut w = JsonWriter::pretty();
    w.begin_object();
    w.field_str("build_dir", &plan.build_dir.display().to_string());
    w.key("actions").begin_array();
    for a in &plan.actions {
        w.begin_object();
        w.field_u64("id", a.id.0 as u64);
        w.field_str("kind", a.kind.name());
        w.field_str("target", &sess.label(a.target));
        w.field_str("description", &a.description);
        w.field_strs("command", a.command().iter().map(|s| s.as_str()));
        w.field_strs("inputs", a.inputs.iter().map(|p| p.to_str().unwrap_or("")));
        w.field_strs("outputs", a.outputs.iter().map(|p| p.to_str().unwrap_or("")));
        w.key("deps").begin_array();
        for d in &a.deps {
            w.u64(d.0 as u64);
        }
        w.end_array();
        w.end_object();
    }
    w.end_array();
    w.key("artifacts").begin_object();
    for (t, p) in &plan.artifacts {
        w.field_str(&sess.label(*t), &p.display().to_string());
    }
    w.end_object();
    w.end_object();
    w.finish()
}

fn rel(base: &Path, p: &Path) -> String {
    p.strip_prefix(base).unwrap_or(p).display().to_string()
}

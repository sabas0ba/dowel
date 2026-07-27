//! 依存グラフの書き出し。
//!
//! 「動かしながら中を見る」ための出口である。ログに流すには大きいが、
//! 実行のたびに欲しくなるものをここに集める。
//! stdout に出す（ログは stderr）ため、そのまま `dot -Tsvg` へ流せる。

use crate::graph::Graph;
use crate::session::Session;
use dowel_eval::schema::Block;
use dowel_support::json::JsonWriter;

/// 人間が端末で読む形。
pub fn text(sess: &Session, graph: &Graph) -> String {
    let mut out = String::new();
    for &tid in &graph.order {
        let t = sess.target(tid);
        out.push_str(&format!("{} [{}]\n", sess.label(tid), t.kind.name()));
        for e in graph.deps_of(tid) {
            out.push_str(&format!(
                "  → {}  ({})\n",
                sess.label(e.to),
                if e.block == Block::Public { "public" } else { "private" }
            ));
        }
    }
    if graph.order.is_empty() {
        out.push_str("(no targets)\n");
    }
    out
}

/// Graphviz。`dowel graph --format=dot | dot -Tsvg -o graph.svg`
pub fn dot(sess: &Session, graph: &Graph) -> String {
    let mut out = String::from("digraph dowel {\n  rankdir=LR;\n  node [shape=box];\n");
    for &tid in &graph.order {
        let t = sess.target(tid);
        out.push_str(&format!(
            "  {} [label=\"{}\\n{}\"];\n",
            quote(&sess.label(tid)),
            sess.label(tid),
            t.kind.name()
        ));
    }
    for &tid in &graph.order {
        for e in graph.deps_of(tid) {
            // 伝播する依存を実線、しない依存を破線にする。
            let style = if e.block == Block::Public { "solid" } else { "dashed" };
            out.push_str(&format!(
                "  {} -> {} [style={style}];\n",
                quote(&sess.label(tid)),
                quote(&sess.label(e.to))
            ));
        }
    }
    out.push_str("}\n");
    out
}

/// 機械可読な形。差分を取る用途と、エージェントに渡す用途。
pub fn json(sess: &Session, graph: &Graph) -> String {
    let mut w = JsonWriter::pretty();
    w.begin_object();
    w.key("packages").begin_array();
    for p in &sess.packages {
        w.begin_object();
        w.field_str("name", &p.name);
        w.field_str("version", &p.version);
        w.field_str("root", &p.root.display().to_string());
        w.end_object();
    }
    w.end_array();
    w.key("targets").begin_array();
    for &tid in &graph.order {
        let t = sess.target(tid);
        w.begin_object();
        w.field_str("label", &sess.label(tid));
        w.field_str("kind", t.kind.name());
        w.field_str("package", &sess.package(t.package).name);
        w.key("deps").begin_array();
        for e in graph.deps_of(tid) {
            w.begin_object();
            w.field_str("target", &sess.label(e.to));
            w.field_str("block", if e.block == Block::Public { "public" } else { "private" });
            w.field_str("site", &sess.sm.location(e.site.file, e.site.span));
            w.end_object();
        }
        w.end_array();
        w.end_object();
    }
    w.end_array();
    w.end_object();
    w.finish()
}

fn quote(s: &str) -> String {
    format!("{:?}", s)
}

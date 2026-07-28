//! `dowel why` — 値がそこへ来た経路の表示。
//!
//! 来歴チェーンは値が持つ鎖をそのまま辿ったものであり、専用のデータ構造を
//! 持たない（docs/10-manifest.md 5節）。

use crate::graph::Graph;
use crate::interface;
use crate::session::Session;
use crate::target::TargetId;
use dowel_eval::schema::{self, Block};
use dowel_eval::{Config, Data, Value};
use dowel_support::json::JsonWriter;
use dowel_support::Diagnostic;

pub struct Explanation {
    pub target: String,
    pub prop: String,
    pub ty: String,
    pub merge: &'static str,
    pub items: Vec<Item>,
}

pub struct Item {
    pub value: String,
    pub ty: String,
    /// 宣言された地点から到達した地点への順。
    pub steps: Vec<Step>,
}

pub struct Step {
    pub origin: String,
    pub location: Option<String>,
}

pub fn explain(
    sess: &Session,
    graph: &Graph,
    tid: TargetId,
    prop: &str,
    cfg: &Config,
) -> Result<Explanation, String> {
    let Some(def) =
        schema::lookup(Block::Public, prop).or_else(|| schema::lookup(Block::Root, prop))
    else {
        let mut known = schema::prop_names(Block::Public);
        known.extend(schema::prop_names(Block::Root));
        return Err(format!("unknown property `{prop}`. available: {}", known.join(", ")));
    };

    let mut diags: Vec<Diagnostic> = Vec::new();
    let env = interface::compile_env_fresh(sess, graph, tid, cfg, &mut diags);
    let Some(value) = env.get(prop) else {
        return Ok(Explanation {
            target: sess.label(tid),
            prop: prop.to_string(),
            ty: def.ty.display(),
            merge: def.merge.name(),
            items: Vec::new(),
        });
    };

    let mut items = Vec::new();
    for (rendered, v) in elements(value) {
        items.push(Item { value: rendered, ty: v.ty.display(), steps: steps_of(sess, &v) });
    }

    Ok(Explanation {
        target: sess.label(tid),
        prop: prop.to_string(),
        ty: def.ty.display(),
        merge: def.merge.name(),
        items,
    })
}

/// 併合結果を1要素ずつに分ける。表示文字列は要素の形に応じて作る。
/// 写像の要素は `キー = 値` として見せる。来歴は値側に付いている。
fn elements(value: &Value) -> Vec<(String, Value)> {
    match &value.data {
        Data::List(items) => items.iter().map(|v| (v.display(), v.clone())).collect(),
        Data::Map(m) => {
            m.iter().map(|(k, v)| (format!("{k} = {}", v.display()), v.clone())).collect()
        }
        _ => vec![(value.display(), value.clone())],
    }
}

fn steps_of(sess: &Session, v: &Value) -> Vec<Step> {
    // 鎖は「直近が先」で返るため、宣言側から読めるよう反転する。
    v.prov
        .chain()
        .into_iter()
        .rev()
        .map(|(origin, site)| Step {
            origin: origin.display(),
            location: site.map(|s| sess.sm.location(s.file, s.span)),
        })
        .collect()
}

pub fn render_text(e: &Explanation) -> String {
    let mut out = String::new();
    out.push_str(&format!("{} of {}  ({}, merge = {})\n\n", e.prop, e.target, e.ty, e.merge));
    if e.items.is_empty() {
        out.push_str("  (no values reached this target)\n");
        return out;
    }
    for item in &e.items {
        out.push_str(&format!("{:<40} {}\n", item.value, item.ty));
        for (i, step) in item.steps.iter().enumerate() {
            let indent = "  ".repeat(i + 1);
            let loc = step.location.clone().unwrap_or_default();
            out.push_str(&format!("{indent}← {:<38} {loc}\n", step.origin));
        }
        out.push('\n');
    }
    out
}

pub fn render_json(e: &Explanation) -> String {
    let mut w = JsonWriter::pretty();
    w.begin_object();
    w.field_str("target", &e.target);
    w.field_str("property", &e.prop);
    w.field_str("type", &e.ty);
    w.field_str("merge", e.merge);
    w.key("items").begin_array();
    for item in &e.items {
        w.begin_object();
        w.field_str("value", &item.value);
        w.field_str("type", &item.ty);
        w.key("provenance").begin_array();
        for step in &item.steps {
            w.begin_object();
            w.field_str("origin", &step.origin);
            match &step.location {
                Some(l) => w.field_str("location", l),
                None => w.key("location").null(),
            };
            w.end_object();
        }
        w.end_array();
        w.end_object();
    }
    w.end_array();
    w.end_object();
    w.finish()
}

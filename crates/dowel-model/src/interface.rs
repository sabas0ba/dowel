//! インタフェース併合。
//!
//! ターゲットが**外へ供給する**プロパティ（`interface`）と、
//! **自身のコンパイルに効く**プロパティ（`compile_env`）を区別する。
//!
//! - `interface(T)` = T の `public` ＋ T の `public.deps` の `interface`
//! - `compile_env(T)` = T の `public` ＋ T の `private` ＋ 全依存の `interface`
//!
//! この2つの差が `public` / `private` の意味である。`private` で宣言した依存は
//! 自分のコンパイルには効くが、自分を使う側には見えない。
//!
//! 順序は「自分が先、依存が後」とする。インクルード探索でもリンク順でも、
//! 依存元のものが先に来るのが期待される挙動であるため。

use crate::graph::Graph;
use crate::session::Session;
use crate::target::{PropMap, TargetId};
use dowel_eval::schema::{self, Block, PropDef};
use dowel_eval::value::Origin;
use dowel_eval::{Config, Data, Value};
use dowel_support::{log_trace, Diagnostic};
use std::collections::BTreeMap;

#[derive(Default)]
pub struct Interfaces {
    map: BTreeMap<TargetId, PropMap>,
}

impl Interfaces {
    pub fn get(&self, t: TargetId) -> Option<&PropMap> {
        self.map.get(&t)
    }

    pub fn prop(&self, t: TargetId, name: &str) -> Option<&Value> {
        self.map.get(&t).and_then(|m| m.get(name))
    }
}

/// 全ターゲットの公開インタフェースを求める。
///
/// `graph.order` は依存が先に並んでいるため、1回の走査で足りる。
pub fn compute(sess: &Session, graph: &Graph, cfg: &Config) -> (Interfaces, Vec<Diagnostic>) {
    let _phase = dowel_support::log::Phase::start("interface");
    let mut ifaces = Interfaces::default();
    let mut diags = Vec::new();

    for &tid in &graph.order {
        let target = sess.target(tid);
        let mut props = PropMap::new();
        for def in schema::block_props() {
            let mut reached: Vec<Value> = Vec::new();
            if let Some(v) = target.public.get(def.name) {
                if let Some(v) = dowel_eval::specialize(v, cfg) {
                    reached.push(v);
                }
            }
            for edge in graph.public_deps_of(tid) {
                if let Some(v) = ifaces.prop(edge.to, def.name) {
                    reached.push(tag_propagated(v, &sess.label(edge.to), def.name));
                }
            }
            if reached.is_empty() {
                continue;
            }
            let merged = schema::merge_values(&def, &reached, &sess.sm, &mut diags);
            // 併合は「どの値が到達したか」を見ないと結果を説明できない。
            // `dowel why` は1つの値を掘るが、こちらは全体を並べて見せる。
            log_trace!(
                "  merge {}.{} ({}): {} reached -> {}",
                sess.label(tid),
                def.name,
                def.merge.name(),
                reached.len(),
                merged.display()
            );
            props.insert(def.name.to_string(), merged);
        }
        log_trace!(
            "interface({}) = {}",
            sess.label(tid),
            props.keys().cloned().collect::<Vec<_>>().join(", ")
        );
        ifaces.map.insert(tid, props);
    }

    (ifaces, diags)
}

/// 1つのターゲットをコンパイルするために効くプロパティ。
pub fn compile_env(
    sess: &Session,
    graph: &Graph,
    ifaces: &Interfaces,
    tid: TargetId,
    cfg: &Config,
    diags: &mut Vec<Diagnostic>,
) -> PropMap {
    let target = sess.target(tid);
    let mut props = PropMap::new();
    for def in schema::block_props() {
        let mut reached: Vec<Value> = Vec::new();
        for block in [Block::Public, Block::Private] {
            if let Some(v) = target.props(block).get(def.name) {
                if let Some(v) = dowel_eval::specialize(v, cfg) {
                    reached.push(v);
                }
            }
        }
        // 依存は宣言順。public と private の双方を取り込む。
        for edge in graph.deps_of(tid) {
            if let Some(v) = ifaces.prop(edge.to, def.name) {
                reached.push(tag_propagated(v, &sess.label(edge.to), def.name));
            }
        }
        if reached.is_empty() {
            continue;
        }
        let merged = schema::merge_values(&def, &reached, &sess.sm, diags);
        log_trace!(
            "  compile_env {}.{} ({}): {} reached -> {}",
            sess.label(tid),
            def.name,
            def.merge.name(),
            reached.len(),
            merged.display()
        );
        props.insert(def.name.to_string(), merged);
    }
    props
}

/// 伝播してきた値であることを来歴に刻む。`dowel why` の1段になる。
fn tag_propagated(value: &Value, from: &str, prop: &str) -> Value {
    let origin = || Origin::Propagated { from: from.to_string(), prop: format!("public.{prop}") };
    match &value.data {
        Data::List(items) => Value {
            ty: value.ty.clone(),
            data: Data::List(
                items
                    .iter()
                    .map(|v| Value {
                        prov: v.prov.then(origin(), v.prov.nearest_site()),
                        ..v.clone()
                    })
                    .collect(),
            ),
            prov: value.prov.then(origin(), value.prov.nearest_site()),
        },
        Data::Map(m) => Value {
            ty: value.ty.clone(),
            data: Data::Map(
                m.iter()
                    .map(|(k, v)| {
                        (
                            k.clone(),
                            Value {
                                prov: v.prov.then(origin(), v.prov.nearest_site()),
                                ..v.clone()
                            },
                        )
                    })
                    .collect(),
            ),
            prov: value.prov.then(origin(), value.prov.nearest_site()),
        },
        _ => Value { prov: value.prov.then(origin(), value.prov.nearest_site()), ..value.clone() },
    }
}

/// プロパティ定義を名前から引く。CLI が `why` で使う。
pub fn prop_def(name: &str) -> Option<PropDef> {
    schema::lookup(Block::Public, name).or_else(|| schema::lookup(Block::Root, name))
}

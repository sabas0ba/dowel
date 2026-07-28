//! インタフェース併合。
//!
//! ターゲットが外へ供給するプロパティ（`interface`）と、
//! 自身のコンパイルに効くプロパティ（`compile_env`）を区別する。
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
use dowel_support::Diagnostic;
use std::collections::BTreeMap;

/// 派生クエリの入力を渡し、インタフェース段の診断を集める。
///
/// 併合そのものは [`crate::query::interface`] にある。値を持ち回る器は返さない。
/// 依存のインタフェースはクエリが自身で辿るため、下流へ配る必要がない。
///
/// 中身の変わらない編集では併合が再計算されない（early cutoff）。
pub fn prepare(sess: &Session, graph: &Graph, cfg: &Config) -> Vec<Diagnostic> {
    let _phase = dowel_support::log::Phase::start("interface");
    sess.declare_derivations(cfg, graph);

    let mut diags = Vec::new();
    // `graph.order` は依存が先。クエリは自身で依存を辿るため順序を要さないが、
    // 診断の並びを読み込み順に揃えるためこの順で問い合わせる。
    for &tid in &graph.order {
        diags.extend(sess.interface_of(tid).diagnostics.iter().cloned());
    }
    diags
}

/// 1つのターゲットをコンパイルするために効くプロパティ。
///
/// `ifaces` は受け取らない。依存のインタフェースはクエリが自身で辿る。
pub fn compile_env(sess: &Session, tid: TargetId, diags: &mut Vec<Diagnostic>) -> PropMap {
    let merged = sess.compile_env_of(tid);
    diags.extend(merged.diagnostics.iter().cloned());
    merged.props.clone()
}

/// メモを経由せずに併合し直す。
///
/// 派生のメモは early cutoff の対象であり、再利用された値の来歴が持つスパンは
/// 直前の編集を反映していないことがある。下流が来歴から読むのは宣言位置の
/// ファイルだけであり、それは要約に含まれるため一致する。位置そのものを
/// 表示する経路（`dowel why`）だけがここを通る。
pub fn compile_env_fresh(
    sess: &Session,
    graph: &Graph,
    tid: TargetId,
    cfg: &Config,
    diags: &mut Vec<Diagnostic>,
) -> PropMap {
    // 依存が先に並んでいるため、1回の走査でインタフェースが揃う。
    let mut ifaces: BTreeMap<TargetId, PropMap> = BTreeMap::new();
    for &dep in &graph.order {
        let own = [sess.target(dep).public.clone()];
        let deps: Vec<TargetId> = graph.public_deps_of(dep).map(|e| e.to).collect();
        ifaces.insert(dep, merge_block(sess, cfg, &own, &deps, &ifaces, diags));
    }
    let t = sess.target(tid);
    let own = [t.public.clone(), t.private.clone()];
    let deps: Vec<TargetId> = graph.deps_of(tid).iter().map(|e| e.to).collect();
    merge_block(sess, cfg, &own, &deps, &ifaces, diags)
}

/// 宣言された値と依存のインタフェースを、プロパティごとに併合する。
fn merge_block(
    sess: &Session,
    cfg: &Config,
    own: &[PropMap],
    deps: &[TargetId],
    ifaces: &BTreeMap<TargetId, PropMap>,
    diags: &mut Vec<Diagnostic>,
) -> PropMap {
    let mut props = PropMap::new();
    for def in schema::block_props() {
        let mut reached: Vec<Value> = Vec::new();
        for block in own {
            if let Some(v) = block.get(def.name) {
                if let Some(v) = dowel_eval::specialize(v, cfg) {
                    reached.push(v);
                }
            }
        }
        for &dep in deps {
            if let Some(v) = ifaces.get(&dep).and_then(|m| m.get(def.name)) {
                reached.push(tag_propagated(v, &sess.label(dep), def.name));
            }
        }
        if reached.is_empty() {
            continue;
        }
        props.insert(def.name.to_string(), schema::merge_values(&def, &reached, diags));
    }
    props
}

/// 伝播してきた値であることを来歴に刻む。`dowel why` の1段になる。
pub(crate) fn tag_propagated(value: &Value, from: &str, prop: &str) -> Value {
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

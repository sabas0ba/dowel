//! 依存グラフ。
//!
//! 辺は具体化後に決まる。`deps = [dep("zlib") when feature.zlib]` は
//! 機能フラグによって辺が現れたり消えたりするため、構成なしにグラフは定まらない。

use crate::session::Session;
use crate::target::{PackageId, TargetId};
use dowel_eval::schema::{Block, TableKind};
use dowel_eval::{Config, Data, Site, Value};
use dowel_support::diag::closest;
use dowel_support::{log_debug, log_trace, Diagnostic};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct Edge {
    pub to: TargetId,
    /// どちらのブロックで宣言されたか。`public` の依存は伝播する
    pub block: Block,
    pub site: Site,
}

pub struct Graph {
    pub edges: BTreeMap<TargetId, Vec<Edge>>,
    /// 依存が先、依存元が後。インタフェース併合はこの順に計算する
    pub order: Vec<TargetId>,
}

impl Graph {
    pub fn deps_of(&self, t: TargetId) -> &[Edge] {
        self.edges.get(&t).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// 伝播する依存（`public` ブロックで宣言されたもの）のみ。
    pub fn public_deps_of(&self, t: TargetId) -> impl Iterator<Item = &Edge> {
        self.deps_of(t).iter().filter(|e| e.block == Block::Public)
    }

    /// リンクに必要な推移閉包。順序は「依存元が先」。
    pub fn link_closure(&self, root: TargetId) -> Vec<TargetId> {
        let mut out = Vec::new();
        let mut stack = vec![root];
        let mut seen = std::collections::BTreeSet::new();
        while let Some(t) = stack.pop() {
            if !seen.insert(t) {
                continue;
            }
            out.push(t);
            // 逆順に積むことで宣言順を保つ。
            for e in self.deps_of(t).iter().rev() {
                stack.push(e.to);
            }
        }
        out
    }
}

pub fn build(sess: &Session, cfg: &Config) -> (Graph, Vec<Diagnostic>) {
    let _phase = dowel_support::log::Phase::start("graph");
    let mut diags = Vec::new();
    let mut edges: BTreeMap<TargetId, Vec<Edge>> = BTreeMap::new();

    for target in &sess.targets {
        let mut out = Vec::new();
        log_trace!("resolving deps of {}", sess.label(target.id));
        for block in [Block::Public, Block::Private] {
            let Some(value) = target.props(block).get("deps") else { continue };
            let Some(value) = dowel_eval::specialize(value, cfg) else { continue };
            for item in items_of(&value) {
                match &item.data {
                    Data::Target(name) => match resolve_target(sess, target.package, name) {
                        Some(to) => out.push(Edge {
                            to,
                            block,
                            site: item.prov.nearest_site().unwrap_or(target.site),
                        }),
                        None => diags.push(unknown_target(sess, target.package, name, &item)),
                    },
                    Data::Dep(name) => {
                        let found = resolve_package_targets(sess, target.package, name);
                        match found {
                            Ok(targets) if targets.is_empty() => diags.push(
                                Diagnostic::error(
                                    "empty-dependency",
                                    format!("dependency `{name}` has no lib target"),
                                )
                                .with_label(label_at(&item, "this dependency supplies nothing")),
                            ),
                            Ok(targets) => {
                                for to in targets {
                                    out.push(Edge {
                                        to,
                                        block,
                                        site: item.prov.nearest_site().unwrap_or(target.site),
                                    });
                                }
                            }
                            Err(Unresolved::NotDeclared) => {
                                diags.push(undeclared_dep(sess, target.package, name, &item))
                            }
                            Err(Unresolved::Inactive) => diags.push(
                                Diagnostic::error(
                                    "inactive-dependency",
                                    format!("`{name}` is optional and its feature is not enabled"),
                                )
                                .with_label(label_at(
                                    &item,
                                    "this dependency is not part of the current configuration",
                                ))
                                .note(format!(
                                    "write `dep(\"{name}\") when feature.{name}` so the reference \
                                     appears with the feature"
                                ))
                                .note(format!(
                                    "or enable it with `--features={name}`, or drop `optional` \
                                     from its declaration"
                                )),
                            ),
                            // 供給形態が未実装なものは `dowel.toml` の読み取りで
                            // 既に診断済み。同じことを2度言わない。
                            Err(Unresolved::AlreadyReported) => {}
                        }
                    }
                    Data::Error => {}
                    _ => diags.push(
                        Diagnostic::error(
                            "invalid-dependency",
                            format!(
                                "element of `deps` is not a dependency reference: {}",
                                item.display()
                            ),
                        )
                        .with_label(label_at(&item, "write `dep(\"...\")` or `target(\"...\")`")),
                    ),
                }
            }
        }
        if !out.is_empty() {
            log_trace!(
                "{} → {}",
                sess.label(target.id),
                out.iter().map(|e| sess.label(e.to)).collect::<Vec<_>>().join(", ")
            );
        }
        edges.insert(target.id, out);
    }

    let (order, cycle_diags) = topological_order(sess, &edges);
    log_trace!(
        "topological order: {}",
        order.iter().map(|t| sess.label(*t)).collect::<Vec<_>>().join(" < ")
    );
    diags.extend(cycle_diags);

    log_debug!(
        "dependency graph: {} nodes, {} edges",
        edges.len(),
        edges.values().map(|v| v.len()).sum::<usize>()
    );

    (Graph { edges, order }, diags)
}

fn items_of(value: &Value) -> Vec<Value> {
    match &value.data {
        Data::List(items) => items.clone(),
        Data::Error => Vec::new(),
        _ => vec![value.clone()],
    }
}

fn label_at(item: &Value, msg: &str) -> dowel_support::Label {
    match item.prov.nearest_site() {
        Some(s) => dowel_support::Label::primary(s.file, s.span, msg.to_string()),
        None => dowel_support::Label::primary(
            dowel_support::FileId(0),
            dowel_support::Span::EMPTY,
            msg.to_string(),
        ),
    }
}

fn resolve_target(sess: &Session, pkg: PackageId, name: &str) -> Option<TargetId> {
    sess.targets.iter().find(|t| t.package == pkg && t.name == name).map(|t| t.id)
}

enum Unresolved {
    NotDeclared,
    /// 任意の依存で、対応する機能が有効でない。読み込んでいないため解決できない
    Inactive,
    AlreadyReported,
}

fn resolve_package_targets(
    sess: &Session,
    from: PackageId,
    dep_name: &str,
) -> Result<Vec<TargetId>, Unresolved> {
    let Some(dep) = sess.package(from).deps.iter().find(|d| d.name == dep_name) else {
        return Err(Unresolved::NotDeclared);
    };
    // 有効でない任意の依存は読み込んでいない。解決できないのは
    // 供給形態が未実装だからではないため、区別して報告する。
    if !crate::package::is_active(dep, sess.active_features()) {
        return Err(Unresolved::Inactive);
    }
    let Some(pid) = sess.dep_package(from, dep_name) else {
        return Err(Unresolved::AlreadyReported);
    };
    Ok(sess
        .targets
        .iter()
        .filter(|t| t.package == pid && t.kind == TableKind::Lib)
        .map(|t| t.id)
        .collect())
}

fn unknown_target(sess: &Session, pkg: PackageId, name: &str, item: &Value) -> Diagnostic {
    let names: Vec<&str> =
        sess.targets.iter().filter(|t| t.package == pkg).map(|t| t.name.as_str()).collect();
    let mut d = Diagnostic::error("unknown-target", format!("no target named `{name}`"))
        .with_label(label_at(item, "no target with this name in the same package"))
        .note(format!(
            "targets in this package: {}",
            if names.is_empty() { "(none)".to_string() } else { names.join(", ") }
        ));
    if let Some(c) = closest(name, names.iter().copied()) {
        if let Some(s) = item.prov.nearest_site() {
            d = d.suggest(s.file, s.span, format!("target({c:?})"), format!("did you mean `{c}`?"));
        }
    }
    d
}

fn undeclared_dep(sess: &Session, pkg: PackageId, name: &str, item: &Value) -> Diagnostic {
    let names: Vec<&str> = sess.package(pkg).deps.iter().map(|d| d.name.as_str()).collect();
    let mut d = Diagnostic::error(
        "undeclared-dependency",
        format!("dependency `{name}` is not declared in `dowel.toml`"),
    )
    .with_label(label_at(item, "undeclared dependency"))
    .note(format!(
        "declared in `dowel.toml`: {}",
        if names.is_empty() { "(none)".to_string() } else { names.join(", ") }
    ))
    .note("use `target(\"...\")` to refer to a target in the same package");
    if let Some(c) = closest(name, names.iter().copied()) {
        if let Some(s) = item.prov.nearest_site() {
            d = d.suggest(s.file, s.span, format!("dep({c:?})"), format!("did you mean `{c}`?"));
        }
    }
    d
}

/// 深さ優先による順序づけ。閉路は診断にして、その辺を無視して続行する。
fn topological_order(
    sess: &Session,
    edges: &BTreeMap<TargetId, Vec<Edge>>,
) -> (Vec<TargetId>, Vec<Diagnostic>) {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        None,
        InProgress,
        Done,
    }
    let mut marks: BTreeMap<TargetId, Mark> = edges.keys().map(|k| (*k, Mark::None)).collect();
    let mut order = Vec::new();
    let mut diags = Vec::new();
    let mut path: Vec<TargetId> = Vec::new();

    // 反復による深さ優先。深い依存でスタックを溢れさせないため。
    for &root in edges.keys() {
        if marks.get(&root) != Some(&Mark::None) {
            continue;
        }
        let mut stack: Vec<(TargetId, usize)> = vec![(root, 0)];
        marks.insert(root, Mark::InProgress);
        path.push(root);
        while let Some((node, idx)) = stack.pop() {
            let deps = edges.get(&node).map(|v| v.as_slice()).unwrap_or(&[]);
            if idx < deps.len() {
                stack.push((node, idx + 1));
                let next = deps[idx].to;
                match marks.get(&next).copied().unwrap_or(Mark::None) {
                    Mark::None => {
                        marks.insert(next, Mark::InProgress);
                        path.push(next);
                        stack.push((next, 0));
                    }
                    Mark::InProgress => {
                        diags.push(cycle_diagnostic(sess, &path, next, deps[idx].site));
                    }
                    Mark::Done => {}
                }
            } else {
                marks.insert(node, Mark::Done);
                path.pop();
                order.push(node);
            }
        }
    }
    (order, diags)
}

fn cycle_diagnostic(
    sess: &Session,
    path: &[TargetId],
    back_to: TargetId,
    site: Site,
) -> Diagnostic {
    let start = path.iter().position(|t| *t == back_to).unwrap_or(0);
    let mut chain: Vec<String> = path[start..].iter().map(|t| sess.label(*t)).collect();
    chain.push(sess.label(back_to));
    Diagnostic::error("dependency-cycle", "dependency cycle")
        .at(site.file, site.span, "this edge closes the cycle")
        .note(chain.join(" → "))
        .note("like templates, cycles in the dependency graph are detected statically and fail")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_closure_preserves_declaration_order() {
        let mut edges = BTreeMap::new();
        let site = Site::new(dowel_support::FileId(0), dowel_support::Span::EMPTY);
        edges.insert(
            TargetId(0),
            vec![
                Edge { to: TargetId(1), block: Block::Private, site },
                Edge { to: TargetId(2), block: Block::Private, site },
            ],
        );
        edges.insert(TargetId(1), vec![Edge { to: TargetId(3), block: Block::Public, site }]);
        edges.insert(TargetId(2), vec![]);
        edges.insert(TargetId(3), vec![]);
        let g = Graph { edges, order: Vec::new() };
        assert_eq!(
            g.link_closure(TargetId(0)),
            vec![TargetId(0), TargetId(1), TargetId(3), TargetId(2)]
        );
    }
}

//! 構成による具体化。
//!
//! `Cfg<T>` を `T` に落とす段階である。マニフェスト評価とは分離されており、
//! `--release` や `--target` の切り替えではこの段階だけをやり直す
//! （docs/10-manifest.md 3節）。

use crate::config::{CfgValue, Config};
use crate::value::{Data, Origin, Pattern, Pred, Value};
use dowel_support::log_trace;

/// 構成を与えて条件を解決する。
///
/// 述語が成立しない値は `None` になる。列の要素として現れた場合は取り除かれる。
pub fn specialize(value: &Value, cfg: &Config) -> Option<Value> {
    match &value.data {
        Data::When { pred, inner } => {
            if !eval_pred(pred, cfg) {
                log_trace!("  when {} is false, dropping {}", pred.display(), inner.display());
                return None;
            }
            let inner = specialize(inner, cfg)?;
            Some(Value {
                ty: inner.ty.clone(),
                data: inner.data.clone(),
                prov: inner.prov.then(Origin::WhenTrue(pred.display()), value.prov.site()),
            })
        }
        Data::Match { scrutinee, arms } => {
            let actual = cfg.lookup(scrutinee)?;
            let actual = actual.display();
            let arm = arms
                .iter()
                .find(|a| matches!(&a.pattern, Pattern::Value(v) if *v == actual))
                .or_else(|| arms.iter().find(|a| a.pattern == Pattern::Wildcard))?;
            log_trace!(
                "  match {} == {actual:?} -> arm {}",
                scrutinee.display(),
                arm.pattern.display()
            );
            let chosen = specialize(&arm.value, cfg)?;
            Some(Value {
                ty: chosen.ty.clone(),
                data: chosen.data.clone(),
                prov: chosen.prov.then(Origin::MatchArm(arm.pattern.display()), Some(arm.site)),
            })
        }
        // パッケージの定数（ADR-0020）。ここで埋める——評価時に埋めると、
        // ファイルの内容で鍵付けした保存に古い版が残る。
        Data::PkgRef(name) => {
            let v = cfg.pkg_constant(name)?;
            log_trace!("  pkg.{name} = {v:?}");
            Some(Value {
                ty: crate::value::Type::Str,
                data: Data::Str(v.to_string()),
                ..value.clone()
            })
        }
        Data::List(items) => {
            let out: Vec<Value> = items.iter().filter_map(|v| specialize(v, cfg)).collect();
            Some(Value { ty: value.ty.concrete().clone(), data: Data::List(out), ..value.clone() })
        }
        Data::Map(map) => {
            let mut out = std::collections::BTreeMap::new();
            for (k, v) in map {
                if let Some(v) = specialize(v, cfg) {
                    out.insert(k.clone(), v);
                }
            }
            Some(Value { ty: value.ty.concrete().clone(), data: Data::Map(out), ..value.clone() })
        }
        _ => Some(value.clone()),
    }
}

fn eval_pred(pred: &Pred, cfg: &Config) -> bool {
    match pred {
        Pred::Flag(key) => matches!(cfg.lookup(key), Some(CfgValue::Bool(true))),
        Pred::Eq(key, expected) => match cfg.lookup(key) {
            Some(v) => v.display() == *expected,
            None => false,
        },
        // 合成は木のまま辿る（ADR-0032）。優先順位は構文解析が畳んでいる。
        Pred::Not(p) => !eval_pred(p, cfg),
        Pred::And(a, b) => eval_pred(a, cfg) && eval_pred(b, cfg),
        Pred::Or(a, b) => eval_pred(a, cfg) || eval_pred(b, cfg),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Opt;
    use crate::value::{CfgKey, MatchArm, Ns, Prov, Site, Type};
    use dowel_support::{FileId, Span};

    fn site() -> Site {
        Site::new(FileId(0), Span::new(0, 1))
    }

    fn s(v: &str) -> Value {
        Value::str(v, Prov::at(Origin::Literal, site()))
    }

    #[test]
    fn match_selects_an_arm_from_the_configuration() {
        let v = Value {
            ty: Type::Cfg(Box::new(Type::Str)),
            data: Data::Match {
                scrutinee: CfgKey { ns: Ns::Cfg, name: "opt".into() },
                arms: vec![
                    MatchArm {
                        pattern: Pattern::Value("debug".into()),
                        value: s("-O0"),
                        site: site(),
                    },
                    MatchArm {
                        pattern: Pattern::Value("release".into()),
                        value: s("-O2"),
                        site: site(),
                    },
                ],
            },
            prov: Prov::none(),
        };
        let mut cfg = Config::host_default();
        assert_eq!(specialize(&v, &cfg).unwrap().as_str(), Some("-O0"));
        cfg.opt = Opt::Release;
        let chosen = specialize(&v, &cfg).unwrap();
        assert_eq!(chosen.as_str(), Some("-O2"));
        // どのアームを選んだかが来歴に残る。
        assert!(matches!(chosen.prov.origin(), Some(Origin::MatchArm(p)) if p == "release"));
    }

    fn eq(ns: Ns, name: &str, v: &str) -> Pred {
        Pred::Eq(CfgKey { ns, name: name.into() }, v.into())
    }

    #[test]
    fn the_composed_predicates_follow_their_truth_tables() {
        // 合成は木のまま辿る（ADR-0032）。ホストは linux としてこれを見る。
        let cfg = Config::host_default();
        let linux = eq(Ns::Target, "os", "linux");
        let macos = eq(Ns::Target, "os", "macos");
        assert!(eval_pred(&linux, &cfg));
        assert!(!eval_pred(&macos, &cfg));

        // or は片方で足りる。二行に分けて書いていたものがこれである。
        assert!(eval_pred(&Pred::Or(Box::new(linux.clone()), Box::new(macos.clone())), &cfg));
        assert!(!eval_pred(&Pred::Or(Box::new(macos.clone()), Box::new(macos.clone())), &cfg));

        // and は両方要る。
        assert!(!eval_pred(&Pred::And(Box::new(linux.clone()), Box::new(macos.clone())), &cfg));
        assert!(eval_pred(&Pred::And(Box::new(linux.clone()), Box::new(linux.clone())), &cfg));

        // not は語彙が増えても正しいままである。値を列挙する書き方は、
        // `target.os` に語が足された日に静かに誤る。
        assert!(eval_pred(&Pred::Not(Box::new(macos.clone())), &cfg));
        assert!(!eval_pred(&Pred::Not(Box::new(linux.clone())), &cfg));

        // 入れ子。
        let windows = eq(Ns::Target, "os", "windows");
        let unix = Pred::Or(Box::new(linux.clone()), Box::new(macos.clone()));
        assert!(eval_pred(
            &Pred::And(Box::new(unix), Box::new(Pred::Not(Box::new(windows)))),
            &cfg
        ));
    }

    #[test]
    fn a_composed_predicate_reads_back_with_the_parentheses_it_needs() {
        let a = eq(Ns::Target, "os", "linux");
        let b = eq(Ns::Target, "os", "macos");
        let c = eq(Ns::Cfg, "opt", "debug");
        // `and` は `or` より強いので、`a and b` の側に括弧は要らない。
        let and_or = Pred::Or(
            Box::new(Pred::And(Box::new(a.clone()), Box::new(b.clone()))),
            Box::new(c.clone()),
        );
        assert_eq!(
            and_or.display(),
            "target.os == \"linux\" and target.os == \"macos\" or cfg.opt == \"debug\""
        );
        // 逆向きは括弧が要る。無ければ別の木として読み直される。
        let or_and = Pred::And(
            Box::new(Pred::Or(Box::new(a.clone()), Box::new(b.clone()))),
            Box::new(c.clone()),
        );
        assert_eq!(
            or_and.display(),
            "(target.os == \"linux\" or target.os == \"macos\") and cfg.opt == \"debug\""
        );
        // `not` の下も同じ。
        assert_eq!(
            Pred::Not(Box::new(Pred::Or(Box::new(a.clone()), Box::new(b.clone())))).display(),
            "not (target.os == \"linux\" or target.os == \"macos\")"
        );
        // 読む鍵は全部数える。
        assert_eq!(or_and.keys().len(), 3);
    }

    #[test]
    fn a_false_when_drops_the_element() {
        let cond = Value {
            ty: Type::Cfg(Box::new(Type::Str)),
            data: Data::When {
                pred: Pred::Flag(CfgKey { ns: Ns::Feature, name: "zlib".into() }),
                inner: Box::new(s("-lz")),
            },
            prov: Prov::none(),
        };
        let list = Value::list(Type::Str, vec![s("-lm"), cond], Prov::none());
        let mut cfg = Config::host_default();

        let off = specialize(&list, &cfg).unwrap();
        assert_eq!(off.as_list().unwrap().len(), 1);

        cfg.features.insert("p/zlib".into());
        let cfg = cfg.for_package("p");
        let on = specialize(&list, &cfg).unwrap();
        assert_eq!(on.as_list().unwrap().len(), 2);
        assert_eq!(on.as_list().unwrap()[1].as_str(), Some("-lz"));
    }

    #[test]
    fn specialized_values_carry_no_conditions() {
        let cond = Value {
            ty: Type::Cfg(Box::new(Type::Str)),
            data: Data::When {
                pred: Pred::Eq(CfgKey { ns: Ns::Cfg, name: "opt".into() }, "debug".into()),
                inner: Box::new(s("-g")),
            },
            prov: Prov::none(),
        };
        let out = specialize(&cond, &Config::host_default()).unwrap();
        assert!(!out.is_conditional());
        assert_eq!(out.ty, Type::Str);
    }

    #[test]
    fn a_package_constant_is_filled_in_at_specialization() {
        // ADR-0020。評価時ではなくここで埋める——評価の結果はファイルの内容で
        // 鍵付けして保存されるが、`dowel.toml` の版が動いても `dowel.build` の
        // 内容は変わらない。
        let mut cfg = Config::host_default();
        cfg.versions.insert("hashx".into(), "0.4.0".into());
        let cfg = cfg.for_package("hashx");

        let v = Value {
            ty: Type::Str,
            data: Data::PkgRef("version".into()),
            prov: Prov::at(Origin::Literal, Site::new(FileId(0), Span::new(0, 3))),
        };
        let out = specialize(&v, &cfg).expect("the reference must resolve");
        assert_eq!(out.data, Data::Str("0.4.0".into()));
        assert_eq!(out.ty, Type::Str);

        let name = Value { data: Data::PkgRef("name".into()), ..v.clone() };
        assert_eq!(specialize(&name, &cfg).unwrap().data, Data::Str("hashx".into()));
    }
}

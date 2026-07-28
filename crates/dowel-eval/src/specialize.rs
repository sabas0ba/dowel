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

        cfg.features.insert("zlib".into());
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
}

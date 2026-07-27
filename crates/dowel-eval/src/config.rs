//! 構成（configuration）と `cfg` 名前空間の語彙。
//!
//! **この語彙は暫定である。** Q1（docs/99-open-questions.md）は未決であり、
//! ここにあるのは決定ではなく、実装を進めるための仮置きである。
//! 決定時に ADR を起こして差し替える。
//!
//! 語彙を閉じた集合として実装に置くことには、決定を待つ間も意味がある。
//! 未知のキーが型検査で落ちるため、Q1 の決定が「どの次元が実際に必要か」を
//! 使用実績から判断できるようになる。

use crate::value::{CfgKey, Ns};
use std::collections::BTreeSet;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Opt {
    Debug,
    Release,
}

impl Opt {
    pub fn name(self) -> &'static str {
        match self {
            Opt::Debug => "debug",
            Opt::Release => "release",
        }
    }

    pub fn parse(s: &str) -> Option<Opt> {
        match s {
            "debug" => Some(Opt::Debug),
            "release" => Some(Opt::Release),
            _ => None,
        }
    }
}

/// アクション生成時に与える構成。
///
/// マニフェスト評価はこれを参照しない。`--release` と `--target` の切り替えで
/// 評価をやり直さないための分離である（docs/10-manifest.md 3節）。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Config {
    pub opt: Opt,
    /// ターゲットトリプル
    pub target: String,
    pub host_os: String,
    pub host_arch: String,
    pub features: BTreeSet<String>,
    /// 選択された C ツールチェーンの識別子
    pub tc_c: String,
}

impl Config {
    pub fn host_default() -> Config {
        Config {
            opt: Opt::Debug,
            target: default_triple(),
            host_os: host_os().to_string(),
            host_arch: host_arch().to_string(),
            features: BTreeSet::new(),
            tc_c: "cc".to_string(),
        }
    }

    /// 構成を一意に表す短い識別子。ビルドディレクトリ名に使う。
    pub fn id(&self) -> String {
        let mut s = format!("{}-{}", self.target, self.opt.name());
        if !self.features.is_empty() {
            s.push('-');
            s.push_str(&self.features.iter().cloned().collect::<Vec<_>>().join("+"));
        }
        s
    }

    pub fn lookup(&self, key: &CfgKey) -> Option<CfgValue> {
        match (key.ns, key.name.as_str()) {
            (Ns::Cfg, "opt") => Some(CfgValue::Str(self.opt.name().to_string())),
            (Ns::Cfg, "target") => Some(CfgValue::Str(self.target.clone())),
            (Ns::Host, "os") => Some(CfgValue::Str(self.host_os.clone())),
            (Ns::Host, "arch") => Some(CfgValue::Str(self.host_arch.clone())),
            (Ns::Tc, "c") => Some(CfgValue::Str(self.tc_c.clone())),
            (Ns::Feature, name) => Some(CfgValue::Bool(self.features.contains(name))),
            _ => None,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CfgValue {
    Str(String),
    Bool(bool),
}

impl CfgValue {
    pub fn display(&self) -> String {
        match self {
            CfgValue::Str(s) => s.clone(),
            CfgValue::Bool(b) => b.to_string(),
        }
    }
}

/// キーの値域。`match` の網羅性検査に使う。
pub enum Domain {
    /// 有限の閉じた集合。`_` なしで網羅できる
    Finite(&'static [&'static str]),
    /// 真偽
    Bool,
    /// 無限（自由文字列）。`_` アームを必須とする
    Open,
}

/// 語彙表。`dowel schema dump --section=cfg` の出力元でもある。
pub const VOCABULARY: &[(&str, &str, Domain, &str)] = &[
    ("cfg", "opt", Domain::Finite(&["debug", "release"]), "最適化構成"),
    ("cfg", "target", Domain::Open, "ターゲットトリプル"),
    ("host", "os", Domain::Finite(&["linux", "macos", "windows"]), "ビルドホストの OS"),
    (
        "host",
        "arch",
        Domain::Finite(&["x86_64", "aarch64", "riscv64"]),
        "ビルドホストのアーキテクチャ",
    ),
    ("feature", "*", Domain::Bool, "機能フラグ（dowel.toml の [features] で宣言されたもの）"),
    ("tc", "c", Domain::Open, "選択された C ツールチェーンの識別子"),
];

/// キーが語彙に存在するか。存在しなければ型検査で落とす。
pub fn domain_of(key: &CfgKey) -> Option<&'static Domain> {
    VOCABULARY.iter().find_map(|(ns, name, dom, _)| {
        if *ns == key.ns.name() && (*name == key.name || *name == "*") {
            Some(dom)
        } else {
            None
        }
    })
}

/// 同じ名前空間の既知のキー。診断の候補提示に使う。
pub fn known_keys(ns: Ns) -> Vec<String> {
    VOCABULARY
        .iter()
        .filter(|(n, _, _, _)| *n == ns.name())
        .map(|(n, name, _, _)| format!("{n}.{name}"))
        .collect()
}

pub fn host_os() -> &'static str {
    if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unknown"
    }
}

pub fn host_arch() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "riscv64") {
        "riscv64"
    } else {
        "unknown"
    }
}

/// ホストのターゲットトリプル。
///
/// 暫定的に OS と arch から組み立てる。ツールチェーンに問い合わせて確定させるのは
/// プローブ事実 DB（Phase 2）の仕事であり、そこで置き換える。
pub fn default_triple() -> String {
    match host_os() {
        "linux" => format!("{}-unknown-linux-gnu", host_arch()),
        "macos" => format!("{}-apple-darwin", host_arch()),
        "windows" => format!("{}-pc-windows-msvc", host_arch()),
        other => format!("{}-unknown-{other}", host_arch()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 語彙にないキーは見つからない() {
        assert!(domain_of(&CfgKey { ns: Ns::Cfg, name: "opt".into() }).is_some());
        assert!(domain_of(&CfgKey { ns: Ns::Cfg, name: "optimization".into() }).is_none());
        // feature は任意の名前を受ける。
        assert!(domain_of(&CfgKey { ns: Ns::Feature, name: "zlib".into() }).is_some());
    }

    #[test]
    fn 構成から値を引ける() {
        let mut c = Config::host_default();
        c.features.insert("zlib".into());
        assert_eq!(
            c.lookup(&CfgKey { ns: Ns::Cfg, name: "opt".into() }),
            Some(CfgValue::Str("debug".into()))
        );
        assert_eq!(
            c.lookup(&CfgKey { ns: Ns::Feature, name: "zlib".into() }),
            Some(CfgValue::Bool(true))
        );
        assert_eq!(
            c.lookup(&CfgKey { ns: Ns::Feature, name: "png".into() }),
            Some(CfgValue::Bool(false))
        );
    }

    #[test]
    fn 構成識別子は機能フラグを含む() {
        let mut c = Config::host_default();
        c.opt = Opt::Release;
        c.target = "x86_64-unknown-linux-gnu".into();
        assert_eq!(c.id(), "x86_64-unknown-linux-gnu-release");
        c.features.insert("zlib".into());
        assert_eq!(c.id(), "x86_64-unknown-linux-gnu-release-zlib");
    }
}

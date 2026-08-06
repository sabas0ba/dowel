//! 構成（configuration）と `cfg` 名前空間の語彙。
//!
//! この語彙は暫定である。 Q1（docs/99-open-questions.md）は未決であり、
//! ここにあるのは決定ではなく、実装を進めるための仮置きである。
//! 決定時に ADR を起こして差し替える。
//!
//! 語彙を閉じた集合として実装に置くことには、決定を待つ間も意味がある。
//! 未知のキーが型検査で落ちるため、Q1 の決定が「どの次元が実際に必要か」を
//! 使用実績から判断できるようになる。

use crate::value::{CfgKey, Ns};
use std::collections::{BTreeMap, BTreeSet};

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
    /// 有効な機能。`<パッケージ>/<機能>` の形で持つ（ADR-0017）。
    /// 同じ名前の機能でも、宣言したパッケージが違えば別の機能である
    pub features: BTreeSet<String>,
    /// いま具体化しているパッケージ。`feature.<名前>` はこれで修飾して引く。
    /// 空はどのパッケージにも属さない位置（構成そのもの）を表す
    package: String,
    /// 選択された道具。道具名（[`TOOLS`]）→ コマンド。
    /// 既定は [`TOOLS`] が与え、`[toolchain]` の宣言が上書きする
    tools: BTreeMap<String, String>,
}

/// ツールチェーンを構成する道具の表。（名前, 既定のコマンド）。
///
/// 道具を増やすとき（例: disasm、objcopy）はここに1行と、[`VOCABULARY`] の
/// `tc.<名前>` の行を足す（両者の一致は検査される）。`[toolchain]` のキー・
/// 既定値・宣言の写し・`toolchain-mismatch` の比較は全てこの表から回る。
/// **いつ実在を確かめるか**だけは表に置かない。C コンパイラは常に、
/// C++ は C++ ソースが現れたとき、archiver は書庫を作るときに要る——
/// 要不要は道具を使う側の意味論であり、使う箇所が判断する。
pub const TOOLS: &[(&str, &str)] = &[
    ("c", "cc"),
    ("cxx", "c++"),
    ("ar", "ar"),
    ("objcopy", "objcopy"),
    ("size", "size"),
    ("nm", "nm"),
    ("objdump", "objdump"),
    ("readelf", "readelf"),
];

/// パスの1要素として安全な形にする。
///
/// 潰す先を `--` にするのは、単一の区切り文字にすると別々の名前が同じ形へ
/// 落ちうるためである（`a/b` と `a-b` はどちらも `a-b` になる）。機能名と
/// トリプルに使える文字は英数字と `_` `-` `.` `+` であり、この中に `--` は
/// 現れない——`a-b` は `a-b` のまま、`a/b` は `a--b` になる。
///
/// 可逆である必要は無い。同じ構成が同じ識別子になり、違う構成が違う識別子に
/// なることだけが要件である（issue #68）。
pub fn path_safe(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+') {
            out.push(c);
        } else {
            out.push_str("--");
        }
    }
    out
}

/// 道具の既定のコマンド。
pub fn default_tool(name: &str) -> &'static str {
    TOOLS.iter().find(|(n, _)| *n == name).map(|(_, d)| *d).unwrap_or("")
}

impl Config {
    pub fn host_default() -> Config {
        Config {
            opt: Opt::Debug,
            target: default_triple(),
            host_os: host_os().to_string(),
            host_arch: host_arch().to_string(),
            features: BTreeSet::new(),
            package: String::new(),
            tools: TOOLS.iter().map(|(n, d)| (n.to_string(), d.to_string())).collect(),
        }
    }

    /// 道具のコマンド。[`TOOLS`] に無い名前は空文字列（呼び手の誤り）。
    pub fn tool(&self, name: &str) -> &str {
        self.tools.get(name).map(String::as_str).unwrap_or("")
    }

    /// このパッケージの値を具体化するための写し。
    ///
    /// `feature.<名前>` の判定だけが変わる。構成そのもの（最適化・トリプル・
    /// 道具）は1回のビルドで1つであり、パッケージごとに変わらない。
    pub fn for_package(&self, name: &str) -> Config {
        Config { package: name.to_string(), ..self.clone() }
    }

    /// いま具体化しているパッケージ。
    pub fn package(&self) -> &str {
        &self.package
    }

    /// 道具のコマンドを差し替える。`[toolchain]` の宣言の写しに使う。
    pub fn set_tool(&mut self, name: &str, command: String) {
        self.tools.insert(name.to_string(), command);
    }

    /// 構成を一意に表す短い識別子。ビルドディレクトリ名に使う。
    ///
    /// 1つの構成が1つのディレクトリになることを保つため、パスの区切りに
    /// なりうる文字を潰す（[`path_safe`]）。依存先へ転送する機能名は
    /// `dep/feature` の形を採るため、潰さないと `/` がそのまま区切りとして
    /// 展開され、1構成が2階層に割れる（issue #68）。
    pub fn id(&self) -> String {
        let mut s = format!("{}-{}", path_safe(&self.target), self.opt.name());
        if !self.features.is_empty() {
            s.push('-');
            let names: Vec<String> = self.features.iter().map(|f| path_safe(f)).collect();
            s.push_str(&names.join("+"));
        }
        s
    }

    pub fn lookup(&self, key: &CfgKey) -> Option<CfgValue> {
        match (key.ns, key.name.as_str()) {
            (Ns::Cfg, "opt") => Some(CfgValue::Str(self.opt.name().to_string())),
            (Ns::Cfg, "target") => Some(CfgValue::Str(self.target.clone())),
            (Ns::Host, "os") => Some(CfgValue::Str(self.host_os.clone())),
            (Ns::Host, "arch") => Some(CfgValue::Str(self.host_arch.clone())),
            (Ns::Tc, name) => self.tools.get(name).map(|t| CfgValue::Str(t.clone())),
            // 機能はパッケージに属する。`feature.x` は「このパッケージで
            // x が有効か」であり、他のパッケージの `x` では真にならない。
            (Ns::Feature, name) => {
                Some(CfgValue::Bool(self.features.contains(&format!("{}/{name}", self.package))))
            }
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
    ("cfg", "opt", Domain::Finite(&["debug", "release"]), "optimization configuration"),
    ("cfg", "target", Domain::Open, "target triple"),
    (
        "host",
        "os",
        Domain::Finite(&["linux", "macos", "windows"]),
        "operating system of the build host",
    ),
    (
        "host",
        "arch",
        Domain::Finite(&["x86_64", "aarch64", "riscv64"]),
        "architecture of the build host",
    ),
    ("feature", "*", Domain::Bool, "feature flag declared in [features] of dowel.toml"),
    ("tc", "c", Domain::Open, "identifier of the selected C toolchain"),
    ("tc", "cxx", Domain::Open, "identifier of the selected C++ toolchain"),
    ("tc", "ar", Domain::Open, "identifier of the selected archiver"),
    ("tc", "objcopy", Domain::Open, "identifier of the selected object copier"),
    ("tc", "size", Domain::Open, "identifier of the selected size reporter"),
    ("tc", "nm", Domain::Open, "identifier of the selected symbol lister"),
    ("tc", "objdump", Domain::Open, "identifier of the selected object dumper"),
    ("tc", "readelf", Domain::Open, "identifier of the selected ELF reader"),
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
    fn keys_outside_the_vocabulary_are_not_found() {
        assert!(domain_of(&CfgKey { ns: Ns::Cfg, name: "opt".into() }).is_some());
        assert!(domain_of(&CfgKey { ns: Ns::Cfg, name: "optimization".into() }).is_none());
        // feature は任意の名前を受ける。
        assert!(domain_of(&CfgKey { ns: Ns::Feature, name: "zlib".into() }).is_some());
    }

    #[test]
    fn looks_up_values_from_the_configuration() {
        let mut c = Config::host_default();
        c.features.insert("p/zlib".into());
        let c = c.for_package("p");
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
    fn the_tool_table_and_the_vocabulary_agree() {
        // 道具は TOOLS が正であり、語彙の tc.* はその説明である。
        // 片方だけに足すと、宣言できるのに参照できない（またはその逆の）
        // 道具ができる。
        let vocab: BTreeSet<&str> =
            VOCABULARY.iter().filter(|(ns, ..)| *ns == "tc").map(|(_, n, ..)| *n).collect();
        let tools: BTreeSet<&str> = TOOLS.iter().map(|(n, _)| *n).collect();
        assert_eq!(vocab, tools);
    }

    #[test]
    fn tools_default_from_the_table_and_declarations_override() {
        let mut c = Config::host_default();
        assert_eq!(c.tool("c"), "cc");
        assert_eq!(c.tool("ar"), "ar");
        c.set_tool("ar", "llvm-ar".into());
        assert_eq!(c.tool("ar"), "llvm-ar");
        assert_eq!(
            c.lookup(&CfgKey { ns: Ns::Tc, name: "ar".into() }),
            Some(CfgValue::Str("llvm-ar".into()))
        );
    }

    #[test]
    fn features_belong_to_the_package_that_declared_them() {
        // 同じ名前でも、宣言したパッケージが違えば別の機能である（ADR-0017）。
        let mut c = Config::host_default();
        c.features.insert("app/x".into());
        c.features.insert("lib/y".into());
        let feat = |c: &Config, n: &str| c.lookup(&CfgKey { ns: Ns::Feature, name: n.into() });

        let app = c.for_package("app");
        assert_eq!(feat(&app, "x"), Some(CfgValue::Bool(true)));
        assert_eq!(feat(&app, "y"), Some(CfgValue::Bool(false)));

        let lib = c.for_package("lib");
        assert_eq!(feat(&lib, "y"), Some(CfgValue::Bool(true)));
        assert_eq!(feat(&lib, "x"), Some(CfgValue::Bool(false)));
    }

    #[test]
    fn the_identifier_stays_one_path_component() {
        // 転送する機能名は `dep/feature` の形を採る。`/` をそのまま識別子へ
        // 入れると、1つの構成が2階層のディレクトリに割れる（issue #68）。
        let mut c = Config::host_default();
        c.target = "x86_64-unknown-linux-gnu".into();
        c.features.insert("core/deep".into());
        let id = c.id();
        assert!(!id.contains('/'), "{id}");
        assert!(id.contains("core--deep"), "{id}");

        // 潰した結果が別の名前と衝突しない。
        let mut other = Config::host_default();
        other.target = "x86_64-unknown-linux-gnu".into();
        other.features.insert("core-deep".into());
        assert_ne!(c.id(), other.id());
    }

    #[test]
    fn configuration_id_includes_feature_flags() {
        let mut c = Config::host_default();
        c.opt = Opt::Release;
        c.target = "x86_64-unknown-linux-gnu".into();
        assert_eq!(c.id(), "x86_64-unknown-linux-gnu-release");
        c.features.insert("zlib".into());
        assert_eq!(c.id(), "x86_64-unknown-linux-gnu-release-zlib");
    }
}

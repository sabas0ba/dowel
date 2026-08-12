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
    /// パッケージ名 → 版。`pkg.version` が、いま具体化しているパッケージの分を
    /// 引く（ADR-0020）。機能（`features`）と同じく、読み込みが済んでから
    /// 構成へ載せる
    pub versions: BTreeMap<String, String>,
    /// 選択された道具。道具名（[`TOOLS`]）→ コマンド。
    /// 既定は [`TOOLS`] が与え、`[toolchain]` の宣言が上書きする
    tools: BTreeMap<String, String>,
    /// 引数の綴り方（ADR-0027）。三つ組から導き、`[toolchain] style` が上書きする
    pub style: Style,
    /// ホストの三つ組。「対象がホストと同じか」の判定がこれを読む。
    ///
    /// 既定は OS と構成から組み立てた近似だが、C コンパイラに `-dumpmachine`
    /// で訊けた場合はその名乗りに差し替わる（[ADR-0028](../../../docs/adr/0028-probe-facts.md)）。
    /// 近似のままだと、`x86_64-pc-linux-gnu` を名乗る道具を持つ機械で
    /// `--target` にその綴りを渡した利用者が、クロス扱いされてランナーを
    /// 求められる
    pub host: String,
}

/// 道具に渡す引数の綴り方（[ADR-0027](../../../docs/adr/0027-toolchain-style.md)）。
///
/// 道具の**名前**は宣言できても、綴りが Unix 固定だと `cl` は解釈できない
/// 命令が組み上がる。しかも `-MD` は MSVC で「動的 CRT をリンクする」という
/// **別の、それ自体は正当な意味**を持つ——依存の書き出しを頼んだつもりの旗が
/// ABI を選ぶ旗になる（issue #113）。
///
/// 様式は2つしか無い。GNU（gcc / clang / MinGW）と MSVC（cl / clang-cl）で
/// あり、それ以外は前者に準ずる。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Style {
    #[default]
    Gnu,
    Msvc,
}

impl Style {
    pub fn parse(s: &str) -> Option<Style> {
        match s {
            "gnu" => Some(Style::Gnu),
            "msvc" => Some(Style::Msvc),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Style::Gnu => "gnu",
            Style::Msvc => "msvc",
        }
    }

    pub const ALL: &'static [&'static str] = &["gnu", "msvc"];
}

/// 三つ組から様式を導く。
///
/// `x86_64-pc-windows-msvc` は MSVC の道具を指す三つ組である——`--target` に
/// 既に書かれている以上、宣言させ直す理由が無い（ADR-0026 と同じ判断）。
/// `[toolchain] style` はこの導出を上書きする。
pub fn triple_style(triple: &str) -> Style {
    match triple.rsplit('-').next() {
        Some("msvc") => Style::Msvc,
        _ => Style::Gnu,
    }
}

/// ツールチェーンを構成する道具の表。（名前, GNU の既定, MSVC の既定）。
///
/// 道具を増やすとき（例: disasm、objcopy）はここに1行と、[`VOCABULARY`] の
/// `tc.<名前>` の行を足す（両者の一致は検査される）。`[toolchain]` のキー・
/// 既定値・宣言の写し・`toolchain-mismatch` の比較は全てこの表から回る。
/// **いつ実在を確かめるか**だけは表に置かない。C コンパイラは常に、
/// C++ は C++ ソースが現れたとき、archiver は書庫を作るときに要る——
/// 要不要は道具を使う側の意味論であり、使う箇所が判断する。
pub const TOOLS: &[(&str, &str, &str)] = &[
    ("c", "cc", "cl"),
    ("cxx", "c++", "cl"),
    ("ar", "ar", "lib"),
    // リンカ。GNU では driver が兼ねるので既定を持たない——空は
    // 「`tc.c` / `tc.cxx` がリンクする」を意味する。MSVC では別物である
    // （`link.exe`）。
    ("link", "", "link"),
    ("objcopy", "objcopy", "objcopy"),
    ("size", "size", "size"),
    ("nm", "nm", "dumpbin"),
    ("objdump", "objdump", "dumpbin"),
    ("readelf", "readelf", "dumpbin"),
    ("debug", "gdb", "cdb"),
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

/// 道具の既定のコマンド。様式で変わる——GNU の `ar` は MSVC で `lib` である。
pub fn default_tool(name: &str, style: Style) -> &'static str {
    TOOLS
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, gnu, msvc)| match style {
            Style::Gnu => *gnu,
            Style::Msvc => *msvc,
        })
        .unwrap_or("")
}

impl Config {
    pub fn host_default() -> Config {
        Config::for_target(default_triple())
    }

    /// 三つ組に対応する既定の構成。様式と道具の既定がここで決まる。
    pub fn for_target(target: String) -> Config {
        let style = triple_style(&target);
        Config {
            opt: Opt::Debug,
            target,
            host_os: host_os().to_string(),
            host_arch: host_arch().to_string(),
            features: BTreeSet::new(),
            package: String::new(),
            versions: BTreeMap::new(),
            tools: TOOLS
                .iter()
                .map(|(n, _, _)| (n.to_string(), default_tool(n, style).to_string()))
                .collect(),
            style,
            host: default_triple(),
        }
    }

    /// 様式を変える。宣言（`[toolchain] style`）が導出を上書きしたときに、
    /// 明示されていない道具の既定も付いて動く必要がある。
    pub fn set_style(&mut self, style: Style) {
        self.style = style;
        for (name, _, _) in TOOLS {
            self.tools.insert(name.to_string(), default_tool(name, style).to_string());
        }
    }

    /// 対象がホストと同じ機械か。
    ///
    /// 綴りの一致で見る。ホストの三つ組は道具の名乗りに差し替わりうるので、
    /// 「近似の綴りで書かれた `--target`」と「道具が名乗る綴り」の両方が
    /// ホストとして通る（ADR-0028）。
    pub fn targets_host(&self) -> bool {
        self.target == self.host || self.target == default_triple()
    }

    /// ホストの三つ組を、道具が名乗ったもので置き換える。
    pub fn set_host(&mut self, triple: String) {
        self.host = triple;
    }

    /// リンクに使う道具。GNU では driver が兼ねる（`link` は空）。
    pub fn linker(&self, needs_cxx: bool) -> &str {
        match self.tool("link") {
            "" => self.tool(if needs_cxx { "cxx" } else { "c" }),
            explicit => explicit,
        }
    }

    /// 道具のコマンド。[`TOOLS`] に無い名前は空文字列（呼び手の誤り）。
    pub fn tool(&self, name: &str) -> &str {
        self.tools.get(name).map(String::as_str).unwrap_or("")
    }

    /// このパッケージの値を具体化するための写し。
    ///
    /// 変わるのは `feature.<名前>` の判定と `pkg.*` の値だけである。構成そのもの
    /// （最適化・トリプル・道具）は1回のビルドで1つであり、パッケージごとに
    /// 変わらない。
    pub fn for_package(&self, name: &str) -> Config {
        Config { package: name.to_string(), ..self.clone() }
    }

    /// いま具体化しているパッケージ。
    pub fn package(&self) -> &str {
        &self.package
    }

    /// パッケージの定数（ADR-0020）。`PKG_CONSTANTS` に無い名前は `None`。
    pub fn pkg_constant(&self, name: &str) -> Option<&str> {
        match name {
            "name" => Some(&self.package),
            "version" => self.versions.get(&self.package).map(String::as_str),
            _ => None,
        }
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
            // 対象の性質は三つ組から導く。新しい入力は要らない——`--target`
            // で既に与えられている（ADR-0026）。
            (Ns::Target, "os") => Some(CfgValue::Str(triple_os(&self.target).to_string())),
            (Ns::Target, "arch") => Some(CfgValue::Str(triple_arch(&self.target).to_string())),
            (Ns::Target, "env") => Some(CfgValue::Str(triple_env(&self.target).to_string())),
            (Ns::Tc, name) => self.tools.get(name).map(|t| CfgValue::Str(t.clone())),
            // 機能はパッケージに属する。`feature.x` は「このパッケージで
            // x が有効か」であり、他のパッケージの `x` では真にならない。
            (Ns::Feature, name) => {
                Some(CfgValue::Bool(self.features.contains(&format!("{}/{name}", self.package))))
            }
            (Ns::Pkg, name) => self.pkg_constant(name).map(|s| CfgValue::Str(s.to_string())),
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
    (
        "target",
        "os",
        Domain::Finite(TARGET_OSES),
        "operating system being built for, read off the target triple",
    ),
    (
        "target",
        "arch",
        Domain::Finite(TARGET_ARCHES),
        "architecture being built for, read off the target triple",
    ),
    (
        "target",
        "env",
        Domain::Finite(TARGET_ENVS),
        "C runtime being built against, read off the target triple",
    ),
    ("feature", "*", Domain::Bool, "feature flag declared in [features] of dowel.toml"),
    ("tc", "c", Domain::Open, "identifier of the selected C toolchain"),
    ("tc", "cxx", Domain::Open, "identifier of the selected C++ toolchain"),
    ("tc", "ar", Domain::Open, "identifier of the selected archiver"),
    (
        "tc",
        "link",
        Domain::Open,
        "identifier of the selected linker; empty when the compiler driver links",
    ),
    ("tc", "objcopy", Domain::Open, "identifier of the selected object copier"),
    ("tc", "size", Domain::Open, "identifier of the selected size reporter"),
    ("tc", "nm", Domain::Open, "identifier of the selected symbol lister"),
    ("tc", "objdump", Domain::Open, "identifier of the selected object dumper"),
    ("tc", "readelf", Domain::Open, "identifier of the selected ELF reader"),
    ("tc", "debug", Domain::Open, "identifier of the selected debugger"),
];

/// パッケージの定数（[ADR-0020](../../../docs/adr/0020-package-constants.md)）。
/// （名前, 説明）。
///
/// `cfg` の語彙（[`VOCABULARY`]）とは別の表である。あちらはビルドが走る構成を
/// 述べるもので、値域と網羅性の規則を持ち、構成の同一性にも関わる。パッケージの
/// 定数はそのどれでもない。同じ表に混ぜると、`match pkg.version` が「版で
/// ビルドを分岐できる」と述べることになる。
pub const PKG_CONSTANTS: &[(&str, &str)] = &[
    ("name", "the package name from [package] of dowel.toml"),
    ("version", "the package version"),
];

/// パッケージの定数の名前か。
pub fn is_pkg_constant(name: &str) -> bool {
    PKG_CONSTANTS.iter().any(|(n, _)| *n == name)
}

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

/// 対象の OS の値域（[ADR-0026](../../../docs/adr/0026-target-os-arch.md)）。
///
/// `other` を持つのは、`--target` が自由文字列だからである。任意の三つ組に
/// 写像先が無いと有限領域にできず、有限でなければ `match` の網羅性検査が
/// 効かない——そしてそれこそが、三つ組を数え上げる形の一番の弱点だった
/// （issue #115）。
pub const TARGET_OSES: &[&str] = &["linux", "macos", "windows", "none", "other"];

/// 対象の構成の値域。同じ理由で `other` を持つ。
pub const TARGET_ARCHES: &[&str] = &["x86_64", "x86", "aarch64", "arm", "riscv64", "other"];

/// 対象の C ランタイムの値域
/// （[ADR-0042](../../../docs/adr/0042-abi-label-components.md)）。
///
/// `target.os` が答えない軸である——`linux-gnu` と `linux-musl` は同じ OS で
/// あって、繋がらない2つの実行環境である。`none` はランタイムを名乗らない
/// 三つ組、`other` は写像先の無いものを受ける。
pub const TARGET_ENVS: &[&str] = &["gnu", "musl", "msvc", "apple", "none", "other"];

/// 三つ組から対象の OS を読む。
///
/// 綴りは三つ組のものではなく語彙のものにする（`darwin` ではなく `macos`）。
/// `host.os` と同じ値を同じ名前で読めることが、対を成す語の要件である。
///
/// 判定は要素の走査で行う。三つ組の要素数は3とも4とも限らず
/// （`thumbv7em-none-eabihf` は vendor を持たない）、位置で決められない。
pub fn triple_os(triple: &str) -> &'static str {
    let parts: Vec<&str> = triple.split('-').collect();
    for p in &parts {
        // `linux-gnu` も `linux-musl` も OS は linux である。ABI は別の軸。
        if p.starts_with("linux") {
            return "linux";
        }
        if p.starts_with("windows") {
            return "windows";
        }
        if p.starts_with("darwin") || *p == "macos" || p.starts_with("ios") {
            return "macos";
        }
    }
    // ベアメタル。`none` を名乗る三つ組と、OS を1つも名乗らないまま
    // `eabi` で終わる三つ組（`thumbv7m-none-eabi`）の両方がある。
    if parts.iter().any(|p| *p == "none" || p.starts_with("eabi") || *p == "elf") {
        return "none";
    }
    "other"
}

/// 三つ組から対象の構成を読む。先頭の要素が構成である。
pub fn triple_arch(triple: &str) -> &'static str {
    let head = triple.split('-').next().unwrap_or("");
    match head {
        "x86_64" | "amd64" => "x86_64",
        "aarch64" | "arm64" => "aarch64",
        "i386" | "i486" | "i586" | "i686" | "x86" => "x86",
        _ if head.starts_with("riscv64") => "riscv64",
        // 32ビットの ARM は綴りが多い（`armv7`、`thumbv7em`、`armebv7r`）。
        // 一列に並ぶ族なので1つの語にまとめる——分けても、書き手は結局
        // 全部を数え上げることになる。
        _ if head.starts_with("arm") || head.starts_with("thumb") => "arm",
        _ => "other",
    }
}

/// 三つ組から対象の C ランタイムを読む（ADR-0042）。
///
/// 位置では決められない。`x86_64-unknown-linux-musl` は4要素、
/// `aarch64-apple-darwin` は3要素でランタイムを名乗らない。名乗らないものは
/// OS が決めている——Apple の platform に libc の選択は無い。
pub fn triple_env(triple: &str) -> &'static str {
    for p in triple.split('-') {
        // `gnueabihf` も `musleabi` も、繋がるかどうかを決める語は頭にある。
        if p.starts_with("gnu") {
            return "gnu";
        }
        if p.starts_with("musl") {
            return "musl";
        }
        if p == "msvc" {
            return "msvc";
        }
    }
    match triple_os(triple) {
        // Apple の platform は libc を選ばせない。三つ組が黙っているのは
        // 選択肢が無いからであって、不明だからではない。
        "macos" => "apple",
        "none" => "none",
        _ => "other",
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
    fn the_target_triple_yields_an_os_and_an_arch() {
        // 綴りは語彙のもの。`host.os` と同じ値を同じ名前で読めることが、
        // 対を成す語の要件である（issue #115）。
        assert_eq!(triple_os("x86_64-pc-windows-gnu"), "windows");
        assert_eq!(triple_os("x86_64-pc-windows-msvc"), "windows");
        assert_eq!(triple_os("aarch64-unknown-linux-musl"), "linux");
        assert_eq!(triple_os("x86_64-apple-darwin"), "macos");
        // ベアメタルは2つの綴り方がある。
        assert_eq!(triple_os("thumbv7em-none-eabihf"), "none");
        assert_eq!(triple_os("riscv32imac-unknown-none-elf"), "none");
        // 写像先の無い三つ組。`--target` は自由文字列なので必ず在る。
        assert_eq!(triple_os("x86_64-unknown-freebsd"), "other");

        assert_eq!(triple_arch("x86_64-pc-windows-gnu"), "x86_64");
        assert_eq!(triple_arch("i686-pc-windows-gnu"), "x86");
        assert_eq!(triple_arch("aarch64-apple-darwin"), "aarch64");
        assert_eq!(triple_arch("riscv64gc-unknown-linux-gnu"), "riscv64");
        // 32ビット ARM は綴りが多く、1つの語にまとめる。
        assert_eq!(triple_arch("armv7-unknown-linux-gnueabihf"), "arm");
        assert_eq!(triple_arch("thumbv7em-none-eabihf"), "arm");
        assert_eq!(triple_arch("s390x-unknown-linux-gnu"), "other");

        // C ランタイムは OS が答えない軸である（ADR-0042）。同じ `linux` の
        // 下で、繋がらない2つが並ぶ。
        assert_eq!(triple_env("x86_64-unknown-linux-gnu"), "gnu");
        assert_eq!(triple_env("x86_64-unknown-linux-musl"), "musl");
        assert_eq!(triple_env("x86_64-pc-windows-msvc"), "msvc");
        assert_eq!(triple_env("x86_64-pc-windows-gnu"), "gnu");
        // 接尾辞が付いても、繋がるかどうかを決める語は頭にある。
        assert_eq!(triple_env("armv7-unknown-linux-gnueabihf"), "gnu");
        assert_eq!(triple_env("armv7-unknown-linux-musleabi"), "musl");
        // Apple の platform は libc を選ばせない。黙っているのは選択肢が
        // 無いからであって、不明だからではない。
        assert_eq!(triple_env("aarch64-apple-darwin"), "apple");
        assert_eq!(triple_env("thumbv7em-none-eabihf"), "none");
        assert_eq!(triple_env("x86_64-unknown-freebsd"), "other");
    }

    #[test]
    fn every_derived_value_is_inside_the_declared_domain() {
        // 有限領域だと宣言した以上、導出がその外の値を返してはならない。
        // 返すと、網羅した `match` が実行時に落ちる腕を持つことになる。
        let triples = [
            "x86_64-unknown-linux-gnu",
            "x86_64-pc-windows-gnu",
            "x86_64-apple-darwin",
            "thumbv7em-none-eabihf",
            "s390x-unknown-freebsd",
            "",
            "nonsense",
        ];
        for t in triples {
            assert!(TARGET_OSES.contains(&triple_os(t)), "`{t}` gave an os outside the domain");
            assert!(
                TARGET_ARCHES.contains(&triple_arch(t)),
                "`{t}` gave an arch outside the domain"
            );
            assert!(TARGET_ENVS.contains(&triple_env(t)), "`{t}` gave an env outside the domain");
        }
    }

    #[test]
    fn the_target_namespace_reads_the_configured_triple() {
        let mut cfg = Config::host_default();
        cfg.target = "x86_64-pc-windows-gnu".into();
        let os = cfg.lookup(&CfgKey { ns: Ns::Target, name: "os".into() });
        assert_eq!(os, Some(CfgValue::Str("windows".into())));
        // `host.*` は残る。組む側を見たい場面は実在する。
        assert_eq!(
            cfg.lookup(&CfgKey { ns: Ns::Host, name: "os".into() }),
            Some(CfgValue::Str(host_os().to_string()))
        );
    }

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
    fn package_constants_come_from_the_package_being_specialized() {
        // ADR-0020。同じ構成でも、パッケージが変われば `pkg.*` は変わる。
        let mut c = Config::host_default();
        c.versions.insert("app".into(), "1.2.3".into());
        c.versions.insert("lib".into(), "0.4.0".into());

        let app = c.for_package("app");
        assert_eq!(app.pkg_constant("name"), Some("app"));
        assert_eq!(app.pkg_constant("version"), Some("1.2.3"));
        assert_eq!(c.for_package("lib").pkg_constant("version"), Some("0.4.0"));
        assert_eq!(app.pkg_constant("author"), None);
    }

    #[test]
    fn package_constants_are_not_configuration_keys() {
        // 構成の語彙とは別の表である。混ぜると `match pkg.version` が
        // 「版でビルドを分岐できる」と述べることになる。
        assert!(domain_of(&CfgKey { ns: Ns::Pkg, name: "version".into() }).is_none());
        assert!(is_pkg_constant("version"));
        assert!(!is_pkg_constant("opt"));
    }

    #[test]
    fn the_tool_table_and_the_vocabulary_agree() {
        // 道具は TOOLS が正であり、語彙の tc.* はその説明である。
        // 片方だけに足すと、宣言できるのに参照できない（またはその逆の）
        // 道具ができる。
        let vocab: BTreeSet<&str> =
            VOCABULARY.iter().filter(|(ns, ..)| *ns == "tc").map(|(_, n, ..)| *n).collect();
        let tools: BTreeSet<&str> = TOOLS.iter().map(|(n, _, _)| *n).collect();
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

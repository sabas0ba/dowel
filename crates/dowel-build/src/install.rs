//! ビルド木の外へ出す（[ADR-0041](../../../docs/adr/0041-install.md)）。
//!
//! ここまでの全ては1つのビルド木の中の話だった。共有ライブラリを宣言する
//! 目的は配ることであり、配る手段が無ければ宣言は途中で終わっている。
//!
//! 中身は写しであって作り直しではない。組んだものと配ったものが同じ
//! バイト列であることが、検査した対象と配った対象を同じものにする。
//! 実行時の探索路が自分自身からの相対で記録されているので、写すだけで
//! 済む（[`crate::toolstyle::relocatable_search_path`]）。

use crate::plan::Plan;
use crate::toolstyle;
use dowel_eval::schema::TableKind;
use dowel_eval::Config;
use dowel_model::graph::Graph;
use dowel_model::{Session, TargetId};
use dowel_support::Diagnostic;
use std::path::{Path, PathBuf};

/// `entries` が答えるもの。
///
/// 写す一覧だけでは足りない。配った面が読めるかを確かめるのは写した**後**で
/// あり（[ADR-0060](../../../docs/adr/0060-the-surface-is-readable.md)）、
/// そのときには「どのヘッダを、どの宣言に従って配ったか」が要る。
pub struct Entries {
    pub items: Vec<Item>,
    pub diagnostics: Vec<Diagnostic>,
    /// 配ったヘッダ。写し終えてから読めるかを確かめる
    pub headers: Vec<crate::surface::Header>,
    /// 使う側が `-I` に載せる場所
    pub include_root: PathBuf,
}

/// 入れる先に置く1件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    /// ビルド木の中の1ファイルを写す
    Copy { from: PathBuf, to: PathBuf },
    /// 版を持たない名前を、版付きの実体の隣に置く（ADR-0040）
    Link { at: PathBuf, to: String },
    /// dowel が組み立てた本文を書く。pkg-config の記述がこれである
    /// （[ADR-0043](../../../docs/adr/0043-pkgconfig-generation.md)）
    Write { at: PathBuf, text: String },
}

impl Item {
    /// 置かれる場所。表示と重複除去が読む。
    pub fn destination(&self) -> &Path {
        match self {
            Item::Copy { to, .. } => to,
            Item::Link { at, .. } => at,
            Item::Write { at, .. } => at,
        }
    }
}

/// 何をどこへ置くか。書き込みは行わない。
///
/// `destdir` は先頭に付くだけで、記録される内容には現れない。実行時の
/// 探索路が相対である以上、段取り用のディレクトリへ入れても、そこから
/// `prefix` へ移した後も同じものが動く。
pub fn entries(
    sess: &Session,
    plan: &Plan,
    graph: &Graph,
    cfg: &Config,
    prefix: &Path,
    destdir: Option<&Path>,
    targets: &[TargetId],
) -> Entries {
    let root = destdir.map(|d| join_prefix(d, prefix)).unwrap_or_else(|| prefix.to_path_buf());
    // 走査は答そのものへ積む。配る項目、配ったヘッダ、述べたことは同じ1件の
    // 走査から出るものであり、別々の器に分けても最後に1つへ戻す。
    let mut out = Entries {
        items: Vec::new(),
        diagnostics: Vec::new(),
        headers: Vec::new(),
        include_root: root.join("include"),
    };

    for &tid in targets {
        let target = sess.target(tid);
        let Some(artifact) = plan.artifacts.get(&tid) else { continue };
        let Some(name) = artifact.file_name() else { continue };
        let dir = match target.kind {
            TableKind::Bin => "bin",
            TableKind::Lib => "lib",
            // 検査と計測は配るものではない。物を確かめる道具であって、物ではない。
            _ => continue,
        };
        out.items.push(Item::Copy { from: artifact.clone(), to: root.join(dir).join(name) });

        if target.kind == TableKind::Lib {
            if plan.shared_libraries.contains(artifact) {
                alias_of(cfg, &target.name, artifact, &root.join("lib"), &mut out.items);
            }
            headers(sess, tid, cfg, &plan.build_dir, &mut out);
        }
    }

    // 入れた実行ファイルが実行時に要する共有ライブラリ。計画に載っている
    // ものは、要求した目標のリンク閉包に在るものだけである。
    //
    // 別のパッケージのものも写す。パッケージが配布の単位である（ADR-0038）
    // 以上これは踏み越えだが、写さなければ入れた実行ファイルは動かない。
    for lib in &plan.shared_libraries {
        let Some(name) = lib.file_name() else { continue };
        let to = root.join("lib").join(name);
        if out.items.iter().any(|i| i.destination() == to) {
            continue;
        }
        out.items.push(Item::Copy { from: lib.clone(), to });
        if let Some(tid) = plan.artifacts.iter().find(|(_, p)| *p == lib).map(|(t, _)| *t) {
            alias_of(cfg, &sess.target(tid).name, lib, &root.join("lib"), &mut out.items);
        }
    }

    // pkg-config の記述は、入れるライブラリが出揃ってから書く。挙げてよい
    // `Requires` は、この実行で実際に書いた記述だけだからである（ADR-0043）。
    let described: Vec<TargetId> = targets
        .iter()
        .copied()
        .filter(|t| sess.target(*t).kind == TableKind::Lib)
        .filter(|t| plan.artifacts.contains_key(t))
        .collect();
    let installed_libs: Vec<String> =
        described.iter().map(|t| sess.package(sess.target(*t).package).name.clone()).collect();
    for &tid in &described {
        out.items.push(Item::Write {
            at: root.join("lib/pkgconfig").join(format!("{}.pc", sess.target(tid).name)),
            text: pkgconfig(sess, tid, graph, cfg, prefix, &installed_libs, &described),
        });
    }

    out.items.sort_by(|a, b| a.destination().cmp(b.destination()));
    out.items.dedup_by(|a, b| a.destination() == b.destination());
    out
}

/// 版付きの共有ライブラリに添える、版を持たない名前（ADR-0040）。
///
/// 共有ライブラリでない成果物には何も添えない。書庫は版を持たず、
/// `-lcore` が当たる名前がそれ自身である。
fn alias_of(cfg: &Config, target: &str, library: &Path, lib_dir: &Path, items: &mut Vec<Item>) {
    if !toolstyle::has_link_name_alias(cfg) {
        return;
    }
    let link_name = toolstyle::shared_library_link_name(cfg, target);
    let Some(real) = library.file_name().map(|n| n.to_string_lossy().to_string()) else { return };
    if real == link_name {
        return;
    }
    items.push(Item::Link { at: lib_dir.join(&link_name), to: real });
}

/// 公開しているヘッダ。`public` の `includes` が指すディレクトリの中身である。
///
/// 探索路をそのまま写すのは推測ではない。`public.includes` は「使う側の
/// 探索路に載る」と述べた宣言であり、そこから辿れるものは既に面である。
fn headers(sess: &Session, tid: TargetId, cfg: &Config, build_dir: &Path, out: &mut Entries) {
    let include_dir = out.include_root.clone();
    // 使う側と同じ条件で読むために要るもの（ADR-0060）。`.h` をどちらの言語で
    // 読むかを決めるターゲットの言語と、その言語で使う側の行に載る語である。
    // 語は言語ごとに違うので、両方を1度ずつ求めてヘッダごとに選ぶ——面が
    // 大きいほど、ここを1つずつ求め直す方が高くつく。
    let from_cxx = crate::plan::compiles_cxx(sess, tid, cfg);
    let c_words =
        crate::plan::consumer_words(sess, tid, cfg, build_dir, crate::toolstyle::HeaderLanguage::C);
    let cxx_words = crate::plan::consumer_words(
        sess,
        tid,
        cfg,
        build_dir,
        crate::toolstyle::HeaderLanguage::Cxx,
    );

    for (dir, site) in crate::plan::public_include_dirs(sess, tid, cfg) {
        if !dir.is_dir() {
            let mut d = Diagnostic::warning(
                "uninstallable-headers",
                format!("`{}` is not a directory; its headers are not installed", dir.display()),
            );
            if let Some(s) = site {
                d = d.at(s.file, s.span, "a consumer compiles against this");
            }
            out.diagnostics.push(
                d.note("`public.includes` names the directories a consumer compiles against"),
            );
            continue;
        }
        let files = files_under(&dir);
        report_sources_among_headers(&dir, &files, site, &mut out.diagnostics);
        for file in &files {
            let Ok(rel) = file.strip_prefix(&dir) else { continue };
            let to = include_dir.join(rel);
            // 配った面は、配ったものだけで読めなければならない（ADR-0060）。
            // 直す先は、それを配ると決めたこの宣言である。
            let language = crate::surface::language(&to, from_cxx);
            let words = match language {
                crate::toolstyle::HeaderLanguage::C => c_words.clone(),
                crate::toolstyle::HeaderLanguage::Cxx => cxx_words.clone(),
            };
            out.headers.push(crate::surface::Header { at: to.clone(), site, language, words });
            out.items.push(Item::Copy { from: file.clone(), to });
        }
    }
}

/// 配る面の中に、dowel が翻訳できる綴りのファイルが在れば述べる
/// （[ADR-0059](../../../docs/adr/0059-an-interface-directory-holds-the-interface.md)）。
///
/// **濾さずに述べる。** `public.includes` は「使う側の探索路に載る」と述べた
/// 宣言であり、ディレクトリごと載る。一部だけ配れば、`#include "impl.c"` の
/// ような書き方をする単一ファイルのライブラリが壊れる——何を配るかを
/// 拡張子から決めるのは推測である。
///
/// 述べるのは、それが**版図の取り違え**だからである。ヘッダとソースが同じ
/// ディレクトリに在る構成では、宣言が二役を負う——「翻訳時にどこを探すか」と
/// 「何を配るか」——ことになり、配られる側からは `include/` に `.c` が並んで
/// 見える。
fn report_sources_among_headers(
    dir: &Path,
    files: &[PathBuf],
    site: Option<dowel_eval::Site>,
    diags: &mut Vec<Diagnostic>,
) {
    let sources: Vec<String> = files
        .iter()
        .filter(|f| crate::plan::is_source(f))
        .filter_map(|f| f.strip_prefix(dir).ok())
        .map(|r| r.display().to_string())
        .collect();
    if sources.is_empty() {
        return;
    }
    // 宣言1つに1件。ファイルごとに出すと、木の大きさだけ同じことを言う
    // （issue #158 と同じ判断）。
    let shown = if sources.len() > 3 {
        format!("{}, and {} more", sources[..3].join(", "), sources.len() - 3)
    } else {
        sources.join(", ")
    };
    let mut d = Diagnostic::warning(
        "source-among-headers",
        format!(
            "`{}` holds {} that dowel compiles, and install ships them as the interface",
            dir.display(),
            if sources.len() == 1 {
                "a file".to_string()
            } else {
                format!("{} files", sources.len())
            }
        ),
    );
    if let Some(s) = site {
        d = d.at(s.file, s.span, "a consumer compiles against this directory");
    }
    diags.push(
        d.note(format!("they land under `include/`: {shown}"))
            .note("the whole directory is shipped, unfiltered: a header-only library may `#include` a `.c`, and dowel does not guess which files are the interface")
            .note("put the headers in a directory of their own if that is not what you meant"),
    );
}

/// 木の下のファイル。順序を決めておくのは、表示が走るたびに変わらないため。
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(e.path()),
                Ok(t) if t.is_file() => out.push(e.path()),
                _ => {}
            }
        }
    }
    out.sort();
    out
}

/// `destdir` の下に `prefix` を継ぐ。`prefix` の根は落とす。
///
/// `--destdir=/tmp/pkg --prefix=/usr` は `/tmp/pkg/usr` である。継ぎ方を
/// 間違えると、段取り用のつもりのディレクトリが実際の `/usr` になる。
fn join_prefix(destdir: &Path, prefix: &Path) -> PathBuf {
    let mut out = destdir.to_path_buf();
    // 根も `.` も `..` も継がない。段取り用のディレクトリの外へ出る
    // 書き込みは、`--destdir` の意味を壊す。
    for c in prefix.components() {
        if let std::path::Component::Normal(s) = c {
            out.push(s);
        }
    }
    out
}

/// 実際に置く。既に在るものは置き換える。
pub fn perform(items: &[Item]) -> Result<(), String> {
    for item in items {
        let dest = item.destination();
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        match item {
            Item::Copy { from, to } => {
                // 先に外す。上書きは、置き換えるつもりの先が記号連結だった
                // 場合にその指す先を書いてしまう。
                let _ = std::fs::remove_file(to);
                std::fs::copy(from, to).map_err(|e| {
                    format!("cannot copy {} to {}: {e}", from.display(), to.display())
                })?;
            }
            Item::Write { at, text } => {
                std::fs::write(at, text)
                    .map_err(|e| format!("cannot write {}: {e}", at.display()))?;
            }
            Item::Link { at, to } => {
                let _ = std::fs::remove_file(at);
                #[cfg(unix)]
                std::os::unix::fs::symlink(to, at)
                    .map_err(|e| format!("cannot place {}: {e}", at.display()))?;
                #[cfg(not(unix))]
                let _ = to;
            }
        }
    }
    Ok(())
}

// --- pkg-config の記述（[ADR-0043](../../../docs/adr/0043-pkgconfig-generation.md)）---

/// 入れたライブラリを、dowel を知らない道具から見つけられるようにする。
///
/// dowel は `.pc` を**読む**側であり（[ADR-0015]）、書く側が無かった。
/// 結果として「ライブラリを dowel へ移すには使う側も全部同時に移す」ことに
/// なり、これは漸進的な導入という前提と正面から反する。
///
/// 書く内容は既に宣言されているものだけである。公開の面は `public` に
/// 書かれており、`.pc` はそれを別の記法で述べ直したものにすぎない。
///
/// [ADR-0015]: ../../../docs/adr/0015-version-deps-pkgconfig.md
fn pkgconfig(
    sess: &Session,
    tid: TargetId,
    graph: &Graph,
    cfg: &Config,
    prefix: &Path,
    installed_libs: &[String],
    described: &[TargetId],
) -> String {
    let target = sess.target(tid);
    let pkg = sess.package(target.package);
    let mut s = String::new();
    // 記録するのは `prefix` であって、段取り用のディレクトリではない
    // （ADR-0041）。入れた先を指さない記述は、その場では正しく見えて
    // 配った先で外れる。
    s.push_str(&format!("prefix={}\n", prefix.display()));
    s.push_str("exec_prefix=${prefix}\n");
    s.push_str("libdir=${prefix}/lib\n");
    s.push_str("includedir=${prefix}/include\n\n");
    s.push_str(&format!("Name: {}\n", target.name));
    // `Description` は pkg-config が要求する。書かれていなければ名前で代える
    // ——空の記述はファイルを不正にする。
    let description =
        if pkg.description.is_empty() { target.name.clone() } else { pkg.description.clone() };
    s.push_str(&format!("Description: {description}\n"));
    s.push_str(&format!("Version: {}\n", pkg.version));

    // 名指しできるのは、この実行で実際に書いた記述だけである。無い `.pc` を
    // 挙げると pkg-config はその場で失敗する——黙って落とすより悪い。
    //
    // 乗っているライブラリを先に挙げる。静的な書庫は自分の要件を運べない
    // ので、名指さなければ使う側は未定義参照を受け取る——同じ実行で隣に
    // 書いた記述であり、「確かに在るもの」の条件を満たす唯一の場合である
    // （issue #156）。順序はリンク順（依存元が先）である。
    let mut requires: Vec<String> = graph
        .link_closure(tid)
        .into_iter()
        .filter(|t| *t != tid && described.contains(t))
        .map(|t| sess.target(t).name.clone())
        .collect();
    requires.extend(pkg.deps.iter().filter_map(|d| match &d.kind {
        // システムのパッケージは pkg-config の名前がそのまま鍵である。
        dowel_model::package::DepKind::PkgConfig { min_version } => {
            Some(format!("{} >= {min_version}", d.name))
        }
        _ if installed_libs.contains(&d.name) => Some(d.name.clone()),
        _ => None,
    }));
    if !requires.is_empty() {
        s.push_str(&format!("Requires: {}\n", requires.join(", ")));
    }

    let mut cflags = Vec::new();
    if !crate::plan::public_include_dirs(sess, tid, cfg).is_empty() {
        cflags.push("-I${includedir}".to_string());
    }
    // 公開の定義と旗も面の一部である。dowel の利用者が受け取るものと、
    // pkg-config の利用者が受け取るものが違ってはならない。
    cflags.extend(crate::plan::public_words(sess, tid, cfg));
    if !cflags.is_empty() {
        s.push_str(&format!("Cflags: {}\n", cflags.join(" ")));
    }
    // 公開の `link_flags` も面の一部である。静的ライブラリが `-lm` を要する
    // なら、それは使う側のリンク行に載らなければならない。
    let mut libs = vec!["-L${libdir}".to_string(), format!("-l{}", target.name)];
    libs.extend(crate::plan::public_link_flags(sess, tid, cfg));
    s.push_str(&format!("Libs: {}\n", libs.join(" ")));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destdir_carries_the_prefix_without_its_root() {
        assert_eq!(
            join_prefix(Path::new("/tmp/pkg"), Path::new("/usr")),
            Path::new("/tmp/pkg/usr")
        );
        assert_eq!(
            join_prefix(Path::new("/tmp/pkg"), Path::new("/usr/local")),
            Path::new("/tmp/pkg/usr/local")
        );
        // 相対の prefix も同じ扱いになる。`..` は継がない——段取り用の
        // ディレクトリの外に出る書き込みは、`--destdir` の意味を壊す。
        assert_eq!(
            join_prefix(Path::new("/tmp/pkg"), Path::new("../etc")),
            Path::new("/tmp/pkg/etc")
        );
    }
}

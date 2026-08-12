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
use dowel_model::{Session, TargetId};
use dowel_support::Diagnostic;
use std::path::{Path, PathBuf};

/// 入れる先に置く1件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    /// ビルド木の中の1ファイルを写す
    Copy { from: PathBuf, to: PathBuf },
    /// 版を持たない名前を、版付きの実体の隣に置く（ADR-0040）
    Link { at: PathBuf, to: String },
}

impl Item {
    /// 置かれる場所。表示と重複除去が読む。
    pub fn destination(&self) -> &Path {
        match self {
            Item::Copy { to, .. } => to,
            Item::Link { at, .. } => at,
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
    cfg: &Config,
    prefix: &Path,
    destdir: Option<&Path>,
    targets: &[TargetId],
) -> (Vec<Item>, Vec<Diagnostic>) {
    let mut items: Vec<Item> = Vec::new();
    let mut diags = Vec::new();
    let root = destdir.map(|d| join_prefix(d, prefix)).unwrap_or_else(|| prefix.to_path_buf());

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
        items.push(Item::Copy { from: artifact.clone(), to: root.join(dir).join(name) });

        if target.kind == TableKind::Lib {
            if plan.shared_libraries.contains(artifact) {
                alias_of(cfg, &target.name, artifact, &root.join("lib"), &mut items);
            }
            headers(sess, tid, cfg, &root.join("include"), &mut items, &mut diags);
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
        if items.iter().any(|i| i.destination() == to) {
            continue;
        }
        items.push(Item::Copy { from: lib.clone(), to });
        if let Some(tid) = plan.artifacts.iter().find(|(_, p)| *p == lib).map(|(t, _)| *t) {
            alias_of(cfg, &sess.target(tid).name, lib, &root.join("lib"), &mut items);
        }
    }

    items.sort_by(|a, b| a.destination().cmp(b.destination()));
    items.dedup_by(|a, b| a.destination() == b.destination());
    (items, diags)
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
fn headers(
    sess: &Session,
    tid: TargetId,
    cfg: &Config,
    include_dir: &Path,
    items: &mut Vec<Item>,
    diags: &mut Vec<Diagnostic>,
) {
    for dir in crate::plan::public_include_dirs(sess, tid, cfg) {
        if !dir.is_dir() {
            diags.push(
                Diagnostic::warning(
                    "uninstallable-headers",
                    format!(
                        "`{}` is not a directory; its headers are not installed",
                        dir.display()
                    ),
                )
                .note("`public.includes` names the directories a consumer compiles against"),
            );
            continue;
        }
        for file in files_under(&dir) {
            let Ok(rel) = file.strip_prefix(&dir) else { continue };
            items.push(Item::Copy { from: file.clone(), to: include_dir.join(rel) });
        }
    }
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

//! `exports` 宣言と共有ライブラリの公開シンボルの照合
//! （[ADR-0039](../../../docs/adr/0039-exports-are-checked.md)）。
//!
//! `exports` のシンボル名を誤っても、共有ライブラリのビルドが成功する場合がある。
//! そのシンボルは公開されず、利用側のリンク時に undefined reference が発生する。
//! この検査は公開シンボルの不足を提供側のビルドで検出する。
//!
//! リンカには頼めない。`-Wl,-u` も `--no-undefined` も、共有ライブラリが
//! 未定義記号を持ちうる以上、欠けた記号を誤りにしない。
//!
//! ビルド済みライブラリのシンボル一覧を外部ツールで取得して照合する。
//! オブジェクトファイル形式の解析は外部ツールに任せる（ADR-0001）。

use crate::plan::DeclaredExports;
use crate::toolstyle;
use dowel_eval::Config;
use dowel_support::{log_debug, Diagnostic};
use std::process::Command;

/// 宣言した記号が実際に書き出されているか確かめる。
///
/// 検査ツールを起動できない場合は検査を省略し、ビルド自体は失敗させない。
pub fn check(declared: &[DeclaredExports], cfg: &Config) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    if declared.is_empty() {
        return diags;
    }
    let tool = cfg.tool("nm").to_string();
    if !crate::exec::program_exists(&tool) {
        log_debug!("exports: `{tool}` is not on PATH; not checking the surface");
        return diags;
    }

    for d in declared {
        // 成果物が無いのは、ビルドがそこまで進まなかったということである。
        // 面の誤りとして報告する場面ではない。
        if !d.library.is_file() {
            continue;
        }
        let args = toolstyle::list_exports(cfg, &d.library);
        let out = match Command::new(&tool).args(&args).output() {
            Ok(o) if o.status.success() => o,
            // 起動できた上での失敗も、面の誤りの証拠にはならない。
            other => {
                log_debug!(
                    "exports: `{tool}` did not answer for {}: {other:?}",
                    d.library.display()
                );
                continue;
            }
        };
        let found = toolstyle::parse_exports(cfg, &String::from_utf8_lossy(&out.stdout));
        log_debug!("exports: {} exports {} symbol(s)", d.library.display(), found.len());

        for name in &d.names {
            // 比べるのはリンカが見る綴りである。Mach-O の `_` は dowel が
            // 付けたものなので、比べる前に外す（ADR-0039）。
            let bare = name.as_str();
            if found.iter().any(|f| f == bare || f.strip_prefix('_') == Some(bare)) {
                continue;
            }
            let mut diag = Diagnostic::error(
                "unexported-symbol",
                format!("`{name}` is declared in `exports` but the library does not export it"),
            )
            .at(d.site.file, d.site.span, "declared here")
            .note(format!("asked `{tool}` about {}", d.library.display()));
            if let Some(c) = dowel_support::diag::closest(bare, found.iter().map(|s| s.as_str())) {
                diag = diag.note(format!("the library does export `{c}`"));
            }
            diags.push(
                diag.note("a misspelling here is silent until a consumer fails to link against it"),
            );
        }
    }
    diags
}

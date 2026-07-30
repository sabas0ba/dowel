//! `version` 依存の解決（pkg-config への委譲、[ADR-0015]）。
//!
//! C の世界に単一レジストリは無い（docs/00-overview.md 1節）。依存供給は
//! 委譲する決定（[ADR-0001]）に従い、`version = "..."` はシステムの
//! pkg-config で解決する。実在と版の下限を確かめ、`--cflags` / `--libs` を
//! 外部ノードの公開プロパティとして取り込む。
//!
//! 版の制約は**下限**（`--atleast-version`）である。比較の実装を自前に
//! 持たず、pkg-config 自身の判定に委ねる。
//!
//! [ADR-0001]: ../../../docs/adr/0001-toolchain-vs-supply.md
//! [ADR-0015]: ../../../docs/adr/0015-version-deps-pkgconfig.md

use dowel_eval::Site;
use dowel_support::{log_debug, Diagnostic};
use std::process::Command;

pub struct Resolved {
    pub version: String,
    /// `--cflags` の語。`-I...` を含む
    pub cflags: Vec<String>,
    /// `--libs` の語。`-L...` / `-l...` を含む
    pub libs: Vec<String>,
}

/// pkg-config で1つのモジュールを解決する。
pub fn resolve(name: &str, min_version: &str, site: Site) -> Result<Resolved, Box<Diagnostic>> {
    let fail = |what: String, hint: &str| {
        Box::new(
            Diagnostic::error(
                "unsatisfied-dependency",
                format!("system package `{name}` {what}"),
            )
            .at(site.file, site.span, "declared here")
            .note(hint.to_string())
            .note("`version` dependencies resolve through pkg-config (docs/adr/0015-version-deps-pkgconfig.md)"),
        )
    };

    let version = match query(name, &["--modversion"]) {
        Ok(v) => v.trim().to_string(),
        Err(e) => {
            return Err(fail(
                "was not found by pkg-config".into(),
                &format!("{e}. install it, or declare the dependency with `path` or `git`"),
            ))
        }
    };
    if !min_version.is_empty()
        && query(name, &[&format!("--atleast-version={min_version}")]).is_err()
    {
        return Err(fail(
            format!("is version {version}, which does not satisfy >= {min_version}"),
            "the constraint is a minimum version; lower it or update the system package",
        ));
    }

    let cflags = words(&query(name, &["--cflags"]).unwrap_or_default());
    let libs = words(&query(name, &["--libs"]).unwrap_or_default());
    log_debug!(
        "pkg-config `{name}` {version}: {} cflag(s), {} lib flag(s)",
        cflags.len(),
        libs.len()
    );
    Ok(Resolved { version, cflags, libs })
}

fn query(name: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new("pkg-config")
        .args(args)
        .arg(name)
        .output()
        .map_err(|e| format!("cannot run pkg-config: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(if err.trim().is_empty() {
            "pkg-config declined".to_string()
        } else {
            err.trim().to_string()
        })
    }
}

fn words(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_string).collect()
}

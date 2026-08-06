//! direct バックエンド。逐次実行。
//!
//! 外部の生成器が無い環境でも動き、何より「生成器の挙動に依存せず
//! ビルドグラフ自体が正しいか」を切り分けられる。
//!
//! 最新性は素朴な mtime 比較で判定する。ヘッダ依存はコンパイラが書いた
//! depfile を読む。ここで作った機構は将来、内容アドレスによるアクション
//! キャッシュへ置き換わる（docs/20-architecture.md 8節）。

use crate::backend::{Backend, BuildGraph, Step};
use crate::exec::{CommandLog, Failure};
use dowel_support::{log_debug, log_info, log_trace};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

pub struct Direct;

impl Backend for Direct {
    fn name(&self) -> &'static str {
        "direct"
    }

    /// 書き出すものが無い。実行そのものがこのバックエンドの出力である。
    fn emit(&self, _g: &BuildGraph) -> Result<Vec<PathBuf>, Failure> {
        Ok(Vec::new())
    }

    fn run(&self, g: &BuildGraph, _jobs: Option<usize>) -> Result<(), Failure> {
        let mut ran = 0usize;
        let mut skipped = 0usize;
        let previous = CommandLog::load(&g.build_dir);
        for i in g.order() {
            let step = &g.steps[i];
            // コマンドが変わっていれば、時刻を見るまでもなく作り直す。
            if !previous.matches(step) {
                log_trace!("  stale: the command changed since the last run");
            } else if is_up_to_date(step) {
                log_trace!("up to date: {}", step.description);
                skipped += 1;
                continue;
            }
            run_step(g, step)?;
            ran += 1;
        }
        log_debug!("ran {ran} steps, skipped {skipped} already up to date");
        Ok(())
    }
}

fn run_step(g: &BuildGraph, step: &Step) -> Result<(), Failure> {
    for out in &step.outputs {
        if let Some(parent) = out.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    log_info!("{}", step.description);
    log_debug!("  {}", step.command_line());

    let mut cmd = Command::new(&step.program);
    cmd.args(&step.arguments);
    cmd.current_dir(&g.build_dir);
    let out = cmd.output().map_err(|e| {
        Failure::of(
            &step.description,
            step.command_line(),
            format!("{e} (cannot start `{}`)", step.program),
        )
    })?;
    if !out.status.success() {
        return Err(Failure {
            description: step.description.clone(),
            command: step.command_line(),
            status: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        });
    }
    Ok(())
}

/// 出力が全ての入力より新しいか。
///
/// 「なぜ再実行されたのか（されなかったのか）」は最も問い合わせの多い挙動である。
/// 判断の根拠を trace に落としておく。
fn is_up_to_date(step: &Step) -> bool {
    // 出力が1つでも欠けていれば再実行する。
    let mut oldest_output: Option<SystemTime> = None;
    for out in &step.outputs {
        let Some(t) = mtime(out) else {
            log_trace!("  stale: output missing {}", out.display());
            return false;
        };
        oldest_output = Some(oldest_output.map_or(t, |cur: SystemTime| cur.min(t)));
    }
    let Some(oldest_output) = oldest_output else { return false };

    let mut inputs: Vec<PathBuf> = step.inputs.clone();
    if let Some(d) = &step.depfile {
        // depfile が宣言されているのに無い場合、このステップのヘッダ依存は
        // 1件も分からない。情報が無い状態で「最新である」と結論すると、
        // 別の機構（かつての ninja の `deps = gcc` など）が `.d` を畳んだ
        // ツリーで、ヘッダの変更が黙って見落とされる（issue #41）。
        // 保守的に組み直し、`.d` を作り直す。
        if !d.exists() {
            log_trace!("  stale: no dependency record ({} is missing)", d.display());
            return false;
        }
        inputs.extend(read_depfile(d));
    }
    for input in &inputs {
        match mtime(input) {
            // 入力が消えているなら再実行して誤りを表に出す。
            None => {
                log_trace!("  stale: input missing {}", input.display());
                return false;
            }
            Some(t) if t > oldest_output => {
                log_trace!("  stale: {} is newer than the output", input.display());
                return false;
            }
            Some(_) => {}
        }
    }
    true
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).ok().and_then(|m| m.modified().ok())
}

/// make 形式の depfile から依存を読む。
///
/// `target: a.h b.h \` の形。行末の `\` による継続と、
/// 空白のエスケープ（`\ `）を扱う。
pub fn read_depfile(path: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    let joined = text.replace("\\\n", " ").replace("\\\r\n", " ");
    let Some((_, rhs)) = joined.split_once(':') else { return Vec::new() };

    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = rhs.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&' ') => {
                cur.push(' ');
                chars.next();
            }
            c if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(PathBuf::from(std::mem::take(&mut cur)));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(PathBuf::from(cur));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-scratch");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reads_continuation_lines_in_a_depfile() {
        let p = scratch().join("depfile-test.d");
        std::fs::write(&p, "a.o: src/a.c \\\n  include/a.h \\\n  include/b.h\n").unwrap();
        let deps = read_depfile(&p);
        assert_eq!(
            deps,
            vec![
                PathBuf::from("src/a.c"),
                PathBuf::from("include/a.h"),
                PathBuf::from("include/b.h")
            ]
        );
    }

    #[test]
    fn reads_paths_containing_spaces() {
        let p = scratch().join("depfile-space.d");
        std::fs::write(&p, "a.o: my\\ dir/a.h\n").unwrap();
        assert_eq!(read_depfile(&p), vec![PathBuf::from("my dir/a.h")]);
    }

    #[test]
    fn a_missing_depfile_is_empty() {
        assert!(read_depfile(Path::new("/nonexistent/x.d")).is_empty());
    }
}

//! ninja ファイルの生成。
//!
//! 実行層は当面 ninja をそのまま使う（docs/00-overview.md 7節）。
//! 隔離実行と CAS へ置き換える経路は閉じていないが、先に作る必要はない。
//!
//! コマンドは規則ではなく辺ごとの変数 `$cmd` に置く。フラグの組み立てを
//! ninja の変数展開に分散させず、1箇所（`Action::command_line`）に閉じるためである。
//! ninja 側の引用規則と自前の引用規則が二重に効く事故を避ける。

use crate::action::ActionKind;
use crate::plan::Plan;

pub fn generate(plan: &Plan) -> String {
    let mut out = String::new();
    out.push_str("# このファイルは dowel が生成した。手で編集しない。\n");
    out.push_str("ninja_required_version = 1.10\n\n");

    out.push_str("rule cc\n");
    out.push_str("  command = $cmd\n");
    out.push_str("  description = $desc\n");
    out.push_str("  depfile = $depfile\n");
    // ヘッダ依存は深さ優先の再走査ではなくコンパイラの出力から取る。
    out.push_str("  deps = gcc\n\n");

    out.push_str("rule ar\n");
    out.push_str("  command = rm -f $out && $cmd\n");
    out.push_str("  description = $desc\n\n");

    out.push_str("rule link\n");
    out.push_str("  command = $cmd\n");
    out.push_str("  description = $desc\n\n");

    for action in &plan.actions {
        let outputs: Vec<String> =
            action.outputs.iter().map(|p| path(&p.display().to_string())).collect();
        let inputs: Vec<String> =
            action.inputs.iter().map(|p| path(&p.display().to_string())).collect();
        out.push_str(&format!(
            "build {}: {} {}\n",
            outputs.join(" "),
            action.kind.name(),
            inputs.join(" ")
        ));
        out.push_str(&format!("  cmd = {}\n", value(&action.command_line())));
        out.push_str(&format!("  desc = {}\n", value(&action.description)));
        if action.kind == ActionKind::Compile {
            if let Some(d) = &action.depfile {
                out.push_str(&format!("  depfile = {}\n", value(&d.display().to_string())));
            }
        }
        out.push('\n');
    }

    // 既定のターゲットは要求されたものの成果物。
    let defaults: Vec<String> = plan
        .requested
        .iter()
        .filter_map(|t| plan.artifacts.get(t))
        .map(|p| path(&p.display().to_string()))
        .collect();
    if !defaults.is_empty() {
        out.push_str(&format!("default {}\n", defaults.join(" ")));
    }
    out
}

/// パスの位置での ninja エスケープ。空白と `:` が区切りとして効くため。
fn path(s: &str) -> String {
    s.replace('$', "$$").replace(' ', "$ ").replace(':', "$:")
}

/// 変数値の位置での ninja エスケープ。
fn value(s: &str) -> String {
    s.replace('$', "$$").replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn パスのエスケープ() {
        assert_eq!(path("/a/b c:d$e"), "/a/b$ c$:d$$e");
    }

    #[test]
    fn 変数値のエスケープ() {
        assert_eq!(value("cc -DX=$HOME"), "cc -DX=$$HOME");
    }
}

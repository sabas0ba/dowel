//! `compile_commands.json` の出力。
//!
//! clangd への供給（docs/30-devexp.md 3.1節）。単一構成しか表現できない形式であり、
//! C++20 モジュールの情報も持たないが、現時点で唯一の接点である。
//!
//! `command` 文字列ではなく `arguments` 配列で書く。引用を経由しないため、
//! 空白を含むパスで壊れない。

use crate::plan::Plan;
use dowel_support::json::JsonWriter;

pub fn generate(plan: &Plan) -> String {
    let mut w = JsonWriter::pretty();
    w.begin_array();
    for cc in &plan.compile_commands {
        w.begin_object();
        w.field_str("directory", &cc.directory.display().to_string());
        w.field_str("file", &cc.file.display().to_string());
        w.field_strs("arguments", cc.arguments.iter().map(|s| s.as_str()));
        w.field_str("output", &cc.output.display().to_string());
        w.end_object();
    }
    w.end_array();
    w.finish()
}

/// 書き出し先は2箇所。
///
/// - ビルドディレクトリ（構成ごとの正本）
/// - プロジェクトルート（clangd はソースの親方向にしか探さないため）
pub fn write(
    plan: &Plan,
    project_root: &std::path::Path,
) -> std::io::Result<Vec<std::path::PathBuf>> {
    let json = generate(plan);
    let mut written = Vec::new();
    std::fs::create_dir_all(&plan.build_dir)?;
    let a = plan.build_dir.join("compile_commands.json");
    std::fs::write(&a, &json)?;
    written.push(a);
    let b = project_root.join("compile_commands.json");
    std::fs::write(&b, &json)?;
    written.push(b);
    Ok(written)
}

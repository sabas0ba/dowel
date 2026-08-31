//! 独自形式のビルドグラフ（`build-graph.json`）。
//!
//! このリポジトリの外にあるバックエンドと繋ぐための接点である
//! （[ADR-0018](../../../docs/adr/0018-backend-layer.md)、
//! docs/14-build-graph.md）。書き出すのは `BuildGraph` そのもので、読み直せば
//! 等しい `BuildGraph` に戻る。ninja / make / direct が受け取るのも同じ値で
//! あるため、「この形式に何かが足りない」という状態は作れない。
//!
//! `dowel graph --kind=action --format=json` が出すのもこの文書である。
//! アクショングラフの JSON 表現は1つしかない。

use crate::action::ActionKind;
use crate::backend::{Backend, BuildGraph, Step};
use crate::exec::Failure;
use crate::toolstyle::Deps;
use dowel_support::json::{self, Json, JsonWriter};
use dowel_support::log_debug;
use std::path::PathBuf;

pub struct Graph;

pub const FILE: &str = "build-graph.json";

/// 形式の名前。読み手はこれを見て自分の知っている文書か判断する。
pub const FORMAT: &str = "dowel-build-graph";

/// 形式の版。互換でない変更のたびに上げる。
///
/// 3 は計画が生成する入力（`prepared_files`）と共有ライブラリの別名
/// （`link_aliases`）を加えた版である。版2の読み手が読み飛ばすと、export map
/// が無いか古いままになり、版付き共有ライブラリの通常名も置かれない。
///
/// 2 は作業ディレクトリ（`cwd`、[ADR-0054](../../../../docs/adr/0054-generated-sources.md)）
/// と道具の刻印（`tool_stamps`、
/// [ADR-0055](../../../../docs/adr/0055-tool-identity-in-freshness.md)）を
/// 加えた版である。どちらも版1の読み手が読み飛ばすと**別のビルドになる**
/// ——生成が違う場所で走り、刻印の無い入力に規則が無いと言われる。
pub const VERSION: u64 = 3;

impl Backend for Graph {
    fn name(&self) -> &'static str {
        "graph"
    }

    /// 成果物を作らない。走らせるのは、この文書を受け取った側である。
    fn builds(&self) -> bool {
        false
    }

    fn emit(&self, g: &BuildGraph) -> Result<Vec<PathBuf>, Failure> {
        let path = g.build_dir.join(FILE);
        std::fs::create_dir_all(&g.build_dir)
            .and_then(|_| std::fs::write(&path, render(g)))
            .map_err(|e| {
                Failure::of("writing the build graph", path.display().to_string(), e.to_string())
            })?;
        log_debug!("wrote {}", path.display());
        Ok(vec![path])
    }

    fn run(&self, g: &BuildGraph, _jobs: Option<usize>) -> Result<(), Failure> {
        self.emit(g).map(|_| ())
    }
}

pub fn render(g: &BuildGraph) -> String {
    let mut w = JsonWriter::pretty();
    w.begin_object();
    w.field_str("format", FORMAT);
    w.field_u64("version", VERSION);
    w.field_str("build_dir", &g.build_dir.display().to_string());
    // ヘッダ依存の取り方。読む側の道具も、`.d` を読むのか標準出力を拾うのかを
    // 知らなければ最新性を判定できない（ADR-0027）。
    w.field_str(
        "deps",
        match g.deps {
            Deps::Depfile => "depfile",
            Deps::ShowIncludes => "show-includes",
        },
    );
    w.key("steps").begin_array();
    for s in &g.steps {
        w.begin_object();
        w.field_u64("id", s.id as u64);
        w.field_str("kind", s.kind.name());
        w.field_str("target", &s.target);
        w.field_str("description", &s.description);
        w.field_str("program", &s.program);
        w.field_strs("arguments", s.arguments.iter().map(|a| a.as_str()));
        w.field_strs("inputs", s.inputs.iter().map(|p| p.to_str().unwrap_or("")));
        w.field_strs("outputs", s.outputs.iter().map(|p| p.to_str().unwrap_or("")));
        // depfile は無いことがある。欄そのものを落とす——`null` を置くと、
        // 読み手が「型が違う」と「無い」を区別する手間を負う。
        if let Some(d) = &s.depfile {
            w.field_str("depfile", &d.display().to_string());
        }
        // 作業ディレクトリも同じく、在るときだけ書く（ADR-0054）。
        if let Some(d) = &s.cwd {
            w.field_str("cwd", &d.display().to_string());
        }
        w.key("deps").begin_array();
        for d in &s.deps {
            w.u64(*d as u64);
        }
        w.end_array();
        w.end_object();
    }
    w.end_array();
    w.key("artifacts").begin_array();
    for (target, path) in &g.artifacts {
        w.begin_object();
        w.field_str("target", target);
        w.field_str("path", &path.display().to_string());
        w.end_object();
    }
    w.end_array();
    w.field_strs("default_outputs", g.default_outputs.iter().map(|p| p.to_str().unwrap_or("")));
    // 道具の刻印（ADR-0055）。ステップの入力に現れるので、この文書を受け取った
    // 側が書けなければグラフは走らない。
    w.key("tool_stamps").begin_array();
    for (path, identity) in &g.tool_stamps {
        w.begin_object();
        w.field_str("path", &path.display().to_string());
        w.field_str("identity", identity);
        w.end_object();
    }
    w.end_array();
    // 計画が生成する入力。計画そのものは書かず、走らせる側が内容の違う
    // ものだけを書く。外部バックエンドにも同じ事実を渡す。
    w.key("prepared_files").begin_array();
    for (path, contents) in &g.prepared_files {
        w.begin_object();
        w.field_str("path", &path.display().to_string());
        w.field_str("contents", contents);
        w.end_object();
    }
    w.end_array();
    // 版付き共有ライブラリの、版を持たない通常名。指し先は隣の実体を
    // 相対で述べる。
    w.key("link_aliases").begin_array();
    for (path, target) in &g.link_aliases {
        w.begin_object();
        w.field_str("path", &path.display().to_string());
        w.field_str("target", &target.display().to_string());
        w.end_object();
    }
    w.end_array();
    w.end_object();
    w.finish()
}

/// 文書を読み直す。
///
/// 知らない形式や版は推測せずに断る。ビルドの指示を「たぶんこうだろう」で
/// 実行するのは、間違ったものを黙って作る一番短い道である。
pub fn parse(text: &str) -> Result<BuildGraph, String> {
    let doc = json::parse(text).ok_or_else(|| "not valid JSON".to_string())?;
    match doc.get("format").and_then(|v| v.as_str()) {
        Some(FORMAT) => {}
        Some(other) => return Err(format!("not a {FORMAT} document (format is `{other}`)")),
        None => return Err("not a build graph document (no `format`)".into()),
    }
    match doc.get("version").and_then(|v| v.as_i64()) {
        Some(v) if v == VERSION as i64 => {}
        Some(v) => return Err(format!("version {v} is not readable (this build reads {VERSION})")),
        None => return Err("no `version`".into()),
    }
    let build_dir = PathBuf::from(str_field(&doc, "build_dir")?);

    let mut steps = Vec::new();
    for s in array(&doc, "steps")? {
        let kind_name = str_field(s, "kind")?;
        let kind = ActionKind::parse(kind_name)
            .ok_or_else(|| format!("`{kind_name}` is not a step kind"))?;
        steps.push(Step {
            id: s.get("id").and_then(|v| v.as_i64()).ok_or("a step has no `id`")? as usize,
            kind,
            target: str_field(s, "target")?.to_string(),
            description: str_field(s, "description")?.to_string(),
            program: str_field(s, "program")?.to_string(),
            arguments: strings(s, "arguments")?,
            inputs: strings(s, "inputs")?.into_iter().map(PathBuf::from).collect(),
            outputs: strings(s, "outputs")?.into_iter().map(PathBuf::from).collect(),
            depfile: s.get("depfile").and_then(|v| v.as_str()).map(PathBuf::from),
            deps: array(s, "deps")?
                .iter()
                .map(|v| v.as_i64().map(|n| n as usize).ok_or("a dependency is not a number"))
                .collect::<Result<_, _>>()?,
            cwd: s.get("cwd").and_then(|v| v.as_str()).map(PathBuf::from),
        });
    }

    let mut artifacts = Vec::new();
    for a in array(&doc, "artifacts")? {
        artifacts.push((str_field(a, "target")?.to_string(), PathBuf::from(str_field(a, "path")?)));
    }

    let deps = match doc.get("deps").and_then(|v| v.as_str()) {
        Some("show-includes") => Deps::ShowIncludes,
        Some("depfile") | None => Deps::Depfile,
        Some(other) => return Err(format!("`deps` is `{other}`, which this build does not know")),
    };
    // 刻印を持たないビルドはありうる（アクションが1つも無い）。版が合って
    // いる以上、無いことは「書くものが無い」であって古い文書ではない。
    let mut tool_stamps = Vec::new();
    for t in doc.get("tool_stamps").and_then(|v| v.as_array()).unwrap_or(&[]) {
        tool_stamps
            .push((PathBuf::from(str_field(t, "path")?), str_field(t, "identity")?.to_string()));
    }
    let mut prepared_files = Vec::new();
    for f in doc.get("prepared_files").and_then(|v| v.as_array()).unwrap_or(&[]) {
        prepared_files
            .push((PathBuf::from(str_field(f, "path")?), str_field(f, "contents")?.to_string()));
    }
    let mut link_aliases = Vec::new();
    for a in doc.get("link_aliases").and_then(|v| v.as_array()).unwrap_or(&[]) {
        link_aliases
            .push((PathBuf::from(str_field(a, "path")?), PathBuf::from(str_field(a, "target")?)));
    }

    Ok(BuildGraph {
        build_dir,
        steps,
        artifacts,
        default_outputs: strings(&doc, "default_outputs")?.into_iter().map(PathBuf::from).collect(),
        deps,
        tool_stamps,
        prepared_files,
        link_aliases,
    })
}

fn str_field<'a>(v: &'a Json, key: &str) -> Result<&'a str, String> {
    v.get(key).and_then(|v| v.as_str()).ok_or_else(|| format!("`{key}` is missing or not a string"))
}

fn array<'a>(v: &'a Json, key: &str) -> Result<&'a [Json], String> {
    v.get(key)
        .and_then(|v| v.as_array())
        .ok_or_else(|| format!("`{key}` is missing or not an array"))
}

fn strings(v: &Json, key: &str) -> Result<Vec<String>, String> {
    array(v, key)?
        .iter()
        .map(|e| {
            e.as_str().map(|s| s.to_string()).ok_or_else(|| format!("`{key}` holds a non-string"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> BuildGraph {
        BuildGraph {
            build_dir: PathBuf::from("/b"),
            steps: vec![
                Step {
                    id: 0,
                    kind: ActionKind::Compile,
                    target: "app:app".into(),
                    description: "CC a.o".into(),
                    program: "cc".into(),
                    arguments: vec!["-c".into(), "/s/a.c".into(), "-o".into(), "/b/a.o".into()],
                    inputs: vec![PathBuf::from("/s/a.c")],
                    outputs: vec![PathBuf::from("/b/a.o")],
                    depfile: Some(PathBuf::from("/b/a.o.d")),
                    deps: vec![],
                    cwd: None,
                },
                Step {
                    id: 1,
                    kind: ActionKind::Link,
                    target: "app:app".into(),
                    description: "LINK app".into(),
                    program: "cc".into(),
                    arguments: vec!["/b/a.o".into(), "-o".into(), "/b/app".into()],
                    inputs: vec![PathBuf::from("/b/a.o")],
                    outputs: vec![PathBuf::from("/b/app")],
                    depfile: None,
                    deps: vec![0],
                    cwd: None,
                },
                // 生成は作業ディレクトリを持つ（ADR-0054）。それが往復で
                // 落ちると、読み直した文書は同じ命令を別の場所で走らせる。
                Step {
                    id: 2,
                    kind: ActionKind::Generate,
                    target: "app:app".into(),
                    description: "GEN parser.c".into(),
                    program: "bison".into(),
                    arguments: vec!["-o".into(), "parser.c".into(), "/s/parser.y".into()],
                    inputs: vec![PathBuf::from("/s/parser.y")],
                    outputs: vec![PathBuf::from("/b/generated/app/app/parser/parser.c")],
                    depfile: None,
                    deps: vec![],
                    cwd: Some(PathBuf::from("/b/generated/app/app/parser")),
                },
            ],
            artifacts: vec![("app:app".into(), PathBuf::from("/b/app"))],
            deps: Deps::Depfile,
            default_outputs: vec![PathBuf::from("/b/app")],
            tool_stamps: vec![(
                PathBuf::from("/b/tools/cc-0badcafe.stamp"),
                "/usr/bin/cc:12:34".into(),
            )],
            prepared_files: vec![(
                PathBuf::from("/b/lib/core.map"),
                "{ global: core_open; local: *; };\n".into(),
            )],
            link_aliases: vec![(PathBuf::from("/b/lib/libcore.so"), PathBuf::from("libcore.so.2"))],
        }
    }

    #[test]
    fn a_document_reads_back_as_the_same_graph() {
        // この形式が実行に足りることの担保。読み直した値をバックエンドが
        // 受け取っても同じビルドになる。
        assert_eq!(parse(&render(&sample())), Ok(sample()));
    }

    #[test]
    fn a_document_names_its_format_and_version() {
        let text = render(&sample());
        assert!(text.contains("\"format\": \"dowel-build-graph\""), "{text}");
        assert!(text.contains("\"version\": 3"), "{text}");
    }

    #[test]
    fn a_step_without_a_depfile_has_no_depfile_field() {
        let text = render(&sample());
        // 鍵として数える。`deps` の値も同じ綴りを持つ。
        assert_eq!(text.matches("\"depfile\":").count(), 1, "{text}");
    }

    #[test]
    fn a_step_without_a_working_directory_has_no_cwd_field() {
        let text = render(&sample());
        assert_eq!(text.matches("\"cwd\":").count(), 1, "{text}");
    }

    #[test]
    fn a_future_version_is_refused_rather_than_guessed() {
        let text = render(&sample()).replace("\"version\": 3", "\"version\": 4");
        let e = parse(&text).unwrap_err();
        assert!(e.contains("version 4"), "{e}");
    }

    #[test]
    fn another_json_document_is_refused() {
        let e = parse("{\"format\": \"compile_commands\"}").unwrap_err();
        assert!(e.contains("compile_commands"), "{e}");
    }

    #[test]
    fn a_truncated_document_says_what_is_missing() {
        let e = parse("{\"format\": \"dowel-build-graph\", \"version\": 3}").unwrap_err();
        assert!(e.contains("build_dir"), "{e}");
    }
}

//! コマンドライン引数の解析。
//!
//! 外部 crate を使わない（[ADR-0007]）。引数解析は起動時間に直接効き、
//! かつ本システムの中核ではないため、必要な形だけを自前で持つ。
//!
//! 未知の引数には編集距離で候補を出す。診断品質への投資という方針は
//! マニフェストの診断だけでなく CLI にも掛かる（docs/30-devexp.md 4節）。
//!
//! [ADR-0007]: ../../../docs/adr/0007-implementation-language.md

use dowel_support::diag::closest;
use dowel_support::log::{Format, Level};
use std::path::PathBuf;

pub const USAGE: &str = r#"dowel — C/C++ 向けビルドシステム（開発中）

使い方:
    dowel <コマンド> [オプション]

コマンド:
    check              マニフェストを評価して診断する。ビルドしない
    build [ターゲット] ビルドする。ターゲット省略時は全ての bin と test
    why <ターゲット> <プロパティ>
                       値がそこへ来た経路を表示する
    graph              依存グラフまたはアクショングラフを書き出す
    schema dump        スキーマと構成語彙を機械可読な形で出力する

共通オプション:
    -C, --directory <パス>   このディレクトリのパッケージを対象にする（既定: .）
        --config <名前>      debug | release（既定: debug）
        --target <トリプル>  ターゲットトリプル（既定: ホスト）
        --features <名前,…>  有効にする機能フラグ
        --no-default-features
                             [features] の default を取り込まない
        --message-format <形式>
                             human | json（既定: human）
    -v, --verbose            ログを詳しくする。重ねると更に詳しくなる
        --log-level <水準>   off|error|warn|info|debug|trace（環境変数 DOWEL_LOG も可）
        --log-format <形式>  text | json
        --color <いつ>       auto | always | never
    -h, --help               この説明
    -V, --version            版

build のオプション:
        --executor <実行器>  ninja | direct（既定: ninja があれば ninja）
    -j, --jobs <数>          並列度（ninja に渡す）
        --no-compdb          compile_commands.json を書かない

graph のオプション:
        --kind <種類>        target | action（既定: target）
        --format <形式>      text | dot | json（既定: text）

why のオプション:
        --format <形式>      text | json（既定: text）

例:
    dowel check --message-format=json
    dowel graph --kind=action --format=dot | dot -Tsvg -o actions.svg
    dowel why app:app includes
    DOWEL_LOG=debug dowel build
"#;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Command {
    Check,
    Build { targets: Vec<String> },
    Why { target: String, property: String },
    Graph,
    SchemaDump,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MessageFormat {
    Human,
    Json,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GraphKind {
    Target,
    Action,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OutFormat {
    Text,
    Dot,
    Json,
}

#[derive(Clone, Debug)]
pub struct Options {
    pub command: Command,
    pub directory: PathBuf,
    pub config: String,
    pub target: Option<String>,
    pub features: Vec<String>,
    pub default_features: bool,
    pub message_format: MessageFormat,
    pub log_level: Option<Level>,
    pub log_format: Format,
    pub color: bool,
    pub executor: Option<String>,
    pub jobs: Option<usize>,
    pub compdb: bool,
    pub graph_kind: GraphKind,
    pub out_format: OutFormat,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            command: Command::Check,
            directory: PathBuf::from("."),
            config: "debug".into(),
            target: None,
            features: Vec::new(),
            default_features: true,
            message_format: MessageFormat::Human,
            log_level: None,
            log_format: Format::Text,
            color: false,
            executor: None,
            jobs: None,
            compdb: true,
            graph_kind: GraphKind::Target,
            out_format: OutFormat::Text,
        }
    }
}

pub enum Parsed {
    Run(Box<Options>),
    Help,
    Version,
}

const COMMANDS: &[&str] = &["check", "build", "why", "graph", "schema"];

pub fn parse<I: IntoIterator<Item = String>>(argv: I) -> Result<Parsed, String> {
    let args: Vec<String> = argv.into_iter().collect();
    let mut opts = Options::default();
    let mut positional: Vec<String> = Vec::new();
    let mut verbose = 0u8;
    let mut color_mode = "auto".to_string();
    let mut command: Option<String> = None;
    let mut i = 0usize;

    while i < args.len() {
        let arg = args[i].clone();
        i += 1;

        if arg == "-h" || arg == "--help" {
            return Ok(Parsed::Help);
        }
        if arg == "-V" || arg == "--version" {
            return Ok(Parsed::Version);
        }
        if arg == "-v" || arg == "--verbose" {
            verbose += 1;
            continue;
        }

        // `--name=値` と `--name 値` の双方を受ける。
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) if arg.starts_with('-') => (n.to_string(), Some(v.to_string())),
            _ => (arg.clone(), None),
        };
        let mut take = |flag: &str| -> Result<String, String> {
            match &inline {
                Some(v) => Ok(v.clone()),
                None => {
                    let v = args.get(i).cloned();
                    i += 1;
                    v.ok_or_else(|| format!("`{flag}` に値がない"))
                }
            }
        };

        match name.as_str() {
            "-C" | "--directory" => opts.directory = PathBuf::from(take("--directory")?),
            "--config" => opts.config = take("--config")?,
            "--target" => opts.target = Some(take("--target")?),
            "--features" => {
                let v = take("--features")?;
                opts.features
                    .extend(v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()));
            }
            "--no-default-features" => opts.default_features = false,
            "--message-format" => {
                opts.message_format = match take("--message-format")?.as_str() {
                    "human" => MessageFormat::Human,
                    "json" => MessageFormat::Json,
                    other => {
                        return Err(format!("`--message-format` は human か json（`{other}`）"))
                    }
                }
            }
            "--log-level" => {
                let v = take("--log-level")?;
                opts.log_level = Some(
                    Level::parse(&v).ok_or_else(|| format!("`--log-level` の値が不正: `{v}`"))?,
                );
            }
            "--log-format" => {
                opts.log_format = match take("--log-format")?.as_str() {
                    "text" => Format::Text,
                    "json" => Format::Json,
                    other => return Err(format!("`--log-format` は text か json（`{other}`）")),
                }
            }
            "--color" => color_mode = take("--color")?,
            "--executor" => opts.executor = Some(take("--executor")?),
            "-j" | "--jobs" => {
                let v = take("--jobs")?;
                opts.jobs = Some(v.parse().map_err(|_| format!("`--jobs` は数値（`{v}`）"))?);
            }
            "--no-compdb" => opts.compdb = false,
            "--kind" => {
                opts.graph_kind = match take("--kind")?.as_str() {
                    "target" => GraphKind::Target,
                    "action" => GraphKind::Action,
                    other => return Err(format!("`--kind` は target か action（`{other}`）")),
                }
            }
            "--format" => {
                opts.out_format = match take("--format")?.as_str() {
                    "text" => OutFormat::Text,
                    "dot" => OutFormat::Dot,
                    "json" => OutFormat::Json,
                    other => return Err(format!("`--format` は text / dot / json（`{other}`）")),
                }
            }
            other if other.starts_with('-') => {
                let known = [
                    "--directory",
                    "--config",
                    "--target",
                    "--features",
                    "--no-default-features",
                    "--message-format",
                    "--log-level",
                    "--log-format",
                    "--color",
                    "--executor",
                    "--jobs",
                    "--no-compdb",
                    "--kind",
                    "--format",
                    "--verbose",
                    "--help",
                    "--version",
                ];
                let mut msg = format!("未知のオプション `{other}`");
                if let Some(c) = closest(other, known) {
                    msg.push_str(&format!("。`{c}` の誤りではないか"));
                }
                return Err(msg);
            }
            _ => {
                if command.is_none() {
                    command = Some(arg);
                } else {
                    positional.push(arg);
                }
            }
        }
    }

    if verbose > 0 && opts.log_level.is_none() {
        opts.log_level = Some(if verbose == 1 { Level::Info } else { Level::Debug });
    }
    opts.color = match color_mode.as_str() {
        "always" => true,
        "never" => false,
        // 端末かどうかを判定する術を標準ライブラリだけでは持たないため、
        // 既定は色なしとする。必要なら `--color=always` を明示する。
        _ => false,
    };

    let Some(cmd) = command else { return Ok(Parsed::Help) };
    opts.command = match cmd.as_str() {
        "check" => Command::Check,
        "build" => Command::Build { targets: positional },
        "graph" => Command::Graph,
        "why" => {
            if positional.len() != 2 {
                return Err("`why` は <ターゲット> <プロパティ> の2つを取る".into());
            }
            Command::Why { target: positional[0].clone(), property: positional[1].clone() }
        }
        "schema" => match positional.first().map(|s| s.as_str()) {
            Some("dump") => Command::SchemaDump,
            Some(other) => return Err(format!("`schema` の下位コマンドは dump（`{other}`）")),
            None => return Err("`schema dump` と書く".into()),
        },
        other => {
            let mut msg = format!("未知のコマンド `{other}`");
            if let Some(c) = closest(other, COMMANDS.iter().copied()) {
                msg.push_str(&format!("。`{c}` の誤りではないか"));
            }
            return Err(msg);
        }
    };

    Ok(Parsed::Run(Box::new(opts)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Result<Options, String> {
        match parse(args.iter().map(|s| s.to_string()))? {
            Parsed::Run(o) => Ok(*o),
            Parsed::Help => Err("help".into()),
            Parsed::Version => Err("version".into()),
        }
    }

    #[test]
    fn 等号と空白の双方を受ける() {
        let a = run(&["check", "--config=release"]).unwrap();
        let b = run(&["check", "--config", "release"]).unwrap();
        assert_eq!(a.config, "release");
        assert_eq!(b.config, "release");
    }

    #[test]
    fn 機能フラグをカンマで分ける() {
        let o = run(&["check", "--features", "zlib, png"]).unwrap();
        assert_eq!(o.features, vec!["zlib", "png"]);
    }

    #[test]
    fn 位置引数がコマンドとターゲットに割り当てられる() {
        let o = run(&["build", "app", "libfoo:foo"]).unwrap();
        assert_eq!(o.command, Command::Build { targets: vec!["app".into(), "libfoo:foo".into()] });
    }

    #[test]
    fn why_は2つの位置引数を要求する() {
        assert!(run(&["why", "app"]).is_err());
        let o = run(&["why", "app", "includes"]).unwrap();
        assert_eq!(o.command, Command::Why { target: "app".into(), property: "includes".into() });
    }

    #[test]
    fn 未知のオプションに候補を出す() {
        let e = run(&["check", "--confg=release"]).unwrap_err();
        assert!(e.contains("--config"), "{e}");
    }

    #[test]
    fn 未知のコマンドに候補を出す() {
        let e = run(&["chek"]).unwrap_err();
        assert!(e.contains("check"), "{e}");
    }

    #[test]
    fn verbose_を重ねると水準が上がる() {
        assert_eq!(run(&["check", "-v"]).unwrap().log_level, Some(Level::Info));
        assert_eq!(run(&["check", "-v", "-v"]).unwrap().log_level, Some(Level::Debug));
        // 明示的な指定が優先する。
        assert_eq!(
            run(&["check", "-v", "--log-level=trace"]).unwrap().log_level,
            Some(Level::Trace)
        );
    }

    #[test]
    fn 引数なしは説明を出す() {
        assert!(matches!(parse(Vec::<String>::new()).unwrap(), Parsed::Help));
    }

    #[test]
    fn 値のないオプションは誤り() {
        assert!(run(&["check", "--config"]).is_err());
    }
}

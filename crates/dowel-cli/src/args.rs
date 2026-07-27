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

pub const USAGE: &str = r#"dowel - a build system for C/C++ (in development)

Usage:
    dowel <command> [options]

Commands:
    check              Evaluate the manifests and report diagnostics. Does not build.
    build [target]     Build. With no target, builds every bin and test.
    test [target]      Build and run test targets. With no target, runs every test.
    why <target> <property>
                       Show how a value reached a target.
    graph              Dump the dependency graph or the action graph.
    schema dump        Print the schema and configuration vocabulary in machine-readable form.

Common options:
    -C, --directory <path>   Operate on the package in this directory (default: .)
        --config <name>      debug | release (default: debug)
        --target <triple>    Target triple (default: host)
        --features <a,b>     Feature flags to enable
        --no-default-features
                             Do not pull in `default` from [features]
        --message-format <fmt>
                             human | json (default: human)
    -v, --verbose            More logging. Repeat for more.
        --log-level <level>  off|error|warn|info|debug|trace (or the DOWEL_LOG variable)
        --log-format <fmt>   text | json
        --color <when>       auto | always | never
    -h, --help               Show this help
    -V, --version            Show the version

build options:
        --executor <name>    ninja | direct (default: ninja when available)
    -j, --jobs <n>           Parallelism, passed to ninja
        --no-compdb          Do not write compile_commands.json

test options:
        --no-run             Build the test targets but do not run them
        --nocapture          Let test output through instead of capturing it

graph options:
        --kind <kind>        target | action (default: target)
        --format <fmt>       text | dot | json (default: text)

why options:
        --format <fmt>       text | json (default: text)

Examples:
    dowel check --message-format=json
    dowel graph --kind=action --format=dot | dot -Tsvg -o actions.svg
    dowel why app:app includes
    dowel test --nocapture
    DOWEL_LOG=debug dowel build
"#;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Command {
    Check,
    Build { targets: Vec<String> },
    Test { targets: Vec<String> },
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
    pub no_run: bool,
    pub nocapture: bool,
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
            no_run: false,
            nocapture: false,
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

const COMMANDS: &[&str] = &["check", "build", "test", "why", "graph", "schema"];

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
                    v.ok_or_else(|| format!("`{flag}` requires a value"))
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
                        return Err(format!(
                            "`--message-format` must be human or json (got `{other}`)"
                        ))
                    }
                }
            }
            "--log-level" => {
                let v = take("--log-level")?;
                opts.log_level = Some(
                    Level::parse(&v)
                        .ok_or_else(|| format!("invalid value for `--log-level`: `{v}`"))?,
                );
            }
            "--log-format" => {
                opts.log_format = match take("--log-format")?.as_str() {
                    "text" => Format::Text,
                    "json" => Format::Json,
                    other => {
                        return Err(format!("`--log-format` must be text or json (got `{other}`)"))
                    }
                }
            }
            "--color" => color_mode = take("--color")?,
            "--executor" => opts.executor = Some(take("--executor")?),
            "-j" | "--jobs" => {
                let v = take("--jobs")?;
                opts.jobs =
                    Some(v.parse().map_err(|_| format!("`--jobs` must be a number (got `{v}`)"))?);
            }
            "--no-compdb" => opts.compdb = false,
            "--no-run" => opts.no_run = true,
            "--nocapture" => opts.nocapture = true,
            "--kind" => {
                opts.graph_kind = match take("--kind")?.as_str() {
                    "target" => GraphKind::Target,
                    "action" => GraphKind::Action,
                    other => {
                        return Err(format!("`--kind` must be target or action (got `{other}`)"))
                    }
                }
            }
            "--format" => {
                opts.out_format = match take("--format")?.as_str() {
                    "text" => OutFormat::Text,
                    "dot" => OutFormat::Dot,
                    "json" => OutFormat::Json,
                    other => {
                        return Err(format!("`--format` must be text, dot or json (got `{other}`)"))
                    }
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
                    "--no-run",
                    "--nocapture",
                    "--kind",
                    "--format",
                    "--verbose",
                    "--help",
                    "--version",
                ];
                let mut msg = format!("unknown option `{other}`");
                if let Some(c) = closest(other, known) {
                    msg.push_str(&format!(". did you mean `{c}`?"));
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
        "test" => Command::Test { targets: positional },
        "graph" => Command::Graph,
        "why" => {
            if positional.len() != 2 {
                return Err("`why` takes two arguments: <target> <property>".into());
            }
            Command::Why { target: positional[0].clone(), property: positional[1].clone() }
        }
        "schema" => match positional.first().map(|s| s.as_str()) {
            Some("dump") => Command::SchemaDump,
            Some(other) => {
                return Err(format!("the only `schema` subcommand is dump (got `{other}`)"))
            }
            None => return Err("write `schema dump`".into()),
        },
        other => {
            let mut msg = format!("unknown command `{other}`");
            if let Some(c) = closest(other, COMMANDS.iter().copied()) {
                msg.push_str(&format!(". did you mean `{c}`?"));
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
    fn accepts_both_equals_and_space() {
        let a = run(&["check", "--config=release"]).unwrap();
        let b = run(&["check", "--config", "release"]).unwrap();
        assert_eq!(a.config, "release");
        assert_eq!(b.config, "release");
    }

    #[test]
    fn splits_feature_flags_on_commas() {
        let o = run(&["check", "--features", "zlib, png"]).unwrap();
        assert_eq!(o.features, vec!["zlib", "png"]);
    }

    #[test]
    fn positional_arguments_map_to_command_and_targets() {
        let o = run(&["build", "app", "libfoo:foo"]).unwrap();
        assert_eq!(o.command, Command::Build { targets: vec!["app".into(), "libfoo:foo".into()] });
    }

    #[test]
    fn why_requires_two_positional_arguments() {
        assert!(run(&["why", "app"]).is_err());
        let o = run(&["why", "app", "includes"]).unwrap();
        assert_eq!(o.command, Command::Why { target: "app".into(), property: "includes".into() });
    }

    #[test]
    fn suggests_a_candidate_for_an_unknown_option() {
        let e = run(&["check", "--confg=release"]).unwrap_err();
        assert!(e.contains("--config"), "{e}");
    }

    #[test]
    fn suggests_a_candidate_for_an_unknown_command() {
        let e = run(&["chek"]).unwrap_err();
        assert!(e.contains("check"), "{e}");
    }

    #[test]
    fn repeating_verbose_raises_the_level() {
        assert_eq!(run(&["check", "-v"]).unwrap().log_level, Some(Level::Info));
        assert_eq!(run(&["check", "-v", "-v"]).unwrap().log_level, Some(Level::Debug));
        // 明示的な指定が優先する。
        assert_eq!(
            run(&["check", "-v", "--log-level=trace"]).unwrap().log_level,
            Some(Level::Trace)
        );
    }

    #[test]
    fn no_arguments_prints_help() {
        assert!(matches!(parse(Vec::<String>::new()).unwrap(), Parsed::Help));
    }

    #[test]
    fn test_command_takes_targets_and_flags() {
        let o = run(&["test", "app:unit", "--no-run"]).unwrap();
        assert_eq!(o.command, Command::Test { targets: vec!["app:unit".into()] });
        assert!(o.no_run);
        assert!(!o.nocapture);
        assert!(run(&["test", "--nocapture"]).unwrap().nocapture);
    }

    #[test]
    fn an_option_without_a_value_is_an_error() {
        assert!(run(&["check", "--config"]).is_err());
    }
}

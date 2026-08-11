//! コマンドライン引数の解析。
//!
//! dowel 本体と同じく外部 crate を使わない（docs/adr/0007-implementation-language.md）。
//! dowelup は shim としてすべての `dowel` 起動の経路に入るため、
//! 依存を持たない軽さがそのまま起動時間になる。

use crate::spec::{self, Spec};
use std::path::PathBuf;

pub const USAGE: &str = r#"dowelup - fetches, pins, and switches versions of dowel

Usage:
    dowelup <command> [options]

Commands:
    install <spec>       Resolve <spec>, build that commit, and install it.
    list                 List the installed versions. `*` marks the default.
    default <spec>       Set the version used where no pin file applies.
                         Installs it first if needed.
    pin <spec>           Resolve <spec> and write the commit hash to
                         .dowel-version. Installs it first if needed.
    which                Print the path of the dowel binary that runs here.
    run <spec> [--] <args...>
                         Run an installed version directly.
    uninstall <spec>     Remove an installed version.
    shim <dir>           Create a `dowel` link in <dir> that dispatches
                         through dowelup.

Version specifiers:
    stable               The newest release tag upstream.
    nightly              The tip of the default branch.
    nightly-YYYY-MM-DD   The last commit on the default branch on that
                         date (UTC).
    X.Y.Z                The release tag vX.Y.Z or X.Y.Z.
    branch:<name>        The tip of a branch.
    tag:<name>           Any tag.
    <sha>                A commit hash; a unique prefix of at least 7 hex
                         digits is enough.

Every specifier is resolved to a commit hash when it is installed or
pinned; running `dowel` never touches the network.

Options:
    -C, --directory <path>   Operate as if started from this directory
                             (default: .)
        --upstream <url>     Fetch dowel from here (default: the
                             DOWELUP_UPSTREAM variable, then
                             https://github.com/sabas0ba/dowel)
        --from-source        Build from source instead of taking a
                             published binary. Release specifiers use a
                             release asset by default, verified against
                             its sha256; only the source build shows
                             which commit the binary came from
    -h, --help               Show this help
    -V, --version            Show the version

Selection when run as `dowel`:
    A `dowel` link created by `dowelup shim` picks a version and hands
    over to it: a leading +<spec> argument (e.g. `dowel +nightly check`),
    else the nearest .dowel-version file up the directory tree, else the
    default set by `dowelup default`.
"#;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Command {
    Install { spec: Spec },
    List,
    Default { spec: Spec },
    Pin { spec: Spec },
    Which,
    Run { needle: String, args: Vec<String> },
    Uninstall { needle: String },
    Shim { dir: PathBuf },
}

#[derive(Clone, Debug)]
pub struct Options {
    pub command: Command,
    pub directory: PathBuf,
    pub upstream: Option<String>,
    /// 事前ビルドを使わず、ソースから組む（ADR-0036）
    pub from_source: bool,
}

pub enum Parsed {
    Help,
    Version,
    Run(Box<Options>),
}

pub fn parse(argv: Vec<String>) -> Result<Parsed, String> {
    let mut directory = PathBuf::from(".");
    let mut upstream: Option<String> = None;
    let mut from_source = false;
    let mut positional: Vec<String> = Vec::new();
    let mut tail: Vec<String> = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        let a = &argv[i];
        match a.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "-V" | "--version" => return Ok(Parsed::Version),
            "-C" | "--directory" => {
                i += 1;
                directory = PathBuf::from(need(&argv, i, a)?);
            }
            "--upstream" => {
                i += 1;
                upstream = Some(need(&argv, i, a)?.to_string());
            }
            "--from-source" => from_source = true,
            _ => {
                if let Some(v) = a.strip_prefix("--directory=") {
                    directory = PathBuf::from(v);
                } else if let Some(v) = a.strip_prefix("--upstream=") {
                    upstream = Some(v.to_string());
                } else if a.starts_with('-') && a.len() > 1 {
                    return Err(format!("unknown option `{a}`; run `dowelup --help` for usage"));
                } else {
                    positional.push(a.clone());
                    // `run` の指定子より後ろは子プロセスの引数。
                    // `--help` 等が子のものか自分のものか曖昧にならないよう、
                    // ここで解析を打ち切ってそのまま渡す。
                    if positional[0] == "run" && positional.len() == 2 {
                        tail = argv[i + 1..].to_vec();
                        if tail.first().is_some_and(|t| t == "--") {
                            tail.remove(0);
                        }
                        break;
                    }
                }
            }
        }
        i += 1;
    }
    let command = command(positional, tail)?;
    Ok(Parsed::Run(Box::new(Options { command, directory, upstream, from_source })))
}

fn need<'a>(argv: &'a [String], i: usize, flag: &str) -> Result<&'a str, String> {
    argv.get(i).map(String::as_str).ok_or_else(|| format!("{flag} needs a value"))
}

fn command(positional: Vec<String>, tail: Vec<String>) -> Result<Command, String> {
    let Some((name, rest)) = positional.split_first() else {
        return Err("no command given; run `dowelup --help` for usage".to_string());
    };
    let cmd = match name.as_str() {
        "install" => Command::Install { spec: spec::parse(&exactly_one(name, rest)?)? },
        "default" => Command::Default { spec: spec::parse(&exactly_one(name, rest)?)? },
        "pin" => Command::Pin { spec: spec::parse(&exactly_one(name, rest)?)? },
        "list" => none(name, rest, Command::List)?,
        "which" => none(name, rest, Command::Which)?,
        "run" => Command::Run { needle: exactly_one(name, rest)?, args: tail },
        "uninstall" => Command::Uninstall { needle: exactly_one(name, rest)? },
        "shim" => Command::Shim { dir: PathBuf::from(exactly_one(name, rest)?) },
        _ => return Err(format!("unknown command `{name}`; run `dowelup --help` for usage")),
    };
    Ok(cmd)
}

fn exactly_one(name: &str, rest: &[String]) -> Result<String, String> {
    match rest {
        [one] => Ok(one.clone()),
        [] => Err(format!("`{name}` needs an argument; run `dowelup --help` for usage")),
        _ => Err(format!("`{name}` takes exactly one argument, got {}", rest.len())),
    }
}

fn none(name: &str, rest: &[String], cmd: Command) -> Result<Command, String> {
    if rest.is_empty() {
        Ok(cmd)
    } else {
        Err(format!("`{name}` takes no arguments"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn the_arguments_after_the_run_specifier_go_to_the_child() {
        let Ok(Parsed::Run(o)) =
            parse(argv(&["run", "nightly", "--", "check", "--config=release"]))
        else {
            panic!("run should parse");
        };
        assert_eq!(
            o.command,
            Command::Run {
                needle: "nightly".to_string(),
                args: argv(&["check", "--config=release"])
            }
        );
        // `--` が無くても、また自分のオプションと同名でも、子のものになる。
        let Ok(Parsed::Run(o)) = parse(argv(&["run", "abc1234", "--help"])) else {
            panic!("run should parse");
        };
        assert_eq!(
            o.command,
            Command::Run { needle: "abc1234".to_string(), args: argv(&["--help"]) }
        );
    }

    #[test]
    fn a_bad_specifier_is_a_usage_error() {
        assert!(parse(argv(&["install", "beta"])).is_err());
        assert!(parse(argv(&["install"])).is_err());
        assert!(parse(argv(&["frobnicate"])).is_err());
        assert!(parse(argv(&["list", "extra"])).is_err());
    }
}

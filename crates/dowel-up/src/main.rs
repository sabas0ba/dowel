//! `dowelup` — dowel 自体の取得・固定・切り替え。
//!
//! 出力の分担は dowel 本体と同じ（docs/60-cli.md）。
//!
//! - **stdout** — 成果物。解決した sha、一覧、パス
//! - **stderr** — 進行と誤り
//!
//! `dowel` という名前で起動された場合は shim として働き、選んだ版へ
//! exec する（docs/adr/0013-self-acquisition.md、docs/61-acquisition.md）。

mod acquire;
mod args;
mod proc;
mod spec;
mod store;

use args::{Command, Options, Parsed};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use store::{Home, Selection};

/// 使い方の誤り。操作の失敗（1）と区別する。
const EXIT_USAGE: u8 = 2;

fn main() -> ExitCode {
    let mut argv: Vec<String> = std::env::args().collect();
    let name = argv
        .first()
        .map(|a| Path::new(a).file_stem().unwrap_or_default().to_string_lossy().into_owned())
        .unwrap_or_default();
    let rest = argv.split_off(usize::min(1, argv.len()));
    if name == "dowel" {
        // shim。ここからの経路はネットワークに触れない。
        return match shim(rest) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let opts = match args::parse(rest) {
        Ok(Parsed::Help) => {
            print!("{}", args::USAGE);
            return ExitCode::SUCCESS;
        }
        Ok(Parsed::Version) => {
            println!("dowelup {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Ok(Parsed::Run(o)) => *o,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    match run(opts) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(opts: Options) -> Result<ExitCode, String> {
    let home = Home::locate()?;
    match opts.command {
        Command::Install { spec } => {
            let url = acquire::upstream(opts.upstream.as_deref());
            let got = acquire::install(&home, &url, &spec)?;
            report(&got);
            println!("{}", got.sha);
        }
        Command::Default { spec } => {
            let url = acquire::upstream(opts.upstream.as_deref());
            let got = acquire::install(&home, &url, &spec)?;
            report(&got);
            store::write_selection(&home.default_file(), &got.sha, &spec.to_string())?;
            eprintln!("the default is now {} (from {spec})", got.sha);
            println!("{}", got.sha);
        }
        Command::Pin { spec } => {
            let url = acquire::upstream(opts.upstream.as_deref());
            let got = acquire::install(&home, &url, &spec)?;
            report(&got);
            let file = opts.directory.join(store::PIN_FILE);
            store::write_selection(&file, &got.sha, &spec.to_string())?;
            eprintln!("{} now pins {} (from {spec})", file.display(), got.sha);
            println!("{}", got.sha);
        }
        Command::List => {
            let default = store::read_selection(&home.default_file()).ok();
            for i in store::installed(&home) {
                let mark = if default.as_deref() == Some(i.sha.as_str()) { "*" } else { " " };
                println!("{mark} {}  {}", i.sha, i.specs.join(", "));
            }
        }
        Command::Which => {
            let sel = store::select(&home, &absolute(&opts.directory)?)?;
            let (sha, source) = describe(&sel);
            let bin = home.bin(sha);
            if !bin.is_file() {
                return Err(not_installed(sha, &source));
            }
            eprintln!("selected by {source}");
            println!("{}", bin.display());
        }
        Command::Run { needle, args } => {
            let list = store::installed(&home);
            let hit = store::match_installed(&list, &needle)?;
            return Ok(exec(&home.bin(&hit.sha), &args));
        }
        Command::Uninstall { needle } => {
            let list = store::installed(&home);
            let sha = store::match_installed(&list, &needle)?.sha.clone();
            let dir = home.version_dir(&sha);
            std::fs::remove_dir_all(&dir)
                .map_err(|e| format!("cannot remove {}: {e}", dir.display()))?;
            eprintln!("uninstalled {sha}");
        }
        Command::Shim { dir } => {
            let link = make_shim(&dir)?;
            println!("{}", link.display());
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `dowel` の名で起動されたときの経路。版を選んで実体へ引き継ぐ。
fn shim(mut args: Vec<String>) -> Result<ExitCode, String> {
    let home = Home::locate()?;
    // 先頭の `+<指定子>` は pin と既定の双方より優先される。
    if let Some(needle) = args.first().and_then(|a| a.strip_prefix('+')) {
        let needle = needle.to_string();
        args.remove(0);
        let list = store::installed(&home);
        let hit = store::match_installed(&list, &needle)?;
        return Ok(exec(&home.bin(&hit.sha), &args));
    }
    let cwd =
        std::env::current_dir().map_err(|e| format!("cannot read the current directory: {e}"))?;
    let sel = store::select(&home, &cwd)?;
    let (sha, source) = describe(&sel);
    let bin = home.bin(sha);
    if !bin.is_file() {
        return Err(not_installed(sha, &source));
    }
    Ok(exec(&bin, &args))
}

fn report(got: &acquire::Acquired) {
    if got.already_installed {
        eprintln!("{} is already installed", got.sha);
    } else {
        eprintln!("installed {}", got.sha);
    }
}

fn describe(sel: &Selection) -> (&str, String) {
    match sel {
        Selection::Pin { file, sha } => (sha, file.display().to_string()),
        Selection::Default { sha } => (sha, "the default".to_string()),
    }
}

fn not_installed(sha: &str, source: &str) -> String {
    format!("{sha} is selected by {source} but is not installed; run `dowelup install {sha}`")
}

/// pin の探索は上へ辿るため、相対パスのままでは始点が定まらない。
fn absolute(dir: &Path) -> Result<PathBuf, String> {
    let cwd =
        std::env::current_dir().map_err(|e| format!("cannot read the current directory: {e}"))?;
    if dir.as_os_str() == "." {
        return Ok(cwd);
    }
    if dir.is_absolute() {
        return Ok(dir.to_path_buf());
    }
    Ok(cwd.join(dir))
}

/// 選んだ版に処理を引き継ぐ。Unix ではプロセスを置き換え、shim を残さない。
#[cfg(unix)]
fn exec(bin: &Path, args: &[String]) -> ExitCode {
    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new(bin).args(args).exec();
    eprintln!("error: cannot start {}: {err}", bin.display());
    ExitCode::FAILURE
}

#[cfg(not(unix))]
fn exec(bin: &Path, args: &[String]) -> ExitCode {
    match std::process::Command::new(bin).args(args).status() {
        Ok(s) => ExitCode::from(s.code().and_then(|c| u8::try_from(c).ok()).unwrap_or(1)),
        Err(err) => {
            eprintln!("error: cannot start {}: {err}", bin.display());
            ExitCode::FAILURE
        }
    }
}

/// `<dir>/dowel` を dowelup へのリンクとして作る。
fn make_shim(dir: &Path) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate dowelup itself: {e}"))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let link = dir.join("dowel");
    // dowel の実物や別の shim を黙って書き換えない。
    if link.symlink_metadata().is_ok() {
        return Err(format!("{} already exists; remove it first", link.display()));
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&exe, &link)
            .map_err(|e| format!("cannot create the link {}: {e}", link.display()))?;
    }
    #[cfg(not(unix))]
    {
        // シンボリックリンクに権限が要る環境があるため、複製で代える。
        std::fs::copy(&exe, &link)
            .map_err(|e| format!("cannot copy dowelup to {}: {e}", link.display()))?;
    }
    Ok(link)
}

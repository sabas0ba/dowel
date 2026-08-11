//! 道具に問い、答を憶えておく（[ADR-0028](../../../docs/adr/0028-probe-facts.md)）。
//!
//! ビルドが依存している事実のうち、マニフェストに書かれていないものがある。
//! 「その道具は在るか」「そのコンパイラは何を名乗るか」。これらは**走った
//! 機械の状態**であって、記録しなければ再現できず、毎回確かめ直すことになる
//! （docs/20-architecture.md 9節）。
//!
//! 器は [`dowel_store::Facts`] が持つ。こちらはプロセスを起こす側である——
//! ストアはプロセスを起動しないので、採取と保管を分けてある。
//!
//! 憶えたことは**プロジェクトを跨いで**共有される。事実は道具のもので
//! あってプロジェクトのものではない。

use dowel_store::facts::{identity, Facts};
use dowel_support::{log_debug, log_trace};
use std::path::{Path, PathBuf};
use std::process::Command;

/// 道具に問う側。事実を憶えており、憶えていることは訊き直さない。
pub struct Prober {
    facts: Facts,
    /// 起こしたプロセスの数。省略が効いているかを測るために持つ
    launched: usize,
}

impl Default for Prober {
    fn default() -> Prober {
        Prober::new()
    }
}

impl Prober {
    pub fn new() -> Prober {
        Prober { facts: Facts::open(), launched: 0 }
    }

    /// 場所を指定して開く。試験が利用者の環境を汚さないために分けてある。
    pub fn in_dir(dir: PathBuf) -> Prober {
        Prober { facts: Facts::open_in(dir), launched: 0 }
    }

    /// 憶えたことを書き出す。
    pub fn save(&self) {
        log_debug!("probe: launched {} process(es)", self.launched);
        self.facts.save();
    }

    /// この実行で実際に起こしたプロセスの数。
    pub fn launched(&self) -> usize {
        self.launched
    }

    /// 道具が起動できる形で在るか。
    ///
    /// PATH の走査は事実として憶える。憶えているのは「解決した先の道すじ」で
    /// あり、鍵には PATH そのものを含める——`PATH` が変われば別の道具が
    /// 見つかりうるので、同じ問いとして扱えない。
    pub fn exists(&mut self, command: &str) -> bool {
        self.resolve(command).is_some()
    }

    /// 道具の実体の場所。見つからなければ `None`。
    pub fn resolve(&mut self, command: &str) -> Option<PathBuf> {
        let path_var = std::env::var("PATH").unwrap_or_default();
        // 道すじを含む指定は PATH に依らない。鍵も分ける——PATH が変わる
        // たびに事実を捨てる理由が無い。
        let key = match Path::new(command).components().count() > 1 {
            true => format!("resolve\t{command}"),
            false => format!("resolve\t{command}\t{}", short(&path_var)),
        };
        if let Some(v) = self.facts.get(&key) {
            // 見つけた先が今も同じものかを確かめる。道具の入れ替えは
            // PATH を変えずに起こる。
            return match v {
                "" => None,
                path => {
                    let p = PathBuf::from(path);
                    match is_executable(&p) {
                        true => Some(p),
                        // 消えていた。憶えたことは捨て、訊き直す。
                        false => {
                            log_trace!("  fact stale: {} is gone", p.display());
                            let found = search(command);
                            self.remember_resolution(key, &found);
                            found
                        }
                    }
                }
            };
        }
        let found = search(command);
        self.remember_resolution(key, &found);
        found
    }

    fn remember_resolution(&mut self, key: String, found: &Option<PathBuf>) {
        self.facts.set(key, found.as_ref().map(|p| p.display().to_string()).unwrap_or_default());
    }

    /// コンパイラが名乗る三つ組（`-dumpmachine`）。
    ///
    /// dowel が組み立てる既定の三つ組は OS と構成から作った近似であり、
    /// 実際に組む道具が何を名乗るかとは別物である（`x86_64-pc-linux-gnu` と
    /// `x86_64-unknown-linux-gnu` は同じ機械の別の綴り）。ここで**道具に
    /// 訊く**ことで、名乗りが記録された入力になる。
    ///
    /// 答えない道具（MSVC の `cl` にこの旗は無い）では `None`。呼び手は
    /// 近似に落ちる——訊けないことは誤りではない。
    pub fn triple(&mut self, compiler: &str) -> Option<String> {
        let path = self.resolve(compiler)?;
        let key = format!("dumpmachine\t{}", identity(&path));
        if let Some(v) = self.facts.get(&key) {
            return match v {
                "" => None,
                t => Some(t.to_string()),
            };
        }
        let answer = self.ask(&path, &["-dumpmachine"]);
        // 答えなかったことも事実である。憶えないと、毎回訊きに行く。
        self.facts.set(key, answer.clone().unwrap_or_default());
        answer
    }

    /// 道具が `--version` に応じるか。生成器（ninja / make）の実在検査。
    ///
    /// 実在するだけでは足りない——同じ名前の別物がある。応答まで見る。
    pub fn responds_to_version(&mut self, program: &str) -> bool {
        let Some(path) = self.resolve(program) else { return false };
        let key = format!("version\t{}", identity(&path));
        if let Some(v) = self.facts.get(&key) {
            return v == "yes";
        }
        self.launched += 1;
        log_trace!("  probing {} --version", path.display());
        let ok = Command::new(&path)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        self.facts.set(key, if ok { "yes".into() } else { "no".into() });
        ok
    }

    /// 道具を起こして最初の1行を採る。失敗したら `None`。
    fn ask(&mut self, program: &Path, args: &[&str]) -> Option<String> {
        self.launched += 1;
        log_trace!("  probing {} {}", program.display(), args.join(" "));
        let out = Command::new(program).args(args).output().ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let first = text.lines().next()?.trim();
        match first.is_empty() {
            true => None,
            false => Some(first.to_string()),
        }
    }
}

/// PATH を走査して道具を探す。
fn search(command: &str) -> Option<PathBuf> {
    let p = Path::new(command);
    if p.components().count() > 1 {
        return is_executable(p).then(|| p.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(command)).find(|c| is_executable(c))
}

fn is_executable(p: &Path) -> bool {
    let Ok(m) = std::fs::metadata(p) else { return false };
    if !m.is_file() {
        return false;
    }
    // 実行ビットは Unix でのみ意味を持つ。他の環境では存在だけを見る。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        m.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// 鍵に混ぜるための短い要約。
///
/// `PATH` はそのまま鍵に入れるには長い。順序も内容も効くので、畳まずに
/// ハッシュを採る——衝突しても「別の PATH を同じ問いと見なす」ことになるが、
/// 見つけた先の実在は引くたびに確かめている。
fn short(s: &str) -> String {
    // FNV-1a。事実の鍵に暗号強度は要らない。
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-scratch/probe")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_tool(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // 書いた直後の実行は `ETXTBSY` になりうる。並列に走る別の試験が
        // fork した瞬間に、まだ開いている書き込みハンドルが子へ渡るためで、
        // 道具の側の問題ではない。実行できるようになるまで待つ。
        for _ in 0..50 {
            match Command::new(&p).arg("--probe-ready").output() {
                Ok(_) => break,
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
        p
    }

    #[test]
    fn a_remembered_answer_does_not_start_the_tool_again() {
        let dir = scratch("remembered");
        let tool = write_tool(&dir, "cc", "#!/bin/sh\necho x86_64-pc-linux-gnu\n");
        let facts = dir.join("facts");

        let mut p = Prober::in_dir(facts.clone());
        assert_eq!(p.triple(tool.to_str().unwrap()).as_deref(), Some("x86_64-pc-linux-gnu"));
        assert_eq!(p.launched(), 1);
        // 同じプロセスの中でも訊き直さない。
        assert_eq!(p.triple(tool.to_str().unwrap()).as_deref(), Some("x86_64-pc-linux-gnu"));
        assert_eq!(p.launched(), 1);
        p.save();

        // 別のプロセスでも。事実がディスクに残っている。
        let mut again = Prober::in_dir(facts);
        assert_eq!(again.triple(tool.to_str().unwrap()).as_deref(), Some("x86_64-pc-linux-gnu"));
        assert_eq!(again.launched(), 0, "the tool was started despite a recorded fact");
    }

    #[test]
    fn replacing_the_tool_replaces_the_fact() {
        // 無効化の機構を持たないのは、鍵が道具の同一性を含むためである。
        let dir = scratch("replaced");
        let tool = write_tool(&dir, "cc", "#!/bin/sh\necho first-triple\n");
        let facts = dir.join("facts");

        let mut p = Prober::in_dir(facts.clone());
        assert_eq!(p.triple(tool.to_str().unwrap()).as_deref(), Some("first-triple"));
        p.save();

        write_tool(&dir, "cc", "#!/bin/sh\necho second-triple-and-longer\n");
        let mut after = Prober::in_dir(facts);
        assert_eq!(
            after.triple(tool.to_str().unwrap()).as_deref(),
            Some("second-triple-and-longer")
        );
        assert_eq!(after.launched(), 1, "the stale fact was used");
    }

    #[test]
    fn a_tool_that_does_not_answer_is_remembered_as_such() {
        // 訊けないことは誤りではない。ただし憶えないと毎回訊きに行く。
        let dir = scratch("silent");
        let tool = write_tool(&dir, "cl", "#!/bin/sh\nexit 1\n");
        let facts = dir.join("facts");

        let mut p = Prober::in_dir(facts.clone());
        assert_eq!(p.triple(tool.to_str().unwrap()), None);
        assert_eq!(p.launched(), 1);
        p.save();

        let mut again = Prober::in_dir(facts);
        assert_eq!(again.triple(tool.to_str().unwrap()), None);
        assert_eq!(again.launched(), 0, "a tool that does not answer was asked again");
    }

    #[test]
    fn a_tool_that_disappeared_is_not_reported_as_present() {
        // 憶えた道すじは、引くたびに実在を確かめる。道具の入れ替えは
        // PATH を変えずに起こる。
        let dir = scratch("vanished");
        let tool = write_tool(&dir, "gone", "#!/bin/sh\n");
        let facts = dir.join("facts");

        let mut p = Prober::in_dir(facts.clone());
        assert!(p.exists(tool.to_str().unwrap()));
        p.save();

        std::fs::remove_file(&tool).unwrap();
        let mut after = Prober::in_dir(facts);
        assert!(!after.exists(tool.to_str().unwrap()), "a removed tool was reported as present");
    }

    #[test]
    fn a_missing_tool_is_not_found() {
        let dir = scratch("missing");
        let mut p = Prober::in_dir(dir.join("facts"));
        assert!(!p.exists("/nonexistent/definitely-not-here"));
        assert_eq!(p.triple("/nonexistent/definitely-not-here"), None);
        // 起動を試みていない。無いものは訊けない。
        assert_eq!(p.launched(), 0);
    }
}

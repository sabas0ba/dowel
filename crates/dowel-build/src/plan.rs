//! アクショングラフの構築。
//!
//! ここが「構成を与えて具体化する」唯一の場所である。マニフェスト評価は
//! 構成を知らず、`Cfg<T>` のまま値を持っている（docs/10-manifest.md 3節）。

use crate::action::{Action, ActionId, ActionKind};
use crate::glob;
use crate::toolstyle;
use dowel_eval::schema::TableKind;
use dowel_eval::{Config, Data, PathBase, Site, Value};
use dowel_model::graph::Graph;
use dowel_model::interface;
use dowel_model::{Session, TargetId};
use dowel_support::{log_debug, log_trace, Diagnostic};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// `compile_commands.json` の1件。
#[derive(Clone, Debug)]
pub struct CompileCommand {
    pub directory: PathBuf,
    pub file: PathBuf,
    pub arguments: Vec<String>,
    pub output: PathBuf,
}

/// 1つの共有ライブラリについて、宣言された面。
pub struct DeclaredExports {
    pub target: TargetId,
    pub library: PathBuf,
    pub names: Vec<String>,
    /// `exports` が書かれた位置。診断が指す先
    pub site: dowel_eval::value::Site,
}

pub struct Plan {
    pub build_dir: PathBuf,
    pub actions: Vec<Action>,
    /// ターゲット → 最終成果物
    pub artifacts: BTreeMap<TargetId, PathBuf>,
    /// ターゲット → 成果物から派生させたもの（`artifacts` ブロック、issue #60）。
    /// 宣言順。これらもビルドの成果物であり、既定で作られる
    pub derived: BTreeMap<TargetId, Vec<PathBuf>>,
    pub compile_commands: Vec<CompileCommand>,
    /// 要求されたターゲット
    pub requested: Vec<TargetId>,
    /// ヘッダ依存の取り方（ADR-0027）。様式が決める
    pub deps: crate::toolstyle::Deps,
    /// 計画に載った共有ライブラリ（[ADR-0038](../../../docs/adr/0038-shared-inside-its-package.md)）。
    ///
    /// 同じパッケージの中では書庫の方に繋ぐので、`.so` を要求する者が
    /// 誰も居なくなりうる。配るために宣言したものが既定のビルドで出て
    /// こないのは誤りなので、自身の出力として並べる（issue #64 と同じ判断）
    pub shared_libraries: Vec<PathBuf>,
    /// 宣言した面と、それを持つはずの成果物
    /// （[ADR-0039](../../../docs/adr/0039-exports-are-checked.md)）。
    /// ビルドの後、出来上がったものに聞いて突き合わせる
    pub declared_exports: Vec<DeclaredExports>,
}

impl Plan {
    /// ビルドが作るもの。ninja の `default` と「何を作ったか」の表示が読む。
    ///
    /// 要求されたターゲットの成果物と、**計画に載った全ターゲット**の派生で
    /// ある。派生をこの計画の全体から採るのは、それがそのターゲット自身の
    /// 出力だからである。依存として書庫が作られるなら、その隣に置くと
    /// 宣言された `.stripped` も作られる——派生が出るかどうかが、自分の
    /// 宣言ではなく「誰かが自分に依存しているか」で決まってはならない
    /// （issue #64）。
    ///
    /// 派生は誰の入力にもならないため、ninja からは `default` に並べない限り
    /// 到達しない。一方 direct 実行器は全アクションを走らせる。並べなければ
    /// 実行器によって出来上がるものが違う（issue #41 と同じ形）。
    pub fn default_outputs(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for t in &self.requested {
            out.extend(self.artifacts.get(t).cloned());
        }
        for derived in self.derived.values() {
            out.extend(derived.iter().cloned());
        }
        // 共有ライブラリは、誰も繋がなくても作る。宣言した目的が配ることに
        // ある以上、「依存として引かれたか」で出来たり出来なかったりしては
        // ならない（ADR-0038）。
        out.extend(self.shared_libraries.iter().cloned());
        out
    }

    pub fn action(&self, id: ActionId) -> &Action {
        &self.actions[id.0]
    }

    /// 依存が先に来る順。直接実行はこの順で走らせる。
    pub fn order(&self) -> Vec<ActionId> {
        // 構築時に依存が先に積まれているため、そのままの順で足りる。
        // 不変条件として検査しておく。
        for a in &self.actions {
            debug_assert!(
                a.deps.iter().all(|d| d.0 < a.id.0),
                "an action depends on a later action"
            );
        }
        self.actions.iter().map(|a| a.id).collect()
    }
}

/// ビルドディレクトリ。構成ごとに分ける。
pub fn build_dir(root: &Path, cfg: &Config) -> PathBuf {
    build_root(root).join(cfg.id())
}

/// 構成ごとのディレクトリを収める場所。
pub fn build_root(root: &Path) -> PathBuf {
    root.join(".dowel").join("build")
}

/// 1つの構成のビルドディレクトリと、その大きさ・最後に触られた時刻。
pub struct BuildDir {
    pub path: PathBuf,
    pub id: String,
    pub bytes: u64,
    /// 最後に書かれてからの日数。読めなければ 0
    pub age_days: u64,
}

/// 在るビルドディレクトリを数え上げる（[ADR-0037](../../../docs/adr/0037-store-gc.md)）。
///
/// 構成と三つ組を切り替えるたびに1つ増え、使わなくなっても残る。実際に
/// 嵩むのはここであり、ストアの値ログより桁が大きい——オブジェクトと
/// 実行ファイルが入る。
pub fn build_dirs(root: &Path) -> Vec<BuildDir> {
    let base = build_root(root);
    let Ok(entries) = std::fs::read_dir(&base) else { return Vec::new() };
    let now = std::time::SystemTime::now();
    let mut out = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        if !path.is_dir() {
            continue;
        }
        let age_days = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .map(|d| d.as_secs() / 86_400)
            .unwrap_or(0);
        out.push(BuildDir {
            id: e.file_name().to_string_lossy().into_owned(),
            bytes: dir_bytes(&path),
            age_days,
            path,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// 木の下のファイルの合計。シンボリックリンクは辿らない。
fn dir_bytes(dir: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(e.path()),
                Ok(t) if t.is_file() => total += e.metadata().map(|m| m.len()).unwrap_or(0),
                _ => {}
            }
        }
    }
    total
}

/// 実行ファイルの綴り。対象の OS が決める（issue #112）。
///
/// Windows 向けではコンパイラドライバが `.exe` を付けて書き出す。dowel が
/// `bin/app` と名指ししても、書かれるのは `bin/app.exe` である。ずれると、
/// 走らせる・派生させる・開くの全てが実在しない道を渡され、**組む段では
/// 現れない**——ninja もこちらも、リンクの成功を終了状態で判断しており、
/// 出力ファイルの実在は確かめないためである。しかも「出力が無い」と
/// 「まだ作っていない」が同じ状態に潰れるので、増分ビルドが永久に収束しない。
///
/// 綴りを決める場所をここ1つにすると、runner も `artifacts` も `debug` も
/// `built:` の印字も指紋も同じ値を読む。
pub fn executable_name(name: &str, cfg: &Config) -> String {
    match dowel_eval::config::triple_os(&cfg.target) {
        "windows" => format!("{name}.exe"),
        _ => name.to_string(),
    }
}

pub fn plan(
    sess: &Session,
    graph: &Graph,
    cfg: &Config,
    requested: &[TargetId],
) -> (Plan, Vec<Diagnostic>) {
    let _phase = dowel_support::log::Phase::start("plan");
    let mut diags = Vec::new();
    let root = sess.root_package().map(|p| p.root.clone()).unwrap_or_else(|| PathBuf::from("."));
    let build_dir = build_dir(&root, cfg);

    // ツールチェーンの宣言はターゲットトリプルで引く。無印の `[toolchain]` は
    // ホスト向け、`[toolchain.<triple>]` はそのトリプル向けである（issue #42）。
    let is_host = cfg.targets_host();
    let root_toolchain = sess.root_package().and_then(|p| p.toolchain_for(&cfg.target, is_host));

    // ツールチェーンが混ざると ABI の前提が崩れる。1回のビルドで1つに限る。
    // 比較するのは今のターゲットトリプルに適用される宣言だけ。別トリプル向けの
    // 宣言は、このビルドに対する要求ではない。
    for p in &sess.packages {
        let Some(decl) = p.toolchain_for(&cfg.target, is_host) else { continue };
        // 取ってきた道具一式を宣言しているなら、比べるのは**解いた後**の
        // 綴りである（ADR-0044）。宣言のままと突き合わせると、書庫の中を
        // 指す宣言は必ず食い違って見える。
        let root = decl
            .source
            .as_ref()
            .and_then(|src| dowel_model::fetch::existing_toolchain(&src.sha256));
        for (name, _, _) in dowel_eval::config::TOOLS {
            let Some(t) = decl.tool(name) else { continue };
            let asked = match (&root, Path::new(&t.command)) {
                (Some(r), path) if !path.is_absolute() && path.components().count() >= 2 => {
                    r.join(path).display().to_string()
                }
                _ => t.command.clone(),
            };
            let used = cfg.tool(name);
            if asked != used {
                diags.push(
                    Diagnostic::warning(
                        "toolchain-mismatch",
                        format!(
                            "package `{}` asks for `{name} = \"{}\"` but the build uses `{used}`",
                            p.name, t.command
                        ),
                    )
                    .note("a package's toolchain does not override the build's (ADR-0031)")
                    .note("ABI label checking assumes a single pinned toolchain"),
                );
            }
        }
    }

    // 固定した対象が実在するかどうかは、記録されない入力を排除する前提である
    // （docs/00-overview.md 2節）。確かめなければ `/bin/sh: not found` が
    // ビルドの失敗として出るだけで、`[toolchain]` のどの行が原因かを示さない。
    // C は常に要る。他の道具はそれを使う箇所が要求する（require_tool）。
    require_tool(&mut diags, cfg, root_toolchain, "c", "C compiler");

    // 必要なターゲットの集合。要求されたものとその推移的依存。
    let mut needed: BTreeSet<TargetId> = BTreeSet::new();
    for &t in requested {
        needed.extend(graph.link_closure(t));
    }
    // 三つ組の外にある目標は組めない。名指しされたものも、依存として
    // 引き込まれたものも同じく断る——黙って外すと、名指しは何も作らずに
    // 成功し、依存は undefined reference としてリンクの段に現れる
    // （issue #126）。既定の数え上げからは呼び出し側が先に外している。
    for &tid in &needed {
        if supports_target(sess, tid, cfg) {
            continue;
        }
        let target = sess.target(tid);
        let declared = collect_root_strs(sess, tid, cfg, "targets");
        let mut d = Diagnostic::error(
            "unsupported-target",
            format!("`{}` is not built for `{}`", sess.label(tid), cfg.target),
        )
        .at(
            target.site.file,
            target.site.span,
            "this target declares the triples it supports",
        );
        for t in &declared {
            d = d.note(format!("declared for {t}"));
        }
        diags.push(d);
    }

    let mut plan = Plan {
        build_dir: build_dir.clone(),
        actions: Vec::new(),
        artifacts: BTreeMap::new(),
        derived: BTreeMap::new(),
        compile_commands: Vec::new(),
        requested: requested.to_vec(),
        deps: toolstyle::deps(cfg),
        shared_libraries: Vec::new(),
        declared_exports: Vec::new(),
    };
    // ターゲット → そのターゲットの成果物を作るアクション
    let mut producer: BTreeMap<TargetId, ActionId> = BTreeMap::new();
    // ターゲット → 依存側がリンクに渡すもの。通常は成果物そのものだが、
    // MSVC の共有ライブラリでは DLL ではなく取り込み用の書庫である
    let mut link_inputs: BTreeMap<TargetId, PathBuf> = BTreeMap::new();
    // 同じパッケージの中から繋ぐときの入力と、それを作るアクション
    // （[ADR-0038](../../../docs/adr/0038-shared-inside-its-package.md)）。
    // 共有ライブラリだけが `link_inputs` と違う値を持つ
    let mut sibling_inputs: BTreeMap<TargetId, (PathBuf, ActionId)> = BTreeMap::new();
    // ターゲット → 自身のソースに C++ を含むか。リンカの選択が読む
    let mut has_cxx: BTreeMap<TargetId, bool> = BTreeMap::new();
    // ターゲット → 別のアセンブラが組み立てた目的コードを持つか（ADR-0050）。
    // その目的ファイルには `.note.GNU-stack` が無く、リンクの側で言い直す
    let mut has_unmarked_asm: BTreeMap<TargetId, bool> = BTreeMap::new();
    // C++ コンパイラの実在検査は C++ ソースが現れたときに1度だけ行う。
    // C だけのビルドに C++ ツールチェーンを要求しないため
    let mut cxx_toolchain_checked = false;
    // 書庫作成器も同じ扱い。書庫を作らないビルドには要求しない
    let mut ar_toolchain_checked = false;
    // 別に宣言されたアセンブラ（ADR-0050）。アセンブリが現れたときだけ要る
    let mut asm_toolchain_checked = false;
    // ビルドと合わない ABI 札。宣言ごとに1件へ畳むので、溜めてから出す
    // （issue #158）
    let mut abi_against_build: Vec<AbiAgainstBuild> = Vec::new();
    // 変換の道具も同じ扱い。使う宣言があったときに1度だけ確かめる
    let mut probed_tools: BTreeSet<String> = BTreeSet::new();

    // 共有ライブラリと、位置独立に翻訳するターゲット（ADR-0030）。
    //
    // 宣言したターゲットだけでは足りない。静的ライブラリが共有ライブラリに
    // 取り込まれるとき、その目的コードも位置独立でなければリンカに弾かれる
    // ——繋ぎ方の宣言は、依存の翻訳の仕方まで動かす。
    let mut position_independent: BTreeSet<TargetId> = BTreeSet::new();
    let mut shared_targets: BTreeSet<TargetId> = BTreeSet::new();
    for &tid in &graph.order {
        if needed.contains(&tid) && is_shared(sess, tid, cfg) {
            shared_targets.insert(tid);
            position_independent.extend(graph.link_closure(tid));
        }
    }

    // `graph.order` は依存が先。成果物ができてからリンクする順になる。
    for &tid in &graph.order {
        if !needed.contains(&tid) {
            continue;
        }
        let target = sess.target(tid);
        // テンプレートは記述の共有であって成果物ではない（ADR-0035）。
        // 展開は模型の側で済んでおり、計画に居る理由が無い——名指しされた
        // ときだけ、そう述べて外す。
        if target.kind == TableKind::Template {
            if requested.contains(&tid) {
                diags.push(
                    Diagnostic::error(
                        "not-a-target",
                        format!("`{}` is a template, not something to build", sess.label(tid)),
                    )
                    .at(target.site.file, target.site.span, "templates produce no artifact")
                    .note("build a target that uses it, as in `use = [template(\"...\")]`"),
                );
            }
            continue;
        }
        let pkg = sess.package(target.package);
        let env = interface::compile_env(sess, tid, &mut diags);
        check_abi_against_build(&env, cfg, sess.label(tid), &mut abi_against_build);

        // 既に在るライブラリ（[ADR-0049](../../../docs/adr/0049-prebuilt-libraries.md)）。
        // 組むものが無いので、翻訳も書庫作成もせずに繋ぐ入力として置く。
        if let Some(value) = root_value(sess, tid, cfg, "prebuilt") {
            if let Some(path) = prebuilt_library(sess, tid, &value, cfg, &mut diags) {
                link_inputs.insert(tid, path.clone());
                plan.artifacts.insert(tid, path);
            }
            continue;
        }

        let sources = collect_sources(sess, tid, cfg, &mut diags);
        has_cxx.insert(tid, sources.iter().any(|s| is_cxx(s)));
        has_unmarked_asm.insert(
            tid,
            cfg.assembler().is_some() && sources.iter().any(|s| language(s) == Some(Language::Asm)),
        );
        if has_cxx[&tid] && !cxx_toolchain_checked {
            cxx_toolchain_checked = true;
            if !is_host && root_toolchain.is_none_or(|t| t.tool("cxx").is_none()) {
                // ホストの `c++` へ落とすと、C++ の翻訳単位だけ別アーキテクチャの
                // オブジェクトになる。黙って組まず、宣言の不足として述べる。
                let mut d = Diagnostic::error(
                    "missing-toolchain",
                    format!("no C++ toolchain is declared for target `{}`", cfg.target),
                );
                if let Some(s) = root_toolchain.and_then(|t| t.site) {
                    d = d.at(s.file, s.span, "add `cxx = \"...\"` here");
                }
                diags.push(d.note(
                    "the sources contain C++, and the host `c++` would produce objects for the wrong architecture",
                ));
            } else {
                require_tool(&mut diags, cfg, root_toolchain, "cxx", "C++ compiler");
            }
        }
        if sources.is_empty() && target.kind != TableKind::Lib {
            diags.push(
                Diagnostic::error("no-sources", format!("`{}` has no sources", sess.label(tid)))
                    .at(target.site.file, target.site.span, "`sources` is empty")
                    .note("set it, for example `sources = glob(\"src/*.c\")`"),
            );
            continue;
        }

        let includes = collect_includes(sess, &env, cfg, &build_dir, &mut diags);
        let defines = collect_defines(&env);
        let flags = collect_flags(sess, &env, cfg, &build_dir, "flags", &mut diags);
        // 言語標準は型付きのプロパティであり、`-std=` はここで組み立てる。
        // 言語別のフラグより前に置く。`c_flags = ["-std=gnu11"]` のような
        // 方言の指定が後に来て勝つようにするため（後勝ちは -std の慣習）
        let mut c_flags = std_flag(&env, "c_std").into_iter().collect::<Vec<_>>();
        c_flags.extend(collect_flags(sess, &env, cfg, &build_dir, "c_flags", &mut diags));
        let asm_flags = collect_flags(sess, &env, cfg, &build_dir, "asm_flags", &mut diags);
        let mut cxx_flags = std_flag(&env, "cxx_std").into_iter().collect::<Vec<_>>();
        cxx_flags.extend(collect_flags(sess, &env, cfg, &build_dir, "cxx_flags", &mut diags));
        // `link_flags` だけは compile_env からではなく、リンク閉包から集める。
        // `private` はリンクの到達可能性を制御しない（issue #56、下の
        // `closure_link_flags`）。
        let link_flags = closure_link_flags(sess, graph, cfg, tid, &build_dir, &mut diags);

        log_debug!(
            "{}: {} sources, {} includes, {} defines",
            sess.label(tid),
            sources.len(),
            includes.len(),
            defines.len()
        );
        // 中間結果を丸ごと出す。コンパイル引数が期待と違うとき、
        // どの段階で入り込んだかはここを見れば分かる。
        for s in &sources {
            log_trace!("  source  {}", s.display());
        }
        for i in &includes {
            log_trace!("  include {}", i.display());
        }
        for (k, v) in &defines {
            log_trace!("  define  {k}={v}");
        }
        if !flags.is_empty() {
            log_trace!("  flags   {}", flags.join(" "));
        }
        if !c_flags.is_empty() {
            log_trace!("  cflags  {}", c_flags.join(" "));
        }
        if !cxx_flags.is_empty() {
            log_trace!("  cxxflags {}", cxx_flags.join(" "));
        }
        if !link_flags.is_empty() {
            log_trace!("  ldflags {}", link_flags.join(" "));
        }

        // --- コンパイル ---
        let mut objects = Vec::new();
        let mut compile_ids = Vec::new();
        for src in &sources {
            // 言語は拡張子で決まる。`cc` は driver であり `.cpp` のコンパイル
            // 自体は通すが、C++ として組むには標準ライブラリと ABI の前提が
            // 揃った driver（`tc.cxx`）を使う必要がある
            // 翻訳できない綴りは `collect_sources` が既に断っている（ADR-0051）。
            let Some(lang) = language(src) else { continue };
            // アセンブリは既定では C の driver に渡す。driver が gas を呼ぶ
            // ので、それで足りる（ADR-0048）。`[toolchain] asm` が宣言されて
            // いればそちらへ行く（ADR-0050）。
            let separate_asm = lang == Language::Asm && cfg.assembler().is_some();
            if lang == Language::Asm && !separate_asm && is_masm_syntax(src) {
                // driver に渡しても「ファイルの形式が分からない」と言われる。
                // 何が要るかは分かっているので、そう述べる。
                diags.push(
                    Diagnostic::error(
                        "missing-assembler",
                        format!("`{}` needs an assembler", rel_display(&pkg.root, src)),
                    )
                    .at(target.site.file, target.site.span, "declared as a source here")
                    .note("`.asm` is MASM or NASM syntax, which the C compiler driver does not accept")
                    .note("declare one, as in `[toolchain] asm = \"nasm\"` in dowel.toml"),
                );
                continue;
            }
            let (compiler, tool) = match lang {
                Language::Cxx => (cfg.tool("cxx"), "CXX"),
                Language::Asm => (cfg.assembler().unwrap_or_else(|| cfg.tool("c")), "AS"),
                Language::C => (cfg.tool("c"), "CC"),
            };
            if separate_asm && !asm_toolchain_checked {
                asm_toolchain_checked = true;
                require_tool(&mut diags, cfg, root_toolchain, "asm", "assembler");
            }
            let obj = object_path(&build_dir, &pkg.name, &target.name, &pkg.root, src, cfg);
            // 依存の記録は必ず1つの `.d` に落ちる。MSVC はコンパイラに
            // 書かせず、実行する側が `/showIncludes` の出力を畳んで書く
            // （ADR-0027）——読む側の機構を様式ごとに増やさないためである。
            let depfile = obj.with_extension(format!("{}.d", toolstyle::object_extension(cfg)));
            // 前処理を通らないアセンブリに依存は無い。`-MD` を渡しても
            // 何も書かれず、宣言した出力が出ないことになる（ADR-0048）。
            // 別のアセンブラには依存を頼まない。頼み方が道具ごとに違い、
            // 書かれない依存ファイルを宣言することになる（ADR-0050）。
            let wants_depfile =
                !separate_asm && (lang != Language::Asm || is_preprocessed_asm(src));
            let mut args: Vec<String> = Vec::new();
            if separate_asm {
                // 渡すのは自身の旗と入出力だけである。翻訳の行の残り——
                // 最適化、`flags`、インクルード検索路、定義——は C の driver の
                // 綴りであり、アセンブラは driver ではない（ADR-0050）。
                args.extend(asm_flags.iter().cloned());
                args.extend(toolstyle::assemble_io(cfg, src, &obj));
            } else {
                args.extend(toolstyle::default_compile_flags(cfg));
                if position_independent.contains(&tid) {
                    args.extend(toolstyle::shared_object_flags(cfg));
                }
                if lang == Language::Asm {
                    args.extend(toolstyle::assemble_flags(cfg));
                }
                args.extend(flags.iter().cloned());
                // 言語別のフラグは共通の `flags` の後。後勝ちの慣習により、
                // 言語別の指定が共通の指定を上書きできる向きにする。
                //
                // アセンブリに `c_flags` は掛けない。`-std=c17` を手書きの
                // アセンブリに渡すのは、言語を取り違えているだけである（ADR-0048）。
                args.extend(
                    match lang {
                        Language::Cxx => &cxx_flags,
                        Language::Asm => &asm_flags,
                        Language::C => &c_flags,
                    }
                    .iter()
                    .cloned(),
                );
                for inc in &includes {
                    args.push(toolstyle::include(cfg, inc));
                }
                for (k, v) in &defines {
                    args.push(toolstyle::define(cfg, k, v));
                }
                // 入出力と依存の綴りは様式が決める（ADR-0027）。
                args.extend(toolstyle::compile_io(
                    cfg,
                    src,
                    &obj,
                    wants_depfile.then_some(depfile.as_path()),
                ));
            }

            let id = ActionId(plan.actions.len());
            plan.actions.push(Action {
                id,
                kind: ActionKind::Compile,
                target: tid,
                program: compiler.to_string(),
                args: args.clone(),
                inputs: vec![src.clone()],
                outputs: vec![obj.clone()],
                depfile: wants_depfile.then_some(depfile),
                description: format!("{tool} {}", rel_display(&build_dir, &obj)),
                deps: Vec::new(),
            });
            let mut arguments = vec![compiler.to_string()];
            arguments.extend(args);
            plan.compile_commands.push(CompileCommand {
                directory: build_dir.clone(),
                file: src.clone(),
                arguments,
                output: obj.clone(),
            });
            log_trace!("  action[{}] {}", id.0, plan.actions[id.0].command_line());
            objects.push(obj);
            compile_ids.push(id);
        }

        // --- 集約（アーカイブ／リンク） ---
        match target.kind {
            TableKind::Lib if is_shared(sess, tid, cfg) => {
                let exports = collect_root_strs(sess, tid, cfg, "exports");
                if exports.is_empty() {
                    // 既定に落とさない。platform ごとに違う意味を持つ宣言は
                    // 宣言ではない（ADR-0030）。
                    diags.push(
                        Diagnostic::error(
                            "missing-exports",
                            format!("shared library `{}` declares no exports", sess.label(tid)),
                        )
                        .at(target.site.file, target.site.span, "`linkage = \"shared\"` is declared here")
                        .note("a shared library's exported symbols are its interface")
                        .note("left to the platform they differ: everything on ELF, nothing on Windows")
                        .note("add `exports = [\"...\"]`"),
                    );
                    continue;
                }

                // ABI の世代（ADR-0040）。書かなければ版を持たない。
                let declared_soversion = root_value(sess, tid, cfg, "soversion");
                let soversion = declared_soversion.as_ref().and_then(|v| v.as_int());
                if soversion.is_some_and(|v| v < 0) {
                    let site = declared_soversion
                        .as_ref()
                        .and_then(|v| v.prov.nearest_site())
                        .unwrap_or(target.site);
                    diags.push(
                        Diagnostic::error(
                            "invalid-soversion",
                            format!(
                                "`soversion` is {}; an ABI generation counts up from 0",
                                soversion.unwrap_or_default()
                            ),
                        )
                        .at(site.file, site.span, "declared here")
                        .note("the number becomes part of the library's file name"),
                    );
                    continue;
                }
                let out = build_dir.join("lib").join(toolstyle::shared_library_name(
                    cfg,
                    &target.name,
                    soversion,
                ));
                // リンカが読む形は対象の形式が決める。生成物はリンクの入力で
                // あり、`exports` を変えれば結び直る。
                let form = toolstyle::export_form(cfg);
                let export_path = build_dir.join("lib").join(format!(
                    "{}.{}",
                    target.name,
                    toolstyle::export_file_extension(form)
                ));
                if let Some(dir) = export_path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                if let Err(e) = std::fs::write(&export_path, toolstyle::export_file(form, &exports))
                {
                    diags.push(Diagnostic::error(
                        "unwritable-build-dir",
                        format!("cannot write {}: {e}", export_path.display()),
                    ));
                    continue;
                }

                // 共有ライブラリも依存を取り込む。リンカの選択は実行ファイルと
                // 同じ判断による——閉包のどこかに C++ があれば C++ の driver。
                let link_needs_cxx = graph
                    .link_closure(tid)
                    .into_iter()
                    .any(|t| has_cxx.get(&t).copied().unwrap_or(false));
                let linker = cfg.linker(link_needs_cxx).to_string();
                let libs: Vec<PathBuf> = graph
                    .link_closure(tid)
                    .into_iter()
                    .filter(|t| *t != tid)
                    .filter_map(|t| link_inputs.get(&t).cloned())
                    .collect();
                let mut inputs_args: Vec<String> =
                    objects.iter().map(|o| o.display().to_string()).collect();
                inputs_args.extend(libs.iter().map(|l| l.display().to_string()));
                // 印の無いアセンブリが閉包に居れば、実行可能スタックは
                // リンクの側で断る（ADR-0050）。`link_flags` はこの後に並ぶ
                // ので、本当に要る場合は言い直せる。
                let mut flags = if graph
                    .link_closure(tid)
                    .into_iter()
                    .any(|t| has_unmarked_asm.get(&t).copied().unwrap_or(false))
                {
                    toolstyle::noexecstack_link_flags(cfg)
                } else {
                    Vec::new()
                };
                flags.extend(link_flags.iter().cloned());
                // 自身が共有ライブラリに繋ぐ場合も、実行時の探索路が要る。
                if graph
                    .link_closure(tid)
                    .into_iter()
                    .any(|t| t != tid && shared_targets.contains(&t))
                {
                    flags.extend(toolstyle::runtime_search_path(cfg, &build_dir.join("lib")));
                    // 置き場所を移しても辿れる言い方も併せて記録する
                    // （ADR-0041）。共有ライブラリ同士は同じ場所に並ぶ。
                    flags.extend(toolstyle::relocatable_search_path(cfg, "."));
                }
                let args = toolstyle::link_shared(cfg, &inputs_args, &flags, &out, &export_path);

                let mut inputs = objects.clone();
                inputs.extend(libs.iter().cloned());
                inputs.push(export_path.clone());
                let mut deps = compile_ids.clone();
                deps.extend(
                    graph
                        .link_closure(tid)
                        .into_iter()
                        .filter(|t| *t != tid)
                        .filter_map(|t| producer.get(&t).copied()),
                );

                let id = ActionId(plan.actions.len());
                plan.actions.push(Action {
                    id,
                    kind: ActionKind::Link,
                    target: tid,
                    program: linker,
                    args,
                    inputs,
                    outputs: vec![out.clone()],
                    depfile: None,
                    description: format!("SHLIB {}", rel_display(&build_dir, &out)),
                    deps,
                });
                log_trace!("  action[{}] {}", id.0, plan.actions[id.0].command_line());
                producer.insert(tid, id);
                // MSVC では繋ぐ相手が DLL ではなく取り込み用の書庫である。
                // 成果物（動かすもの）と、リンクの入力は別物になる。
                link_inputs.insert(
                    tid,
                    match cfg.style {
                        dowel_eval::config::Style::Msvc => out.with_extension("lib"),
                        _ => out.clone(),
                    },
                );
                if soversion.is_some() && toolstyle::has_link_name_alias(cfg) {
                    link_name_alias(
                        &build_dir.join("lib"),
                        &toolstyle::shared_library_link_name(cfg, &target.name),
                        &out,
                    );
                }
                plan.shared_libraries.push(out.clone());
                plan.declared_exports.push(DeclaredExports {
                    target: tid,
                    library: out.clone(),
                    names: exports.clone(),
                    site: target.site,
                });
                plan.artifacts.insert(tid, out);

                // 同じパッケージの中では静的に繋ぐ（ADR-0038）。`exports` は
                // 「一緒に書かれていないコード」に対する境界であり、兄弟の
                // ターゲットは配る相手ではない——自分の検査が自分の面の外に
                // 出るのは、境界の引き方が1段ずれている（issue #134）。
                //
                // 目的コードは既に位置独立なので、書庫は1回の `ar` で済む。
                if !ar_toolchain_checked {
                    ar_toolchain_checked = true;
                    require_tool(&mut diags, cfg, root_toolchain, "ar", "archiver");
                }
                let archive =
                    build_dir.join("lib").join(toolstyle::archive_name(cfg, &target.name));
                let objs: Vec<String> = objects.iter().map(|o| o.display().to_string()).collect();
                let args = toolstyle::archive(cfg, &archive, &objs);
                let aid = ActionId(plan.actions.len());
                plan.actions.push(Action {
                    id: aid,
                    kind: ActionKind::Archive,
                    target: tid,
                    program: cfg.tool("ar").to_string(),
                    args,
                    inputs: objects.clone(),
                    outputs: vec![archive.clone()],
                    depfile: None,
                    description: format!("AR {}", rel_display(&build_dir, &archive)),
                    deps: compile_ids.clone(),
                });
                log_trace!("  action[{}] {}", aid.0, plan.actions[aid.0].command_line());
                sibling_inputs.insert(tid, (archive, aid));
            }
            TableKind::Lib => {
                // 書庫作成器の実在検査は、書庫を作るときに1度だけ行う。
                // コンパイラと同じく、固定した対象の実在は計画段で確かめる
                // （issue #50）。
                if !ar_toolchain_checked {
                    ar_toolchain_checked = true;
                    require_tool(&mut diags, cfg, root_toolchain, "ar", "archiver");
                }
                let out = build_dir.join("lib").join(toolstyle::archive_name(cfg, &target.name));
                let objs: Vec<String> = objects.iter().map(|o| o.display().to_string()).collect();
                let args = toolstyle::archive(cfg, &out, &objs);
                let id = ActionId(plan.actions.len());
                plan.actions.push(Action {
                    id,
                    kind: ActionKind::Archive,
                    target: tid,
                    program: cfg.tool("ar").to_string(),
                    args,
                    inputs: objects.clone(),
                    outputs: vec![out.clone()],
                    depfile: None,
                    description: format!("AR {}", rel_display(&build_dir, &out)),
                    deps: compile_ids.clone(),
                });
                log_trace!("  action[{}] {}", id.0, plan.actions[id.0].command_line());
                producer.insert(tid, id);
                link_inputs.insert(tid, out.clone());
                plan.artifacts.insert(tid, out);
            }
            TableKind::Bin | TableKind::Test | TableKind::Bench => {
                let out = build_dir.join("bin").join(executable_name(&target.name, cfg));
                // リンク閉包のどこかに C++ の翻訳単位があれば、リンクは C++ の
                // driver で行う。C の driver では C++ 標準ライブラリが付かず、
                // 未定義参照は原因（依存先のソース）から離れた場所で報告される
                let link_needs_cxx = graph
                    .link_closure(tid)
                    .into_iter()
                    .any(|t| has_cxx.get(&t).copied().unwrap_or(false));
                // GNU では driver がリンクを兼ね、MSVC では `link.exe` が別物
                // である（ADR-0027）。
                let linker = cfg.linker(link_needs_cxx).to_string();
                // リンク順は依存元が先。静的ライブラリの解決順の要請による。
                let mut libs: Vec<PathBuf> = Vec::new();
                let mut extra_deps: Vec<ActionId> = Vec::new();
                // 同じパッケージの共有ライブラリには、静的な書庫の方で繋ぐ
                // （ADR-0038）。別のパッケージからは面越しに見る。
                for t in graph.link_closure(tid).into_iter().filter(|t| *t != tid) {
                    if let Some((path, aid)) =
                        link_input(sess, tid, t, &link_inputs, &sibling_inputs)
                    {
                        libs.push(path);
                        match aid {
                            Some(a) => extra_deps.push(a),
                            None => extra_deps.extend(producer.get(&t).copied()),
                        }
                    }
                }
                let mut inputs_args: Vec<String> =
                    objects.iter().map(|o| o.display().to_string()).collect();
                inputs_args.extend(libs.iter().map(|l| l.display().to_string()));
                // 印の無いアセンブリが閉包に居れば、実行可能スタックは
                // リンクの側で断る（ADR-0050）。`link_flags` はこの後に並ぶ
                // ので、本当に要る場合は言い直せる。
                let mut flags = if graph
                    .link_closure(tid)
                    .into_iter()
                    .any(|t| has_unmarked_asm.get(&t).copied().unwrap_or(false))
                {
                    toolstyle::noexecstack_link_flags(cfg)
                } else {
                    Vec::new()
                };
                flags.extend(link_flags.iter().cloned());
                // 実行時の探索路が要るのは、**面越しに**共有ライブラリへ
                // 繋いだときだけである。同じパッケージのものは静的に取り
                // 込んでいるので、走らせる先に `.so` は要らない。
                if graph.link_closure(tid).into_iter().any(|t| {
                    t != tid
                        && shared_targets.contains(&t)
                        && sess.target(t).package != sess.target(tid).package
                }) {
                    flags.extend(toolstyle::runtime_search_path(cfg, &build_dir.join("lib")));
                    // 実行ファイルは `bin/` に、ライブラリは `lib/` に並ぶ。
                    // 両者の相対はビルド木でも入れた先でも同じである（ADR-0041）。
                    flags.extend(toolstyle::relocatable_search_path(cfg, "../lib"));
                }
                let args = toolstyle::link(cfg, &inputs_args, &flags, &out);

                let mut inputs = objects.clone();
                inputs.extend(libs.iter().cloned());
                let mut deps = compile_ids.clone();
                deps.extend(extra_deps);

                let id = ActionId(plan.actions.len());
                plan.actions.push(Action {
                    id,
                    kind: ActionKind::Link,
                    target: tid,
                    program: linker.to_string(),
                    args,
                    inputs,
                    outputs: vec![out.clone()],
                    depfile: None,
                    description: format!("LINK {}", rel_display(&build_dir, &out)),
                    deps,
                });
                log_trace!("  action[{}] {}", id.0, plan.actions[id.0].command_line());
                producer.insert(tid, id);
                plan.artifacts.insert(tid, out);
            }
            other => diags.push(
                Diagnostic::error(
                    "unimplemented-kind",
                    format!("cannot produce an artifact for `{}`", other.name()),
                )
                .at(target.site.file, target.site.span, "unimplemented kind"),
            ),
        }

        // --- 成果物からの派生（`artifacts` ブロック、issue #60） ---
        for decl in &target.artifacts {
            let (Some(input), Some(&producer_id)) =
                (plan.artifacts.get(&tid).cloned(), producer.get(&tid))
            else {
                // 成果物を作れなかった種類。既に診断は出ている。
                break;
            };
            if probed_tools.insert(decl.tool.clone()) {
                require_tool(
                    &mut diags,
                    cfg,
                    root_toolchain,
                    &decl.tool,
                    &format!("{} tool", decl.tool),
                );
            }
            // 出力は成果物の拡張子を置き換えたもの。`firmware` → `firmware.bin`、
            // `libfoo.a` → `libfoo.bin`。書式文字列を持ち込まないための規則。
            let out = input.with_extension(&decl.suffix);
            let mut args: Vec<String> = decl
                .args
                .as_ref()
                .and_then(|v| dowel_eval::specialize(v, &cfg.for_package(&pkg.name)))
                .map(|v| {
                    flatten(&v).iter().filter_map(|v| v.as_str().map(str::to_string)).collect()
                })
                .unwrap_or_default();
            // 入力と出力は末尾に位置で置く（ADR-0008）。
            args.push(input.display().to_string());
            args.push(out.display().to_string());

            let id = ActionId(plan.actions.len());
            plan.actions.push(Action {
                id,
                kind: ActionKind::Transform,
                target: tid,
                program: cfg.tool(&decl.tool).to_string(),
                args,
                inputs: vec![input],
                outputs: vec![out.clone()],
                depfile: None,
                description: format!(
                    "{} {}",
                    decl.tool.to_uppercase(),
                    rel_display(&build_dir, &out)
                ),
                deps: vec![producer_id],
            });
            log_trace!("  action[{}] {}", id.0, plan.actions[id.0].command_line());
            plan.derived.entry(tid).or_default().push(out);
        }
    }

    // 溜めた分をここで出す。宣言1つに1件である（issue #158）。
    diags.extend(abi_against_build.into_iter().map(AbiAgainstBuild::into_diagnostic));

    log_debug!(
        "{} actions ({} compile, {} archive, {} link, {} transform)",
        plan.actions.len(),
        count(&plan, ActionKind::Compile),
        count(&plan, ActionKind::Archive),
        count(&plan, ActionKind::Link),
        count(&plan, ActionKind::Transform)
    );

    (plan, diags)
}

fn count(plan: &Plan, kind: ActionKind) -> usize {
    plan.actions.iter().filter(|a| a.kind == kind).count()
}

/// 構成から来る既定のフラグ。マニフェストの `flags` より前に置き、
/// 記述側が後から上書きできるようにする。
/// ソース1つの言語（[ADR-0048](../../../docs/adr/0048-assembly.md)）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Language {
    C,
    Cxx,
    /// アセンブリ。C の driver が組み立てるが、C ではない
    Asm,
}

/// アセンブリとして扱われる拡張子。
///
/// `.S` は前処理を通り、`.s` は通らない。C の driver はどちらも受けるが、
/// 依存を書くのは前者だけである。`.asm` は MASM や NASM の構文であり、
/// driver は受け取れない——言語はアセンブリのままで、組み立てる道具が違う
/// （[ADR-0050](../../../docs/adr/0050-separate-assembler.md)）。
const ASM_EXTENSIONS: &[&str] = &["s", "S", "asm"];

/// 前処理を通るアセンブリか。依存ファイルを頼めるのはこちらだけである。
fn is_preprocessed_asm(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("S")
}

/// MASM / NASM の構文か。C の driver では組み立てられない（ADR-0050）。
fn is_masm_syntax(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("asm")
}

/// C として翻訳される拡張子。
///
/// `.i` は前処理済みの C である。driver はこれも受け取り、前処理を飛ばす。
const C_EXTENSIONS: &[&str] = &["c", "i"];

/// ソース1つの言語。dowel が翻訳できる綴りでなければ `None`
/// （[ADR-0051](../../../docs/adr/0051-source-language-is-closed.md)）。
fn language(path: &Path) -> Option<Language> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if CXX_EXTENSIONS.contains(&ext) {
        Some(Language::Cxx)
    } else if ASM_EXTENSIONS.contains(&ext) {
        Some(Language::Asm)
    } else if C_EXTENSIONS.contains(&ext) {
        Some(Language::C)
    } else {
        None
    }
}

/// C++ として扱われる拡張子。
///
/// `cc` も `c++` も driver であり拡張子で言語を判別するが、選択の結果は
/// コンパイルだけでなくリンク（標準ライブラリの同伴）にも効くため、
/// こちらでも判別して driver とリンカを選ぶ。
const CXX_EXTENSIONS: &[&str] = &["cc", "cp", "cpp", "cxx", "c++", "CPP", "C"];

fn is_cxx(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| CXX_EXTENSIONS.contains(&e))
}

/// 道具の実在を確かめ、無ければ `missing-toolchain` を積む。
///
/// 診断の組み立ては道具に依らない（宣言があればその行を、無ければ既定に
/// 落ちた旨を指す）。**いつ呼ぶか**だけが道具ごとの判断である：C は常に、
/// C++ は C++ ソースが現れたとき、archiver は書庫を作るとき。要不要は
/// 道具を使う側の意味論であり、表には置かない。
fn require_tool(
    diags: &mut Vec<Diagnostic>,
    cfg: &Config,
    root_toolchain: Option<&dowel_model::package::ToolchainDecl>,
    name: &str,
    what: &str,
) {
    let command = cfg.tool(name);
    if crate::exec::program_exists(command) {
        return;
    }
    // 取ってくる宣言が在って、取れていないなら黙る。取得が成り立たなかった
    // ことは既に述べられており（`unfetchable-toolchain` / `needs-fetch`）、
    // ここで重ねて「PATH に無い」と言うと**別の直し方**を指す——翻訳器を
    // 入れに行く動機になる（issue #159）。
    if let Some(src) = root_toolchain.and_then(|t| t.source.as_ref()) {
        if dowel_model::fetch::existing_toolchain(&src.sha256).is_none() {
            return;
        }
    }
    let mut d =
        Diagnostic::error("missing-toolchain", format!("cannot find the {what} `{command}`"));
    match root_toolchain.and_then(|t| t.tool(name)) {
        Some(t) => d = d.at(t.site.file, t.site.span, "declared here"),
        None => {
            d = d.note(format!(
                "no `[toolchain] {name}` is declared, so the default `{}` is used",
                dowel_eval::config::default_tool(name, cfg.style)
            ))
        }
    }
    diags.push(d.note(
        "it must be on PATH, or come from a toolchain this package fetches (`[toolchain] url`)",
    ));
}

/// リンク閉包から集めた `link_flags`。
///
/// 静的な書庫は自分のリンク要件を運べない。書庫が閉包を辿って最終リンクに
/// 乗る（`libs` の収集）以上、その書庫が要求する `link_flags` も同じ閉包を
/// 辿らなければ、書庫だけがあって記号が解けない（issue #56）。
/// `public` / `private` は**翻訳の伝播**（`includes` / `defines` / `flags`）を
/// 堰き止めるものであり、リンクの到達可能性は制御しない。
///
/// 順序は依存元が先・依存が後（書庫と同じ、静的リンクの解決順の要請）。
/// 重複は畳まない——`link_flags` の併合規則は `append`（順序保持）であり、
/// 閉包の各ノードは一度しか現れないため、二重取りも起きない。
fn closure_link_flags(
    sess: &Session,
    graph: &Graph,
    cfg: &Config,
    tid: TargetId,
    build_dir: &Path,
    diags: &mut Vec<Diagnostic>,
) -> Vec<String> {
    let mut out = Vec::new();
    for t in graph.link_closure(tid) {
        let target = sess.target(t);
        for block in [&target.public, &target.private] {
            let Some(v) = block.get("link_flags") else { continue };
            let cfg = cfg.for_package(&sess.package(target.package).name);
            let Some(v) = dowel_eval::specialize(v, &cfg) else { continue };
            for item in flatten(&v) {
                // 道は絶対パスへ展開する。リンクの作業ディレクトリは
                // ビルドディレクトリであり、パッケージの中のリンカスクリプトを
                // 相対で指しても届かない（issue #70）。
                if let Some(abs) = absolute_path(sess, &item, &cfg, build_dir, diags) {
                    out.push(abs.display().to_string());
                } else if let Some(s) = item.as_str() {
                    out.push(s.to_string());
                }
            }
        }
    }
    out
}

fn collect_sources(
    sess: &Session,
    tid: TargetId,
    cfg: &Config,
    diags: &mut Vec<Diagnostic>,
) -> Vec<PathBuf> {
    let target = sess.target(tid);
    let pkg_root = sess.package(target.package).root.clone();
    let Some(value) = target.root.get("sources") else { return Vec::new() };
    // 具体化は宣言したパッケージで行う（ADR-0017）。
    let cfg = cfg.for_package(&sess.package(target.package).name);
    let Some(value) = dowel_eval::specialize(value, &cfg) else { return Vec::new() };

    let mut out = Vec::new();
    for item in flatten(&value) {
        match &item.data {
            Data::Glob(pattern) => {
                let hits = glob::expand(&pkg_root, pattern);
                if hits.is_empty() {
                    let mut d = Diagnostic::warning(
                        "empty-glob",
                        format!("`glob({pattern:?})` matched no files"),
                    );
                    if let Some(s) = item.prov.nearest_site() {
                        d = d.at(s.file, s.span, "no matches");
                    }
                    diags.push(d.note(format!("scanned {}", pkg_root.display())));
                }
                let site = item.prov.nearest_site();
                out.extend(
                    hits.into_iter()
                        .map(|rel| pkg_root.join(rel))
                        .filter(|p| accept_source(p, site, diags)),
                );
            }
            Data::Path(p) if p.base == PathBase::Package => {
                // 明示されたソースは、ここで実在を確かめる。
                // 通さなければ、無いファイルはビルドツールの「no known rule」に、
                // ディレクトリはリンカの「input file unused」になる。
                // どちらもマニフェストのどの行が原因かを示さない。
                let path = pkg_root.join(&p.rel);
                let site = item.prov.nearest_site();
                match std::fs::metadata(&path) {
                    Ok(m) if m.is_dir() => {
                        let mut d = Diagnostic::error(
                            "invalid-source",
                            format!("`{}` is a directory, not a source file", p.rel),
                        );
                        if let Some(s) = site {
                            d = d.at(s.file, s.span, "a directory cannot be compiled");
                        }
                        diags.push(d.note(format!(
                            "use `glob(\"{}/*.c\")` to take the files inside it",
                            p.rel
                        )));
                    }
                    Ok(_) => {
                        if accept_source(&path, site, diags) {
                            out.push(path);
                        }
                    }
                    Err(e) => {
                        let mut d = Diagnostic::error(
                            "unresolved-path",
                            format!("cannot read `{}`: {e}", p.rel),
                        );
                        if let Some(s) = site {
                            d = d.at(s.file, s.span, "declared here");
                        }
                        diags.push(d.note(format!("looked in {}", pkg_root.display())));
                    }
                }
            }
            Data::Error => {}
            _ => {
                let mut d = Diagnostic::error(
                    "invalid-source",
                    format!("element of `sources` is not a path: {}", item.display()),
                );
                if let Some(s) = item.prov.nearest_site() {
                    d = d.at(s.file, s.span, "expected a path");
                }
                diags.push(d);
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// 翻訳できる綴りか確かめ、できなければ `unknown-source-language` を積む
/// （[ADR-0051](../../../docs/adr/0051-source-language-is-closed.md)）。
///
/// 通してしまうと、C の driver は知らない綴りを**警告つきで受け取り**、
/// 終了状態 0 のまま目的ファイルを書かない。失敗はリンカの、ビルド
/// ディレクトリの中のパスについての言葉になり、元のファイルの名前も行も
/// 残らない。現れない出力を宣言したことにもなるので、増分ビルドは収束
/// しなくなる（issue #157、#112 と同じ形）。
fn accept_source(path: &Path, site: Option<Site>, diags: &mut Vec<Diagnostic>) -> bool {
    if language(path).is_some() {
        return true;
    }
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let mut d = Diagnostic::error(
        "unknown-source-language",
        format!("`{name}` is not in a language dowel can compile"),
    );
    if let Some(s) = site {
        d = d.at(s.file, s.span, "declared as a source here");
    }
    diags.push(
        d.note(format!(
            "sources are C ({}), C++ ({}), or assembly ({})",
            list_extensions(C_EXTENSIONS),
            list_extensions(CXX_EXTENSIONS),
            list_extensions(ASM_EXTENSIONS)
        ))
        .note(
            "the C driver takes an unknown spelling with a warning, writes no object, and exits 0",
        ),
    );
    false
}

/// 拡張子の一覧を `.c` `.i` の形で並べる。
fn list_extensions(exts: &[&str]) -> String {
    exts.iter().map(|e| format!("`.{e}`")).collect::<Vec<_>>().join(" ")
}

/// 伝播してきた `Path` を絶対パスにする。
///
/// 基準点は値ではなく宣言された位置が決める。`libfoo` の `dir("include")` は
/// `libfoo` のルートから解決されなければならない。
fn collect_includes(
    sess: &Session,
    env: &dowel_model::PropMap,
    cfg: &Config,
    build_dir: &Path,
    diags: &mut Vec<Diagnostic>,
) -> Vec<PathBuf> {
    let Some(value) = env.get("includes") else { return Vec::new() };
    let mut out = Vec::new();
    for item in flatten(value) {
        let Some(abs) = absolute_path(sess, &item, cfg, build_dir, diags) else { continue };
        if !out.contains(&abs) {
            out.push(abs);
        }
    }
    out
}

/// `Path` の値を絶対パスにする。`Path` でなければ `None`。
///
/// 基準点は値ではなく宣言された位置が決める。`libfoo` の `dir("include")` は
/// `libfoo` のルートから解決されなければならない。解決できない基準は診断する。
fn absolute_path(
    sess: &Session,
    item: &Value,
    cfg: &Config,
    build_dir: &Path,
    diags: &mut Vec<Diagnostic>,
) -> Option<PathBuf> {
    let Data::Path(p) = &item.data else { return None };
    let base = match p.base {
        PathBase::Package => {
            let pkg = item
                .prov
                .nearest_site()
                .and_then(|s| sess.package_of_file(s.file))
                .map(|id| sess.package(id).root.clone());
            match pkg {
                Some(root) => root,
                None => {
                    diags.push(Diagnostic::error(
                        "unresolved-path",
                        format!("cannot determine the base of `{}`", p.rel),
                    ));
                    return None;
                }
            }
        }
        PathBase::BuildDir => build_dir.to_path_buf(),
        // sysroot（ADR-0047）。宣言が無ければ既定に落とさない——落とすと、
        // 指していない場所を指した命令が組み上がり、誤りはコンパイラの
        // 言葉で返ってくる。
        PathBase::Sysroot => match cfg.sysroot() {
            Some(root) => PathBuf::from(root),
            None => {
                let mut d = Diagnostic::error(
                    "missing-sysroot",
                    "`sysroot()` is written but no sysroot is declared",
                )
                .note(format!(
                    "declare `sysroot = \"...\"` in `[toolchain.{}]` of dowel.toml",
                    cfg.target
                ))
                .note("a relative path is resolved against a fetched toolchain (ADR-0044)");
                if let Some(s) = item.prov.nearest_site() {
                    d = d.at(s.file, s.span, "written here");
                }
                diags.push(d);
                return None;
            }
        },
    };
    // `sysroot()` のように相対が空なら、基準点そのものである。継ぐと
    // 末尾に区切りが付き、`-I <root>/` という見え方になる。
    if p.rel.is_empty() {
        return Some(base);
    }
    Some(base.join(&p.rel))
}

/// `-D` の値。型が形を決める。
///
/// `Str` は C の文字列リテラルとして書く。`Int` と `Bool` は裸のトークンで
/// ある。型付きの値を持つ体系で、`Str` を裸で渡すと `0.4.0` のような版が
/// `%s` に渡せない形で届く——`pkg.version`（ADR-0020）を書く意味が無くなる。
/// 裸のトークンが要る場合は数値か真偽値で書く。
fn collect_defines(env: &dowel_model::PropMap) -> Vec<(String, String)> {
    let Some(value) = env.get("defines") else { return Vec::new() };
    let Some(map) = value.as_map() else { return Vec::new() };
    map.iter()
        .map(|(k, v)| {
            let rendered = match &v.data {
                Data::Str(s) => c_string_literal(s),
                Data::Int(i) => i.to_string(),
                Data::Bool(b) => if *b { "1" } else { "0" }.to_string(),
                _ => String::new(),
            };
            (k.clone(), rendered)
        })
        .collect()
}

/// C の文字列リテラル。引用符と逆斜線だけを逃がす。
fn c_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// `c_std` / `cxx_std` から `-std=...` を1つ組み立てる。
///
/// 併合は `max` であり、閉包の中で最も高い標準が既に選ばれている
/// （`dowel_eval::schema::Merge::Max`）。C++17 を要求するライブラリを
/// C++20 の実行ファイルから使う形が、そのまま通る。
/// ターゲット直下のプロパティを、そのパッケージの構成で具体化して読む。
///
/// `sources` と同じ道である。`public` / `private` に置くものではないため
/// `compile_env` には現れない——繋ぎ方も書き出す記号も伝播しない。
fn root_value(sess: &Session, tid: TargetId, cfg: &Config, name: &str) -> Option<Value> {
    let target = sess.target(tid);
    let value = target.root.get(name)?;
    let cfg = cfg.for_package(&sess.package(target.package).name);
    dowel_eval::specialize(value, &cfg)
}

/// 依存にリンクするとき、どのファイルを渡すか
/// （[ADR-0038](../../../docs/adr/0038-shared-inside-its-package.md)）。
///
/// 同じパッケージなら静的な書庫、別のパッケージなら成果物そのもの。
/// `exports` は配る相手に対する境界であり、兄弟のターゲットは配る相手では
/// ない——パッケージが配布の単位だからである。
fn link_input(
    sess: &Session,
    from: TargetId,
    to: TargetId,
    link_inputs: &BTreeMap<TargetId, PathBuf>,
    sibling_inputs: &BTreeMap<TargetId, (PathBuf, ActionId)>,
) -> Option<(PathBuf, Option<ActionId>)> {
    if sess.target(from).package == sess.target(to).package {
        if let Some((path, aid)) = sibling_inputs.get(&to) {
            return Some((path.clone(), Some(*aid)));
        }
    }
    link_inputs.get(&to).map(|p| (p.clone(), None))
}

/// この目標が、この三つ組へ組まれるか（issue #126）。
///
/// 書かなければ全ての三つ組が対象である。`[package] targets` と同じ綴りで、
/// 掛かる範囲だけが違う——パッケージ全体を絞れても、複数の三つ組を支える
/// ライブラリはそこに書けない。支えるのは4つ、その検査が動くのは3つ、と
/// いう形が書けるようにするのがこのプロパティである。
pub fn supports_target(sess: &Session, tid: TargetId, cfg: &Config) -> bool {
    let declared = collect_root_strs(sess, tid, cfg, "targets");
    declared.is_empty() || declared.contains(&cfg.target)
}

fn collect_root_strs(sess: &Session, tid: TargetId, cfg: &Config, name: &str) -> Vec<String> {
    root_value(sess, tid, cfg, name).map(|v| flatten_strs(&v)).unwrap_or_default()
}

/// 既に在るライブラリの位置（[ADR-0049](../../../docs/adr/0049-prebuilt-libraries.md)）。
///
/// dowel は他のビルドシステムを走らせない（ADR-0001）。cargo も zig も go
/// も、静的ライブラリを作るところまでは各々の道具の仕事であり、dowel が
/// 引き受けるのは**その先**——繋ぐこと、面を伝えること、ABI を突き合わせる
/// こと——である。
fn prebuilt_library(
    sess: &Session,
    tid: TargetId,
    value: &Value,
    cfg: &Config,
    diags: &mut Vec<Diagnostic>,
) -> Option<PathBuf> {
    let target = sess.target(tid);
    // 組むものと、既に在るものの両方は書けない。どちらが成果物かが決まらない。
    if target.root.contains_key("sources") {
        diags.push(
            Diagnostic::error(
                "prebuilt-with-sources",
                format!("`{}` declares both `sources` and `prebuilt`", sess.label(tid)),
            )
            .at(target.site.file, target.site.span, "declared here")
            .note("a target either is built here or was built elsewhere, not both"),
        );
        return None;
    }
    if target.kind != TableKind::Lib {
        diags.push(
            Diagnostic::error(
                "prebuilt-not-a-library",
                format!(
                    "`{}` is a `{}`; only a `lib` can be prebuilt",
                    sess.label(tid),
                    target.kind.name()
                ),
            )
            .at(target.site.file, target.site.span, "declared here")
            .note("what is given is a library to link against, not a program to run"),
        );
        return None;
    }
    let path = absolute_path(sess, value, cfg, Path::new(""), diags)?;
    // 実在は計画段で確かめる。道具の実在を確かめるのと同じ理由——無いまま
    // 進むと、リンカの言葉で1段あとに現れる（issue #50）。
    if !path.is_file() {
        let mut d = Diagnostic::error(
            "missing-prebuilt",
            format!("`{}` names a library that is not there", sess.label(tid)),
        )
        .note(format!("looked for {}", path.display()))
        .note("dowel does not run the build that produces it (ADR-0001); run that first");
        if let Some(s) = value.prov.nearest_site() {
            d = d.at(s.file, s.span, "declared here");
        }
        diags.push(d);
        return None;
    }
    Some(path)
}

/// このターゲット自身が公開している翻訳時の語
/// （[ADR-0043](../../../docs/adr/0043-pkgconfig-generation.md)）。
///
/// `defines` と `flags` を、コンパイラに渡す綴りで返す。dowel の利用者が
/// 受け取るものと、pkg-config の利用者が受け取るものが違ってはならない。
pub fn public_words(sess: &Session, tid: TargetId, cfg: &Config) -> Vec<String> {
    let target = sess.target(tid);
    let pkg_cfg = cfg.for_package(&sess.package(target.package).name);
    let mut out = Vec::new();
    if let Some(v) = target.public.get("defines").and_then(|v| dowel_eval::specialize(v, &pkg_cfg))
    {
        let mut env = dowel_model::PropMap::new();
        env.insert("defines".into(), v);
        for (key, value) in collect_defines(&env) {
            out.push(toolstyle::define(cfg, &key, &value));
        }
    }
    if let Some(v) = target.public.get("flags").and_then(|v| dowel_eval::specialize(v, &pkg_cfg)) {
        out.extend(flatten_strs(&v));
    }
    out
}

/// このターゲット自身が公開しているリンク時の語（ADR-0043）。
pub fn public_link_flags(sess: &Session, tid: TargetId, cfg: &Config) -> Vec<String> {
    let target = sess.target(tid);
    let pkg_cfg = cfg.for_package(&sess.package(target.package).name);
    match target.public.get("link_flags").and_then(|v| dowel_eval::specialize(v, &pkg_cfg)) {
        Some(v) => flatten_strs(&v),
        None => Vec::new(),
    }
}

/// 宣言された ABI 札を、このビルドそのものと突き合わせる
/// （[ADR-0042](../../../docs/adr/0042-abi-label-components.md)）。
///
/// 札同士の比較は誰が何を要求するかを見るが、**このビルドが何であるか**は
/// 見ていない。`libc = "musl"` を要求する面を gnu 向けに組めば、要求は
/// 満たされていない——そしてリンクは通り、失敗は実行時に出る。
///
/// dowel が三つ組から導ける成分に限る。導けないものは、ここで言えることが
/// 何も無い。
fn check_abi_against_build(
    env: &dowel_model::PropMap,
    cfg: &Config,
    reached_by: String,
    found: &mut Vec<AbiAgainstBuild>,
) {
    let Some(value) = env.get("abi") else { return };
    let Some(value) = dowel_eval::specialize(value, cfg) else { return };
    let Data::Map(components) = &value.data else { return };
    let Some(declared) = components.get("libc").and_then(|v| v.as_str()) else { return };
    let actual = dowel_eval::config::triple_env(&cfg.target);
    if declared == actual {
        return;
    }
    let site = components.get("libc").and_then(|v| v.prov.nearest_site());
    // 同じ宣言に2度目は積まない。誰が引いているかだけを足す。
    if let Some(m) = found
        .iter_mut()
        .find(|m| m.site == site && m.declared == declared && !m.reached_by.contains(&reached_by))
    {
        m.reached_by.push(reached_by);
        return;
    }
    if found.iter().any(|m| m.site == site && m.declared == declared) {
        return;
    }
    found.push(AbiAgainstBuild {
        declared: declared.to_string(),
        actual: actual.to_string(),
        triple: cfg.target.clone(),
        site,
        reached_by: vec![reached_by],
    });
}

/// ビルドと合わない ABI 札の宣言1つ分。
///
/// 宣言ごとに1件へ畳むために溜める。この検査は**宣言と構成**の関係であり、
/// 誰が引いているかに依らない——ビルドは一様である（ADR-0031）。目標ごとに
/// 出すと、文面も位置も同じレコードが使う側の数だけ並び、「1つ直せば全部
/// 消える」のか「N 箇所直すところがある」のかが読めない（issue #158）。
struct AbiAgainstBuild {
    declared: String,
    actual: String,
    triple: String,
    site: Option<Site>,
    /// この面を引いている目標。件数ではなく、影響の範囲として述べる
    reached_by: Vec<String>,
}

impl AbiAgainstBuild {
    fn into_diagnostic(self) -> Diagnostic {
        let mut d = Diagnostic::error(
            "abi-mismatch",
            format!(
                "this surface requires `libc = \"{}\"` but the build is `{}`",
                self.declared, self.actual
            ),
        );
        if let Some(s) = self.site {
            d = d.at(s.file, s.span, "declared here");
        }
        d = d.note(format!("the target triple is `{}`", self.triple));
        if !self.reached_by.is_empty() {
            let names: Vec<String> = self.reached_by.iter().map(|t| format!("`{t}`")).collect();
            d = d.note(format!("reached by {}", names.join(", ")));
        }
        d.note("nothing later refuses this; the link succeeds and the failure is at run time")
    }
}

/// このターゲット自身が公開しているヘッダの置き場所
/// （[ADR-0041](../../../docs/adr/0041-install.md)）。
///
/// 合成済みの翻訳環境ではなく、**自分の `public` ブロック**を読む。前者には
/// 依存が伝播させたものが混ざっており、それは依存が配るものである。
pub fn public_include_dirs(sess: &Session, tid: TargetId, cfg: &Config) -> Vec<PathBuf> {
    let target = sess.target(tid);
    let Some(value) = target.public.get("includes") else { return Vec::new() };
    let cfg = cfg.for_package(&sess.package(target.package).name);
    let Some(value) = dowel_eval::specialize(value, &cfg) else { return Vec::new() };
    // 解決できない基準は、翻訳の段で既に診断されている。
    let mut ignored = Vec::new();
    let mut out = Vec::new();
    for item in flatten(&value) {
        let Some(abs) = absolute_path(sess, &item, &cfg, Path::new(""), &mut ignored) else {
            continue;
        };
        if !out.contains(&abs) {
            out.push(abs);
        }
    }
    out
}

/// 版付きの実体の隣に、版を持たない名前を置く
/// （[ADR-0040](../../../docs/adr/0040-shared-library-version.md)）。
///
/// `-lcore` が見つけるのはこの名前である。版付きの実体しか無いと、同じ
/// ディレクトリに在る書庫（ADR-0038）の方が拾われ、共有ライブラリを作った
/// はずのビルドが静的に繋がる。
///
/// 行動としてではなく計画時に置く。中身に依存しない別名であり、実体が
/// まだ無くても構わない——記号連結は、指す先が現れた時点で有効になる。
/// 加えて `dowel` は毎回ここを通るので、消されても次で戻る。
fn link_name_alias(dir: &Path, link_name: &str, target: &Path) {
    let alias = dir.join(link_name);
    if alias == target {
        return;
    }
    let Some(file) = target.file_name() else { return };
    #[cfg(unix)]
    {
        // 相対で指す。ビルド木は移せないが、別名が実体を辿れなくなる理由を
        // 1つ減らす。
        if std::fs::read_link(&alias).ok().as_deref() == Some(Path::new(file)) {
            return;
        }
        let _ = std::fs::remove_file(&alias);
        if let Err(e) = std::os::unix::fs::symlink(file, &alias) {
            log_debug!("cannot place {}: {e}", alias.display());
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (alias, file);
    }
}

/// 共有ライブラリとして繋ぐか（ADR-0030）。
///
/// `lib` 以外では意味を持たない。`bin` に `linkage` を書いても実行ファイルの
/// 作り方は変わらない——書けてしまうことは型検査の範囲であり、ここでは
/// 「何を作るか」だけを決める。
fn is_shared(sess: &Session, tid: TargetId, cfg: &Config) -> bool {
    sess.target(tid).kind == TableKind::Lib
        && root_value(sess, tid, cfg, "linkage").and_then(|v| v.as_str().map(|s| s.to_string()))
            == Some("shared".to_string())
}

fn std_flag(env: &dowel_model::PropMap, name: &str) -> Option<String> {
    env.get(name).and_then(|v| v.as_str()).map(|s| format!("-std={s}"))
}

/// 翻訳の旗。`Path` の要素は絶対パスへ展開する
/// （[ADR-0047](../../../docs/adr/0047-sysroot.md)）。
///
/// `link_flags` と同じ扱いである。`-I` と `sysroot()` を並べて書ける形が
/// 要るのは、文字列連結を持たないためで、そこは `link_flags` が先に
/// 通った道である（issue #70）。
fn collect_flags(
    sess: &Session,
    env: &dowel_model::PropMap,
    cfg: &Config,
    build_dir: &Path,
    name: &str,
    diags: &mut Vec<Diagnostic>,
) -> Vec<String> {
    let Some(value) = env.get(name) else { return Vec::new() };
    let mut out = Vec::new();
    for item in flatten(value) {
        if let Some(abs) = absolute_path(sess, &item, cfg, build_dir, diags) {
            out.push(abs.display().to_string());
        } else if let Some(s) = item.as_str() {
            out.push(s.to_string());
        }
    }
    out
}

/// 具体化済みの値から文字列の列を取り出す。入れ子は最後まで解く。
///
/// 引数の列（`args`）を読む箇所が計画の外にもあるため公開する
/// （`dowel inspect` は計画に載らない、issue #60）。
pub fn flatten_strs(value: &Value) -> Vec<String> {
    flatten(value).iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()
}

/// 列の値なら要素へ、そうでなければ自身を1要素として返す。入れ子は最後まで解く。
/// 理由は `dowel_eval::schema` の同名関数に記した。
fn flatten(value: &Value) -> Vec<Value> {
    match &value.data {
        Data::List(items) => items.iter().flat_map(flatten).collect(),
        Data::Error => Vec::new(),
        _ => vec![value.clone()],
    }
}

fn object_path(
    build_dir: &Path,
    pkg: &str,
    target: &str,
    pkg_root: &Path,
    src: &Path,
    cfg: &Config,
) -> PathBuf {
    let rel = src.strip_prefix(pkg_root).unwrap_or(src);
    // パッケージ外のソースでも衝突しないよう、区切りを潰した名前にする。
    let flat = rel.to_string_lossy().replace(['/', '\\', ':'], "_");
    let ext = toolstyle::object_extension(cfg);
    build_dir.join("obj").join(pkg).join(target).join(format!("{flat}.{ext}"))
}

fn rel_display(base: &Path, p: &Path) -> String {
    p.strip_prefix(base).unwrap_or(p).display().to_string()
}

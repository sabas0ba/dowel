//! ホバー。カーソル位置の語について、スキーマが持っている説明を返す。
//!
//! 説明の出所は `dowel_eval::schema` と `dowel_eval::config` である。
//! `dowel schema dump` が出すものと同じ表を読む。二重に持つと、
//! 片方だけを直したときに黙って食い違う。
//!
//! 語の特定は CST を辿って行う。評価済みの値ではなく木を見るのは、
//! 誤りを含むファイルでも説明を出すためである。編集中の入力こそが
//! ホバーを使う場面である。

use dowel_eval::config::{Domain, VOCABULARY};
use dowel_eval::schema::{self, Block, TableKind};
use dowel_support::Span;
use dowel_syntax::{Node, NodeKind, TokenKind};

/// カーソル位置に対する説明と、その語の範囲。
pub struct Hover {
    pub markdown: String,
    pub span: Span,
}

/// `offset` にある語の説明。説明を持たない位置では `None`。
pub fn at(root: &Node, src: &str, offset: u32) -> Option<Hover> {
    let path = enclosing(root, offset);
    let leaf = *path.last()?;

    match leaf.kind {
        // `[lib.foo.public]` の見出し。段ごとに意味が違う。
        NodeKind::TableHeader | NodeKind::ArrayTableHeader => header(leaf, src, offset),
        NodeKind::KeyPath => match path.iter().rev().nth(1).map(|n| n.kind) {
            Some(NodeKind::TableHeader | NodeKind::ArrayTableHeader) => {
                header(path[path.len() - 2], src, offset)
            }
            _ => property(&path, src, offset),
        },
        NodeKind::KeyValue => property(&path, src, offset),
        NodeKind::Call => call(leaf, src, offset),
        NodeKind::NsRef => cfg_key(leaf, src),
        _ => None,
    }
}

/// 根から `offset` を含む最も深い節点までの道。
fn enclosing(root: &Node, offset: u32) -> Vec<&Node> {
    let mut path = vec![root];
    loop {
        let cur = *path.last().expect("the path always has the root");
        match cur.nodes().find(|n| n.span.start <= offset && offset < n.span.end) {
            Some(next) => path.push(next),
            None => return path,
        }
    }
}

/// `offset` にある識別子トークン。
fn ident_at<'a>(node: &Node, src: &'a str, offset: u32) -> Option<(&'a str, Span)> {
    let t = node.tokens().find(|t| {
        matches!(t.kind, TokenKind::Ident) && t.span.start <= offset && offset < t.span.end
    })?;
    Some((src.get(t.span.range())?, t.span))
}

/// 表の見出し。`[lib.foo.public]` の各段を別々に説明する。
fn header(node: &Node, src: &str, offset: u32) -> Option<Hover> {
    let key_path = node.child(NodeKind::KeyPath)?;
    let segments: Vec<(&str, Span)> = key_path
        .tokens()
        .filter(|t| t.kind == TokenKind::Ident)
        .filter_map(|t| Some((src.get(t.span.range())?, t.span)))
        .collect();
    let index = segments.iter().position(|(_, s)| s.start <= offset && offset < s.end)?;
    let (word, span) = segments[index];

    let markdown = match index {
        0 => {
            let kind = TableKind::parse(word)?;
            let mut md = format!("**`{word}`** — table kind\n\n");
            md.push_str(if kind.is_target() {
                "produces an artifact.\n"
            } else {
                "does not produce an artifact.\n"
            });
            if !kind.is_implemented() {
                md.push_str("\nnot implemented yet (docs/91-implementation-status.md).\n");
            }
            md
        }
        1 => format!("**`{word}`** — the name of this {} target\n", segments[0].0),
        2 => {
            let block = Block::parse(word)?;
            let mut md = format!("**`{word}`** — property block\n\n");
            md.push_str(if block == Block::Public {
                "propagates to dependents.\n"
            } else {
                "applies to this target only.\n"
            });
            md.push_str("\nsee docs/10-manifest.md section 2.\n");
            md
        }
        _ => return None,
    };
    Some(Hover { markdown, span })
}

/// プロパティ名。型と併合規則を出す。
fn property(path: &[&Node], src: &str, offset: u32) -> Option<Hover> {
    let key_value = path.iter().rev().find(|n| n.kind == NodeKind::KeyValue)?;
    let key_path = key_value.child(NodeKind::KeyPath)?;
    // 値の側にカーソルがある場合は説明しない。
    if !(key_path.span.start <= offset && offset < key_path.span.end) {
        return None;
    }
    let (word, span) = ident_at(key_path, src, offset)?;

    // 段が複数ある場合（`private.flags`）、先頭がブロック名になりうる。
    let names: Vec<&str> = key_path
        .tokens()
        .filter(|t| t.kind == TokenKind::Ident)
        .filter_map(|t| src.get(t.span.range()))
        .collect();
    if names.len() > 1 && names[0] == word {
        if let Some(block) = Block::parse(word) {
            return Some(Hover {
                markdown: format!(
                    "**`{word}`** — property block\n\n{}\n",
                    if block == Block::Public {
                        "propagates to dependents."
                    } else {
                        "applies to this target only."
                    }
                ),
                span,
            });
        }
    }

    // ブロックは見出しから決まる。どちらのブロックでも同じ集合であり、
    // 説明も同じであるため、見つかった方を使う。
    let def = schema::lookup(Block::Public, word).or_else(|| schema::lookup(Block::Root, word))?;
    Some(Hover {
        markdown: format!(
            "**`{}`** — `{}`\n\nmerge: `{}`\n\n{}\n",
            def.name,
            def.ty.display(),
            def.merge.name(),
            def.doc
        ),
        span,
    })
}

/// 関数呼び出し。署名と説明を出す。
fn call(node: &Node, src: &str, offset: u32) -> Option<Hover> {
    let (word, span) = ident_at(node, src, offset)?;
    let (sig, doc) =
        schema::FUNCTIONS.iter().find(|(name, _, _)| *name == word).map(|(_, s, d)| (*s, *d))?;
    Some(Hover { markdown: format!("**`{word}`** — `{sig}`\n\n{doc}\n"), span })
}

/// 構成参照。値域を出す。
fn cfg_key(node: &Node, src: &str) -> Option<Hover> {
    let names: Vec<&str> = node
        .tokens()
        .filter(|t| t.kind == TokenKind::Ident)
        .filter_map(|t| src.get(t.span.range()))
        .collect();
    if names.len() != 2 {
        return None;
    }
    let (ns, name) = (names[0], names[1]);
    let (_, _, domain, doc) =
        VOCABULARY.iter().find(|(n, key, _, _)| *n == ns && (*key == name || *key == "*"))?;
    let range = match domain {
        Domain::Finite(values) => format!("one of {}", values.join(", ")),
        Domain::Bool => "true or false".to_string(),
        Domain::Open => "any string. `match` on it requires a `_` arm".to_string(),
    };
    Some(Hover {
        markdown: format!(
            "**`{ns}.{name}`** — {doc}\n\n{range}\n\nthe vocabulary is provisional \
             (Q1 in docs/99-open-questions.md).\n"
        ),
        span: node.span,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `|` を置いた位置でホバーする。位置の指定を本文と一緒に読めるようにする。
    fn hover(marked: &str) -> Option<Hover> {
        let offset = marked.find('|').expect("the fixture needs a cursor marker") as u32;
        let src = marked.replacen('|', "", 1);
        let parsed = dowel_syntax::parse(&src, dowel_support::FileId(0));
        at(&parsed.root, &src, offset)
    }

    fn markdown(marked: &str) -> String {
        hover(marked).unwrap_or_else(|| panic!("no hover for `{marked}`")).markdown
    }

    #[test]
    fn a_property_shows_its_type_and_merge_rule() {
        let md = markdown("[lib.foo.public]\ninc|ludes = [dir(\"include\")]\n");
        assert!(md.contains("`includes`"), "{md}");
        assert!(md.contains("Set<Path>"), "{md}");
        assert!(md.contains("merge: `union`"), "{md}");
    }

    #[test]
    fn the_merge_rule_is_the_one_the_schema_declares() {
        // 併合規則はプロパティごとに違う。説明を1つに固定していないこと。
        assert!(markdown("[lib.a.public]\ndefi|nes = { A = 1 }\n").contains("error_on_conflict"));
        assert!(markdown("[lib.a.public]\nfl|ags = []\n").contains("append"));
        assert!(markdown("[lib.a.public]\na|bi = \"x\"\n").contains("must_equal"));
    }

    #[test]
    fn a_root_property_is_found_too() {
        // `sources` は `public` にはなく、ターゲット直下にある。
        let md = markdown("[bin.app]\nsour|ces = glob(\"src/*.c\")\n");
        assert!(md.contains("`sources`"), "{md}");
        assert!(md.contains("does not propagate"), "{md}");
    }

    #[test]
    fn a_table_kind_says_whether_it_produces_an_artifact() {
        assert!(markdown("[l|ib.foo]\n").contains("produces an artifact"));
        assert!(markdown("[run|ner.riscv64]\n").contains("does not produce an artifact"));
        // 未実装の種別はそう書く。
        assert!(markdown("[ben|ch.b]\n").contains("not implemented"));
    }

    #[test]
    fn a_block_says_whether_it_propagates() {
        assert!(markdown("[lib.foo.pub|lic]\n").contains("propagates to dependents"));
        assert!(markdown("[lib.foo.priv|ate]\n").contains("this target only"));
        // ターゲット直下に書いた形も同じ説明になる。
        assert!(markdown("[lib.foo]\npriv|ate.flags = []\n").contains("this target only"));
    }

    #[test]
    fn the_target_name_is_named_after_its_kind() {
        assert!(markdown("[lib.f|oo]\n").contains("the name of this lib target"));
    }

    #[test]
    fn a_function_shows_its_signature() {
        let md = markdown("[bin.app]\nsources = gl|ob(\"src/*.c\")\n");
        assert!(md.contains("`glob`"), "{md}");
        assert!(md.contains("(Str) -> List<Path>"), "{md}");
    }

    #[test]
    fn a_configuration_key_shows_its_domain() {
        let finite = markdown("[bin.a.private]\nflags = match c|fg.opt { _ => [] }\n");
        assert!(finite.contains("one of debug, release"), "{finite}");

        let open = markdown("[bin.a.private]\nflags = match cfg.tar|get { _ => [] }\n");
        assert!(open.contains("requires a `_` arm"), "{open}");

        let boolean = markdown("[bin.a.private]\nflags = [\"-O2\"] when feat|ure.fast\n");
        assert!(boolean.contains("true or false"), "{boolean}");
    }

    #[test]
    fn the_range_covers_the_word_under_the_cursor() {
        let h = hover("[lib.foo.public]\ninc|ludes = []\n").unwrap();
        let src = "[lib.foo.public]\nincludes = []\n";
        assert_eq!(&src[h.span.range()], "includes");
    }

    #[test]
    fn there_is_no_hover_on_a_value_or_on_whitespace() {
        assert!(hover("[lib.foo.public]\nincludes = [dir(\"inc|lude\")]\n").is_none());
        assert!(hover("[lib.foo.public]\nincludes = []\n|").is_none());
        assert!(hover("|\n[lib.foo]\n").is_none());
    }

    #[test]
    fn an_unknown_word_has_no_hover() {
        // 綴りの誤りに説明を作らない。診断が別に出る。
        assert!(hover("[lib.foo.public]\nnosuch|prop = []\n").is_none());
        assert!(hover("[nosu|ch.a]\n").is_none());
        assert!(hover("[bin.a]\nsources = nosuch|fn(\"x\")\n").is_none());
    }

    #[test]
    fn a_file_with_a_syntax_error_still_answers() {
        // 編集中の入力こそがホバーを使う場面である。
        let md = markdown("[lib.foo.public]\ninclu|des = [dir(\"include\"\n");
        assert!(md.contains("`includes`"), "{md}");
    }

    #[test]
    fn any_position_in_any_prefix_is_answerable() {
        // 位置の指定は外から来る。どの位置でも落ちないこと。
        let src = "[lib.foo.public]\nincludes = [dir(\"include\")]\ndefines = { A = 1 }\n\
                   [bin.a.private]\nflags = match cfg.opt { debug => [], _ => [] }\n";
        let parsed = dowel_syntax::parse(src, dowel_support::FileId(0));
        for offset in 0..=src.len() as u32 + 8 {
            let _ = at(&parsed.root, src, offset);
        }
    }
}

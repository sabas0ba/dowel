# Architecture Decision Records

検討の過程で確定した決定と、その根拠を記録する。
決定を覆す場合は、当該 ADR を Superseded とし、新しい ADR を追加する。

| ADR | 決定 | 状態 |
|---|---|---|
| [0001](0001-toolchain-vs-supply.md) | ツールチェーンは所有、依存供給は委譲する | Accepted |
| [0002](0002-no-daemon.md) | 常駐デーモンを持たない | Accepted |
| [0003](0003-manifest-split.md) | マニフェストを `dowel.toml` と `dowel.build` に分離する | Accepted |
| [0004](0004-syntax.md) | 記述構文は TOML 風方言とし、意味論が同じ要素は既存言語から借用する | Accepted |
| [0005](0005-migration.md) | 移行は動的抽出のみ。静的翻訳は行わない | Accepted |
| [0006](0006-naming.md) | 名称は `dowel` とする（一次調査は未了） | Accepted |
| [0007](0007-implementation-language.md) | 実装言語は Rust とし、コアは標準ライブラリのみで書く | Accepted |
| [0008](0008-runner-transfer.md) | ランナーの転送先パスは位置で決め、文字列補間を導入しない | Accepted |
| [0009](0009-file-identity.md) | `FileId` は正規化したパスのハッシュとする | Accepted |
| [0010](0010-check-scope.md) | `check` は計画段まで走らせる | Accepted |
| [0011](0011-cutoff-and-provenance.md) | 派生の指紋はスパンを含まず、来歴を読む経路はメモを経由しない | Accepted |
| [0012](0012-self-acquisition.md) | dowel 自体の取得は別バイナリが担い、参照は commit sha に固定する | Accepted |

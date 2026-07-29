// 実物の `dowel lsp` と話す検査。
//
// 対象のバイナリは DOWEL_LSP_BIN で指すか、リポジトリの target/ から探す。
// 見つからなければ読み飛ばす（単体の検査はバイナリ無しで回る）。
// コンテナ内で回す場合は musl 版（`cargo build -p dowel-cli --target
// x86_64-unknown-linux-musl`）を使うと、コンテナの libc に依存しない。

import * as assert from "node:assert/strict";
import { spawn } from "node:child_process";
import * as fs from "node:fs";
import * as path from "node:path";
import { test } from "node:test";
import { Connection } from "../src/connection";

function findBinary(): string | null {
  const env = process.env.DOWEL_LSP_BIN;
  if (env !== undefined && env !== "") {
    return fs.existsSync(env) ? env : null;
  }
  const root = path.resolve(__dirname, "..", "..", "..", "..");
  for (const candidate of [
    path.join(root, "target", "x86_64-unknown-linux-musl", "debug", "dowel"),
    path.join(root, "target", "debug", "dowel"),
    path.join(root, "target", "release", "dowel"),
  ]) {
    if (fs.existsSync(candidate)) {
      return candidate;
    }
  }
  return null;
}

const binary = findBinary();

test("実サーバとの一巡", { skip: binary === null && "dowel binary not found" }, async () => {
  const child = spawn(binary as string, ["lsp"], { stdio: ["pipe", "pipe", "inherit"] });
  try {
    const connection = new Connection(child.stdin, child.stdout);
    const diagnostics = new Map<string, unknown[]>();
    let announce: (() => void) | null = null;
    connection.onNotification("textDocument/publishDiagnostics", (params) => {
      const p = params as { uri: string; diagnostics: unknown[] };
      diagnostics.set(p.uri, p.diagnostics);
      announce?.();
    });
    /** 次の診断の通知を待つ。取りこぼしを永遠に待たないよう時間を切る。 */
    const nextPublish = () =>
      new Promise<void>((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error("no diagnostics arrived")), 5000);
        announce = () => {
          clearTimeout(timer);
          announce = null;
          resolve();
        };
      });

    // 初期化。能力の宣言が期待どおりであること。
    const init = (await connection.request("initialize", {
      processId: process.pid,
      rootUri: null,
      capabilities: {},
    })) as { capabilities: { hoverProvider: boolean; textDocumentSync: number } };
    assert.equal(init.capabilities.hoverProvider, true);
    assert.equal(init.capabilities.textDocumentSync, 1, "全文同期であること");
    connection.notify("initialized", {});

    // 誤りのあるマニフェストを開くと診断が届く。
    const uri = "file:///t/dowel.build";
    let published = nextPublish();
    connection.notify("textDocument/didOpen", {
      textDocument: {
        uri,
        languageId: "dowel-build",
        version: 1,
        text: '[bin.app]\nsources = glob(\n',
      },
    });
    await published;
    assert.ok((diagnostics.get(uri) ?? []).length > 0, "構文誤りに診断が出ること");

    // 全文を直すと診断が消える。
    published = nextPublish();
    connection.notify("textDocument/didChange", {
      textDocument: { uri, version: 2 },
      contentChanges: [{ text: '[bin.app]\nsources = glob("src/*.c")\n' }],
    });
    await published;
    assert.deepEqual(diagnostics.get(uri), [], "直した後は診断が空になること");

    // ホバー。`glob` の上で署名が出る。
    const hover = (await connection.request("textDocument/hover", {
      textDocument: { uri },
      position: { line: 1, character: 11 },
    })) as { contents: { kind: string; value: string } };
    assert.equal(hover.contents.kind, "markdown");
    assert.match(hover.contents.value, /glob/);

    // 説明の無い位置では null。
    const nothing = await connection.request("textDocument/hover", {
      textDocument: { uri },
      position: { line: 0, character: 0 },
    });
    assert.equal(nothing, null);

    // `dowel.toml` は厳密な TOML として検査される（ADR-0003）。
    // 方言（`match`）を書き込むと診断が出る。
    const tomlUri = "file:///t/dowel.toml";
    published = nextPublish();
    connection.notify("textDocument/didOpen", {
      textDocument: {
        uri: tomlUri,
        languageId: "toml",
        version: 1,
        text: '[package]\nname = match cfg.opt { _ => "x" }\n',
      },
    });
    await published;
    assert.ok(
      (diagnostics.get(tomlUri) ?? []).length > 0,
      "dowel.toml の方言に診断が出ること",
    );

    // 閉じると診断が消える。
    published = nextPublish();
    connection.notify("textDocument/didClose", { textDocument: { uri: tomlUri } });
    await published;
    assert.deepEqual(diagnostics.get(tomlUri), []);

    // 行儀よく畳み、サーバが自分から終わること。
    await connection.request("shutdown");
    connection.notify("exit");
    const code = await new Promise<number | null>((resolve) =>
      child.once("exit", (c) => resolve(c)),
    );
    assert.equal(code, 0);
  } finally {
    child.kill();
  }
});

// 要求と応答の対応付けの検査。入出力は検査用の緩衝で差し替える。

import * as assert from "node:assert/strict";
import { PassThrough } from "node:stream";
import { test } from "node:test";
import { Connection, LspError } from "../src/connection";
import { FrameReader, encode } from "../src/protocol";

/** サーバ側から見た入出力。クライアントの送信を復号して観察できるようにする。 */
function pair() {
  const toServer = new PassThrough();
  const toClient = new PassThrough();
  const connection = new Connection(toServer, toClient);
  const reader = new FrameReader();
  const sent: Record<string, unknown>[] = [];
  toServer.on("data", (chunk: Buffer) => {
    for (const m of reader.push(chunk)) {
      sent.push(m as Record<string, unknown>);
    }
  });
  const reply = (message: object) => toClient.write(encode(message));
  return { connection, sent, reply };
}

function settled(): Promise<void> {
  return new Promise((resolve) => setImmediate(resolve));
}

test("応答は要求の ID で対応付く", async () => {
  const { connection, sent, reply } = pair();
  const first = connection.request("textDocument/hover", { a: 1 });
  const second = connection.request("textDocument/hover", { a: 2 });
  await settled();
  assert.equal(sent.length, 2);
  const [id1, id2] = sent.map((m) => m.id as number);
  // 逆順に応えても取り違えない。
  reply({ jsonrpc: "2.0", id: id2, result: "second" });
  reply({ jsonrpc: "2.0", id: id1, result: "first" });
  assert.equal(await first, "first");
  assert.equal(await second, "second");
});

test("誤りの応答はコードつきで棄却される", async () => {
  const { connection, sent, reply } = pair();
  const request = connection.request("workspace/symbol");
  await settled();
  reply({
    jsonrpc: "2.0",
    id: sent[0].id,
    error: { code: -32601, message: "`workspace/symbol` is not implemented" },
  });
  await assert.rejects(request, (e: LspError) => e.code === -32601);
});

test("通知は登録した受け口へ届く", async () => {
  const { connection, reply } = pair();
  const received: unknown[] = [];
  connection.onNotification("textDocument/publishDiagnostics", (params) => {
    received.push(params);
  });
  reply({
    jsonrpc: "2.0",
    method: "textDocument/publishDiagnostics",
    params: { uri: "file:///a/dowel.build", diagnostics: [] },
  });
  await settled();
  assert.deepEqual(received, [{ uri: "file:///a/dowel.build", diagnostics: [] }]);
});

test("閉じると待っている要求が全て棄却される", async () => {
  const { connection } = pair();
  const request = connection.request("textDocument/hover");
  await settled();
  connection.close("the language server exited");
  // 棄却しないと、応答を待つ側が永遠に待つ。
  await assert.rejects(request, /exited/);
  // 閉じた後の要求は即座に断られる。
  await assert.rejects(connection.request("textDocument/hover"), /exited/);
});

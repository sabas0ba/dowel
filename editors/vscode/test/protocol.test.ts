// 枠付けの検査。到着の切れ目に対する頑健性が主題である。
// サーバ側の対応物は crates/dowel-lsp/src/rpc.rs の検査にある。

import * as assert from "node:assert/strict";
import { test } from "node:test";
import { FrameReader, encode } from "../src/protocol";

test("符号化と復号が往復する", () => {
  const reader = new FrameReader();
  const message = { jsonrpc: "2.0", id: 1, method: "initialize", params: {} };
  assert.deepEqual(reader.push(encode(message)), [message]);
});

test("長さは文字数ではなくバイト数である", () => {
  // 非 ASCII を含む本文で、枠の切れ目がずれないこと。
  const reader = new FrameReader();
  const first = { method: "textDocument/didOpen", params: { text: "名前 = '値'" } };
  const second = { method: "exit" };
  const bytes = Buffer.concat([encode(first), encode(second)]);
  assert.deepEqual(reader.push(bytes), [first, second]);
});

test("1バイトずつ届いても全件が組み上がる", () => {
  const reader = new FrameReader();
  const messages = [
    { jsonrpc: "2.0", id: 1, method: "initialize" },
    { jsonrpc: "2.0", method: "initialized" },
  ];
  const bytes = Buffer.concat(messages.map((m) => encode(m)));
  const received: unknown[] = [];
  for (const byte of bytes) {
    received.push(...reader.push(Buffer.from([byte])));
  }
  assert.deepEqual(received, messages);
});

test("読めない本文は捨てられ、続く1件は失われない", () => {
  const reader = new FrameReader();
  const bad = Buffer.from("Content-Length: 10\r\n\r\n{ not json", "utf8");
  const good = encode({ method: "exit" });
  assert.deepEqual(reader.push(Buffer.concat([bad, good])), [{ method: "exit" }]);
});

test("他の頭部は読み飛ばされる", () => {
  const reader = new FrameReader();
  const body = '{"method":"exit"}';
  const bytes = Buffer.from(
    "Content-Type: application/vscode-jsonrpc; charset=utf-8\r\n" +
      `Content-Length: ${body.length}\r\n\r\n${body}`,
    "utf8",
  );
  assert.deepEqual(reader.push(bytes), [{ method: "exit" }]);
});

test("頭部の名前は大文字小文字を区別しない", () => {
  const reader = new FrameReader();
  const body = '{"method":"exit"}';
  const bytes = Buffer.from(`content-length: ${body.length}\r\n\r\n${body}`, "utf8");
  assert.deepEqual(reader.push(bytes), [{ method: "exit" }]);
});

test("Content-Length を欠く頭部は読み飛ばして次の枠を探す", () => {
  const reader = new FrameReader();
  const stray = Buffer.from("X-Nothing: 1\r\n\r\n", "ascii");
  const good = encode({ method: "exit" });
  assert.deepEqual(reader.push(Buffer.concat([stray, good])), [{ method: "exit" }]);
});

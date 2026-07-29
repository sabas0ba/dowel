// LSP の枠付け（`Content-Length` 頭部）の符号化と復号。
//
// vscode にも child_process にも依存しない。単体で検査するためである。
// サーバ側の対応物は crates/dowel-lsp/src/rpc.rs にあり、挙動を揃えてある。
// 1件の不正で接続を落とさない点も同じ。

/** 本文を1件、頭部つきで符号化する。長さは UTF-8 のバイト数である。 */
export function encode(message: object): Buffer {
  const body = Buffer.from(JSON.stringify(message), "utf8");
  return Buffer.concat([
    Buffer.from(`Content-Length: ${body.length}\r\n\r\n`, "ascii"),
    body,
  ]);
}

/**
 * 流れの上の枠を切り出す。
 *
 * 到着の切れ目は枠の切れ目と一致しない。頭部の途中でも本文の途中でも
 * 千切れて届くため、足りない分は次の呼び出しまで持ち越す。
 */
export class FrameReader {
  private buffer: Buffer = Buffer.alloc(0);
  /** 頭部を読み終えて本文を待っている場合、その本文のバイト数。 */
  private expected: number | null = null;

  /** 届いた分を継ぎ足し、完成した本文を全て返す。 */
  push(chunk: Buffer): unknown[] {
    this.buffer = this.buffer.length === 0 ? chunk : Buffer.concat([this.buffer, chunk]);
    const messages: unknown[] = [];
    for (;;) {
      if (this.expected === null) {
        const end = this.buffer.indexOf("\r\n\r\n");
        if (end < 0) {
          break;
        }
        const header = this.buffer.subarray(0, end).toString("ascii");
        this.buffer = this.buffer.subarray(end + 4);
        const length = contentLength(header);
        if (length === null) {
          // `Content-Length` を欠く頭部は読み飛ばして次の枠を探す。
          continue;
        }
        this.expected = length;
      }
      if (this.buffer.length < this.expected) {
        break;
      }
      const body = this.buffer.subarray(0, this.expected).toString("utf8");
      this.buffer = this.buffer.subarray(this.expected);
      this.expected = null;
      try {
        messages.push(JSON.parse(body));
      } catch {
        // 読めない本文は捨てる。1件の不正で接続を落とさない。
      }
    }
    return messages;
  }
}

/** 頭部から本文のバイト数を取り出す。頭部の名前は大文字小文字を区別しない。 */
function contentLength(header: string): number | null {
  for (const line of header.split("\r\n")) {
    const colon = line.indexOf(":");
    if (colon < 0) {
      continue;
    }
    if (line.slice(0, colon).trim().toLowerCase() === "content-length") {
      const value = Number.parseInt(line.slice(colon + 1).trim(), 10);
      if (Number.isFinite(value) && value >= 0) {
        return value;
      }
    }
  }
  return null;
}

// 1本の入出力の上で JSON-RPC を話す。
//
// 要求と応答の対応付け、通知の配送だけを持つ。プロセスの起動や再起動は
// 呼び手（extension.ts）の責務である。切り離してあるのは、統合テストが
// vscode 無しで実サーバと話せるようにするため。

import { FrameReader, encode } from "./protocol";

/** サーバが要求を断った場合の誤り。コードは JSON-RPC のもの。 */
export class LspError extends Error {
  constructor(
    public readonly code: number,
    message: string,
  ) {
    super(message);
    this.name = "LspError";
  }
}

interface Pending {
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
}

/** 書き込み先。`child.stdin` と検査用の緩衝の双方が満たす。 */
export interface Sink {
  write(chunk: Buffer): unknown;
}

/** 読み取り元。`child.stdout` と検査用の緩衝の双方が満たす。 */
export interface Source {
  on(event: "data", listener: (chunk: Buffer) => void): unknown;
}

export class Connection {
  private nextId = 1;
  private readonly pending = new Map<number, Pending>();
  private readonly handlers = new Map<string, (params: unknown) => void>();
  private closeReason: string | null = null;

  constructor(
    private readonly output: Sink,
    input: Source,
    private readonly trace?: (line: string) => void,
  ) {
    const reader = new FrameReader();
    input.on("data", (chunk) => {
      for (const message of reader.push(chunk)) {
        this.dispatch(message as Record<string, unknown>);
      }
    });
  }

  /** 要求を送り、応答を待つ。誤りの応答は [`LspError`] で棄却される。 */
  request(method: string, params?: unknown): Promise<unknown> {
    if (this.closeReason !== null) {
      return Promise.reject(new Error(this.closeReason));
    }
    const id = this.nextId++;
    this.trace?.(`--> request ${method} (${id})`);
    this.output.write(encode({ jsonrpc: "2.0", id, method, params }));
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
    });
  }

  notify(method: string, params?: unknown): void {
    if (this.closeReason !== null) {
      return;
    }
    this.trace?.(`--> notify ${method}`);
    this.output.write(encode({ jsonrpc: "2.0", method, params }));
  }

  /** 通知の受け口。1つの method には1つだけ。 */
  onNotification(method: string, handler: (params: unknown) => void): void {
    this.handlers.set(method, handler);
  }

  /**
   * 入出力が閉じた後に呼ぶ。待っている要求を全て棄却する。
   * 棄却しないと、応答を待つ側（ホバー等）が永遠に待つ。
   */
  close(reason: string): void {
    this.closeReason = reason;
    for (const p of this.pending.values()) {
      p.reject(new Error(reason));
    }
    this.pending.clear();
  }

  private dispatch(message: Record<string, unknown>): void {
    if (typeof message.method === "string") {
      // サーバは能力を診断とホバーに絞っており、こちらへの要求は送らない。
      // 届くのは通知だけである。
      this.trace?.(`<-- ${message.method}`);
      this.handlers.get(message.method)?.(message.params);
      return;
    }
    const id = typeof message.id === "number" ? message.id : Number(message.id);
    const p = this.pending.get(id);
    if (p === undefined) {
      return;
    }
    this.pending.delete(id);
    const error = message.error as { code?: number; message?: string } | undefined;
    if (error !== undefined && error !== null) {
      this.trace?.(`<-- error for ${id}: ${error.message}`);
      p.reject(new LspError(error.code ?? 0, error.message ?? "unknown error"));
    } else {
      this.trace?.(`<-- response ${id}`);
      p.resolve(message.result);
    }
  }
}

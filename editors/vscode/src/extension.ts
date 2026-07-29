// vscode との接合。
//
// `dowel lsp`（標準入出力で LSP を話す。docs/30-devexp.md 3.2）を起動し、
// 開いているマニフェストを同期して、診断とホバーをエディタへ渡す。
//
// 既製のクライアントライブラリを使わず自前で書いてあるのは、依存を実行時
// ゼロに保つためである。サーバの能力は全文同期・診断・ホバーに絞られており
// （crates/dowel-lsp）、この範囲なら自前の方が小さく、供給網も短い。

import * as childProcess from "node:child_process";
import * as path from "node:path";
import * as vscode from "vscode";
import { Connection } from "./connection";

/** 言語サーバが見るファイル。名前で決まる（docs/adr/0003-manifest-split.md）。 */
const MANIFEST_FILES = ["dowel.toml", "dowel.build"];

const SELECTOR: vscode.DocumentFilter[] = MANIFEST_FILES.map((name) => ({
  scheme: "file",
  pattern: `**/${name}`,
}));

function isManifest(document: vscode.TextDocument): boolean {
  return (
    document.uri.scheme === "file" &&
    MANIFEST_FILES.includes(path.basename(document.uri.fsPath))
  );
}

/** 短命な失敗をこの回数まで立て直す。超えたら手動の再起動を待つ。 */
const MAX_CRASH_RESTARTS = 3;
/** 起動からこの時間を生きたら、失敗の数え直しをする（ミリ秒）。 */
const STABLE_UPTIME_MS = 10_000;

class Server implements vscode.Disposable {
  private child: childProcess.ChildProcessWithoutNullStreams | null = null;
  private connection: Connection | null = null;
  private starting: Promise<Connection | null> | null = null;
  private crashCount = 0;
  private disposed = false;
  /** 起動失敗の通知は1回だけ出す。ホバーの度に同じ窓を重ねない。 */
  private reportedSpawnFailure = false;

  constructor(
    private readonly diagnostics: vscode.DiagnosticCollection,
    private readonly log: vscode.OutputChannel,
  ) {}

  /** 起動済みの接続。落ちていれば起動を試みる。起動できない場合は null。 */
  ready(): Promise<Connection | null> {
    if (this.connection !== null) {
      return Promise.resolve(this.connection);
    }
    this.starting ??= this.start().finally(() => {
      this.starting = null;
    });
    return this.starting;
  }

  async restart(): Promise<void> {
    this.crashCount = 0;
    this.reportedSpawnFailure = false;
    await this.stop();
    await this.ready();
  }

  private async start(): Promise<Connection | null> {
    if (this.disposed) {
      return null;
    }
    const config = vscode.workspace.getConfiguration("dowel");
    const command = config.get<string>("server.path", "dowel");
    const trace = config.get<boolean>("server.trace", false);

    this.log.appendLine(`starting: ${command} lsp`);
    let child: childProcess.ChildProcessWithoutNullStreams;
    try {
      child = childProcess.spawn(command, ["lsp"], { stdio: "pipe" });
    } catch (e) {
      this.spawnFailed(command, e);
      return null;
    }
    // `spawn` の失敗（実行ファイルが無い等）はイベントで届く。
    const startedAt = Date.now();
    child.on("error", (e) => {
      if (this.child === child) {
        this.child = null;
        this.connection = null;
      }
      this.spawnFailed(command, e);
    });
    child.on("exit", (code, signal) => {
      this.log.appendLine(`server exited (code=${code}, signal=${signal})`);
      if (this.child !== child) {
        return; // 意図した停止、または置き換え済み
      }
      this.child = null;
      this.connection?.close("the language server exited");
      this.connection = null;
      this.diagnostics.clear();
      this.recoverFromCrash(startedAt);
    });
    child.stderr.on("data", (chunk: Buffer) => {
      this.log.append(chunk.toString("utf8"));
    });
    // 終了直後の書き込み（EPIPE）を未捕捉例外にしない。exit の処理が別に走る。
    child.stdin.on("error", (e) => {
      this.log.appendLine(`stdin: ${String(e)}`);
    });

    const connection = new Connection(
      child.stdin,
      child.stdout,
      trace ? (line) => this.log.appendLine(line) : undefined,
    );
    connection.onNotification("textDocument/publishDiagnostics", (params) => {
      this.publishDiagnostics(params as PublishDiagnosticsParams);
    });

    this.child = child;
    try {
      await connection.request("initialize", {
        processId: process.pid,
        rootUri: vscode.workspace.workspaceFolders?.[0]?.uri.toString() ?? null,
        capabilities: {},
        clientInfo: { name: "dowel-vscode" },
      });
    } catch (e) {
      // 初期化に応えないサーバは使えない。プロセスごと畳む。
      this.log.appendLine(`initialize failed: ${String(e)}`);
      child.kill();
      return null;
    }
    connection.notify("initialized", {});
    this.connection = connection;

    // 既に開いているマニフェストを知らせる。診断はこの応答として届く。
    for (const document of vscode.workspace.textDocuments) {
      if (isManifest(document)) {
        this.sendOpen(connection, document);
      }
    }
    return connection;
  }

  private spawnFailed(command: string, error: unknown): void {
    this.log.appendLine(`failed to start \`${command} lsp\`: ${String(error)}`);
    if (this.reportedSpawnFailure) {
      return;
    }
    this.reportedSpawnFailure = true;
    void vscode.window
      .showErrorMessage(
        `dowel: failed to start \`${command} lsp\`. ` +
          "Set `dowel.server.path` to the dowel executable.",
        "Open Settings",
      )
      .then((choice) => {
        if (choice === "Open Settings") {
          void vscode.commands.executeCommand(
            "workbench.action.openSettings",
            "dowel.server.path",
          );
        }
      });
  }

  private recoverFromCrash(startedAt: number): void {
    if (this.disposed) {
      return;
    }
    if (Date.now() - startedAt >= STABLE_UPTIME_MS) {
      this.crashCount = 0;
    }
    this.crashCount += 1;
    if (this.crashCount > MAX_CRASH_RESTARTS) {
      // 立て続けに落ちるサーバを回し続けない。原因はログにある。
      void vscode.window
        .showErrorMessage(
          "dowel: the language server keeps crashing; giving up. " +
            "See the `dowel` output channel.",
          "Restart",
        )
        .then((choice) => {
          if (choice === "Restart") {
            void vscode.commands.executeCommand("dowel.restartServer");
          }
        });
      return;
    }
    setTimeout(() => void this.ready(), 1000);
  }

  // --- 文書の同期。エディタの緩衝が正本であり、サーバはディスクを見ない ---

  private sendOpen(connection: Connection, document: vscode.TextDocument): void {
    connection.notify("textDocument/didOpen", {
      textDocument: {
        uri: document.uri.toString(),
        languageId: document.languageId,
        version: document.version,
        text: document.getText(),
      },
    });
  }

  opened(document: vscode.TextDocument): void {
    if (this.connection !== null) {
      this.sendOpen(this.connection, document);
    } else {
      // まだ起動中なら、起動の仕上げが開いている文書をまとめて送る。
      void this.ready();
    }
  }

  changed(event: vscode.TextDocumentChangeEvent): void {
    if (event.contentChanges.length === 0) {
      return;
    }
    // サーバは全文同期（textDocumentSync: 1）を宣言する。常に全文を送る。
    this.connection?.notify("textDocument/didChange", {
      textDocument: {
        uri: event.document.uri.toString(),
        version: event.document.version,
      },
      contentChanges: [{ text: event.document.getText() }],
    });
  }

  saved(document: vscode.TextDocument): void {
    this.connection?.notify("textDocument/didSave", {
      textDocument: { uri: document.uri.toString() },
    });
  }

  closed(document: vscode.TextDocument): void {
    this.connection?.notify("textDocument/didClose", {
      textDocument: { uri: document.uri.toString() },
    });
    this.diagnostics.delete(document.uri);
  }

  // --- サーバからの通知 ---

  private publishDiagnostics(params: PublishDiagnosticsParams): void {
    const uri = vscode.Uri.parse(params.uri);
    const converted = (params.diagnostics ?? []).map((d) => {
      const diagnostic = new vscode.Diagnostic(
        toRange(d.range),
        d.message,
        toSeverity(d.severity),
      );
      diagnostic.source = d.source;
      diagnostic.code = d.code;
      return diagnostic;
    });
    this.diagnostics.set(uri, converted);
  }

  // --- 停止 ---

  private async stop(): Promise<void> {
    const child = this.child;
    const connection = this.connection;
    this.child = null;
    this.connection = null;
    if (child === null) {
      return;
    }
    this.diagnostics.clear();
    if (connection !== null) {
      // 行儀よく畳む。応えないサーバを待ち続けはしない。
      try {
        await Promise.race([
          connection.request("shutdown"),
          delay(1000),
        ]);
      } catch {
        // 落ち方は問わない。この後 kill する。
      }
      connection.notify("exit");
      connection.close("the client shut the server down");
    }
    await Promise.race([exited(child), delay(1000)]);
    child.kill();
  }

  dispose(): void {
    this.disposed = true;
    void this.stop();
  }
}

interface PublishDiagnosticsParams {
  uri: string;
  diagnostics?: LspDiagnostic[];
}

interface LspDiagnostic {
  range: LspRange;
  severity?: number;
  code?: string;
  source?: string;
  message: string;
}

interface LspRange {
  start: { line: number; character: number };
  end: { line: number; character: number };
}

function toRange(range: LspRange): vscode.Range {
  return new vscode.Range(
    range.start.line,
    range.start.character,
    range.end.line,
    range.end.character,
  );
}

function toSeverity(severity: number | undefined): vscode.DiagnosticSeverity {
  switch (severity) {
    case 1:
      return vscode.DiagnosticSeverity.Error;
    case 2:
      return vscode.DiagnosticSeverity.Warning;
    case 3:
      return vscode.DiagnosticSeverity.Information;
    default:
      return vscode.DiagnosticSeverity.Hint;
  }
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function exited(child: childProcess.ChildProcess): Promise<void> {
  return new Promise((resolve) => child.once("exit", () => resolve()));
}

let server: Server | null = null;

export function activate(context: vscode.ExtensionContext): void {
  const log = vscode.window.createOutputChannel("dowel");
  const diagnostics = vscode.languages.createDiagnosticCollection("dowel");
  server = new Server(diagnostics, log);
  const s = server;

  context.subscriptions.push(
    log,
    diagnostics,
    s,
    vscode.commands.registerCommand("dowel.restartServer", () => s.restart()),
    vscode.workspace.onDidOpenTextDocument((d) => {
      if (isManifest(d)) {
        s.opened(d);
      }
    }),
    vscode.workspace.onDidChangeTextDocument((e) => {
      if (isManifest(e.document)) {
        s.changed(e);
      }
    }),
    vscode.workspace.onDidSaveTextDocument((d) => {
      if (isManifest(d)) {
        s.saved(d);
      }
    }),
    vscode.workspace.onDidCloseTextDocument((d) => {
      if (isManifest(d)) {
        s.closed(d);
      }
    }),
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (e.affectsConfiguration("dowel.server")) {
        void s.restart();
      }
    }),
    vscode.languages.registerHoverProvider(SELECTOR, {
      async provideHover(document, position) {
        const connection = await s.ready();
        if (connection === null) {
          return undefined;
        }
        let result: unknown;
        try {
          result = await connection.request("textDocument/hover", {
            textDocument: { uri: document.uri.toString() },
            position: { line: position.line, character: position.character },
          });
        } catch {
          // サーバの再起動中など。ホバーは静かに諦めてよい。
          return undefined;
        }
        const hover = result as {
          contents?: { value?: string };
          range?: LspRange;
        } | null;
        if (!hover?.contents?.value) {
          return undefined;
        }
        return new vscode.Hover(
          new vscode.MarkdownString(hover.contents.value),
          hover.range !== undefined ? toRange(hover.range) : undefined,
        );
      },
    }),
  );

  // マニフェストが既に開かれていれば起動する。無ければ最初の didOpen まで待つ。
  if (vscode.workspace.textDocuments.some(isManifest)) {
    void s.ready();
  }
}

export function deactivate(): void {
  server?.dispose();
  server = null;
}

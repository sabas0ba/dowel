// 実物の VS Code の拡張ホストの中で走る検査。
//
// エディタ API を通して、診断が届き、直せば消え、ホバーが出ることを見る。
// プロトコル層の検査（test/*.test.ts）と違い、ここでは拡張の起動・設定・
// 文書の同期まで含めた全体が対象である。

import * as assert from "node:assert/strict";
import * as vscode from "vscode";

/** 条件が満たるまで待つ。永遠には待たない。 */
async function waitFor<T>(
  what: string,
  probe: () => T | null | Promise<T | null>,
  timeoutMs = 30000,
): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = await probe();
    if (value !== null) {
      return value;
    }
    if (Date.now() > deadline) {
      throw new Error(`timed out waiting for ${what}`);
    }
    await new Promise((r) => setTimeout(r, 200));
  }
}

export async function run(): Promise<void> {
  const binary = process.env.DOWEL_LSP_BIN;
  assert.ok(binary, "DOWEL_LSP_BIN is required");

  // サーバの場所を先に決める。最初の didOpen が正しいバイナリで走るように、
  // マニフェストを開く前に設定する。
  await vscode.workspace
    .getConfiguration("dowel")
    .update("server.path", binary, vscode.ConfigurationTarget.Global);

  const folder = vscode.workspace.workspaceFolders?.[0];
  assert.ok(folder, "the fixture folder is not open");
  const uri = vscode.Uri.joinPath(folder.uri, "dowel.build");
  const document = await vscode.workspace.openTextDocument(uri);
  await vscode.window.showTextDocument(document);

  // 網羅漏れの match に診断が出る。
  const diagnostics = await waitFor("diagnostics", () => {
    const ds = vscode.languages.getDiagnostics(uri);
    return ds.length > 0 ? ds : null;
  });
  const messages = diagnostics.map((d) => d.message).join("\n");
  assert.match(messages, /non-exhaustive match/, messages);
  assert.equal(diagnostics[0].source, "dowel");
  assert.equal(diagnostics[0].code, "non-exhaustive-match");
  assert.equal(diagnostics[0].severity, vscode.DiagnosticSeverity.Error);

  // `glob` の上にスキーマ由来のホバーが出る。
  // 位置は fixture の2行目 `sources = glob("src/*.c")` の `glob` の中。
  const hovers = await waitFor("a hover over `glob`", async () => {
    const hs = await vscode.commands.executeCommand<vscode.Hover[]>(
      "vscode.executeHoverProvider",
      uri,
      new vscode.Position(2, 11),
    );
    return hs !== undefined && hs.length > 0 ? hs : null;
  });
  const hoverText = hovers
    .flatMap((h) => h.contents)
    .map((c) => (typeof c === "string" ? c : c.value))
    .join("\n");
  assert.match(hoverText, /glob/, hoverText);
  assert.match(hoverText, /List<Path>/, hoverText);

  // 網羅させると診断が消える。
  const edit = new vscode.WorkspaceEdit();
  const everything = new vscode.Range(
    document.positionAt(0),
    document.positionAt(document.getText().length),
  );
  edit.replace(
    uri,
    everything,
    '[bin.app]\nsources = glob("src/*.c")\n\n[bin.app.private]\nflags = match cfg.opt {\n    debug   => ["-O0"],\n    release => ["-O2"],\n}\n',
  );
  const applied = await vscode.workspace.applyEdit(edit);
  assert.ok(applied, "the edit was not applied");
  await waitFor("diagnostics to clear", () =>
    vscode.languages.getDiagnostics(uri).length === 0 ? true : null,
  );

  console.log("e2e: all assertions passed");
}

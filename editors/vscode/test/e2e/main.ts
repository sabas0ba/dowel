// 実物の VS Code を落として拡張テストを回す入口。
//
// `@vscode/test-electron` が VS Code を `.vscode-test/` に取得し、
// この拡張を読み込んだ状態で `suite.ts` の `run()` を実行する。
// Electron は表示先を要るため、ヘッドレス環境では `xvfb-run -a` で包む。
//
// 対象の `dowel` バイナリは統合テストと同じ規則で探す:
// DOWEL_LSP_BIN が最優先、無ければリポジトリの target/ を見る。

import { runTests } from "@vscode/test-electron";
import * as fs from "node:fs";
import * as path from "node:path";

function findBinary(): string {
  const env = process.env.DOWEL_LSP_BIN;
  if (env !== undefined && env !== "") {
    if (!fs.existsSync(env)) {
      throw new Error(`DOWEL_LSP_BIN does not exist: ${env}`);
    }
    return env;
  }
  const root = path.resolve(__dirname, "..", "..", "..", "..", "..");
  const candidates = [
    path.join(root, "target", "debug", "dowel"),
    path.join(root, "target", "release", "dowel"),
    path.join(root, "target", "x86_64-unknown-linux-musl", "debug", "dowel"),
  ];
  for (const candidate of candidates) {
    if (fs.existsSync(candidate)) {
      return candidate;
    }
  }
  throw new Error(
    `no dowel binary; build one (cargo build -p dowel-cli) or set DOWEL_LSP_BIN. looked at:\n${candidates.join("\n")}`,
  );
}

async function main(): Promise<void> {
  // out/test/e2e/ から拡張の根へ。
  const extensionDevelopmentPath = path.resolve(__dirname, "..", "..", "..");
  const fixture = path.join(extensionDevelopmentPath, "test", "e2e", "fixture");
  await runTests({
    extensionDevelopmentPath,
    extensionTestsPath: path.resolve(__dirname, "suite"),
    launchArgs: [
      fixture,
      // 検査に不要な入口を切る。root で回す場合 sandbox は使えない。
      "--disable-workspace-trust",
      "--disable-telemetry",
      "--no-sandbox",
      "--disable-gpu",
    ],
    extensionTestsEnv: { DOWEL_LSP_BIN: findBinary() },
  });
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});

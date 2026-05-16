import { spawn } from "node:child_process";

/**
 * codetta-cli の実行ファイルパス。
 *
 * 解決順:
 * 1. 環境変数 `CODETTA_BIN` (絶対パス推奨)
 * 2. `PATH` 上の `codetta`
 *
 * 1. が未設定の場合は単に `"codetta"` を返し、spawn が `PATH` から
 *    解決する (見つからなければ ENOENT で起動時にエラー)。
 */
export function getCodettaBin(): string {
  const bin = process.env.CODETTA_BIN;
  return bin && bin.length > 0 ? bin : "codetta";
}

export interface CliResult {
  /** プロセス終了コード */
  exitCode: number;
  /** stdout を JSON としてパースした結果 (パース不能なら null) */
  json: unknown;
  /** stdout 生文字列 (デバッグ / ログ用) */
  stdout: string;
  /** stderr 生文字列 (人間向けログ) */
  stderr: string;
}

/**
 * codetta-cli を subprocess として呼び出し、stdout JSON / stderr / 終了コードを返す。
 *
 * 設計: stdout は機械可読 JSON のみ (03-cli.md)。
 * stderr はそのまま MCP server の stderr に伝搬させてもよいが、ここでは
 * 文字列として捕捉して呼び出し側に返す (ログするかは tool ハンドラの判断)。
 */
export function runCli(args: string[]): Promise<CliResult> {
  return new Promise((resolvePromise, reject) => {
    const bin = getCodettaBin();
    const child = spawn(bin, args, { stdio: ["ignore", "pipe", "pipe"] });

    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk: Buffer) => {
      stdout += chunk.toString("utf8");
    });
    child.stderr.on("data", (chunk: Buffer) => {
      stderr += chunk.toString("utf8");
    });

    child.on("error", (err) => {
      reject(
        new Error(
          `Failed to spawn codetta-cli (bin=${bin}): ${(err as Error).message}. ` +
            `Set CODETTA_BIN to an absolute path or ensure 'codetta' is on PATH.`,
        ),
      );
    });

    child.on("close", (code) => {
      const exitCode = code ?? -1;
      let json: unknown = null;
      const trimmed = stdout.trim();
      if (trimmed.length > 0) {
        try {
          json = JSON.parse(trimmed);
        } catch {
          json = null;
        }
      }
      resolvePromise({ exitCode, json, stdout, stderr });
    });
  });
}

/**
 * CLI 呼び出しの結果を MCP tool レスポンス用 JSON にまとめる。
 *
 * - 終了コード 0 で stdout が JSON parseable: そのまま返す
 *   (CLI は `{"ok": true, ...}` を返す約束 = 03-cli.md)
 * - 終了コード != 0: CLI が返した `{ok:false,errors:[...]}` を尊重しつつ、
 *   形が崩れていたら CLI_FAILURE で包む。stderr も `hint` に含める
 */
export async function runCliAsToolResult(args: string[]): Promise<unknown> {
  const result = await runCli(args);

  if (result.exitCode === 0 && result.json !== null) {
    return result.json;
  }

  // 終了コードが 0 でも JSON が空 = 予期しない状態
  if (result.exitCode === 0) {
    return {
      ok: false,
      error: {
        code: "CLI_EMPTY_OUTPUT",
        message: "codetta-cli exited 0 but produced no JSON on stdout",
        hint: "This is likely a bug in codetta-cli. Run the command manually to inspect.",
        context: { args, stderr: result.stderr.slice(0, 4096) },
      },
    };
  }

  // CLI が `{ok:false, errors:[...]}` を返している場合はそれを優先
  if (
    result.json !== null &&
    typeof result.json === "object" &&
    (result.json as { ok?: unknown }).ok === false
  ) {
    return result.json;
  }

  return {
    ok: false,
    error: {
      code: "CLI_FAILURE",
      message: `codetta-cli exited with code ${result.exitCode}`,
      hint: "Inspect 'stderr' in context for human-readable details, then retry with corrected arguments.",
      context: {
        exit_code: result.exitCode,
        args,
        stderr: result.stderr.slice(0, 4096),
        stdout: result.stdout.slice(0, 4096),
      },
    },
  };
}

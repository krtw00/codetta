import { homedir } from "node:os";
import { mkdirSync, statSync } from "node:fs";
import { isAbsolute, join, resolve, sep } from "node:path";

const DEFAULT_WORKSPACE = join(homedir(), "codetta-songs");

/**
 * MCP server が扱うファイルの基準ディレクトリ。
 * 設計: docs/design/04-mcp.md「ワークスペース管理」
 *
 * - `CODETTA_WORKSPACE` 環境変数で指定 (絶対パス)
 * - 未指定なら `~/codetta-songs/`
 * - 存在しなければ作成 (mkdir -p)
 */
export function getWorkspace(): string {
  const raw = process.env.CODETTA_WORKSPACE;
  const ws = raw && raw.length > 0 ? resolve(raw) : DEFAULT_WORKSPACE;
  ensureDir(ws);
  return ws;
}

function ensureDir(p: string): void {
  try {
    const st = statSync(p);
    if (!st.isDirectory()) {
      throw new Error(`CODETTA_WORKSPACE exists but is not a directory: ${p}`);
    }
  } catch (err) {
    if ((err as NodeJS.ErrnoException).code === "ENOENT") {
      mkdirSync(p, { recursive: true });
    } else {
      throw err;
    }
  }
}

/**
 * tool に渡された `path` を絶対パスに解決する。
 * - 絶対パスはそのまま
 * - 相対パスは workspace 配下として解釈
 *
 * セキュリティ: 解決後のパスが workspace を脱出する相対パス
 * (`../../etc/passwd` 等) を弾く。明示的に絶対パスを渡された場合は
 * 信頼する (ユーザー / LLM が意図的に外を指している扱い)。
 */
export function resolveSongPath(input: string): string {
  if (isAbsolute(input)) {
    return resolve(input);
  }
  const ws = getWorkspace();
  const abs = resolve(ws, input);
  const wsWithSep = ws.endsWith(sep) ? ws : ws + sep;
  if (abs !== ws && !abs.startsWith(wsWithSep)) {
    throw new Error(
      `Relative path escapes workspace: ${input} -> ${abs} (workspace: ${ws})`,
    );
  }
  return abs;
}

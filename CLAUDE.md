# codetta

## 1. 必読

`.claude/settings.json` に Notification hook が登録されている場合 (= autopilot 自走モード中) は **§3 autopilot 自走モード** を必読 (= セッション完走時の marker 書込み規約)。

## 2. Plane (Issue 管理)

- identifier: `CDT` — https://plane.codenica.dev/codenica/projects/b2c3b41c-e843-4e08-8a34-867dbd984271/issues/
- 共通運用 (Issue 起票 / commit msg / 状況確認) は global `~/.claude/CLAUDE.md` 「プロジェクト管理 (Plane)」 参照

## 3. autopilot 自走モード (= 実験的、 v3.1)

tmux + Notification hook (= `~/.claude/hooks/autopilot-idle.sh`) で `/handoff` ループを自走させる仕組み。 hook は **event 検出 + DB 記録のみ**、 dispatch は親 Claude が `~/dev/autopilot-control/events.db` を watch して tmux send-keys で投入する。 共通の運用ルール (= モデル側プロトコル / 停止条件 / dialog 前 halt marker) は global `~/.claude/CLAUDE.md` の「autopilot 自走モード」 と `/handoff` command 末尾の「autopilot 自走モード中の強制 step」 を必読。 全体仕様は `~/dev/autopilot-control/README.md`。

### codetta 固有

- **marker file**: `~/.claude/state/autopilot/codetta.json`
- **次サイクル可否判定 (= `ready_for_next` 条件)**:
  - Plane `CDT` の active Issue (= state Backlog / Todo / In Progress) を `mcp__plane__list_work_items` で確認、 残っていれば候補あり
  - **4 点 set** 全 OK: `cargo build --workspace` / `cargo clippy --workspace -- -D warnings` / `cargo fmt --all -- --check` / `cargo test --workspace`
  - 上記を満たさなければ `halt` で `reason` に日本語 1 文を残す
- **monolithic file 注意**: `crates/codetta-core/src/dispatch.rs` / `mcp-server/src/main.rs` / `crates/codetta-cli/src/main.rs` 等は並列衝突注意 (= subagent dispatch で worktree 隔離する場合は scope に含めない or 単独 dispatch)
- **完走後の規約**: marker 書込み + 短い完走報告 (= 1-2 文) で turn 終了。 連続 tool call で long-turn を作らない (= user input 待ち状態に入って 60s で hook 発火条件)
- **subagent dispatch 中の取扱 (= v3.1 既知 trap)**: subagent を background dispatch する場合は **dispatch 直前に halt marker (`status=halt`, `reason="subagent 完走待ち (= <task>)"`) を書いてから dispatch** し、 subagent 完走 + 結果統合 + commit/push 後に marker を上書きする

## 4. HANDOFF.md

- 構造 / max 行数 / 「中断 context 復元用」 専用ルールは global `~/.claude/CLAUDE.md` 「プロジェクト管理 (Plane) と セッション引き継ぎ (HANDOFF.md) の役割分担」 → 「HANDOFF.md (短期 / 作業単位の記憶)」 参照

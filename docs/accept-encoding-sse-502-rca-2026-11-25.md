# 根因分析：透传 `Accept-Encoding` 导致流式 `502 stream_ended_before_commit`（2026-11-25）

> 状态：已定位根因，待本机测试与 daemon 重建验证。  
> 影响版本：`v0.12.0` – `v0.13.4`（自 `v0.12.0` 引入 pre-commit 语义门控后在支持 SSE 压缩的上游触发）。  
> 结论修正：推翻此前“上游不认 `claude-opus-5-5`”的误判——模型本身正常，此前用 `curl` 对照成功是因为 `curl` 默认不发送 `Accept-Encoding`（假阳性）。

---

## 1. 现象与实测证据

客户端（如 Claude Code）通过本地 `xfade` 网关（`v0.13.4`）请求支持压缩的上游（`claude/devops`）时，频繁返回 `502 stream_ended_before_commit`，并触发熔断器（`Open, fails=5`）。

本地网关实测（15s 冷却后交替发送）：

| 请求头 | 模型 | 结果 |
|---|---|---|
| `accept-encoding: gzip, deflate, br` | — | 3/3 → `502 stream_ended_before_commit` |
| `accept-encoding: identity` | — | 3/3 → `200`，正常 SSE |
| `accept-encoding: identity` | `claude-opus-5-5` | `200`（模型本身无问题） |
| `accept-encoding: identity` | `claude-opus-4-8` / `claude-sonnet-5-5` | 均 `200` |

*(注：排查中曾出现过一条 `401`，其成因尚未单独归因，但所有 `502 stream_ended_before_commit` 均由此问题导致。)*

---

## 2. 根因链路

1. **`reqwest` 未启用解压能力**  
   `Cargo.toml` 中 `reqwest` 配置为：
   ```toml
   reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "stream", "json"] }
   ```
   未启用 `gzip`、`deflate`、`brotli` 或 `zstd` feature，因此 `reqwest` 不会自动解压上游响应体。

2. **客户端 `Accept-Encoding` 被原样透传**  
   `crates/core/src/proxy/forward.rs` 的 `STRIP_HEADERS` 未包含 `"accept-encoding"`，`build_upstream_request` 会将客户端发来的 `Accept-Encoding: gzip, deflate, br` 原样转发给上游，诱导上游返回压缩字节流。

3. **v0.12.0 Pre-commit 语义门控解析压缩字节失败**  
   - **流式响应**（`run_streaming_pre_commit`）：直接消费 `resp.bytes_stream()`，交由 `SemanticDetector::inspect_chunk` 用 `String::from_utf8_lossy` 逐行解析 `event:` / `data:`。压缩后的二进制字节流无法识别出任何 SSE commit 帧，持续处于 `Buffering` 状态直至上游流结束，触发 `PreCommitOutcome::Failed("stream_ended_before_commit")`，最终返回 `502`。
   - **非流式响应**（`forward.rs` 非流式分支）：`resp.bytes()` 同样收到压缩字节，导致 `serde_json::from_slice` 失败并报 `502 non_json_response`。
   - **为什么 v0.12.0 之前与 `curl` 均能正常工作**：`v0.12.0` 之前纯透传模式下压缩字节连同 `Content-Encoding` 头直接转给客户端解压；而 `curl` 默认不发送 `Accept-Encoding`（除非显式传入 `--compressed`），上游返回明文，因此对照测试出现了假阳性。

---

## 3. 修复方案

1. **剥离 `accept-encoding` 请求头**  
   在 `crates/core/src/proxy/forward.rs` 的 `STRIP_HEADERS` 中加入 `"accept-encoding"`，使发往上游的请求不携带压缩协商头（上游按 HTTP 默认语义返回未压缩 `identity` 响应）。
2. **回归测试**  
   - 在 `crates/core/src/proxy/testsupport.rs` 的 `RecordedRequest` 中记录上游收到的 `accept_encoding` 请求头。
   - 在 `crates/core/src/proxy/mod.rs` 新增 `test_client_accept_encoding_not_forwarded_upstream`，验证客户端携带 `Accept-Encoding: gzip, deflate, br` 时不会被透传到上游。

---

## 4. 遗留验证与环境注意事项

1. **本地工具链（macOS Homebrew）**：若遇 `cargo` 因 `libgit2` 动态链接旧版 `libllhttp.9.3.dylib` 报错，需先执行 `brew reinstall libgit2` 恢复工具链，再运行 `cargo test` 验证回归测试，并重新构建重启本地 daemon 以替换运行中的 `0.13.4`。
2. **熔断器与模型配置**：新 daemon 启动后确认 `claude/devops` 熔断器恢复闭合；同时核对 `~/.claude/settings.json` 中的 `ANTHROPIC_MODEL` 是否为目标模型（排查期间曾为 `kimi-k3`）。

# Agent Switch 协议互转（v0.3）设计文档

日期：2026-07-26
状态：已确认
前置：v0.2（代理模式）已完成并交付

## 1. 背景与目标

v0.2 代理只做透传：入站协议必须与上游协议一致。v0.3 引入 Anthropic→OpenAI 单向互转，让 Claude Code（发 Anthropic messages）能转发到 OpenAI 兼容上游（chat/completions），覆盖包括 tool_use 在内的完整 agentic 能力。

### 核心决策（已与用户确认）

| 决策点 | 结论 |
|--------|------|
| 互转方向 | Anthropic→OpenAI 单向；反向留 v0.4 |
| 上游 OpenAI 协议 | chat/completions（最通用） |
| tool_use | 完整支持，含流式（聚合-再分片策略） |
| 模型映射 | 路由配 model_override（`asw proxy use --model <openai-model>`） |
| thinking/reasoning | 不转，丢弃 Anthropic thinking 块 |

## 2. 触发与路由

- 入站 `POST /v1/messages`（Anthropic 协议）+ 上游 provider 是 OpenAI 兼容（base_url 指向 OpenAI 系端点）→ 走转换路径
- 入站端点与上游协议匹配时（messages→messages、chat→chat、responses→responses）仍走 v0.2 透传
- 转换判定依据：入站 endpoint == "messages" && 决定走 chat 上游。具体策略：proxy_state 新增 `convert` 标志（默认根据入站 endpoint 自动判定，也可手动强制）

简化决策：**入站 messages 端点的请求，一律按"需要转 chat/completions"处理**（因为本地代理的上游都是 OpenAI 兼容端点；若上游恰好是 Anthropic 兼容，v0.2 透传路径仍可命中——通过比较：如果入站是 messages 且要转 chat，则转换；如果上游 base_url 已是 anthropic 端点，则透传）。

实际实现：proxy_state.routes 项可带协议标记。简化：**proxy_state 加 `target_protocol` 列（"chat"|"messages"），默认 "chat"**。入站 messages + target_protocol=chat → 转换；入站 messages + target_protocol=messages → 透传。

## 3. 转换模块 `core/proxy/convert.rs`

### 请求侧（Anthropic → OpenAI chat/completions）

- `system`（字符串或 content block 数组）→ messages 首条 `{role: "system", content: <拼接文本>}`
- `messages[].content` blocks 转换：
  - text block → content 字符串（单 block）或数组 part（多 block）
  - image block → `{type: "image_url", image_url: {url: <data URI>}}`（透传 base64 data URI，多数上游支持）
  - tool_use block（assistant message 内）→ message 的 `tool_calls: [{id, type:"function", function:{name, arguments: <JSON 字符串>}}]`
  - tool_result block（user message 内）→ 拆成 `{role:"tool", tool_call_id, content}` 消息
- `tools[]`（input_schema）→ `tools[]`（`{type:"function", function:{name, description, parameters: input_schema}}`）
- `tool_choice`：`{type:"auto"}`→`"auto"`；`{type:"any"}`→`"required"`；`{type:"tool", name}`→`{type:"function", function:{name}}`
- `max_tokens` 透传；`temperature`/`top_p` 透传
- 丢弃 `anthropic-version`、`anthropic-beta` headers（**当前 v0.2 STRIP_HEADERS 不含这两个，实现时必须加入**）
- 丢弃 `thinking` content blocks（不转 reasoning）

### 响应侧（OpenAI → Anthropic）

非流式：

| OpenAI 响应字段 | Anthropic 响应字段 |
|----------------|-------------------|
| choices[0].message.content | content[0] = {type:"text", text} |
| choices[0].message.tool_calls[] | content[] += {type:"tool_use", id, name, input}（input 为 arguments JSON 解析后的对象，解析失败则 `{}`） |
| choices[0].finish_reason stop | stop_reason "end_turn" |
| choices[0].finish_reason length | stop_reason "max_tokens" |
| choices[0].finish_reason tool_calls | stop_reason "tool_use" |
| usage.prompt_tokens | usage.input_tokens |
| usage.completion_tokens | usage.output_tokens |
| id/model/role | 透传到顶层 |

流式（chat chunk → Anthropic SSE 事件序列）：

| OpenAI chunk | Anthropic SSE |
|-------------|--------------|
| 首个 chunk（含 role） | `message_start`（含 message.id/model/usage.input_tokens）+ `content_block_start`（text block, index 0） |
| delta.content 文本分片 | `content_block_delta`（{type:"text_delta", text}） |
| delta.tool_calls 出现 | 当前 text block（如有）`content_block_stop`；`content_block_start`（tool_use block, index N） |
| delta.tool_calls[].function.arguments 分片 | **按 `delta.tool_calls[].index` 维护独立缓冲**，聚合该 index 的完整 JSON 字符串 |
| 该 tool_call 完成（finish_reason 出现或下一 index 的 tool_call 开始） | 聚合的完整 JSON 字符串作为 `content_block_delta`（{type:"input_json_delta", partial_json: <完整 JSON>}）一次发出；`content_block_stop` |
| finish_reason 出现 | 末尾 text/tool_use block 的 `content_block_stop`；`message_delta`（{stop_reason, stop_seq:null}+usage.output_tokens）；`message_stop` |

聚合-再分片说明：OpenAI 的 tool_calls arguments 是分片字符串（多个 chunk 的 `delta.tool_calls[i].function.arguments` 拼接才是完整 JSON）。代理按 tool_call_id 聚合完整 JSON 后，作为单个 `input_json_delta` 发出。Anthropic SDK 的 input_json_delta 处理器容忍完整 JSON 字符串（它的设计就是为部分 JSON，但完整 JSON 是其特例）。非真增量但可靠。

## 4. CLI 与数据模型

```bash
asw proxy use <name> [fallback...] [--model <openai-model>] [--target chat|messages]
# --model: model_override（可选）；--target: target_protocol，默认 chat
asw proxy status   # 显示 routes + model_override + target_protocol
```

数据模型迁移：

```sql
ALTER TABLE proxy_state ADD COLUMN model_override TEXT;       -- 可空
ALTER TABLE proxy_state ADD COLUMN target_protocol TEXT DEFAULT 'chat';  -- chat | messages
```

向后兼容（IF NOT EXISTS via PRAGMA table_info 检查列存在性；或 CREATE TABLE 时含新列——但旧 db 已存在，需 ALTER）。迁移用 `ALTER TABLE ... ADD COLUMN`（SQLite 支持，幂等性通过先查 pragma column 检查）。

`asw proxy use yy --model gpt-5.6-luna`：写入 model_override；target_protocol 默认 chat。`--target messages` 可显式设为透传模式（用于上游是 Anthropic 兼容时）。

## 5. 转发流程整合

`forward.rs` 的 `try_forward` 前增加转换分支：

1. 入站请求 body（已读全）+ endpoint
2. 取 routes[0] 的 provider + model_override + target_protocol
3. 若 endpoint == "messages" && target_protocol == "chat"：
   - 调 `convert::request_anthropic_to_openai(body, model_override)` 得 OpenAI 请求
   - 上游 URL 用 `{base}/chat/completions`，method POST，body=转换后
   - 转换后请求体若 `stream==true`，需经 `maybe_inject_stream_options` 注入 `include_usage`（与透传路径同处理）
   - 上游响应：若非流式，`convert::response_openai_to_anthropic` 转回；若流式，**构建新的 stream**：解析 OpenAI SSE chunk → 状态机转 Anthropic SSE 事件 → 输出 Anthropic SSE bytes 给客户端，**同时从转换后的 Anthropic SSE 提取 usage**（不复用 v0.2 的 make_tapped_streaming_body 透传 tap）
4. 否则走 v0.2 透传路径

failover 循环不变（每个 route 都按其 target_protocol 决定转换与否，但 v0.3 简化：整条路由共用一个 target_protocol，存在 proxy_state）。

## 6. 测试

- `convert.rs` 单元测试：请求/响应转换的纯函数测试（各 content block 类型、tool_use、tool_result、finish_reason 映射、usage 映射）
- 流式转换集成测试：MockUpstream 发 OpenAI chat 流式 chunk 序列（含 tool_calls 分片），断言代理转出的 SSE 事件序列正确（message_start/content_block_start/text_delta/tool_use 聚合/content_block_stop/message_delta/message_stop）
- 端到端：Claude Code 风格的 messages 请求 → 代理 → MockUpstream chat 响应 → 断言返回 Anthropic 格式
- model_override：请求里 model 被 override 替换
- target_protocol=messages 时走透传（回归 v0.2 行为）

## 7. 明确不做（v0.3 边界）

- OpenAI→Anthropic 反向互转（v0.4）
- 真增量的 tool_use 流式（聚合-再分片已满足 agentic 场景）
- thinking/reasoning 块转换（丢弃）
- 多模态 image 的格式优化（透传 data URI）
- 按 provider 独立 target_protocol（v0.3 整条路由共用）
- 错误响应格式转换（上游 OpenAI 错误体原样回传客户端，不转 Anthropic 错误格式；Claude Code 容忍非 Anthropic 错误体）

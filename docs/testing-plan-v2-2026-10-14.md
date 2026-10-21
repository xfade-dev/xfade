# V2 发版测试方案（2026-10-14）

范围：V2 整体发版验收 —— 阶段1（即时语法 9 子命令 + 会话地基）+ 阶段2（Smart Fader 意图路由 + analyzer）+ 工作区加固改动（failover body 缓存 / convert 路径跳过 passthrough 变换 / 非 smart 跳过 prompt 提取 / loopback host 精确匹配 / clear_routes 保留 smart 开关）。

## 一、自动化基线（发版前门禁，全绿才可打 tag）

```bash
cargo test --workspace                      # 当前 343 全绿（含新增 smart 动态切换 e2e）
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

重点覆盖（均已有单测/e2e）：

| 模块 | 覆盖点 |
|---|---|
| router/classifier.rs | 7 类意图边界、整词前缀匹配（"format" 不命中 "information"） |
| router/decision.rs | plan_for_intent、HealthSignal 桶内排序（无统计者排后）、loopback host 精确匹配（`127.0.0.1.evil.com` 不判 Local） |
| proxy/forward.rs | failover per-override body 缓存、convert 路径跳过 passthrough 变换、非 smart 跳过 prompt 提取 |
| store/db.rs | clear_routes 保留 smart_enabled、空路由集读作 None |
| proxy/mod.rs e2e ×6 | 轻/重意图路由、smart off 静态序、500 failover、circuit-open 跳过、**运行中开关即刻生效（新增）** |

## 二、真二进制验收（scripts/v2-test.sh，可重复执行）

真 daemon + 真 CLI + 本地双 sink（local/cloud tier），隔离环境（`XFADE_DATA_DIR` + `XFADE_SECRETS=file`），不碰用户配置与钥匙串。

```bash
scripts/v2-test.sh          # 27 项断言
```

覆盖：

1. 静态顺序路由（smart off，配置序优先）+ auth 替换为 provider key
2. Smart Fader：轻意图 local 优先、重意图 cloud 优先
3. `proxy smart on|off` 每请求读 db，**不重启 daemon 即刻生效**
4. failover：local 500 时 cloud 兜底，请求仍 200
5. 上游 4xx 原样透传、不触发 failover
6. `proxy use --model` passthrough body model 字段改写；去 override 后透传客户端 model
7. `proxy clear` → 503，且 clear 不重置 smart 开关（CLI 级验证 db.rs 修复）
8. 即时语法回归：codex/opencode/pi instant launch 拒绝并指引 `use`

已知边界（不验收）：analyzer（指纹/git pulse）只写表无消费方，管线接线未做——只要求写表不报错。

## 三、真实上游双链路（发版前必跑）

```bash
XFADE_TEST_BASEURL=http://<host>:<port> \
XFADE_TEST_KEY=sk-... \
XFADE_TEST_MODEL=<model-id> \
  scripts/v2-test.sh        # 追加第 9 节，共 32 项断言
```

凭据走环境变量（取自仓库外 `~/xfade-test.md`），不进脚本不进 git。
断言：直连/过代理均 200、均有非空 content、finish_reason 一致。
2026-10-14 已用 GLM（glm-5-2-260617）实测 32/32 全绿。

## 四、分发链路（tag push + Release 成功后 24h 内逐条跑）

1. `TMPD=$(mktemp -d) && XFADE_INSTALL_DIR="$TMPD/bin" sh -c 'curl -fsSL https://xfade.sh | sh'` → `--version` 是新版本
2. `brew update`（grep error/failed）+ `brew info xfade-dev/xfade/xfade` 显示新版本（**三段式**）
3. 真机 `brew install xfade-dev/xfade/xfade` + `xfade self-update` → already latest
4. tap 公式改动后 `brew audit --formula xfade-dev/xfade/xfade`（desc <80 字符）

## 五、通过门槛

- 一全绿 → 可提交工作区加固改动
- 一 + 二全绿 → 可打 tag
- 三全绿 → 可发 Release
- 四逐条过 → 发版闭环

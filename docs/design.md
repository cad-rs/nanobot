# nanobot：design harness 框架架构设计

> 本文是**第 0 步：定契约**。目标不是写代码，而是把「什么是框架、什么是产品」定成
> 后面每一步的验收标准。
>
> 来源：从 `apps/ocad`（AI CAD 设计工作台）抽取通用 agent 框架到独立仓库
> `cad-rs/nanobot`（remote: https://github.com/cad-rs/nanobot.git）。
> 上游产品是 ocad，留在 rcad-pro；框架被 rcad-pro 以 git submodule 方式消费
> ——与 `cad-rs/workbench-rs` 之于 creator 的关系同构。
>
> **修订 2026-10-22**：范围扩大——nanobot 除内核外**还要提供 gpui 壳、脚本化插件界面、
> 以及 wgpu 渲染槽**（用户/插件可自定义渲染）。见 §1.2、§3、§4.8、§4.9。
>
> **修订 2026-10-22（用户裁决）**：第 3.5 步方向确认——**gpui 壳落在 nanobot**
> （`nanobot-ui`，§4.8），**产品侧保留 wgpu 渲染入口**（`nanobot-gpu` 的共享设备槽
> `shared_gpu()` + `paint_surface()`，§4.9）。本文自本修订起在 nanobot 仓库落地
> （NQ-1 定案）；rcad-pro 侧 `docs/nanobot-architecture.md` 只留指针。
>
> 相关文档（rcad-pro 侧）：`docs/design-harness.md`（ocad 产品设计）、
> `docs/design-harness-architecture.md`（ocad 插件化架构，本设计的输入）、
> `docs/agent-native-layering-principles.md`（分层原则）、
> `docs/agent-host-survey-20261015.md`（自持/借用边界调研）。
> 与 ocad 产品设计的冲突时以 `design-harness.md` 为准；本设计只覆盖「抽出来的框架长什么样」。

---

## 一、定位与范围

### 1.1 nanobot 是什么

nanobot 是一个 **agent harness 框架**：给定领域引擎、工具集和项目容器，
它把 turn/step 循环、事件溯源会话、工具瀑布、审批、plan 状态机、LLM 适配器缝、
skill 运行器、装配流程、**gpui 工作台**、**脚本化插件界面**与 **wgpu 渲染槽**
组合成一个可运行的 harness。

harness 一词沿用 `design-harness.md` §9.1：**承力框架 / 试验台架**——建模、校验、
迭代、能力复用的每一步都在台架上有明确挂点。nanobot 就是那套挂点的实现。

### 1.2 三层结构

nanobot 是三层，层间依赖单向（内核不知道壳的存在）：

```
┌─ 产品层（ocad，留在 rcad-pro）──────────────────────────────┐
│  CAD 引擎 / CAD 工具体 / CAD 验证器 / 领域 skill 包           │
└───────────────┬─────────────────────────────────────────────┘
                │ 实现产品契约（§4）
┌───────────────▼─ 壳层（nanobot-ui / nanobot-rhai / nanobot-gpu）┐
│  gpui 工作台（会话工作区 + 对话流 + plan 审查 + 审批面）        │
│  脚本插件宿主（Rhai → 面板 / 视图 / 工具 / 工具栏）             │
│  GPU 渲染槽（共享设备 + 离屏目标 + 零拷贝合成）                 │
└───────────────┬─────────────────────────────────────────────┘
                │ 事件流 / 审批通道 / 命令投递
┌───────────────▼─ 内核层（GUI-free）──────────────────────────┐
│  cordis / llm / session / tools / agent / skills / boot        │
└──────────────────────────────────────────────────────────────┘
```

- **内核层 GUI-free**：`nanobot-cordis` / `llm` / `session` / `tools` / `agent` /
  `skills` / `boot` 的依赖里**不得出现** `gpui*` / `wgpu`。内核可以被 CLI、测试、
  MCP server 复用，不绑 GUI（`apps/rcad-agent-server` 就是现成的例子）。
- **壳层是 gpui 层**：`nanobot-ui` / `nanobot-rhai` / `nanobot-gpu` 依赖 gpui。
  产品可以直接换掉壳层而保留内核（自检模式就是这么做的）。

### 1.3 自持 / 借用边界（沿用 10.5 裁决）

**自持**（换掉 = 换产品）：turn/step 循环与 plan 状态机、事件日志与 seq、
审批/决策、工具注册表与三道瀑布、prompt 分节装配、local-first 持久化。

**借用且必须可替换**：provider 适配底座（`reqwest` native+wasm 同源 +
`eventsource-stream`）、MCP client（`rmcp`，仅桌面）、对话流渲染
（`pulldown-cmark`）、token 计数（`tiktoken-rs`）。

**不借**：任何自带 session / loop / 消息词汇表的框架。

**运行时纪律（写进 CI 检查，不是文档约定）**：内核 crate 的依赖里不得出现
tokio 的 `rt-multi-thread` 或运行时入口——tokio 在 wasm32-unknown-unknown 上只允许
`sync,macros,io-util,rt,time`，而 web 形态必须交付；loop 骑宿主执行器
（桌面 gpui / web `wasm_bindgen_futures::spawn_local`）。

### 1.4 nanobot 不是什么

- **不是现成 agent 框架**（不是 `rig-core` / `aisdk` 一类）。它**自己持有** loop、
  事件日志与 seq、plan、审批、工具注册表——理由见
  `docs/agent-native-layering-principles.md` 与 `design-harness-architecture.md` §10.5：
  换掉这些就是换产品。
- **不是**「代码插件框架」。插件 = 编译期 Rust 函数 + 声明式包解释执行，
  第三方能力只能是声明式包（skill / knowledge manifest），不携带代码
  （`design-harness.md` §5、§20 裁决不变）。
- **不替代 workbench-rs**。两者是不同轨道：workbench-rs 是通用**应用**框架
  （文档/视图/面板/ribbon/命令），nanobot 是 **agent harness** 框架。
  关系见 §3.3 与裁决点 F。

---

## 二、现状盘点：抽取比预期干净

现状以代码为准（2026-10-21/22 实测，`target/` 不计入）：

| crate | LOC | 依赖 | rcad/CAD 耦合 |
|---|---|---|---|
| `ocad-cordis` | 636 | **零依赖** | **无** |
| `ocad-llm` | 936 | reqwest / eventsource-stream / futures-* | **无** |
| `ocad-agent` | 1476 | `ocad-llm`/`session`/`tools` + **`ocad-engine`/`ocad-verify`** | `EngineExecutor` 一个具体执行器 |
| `ocad-session` | 1313 | serde / serde_json / web-time | `proposal.rs`(229) + 4 个 `sketch_proposal/*` 事件 |
| `ocad-tools` | 11467 | serde / serde_json / thiserror | `sketch_ai/`(≈9.5k) + `builtin.rs`(173) + `contract.rs` 名字映射 |
| `ocad-skills` | 1347 | `ocad-cordis` | `builtins.rs` 的 5 个 CAD skill |
| `ocad-boot` | 1128 | serde / toml / uuid / crc32fast | `DocumentKind`、`.cadx`、`units`/`materials`、`CheckConfig` |

三条结论：

1. **三个 crate 可以直接整体搬走**：`ocad-cordis`（零依赖）、`ocad-llm`、
   `ocad-agent` 减去一个 `EngineExecutor`（60 行）。
2. **三个 crate 的通用核很小**：`ocad-session` 通用核约 850 行（总 1313），
   `ocad-tools` 通用核约 350 行（总 11467），`ocad-skills` 通用核约 940 行（总 1347）。
   拆分是机械的，不是重构。
3. **CAD 耦合是反向的**：`ocad-tools` 的依赖里没有任何 rcad crate；
   CAD 侧（`ocad-sketch-host`）依赖 `ocad-tools` 并实现它的 trait。
   把通用核搬走不会切断任何 CAD 调用路径。

另有一条旁证说明内核确实已被共享：`apps/rcad-agent-server`（MCP server）已经依赖
`ocad-session` + `ocad-tools`。这不是「为对齐而抽」，而是「已经在被两处用」。

### 2.1 一个必须先处理的问题：装配点住在产品里

真正的「Harness 装配」是 `apps/ocad/src/agent_host.rs::assemble()`（60 行），
不是 `ocad-boot`——`ocad-boot` 只是项目/manifest 层（其 lib 文档自述
「no gpui, no workbench」），且它**没有** `Harness` 结构体
（工作区里唯一的 `Harness` 在 `libs/rcad-harness/src/model.rs:91`，
那是线束/电缆 CAD 域模型，与 agent harness 无关）。

所以第 3 步的工作是把这个装配点**上升**为框架能力，而不是在 boot 里重写。

### 2.2 一段不该抽的死代码

`ocad-boot/src/config.rs`（Bundle / Profile / Patch 配置组合，300 行）
**没有任何生产调用者**——只被自己的 `lib.rs` re-export 和自己的测试引用
（`design-harness-architecture.md` §8 描述的 bundle/profile/patch 组合「仍待 M3」）。

把它抽进框架，就是仓库纪律里「不为对齐而建空壳」的教科书案例。
处理方式：**删**，或者在 ocad 里接上再抽。本文按「删」规划。

### 2.3 壳与渲染的现状

- **ocad 有壳，但没有插件化界面**。`libs/ocad-shell`（3204 行）是硬编码的 gpui 壳
  （会话列表 + 会话工作区 + 领域面板），**没有任何 Rhai/脚本插件机制**
  （`grep -rn rhai libs/ocad-*/src` 零命中）。产品想要「插件提供自定义界面」，
  今天做不到。
- **ocad 有视口，但渲染不是插件化的**。`libs/ocad-viewport`（552 行）是
  wgpu↔gpui 零拷贝互操作（`WgpuContextHandle::from_window` → `WgpuRenderTarget`
  → `WgpuRenderer` → `window.paint_surface(SurfaceSource::Texture)`），
  由 `ocad-shell` 直接组装；creator 经 `apps/creator/src/rcad_view.rs`
  包成 `wb::ViewInstance`。**它的依赖是 CAD 的**（`rcad-render` 的
  `Mesh`/`Camera`/`SceneObjectUpload`/`WgpuRenderer`），
  所以「渲染机制」通用、「场景类型」是产品的。
- **workbench-rs 已经有这两块的成熟形态**（本设计的主要参照）：
  - `workbench-ui/src/gpu_view/`（1591 行）：`pub fn shared_gpu(window) -> Option<Rc<Gpu>>`
    ——注释明确写着「供插件自带的自定义 gpui `Element` 建自己的渲染资源」。
    这正是「用户自定义 wgpu 渲染」的现成落点。
  - `workbench-rhai/src/host.rs`（275 行）+ `.vendor/gpui-rhai`（93k 行）：
    Rhai 脚本经 `register_panel` / `register_view` 声明界面，由 gpui-rhai 编译成
    声明式 UiNode 树渲染（`ScriptViewHost` + `FileScriptView::mount`）。

---

## 三、crate 布局

### 3.1 布局表

内核层（GUI-free）：

| crate | 来源 | 职责 |
|---|---|---|
| `nanobot-cordis` | `ocad-cordis` 整体搬 | 插件内核：`Context` / `Events` / `Fiber` / `Disposable` |
| `nanobot-llm` | `ocad-llm` 整体搬 | `LlmAdapter` 缝 + `StreamChunk` / `BlockAssembler` + 内置适配器 |
| `nanobot-session` | `ocad-session` 的通用核 | 事件日志 + 单调 seq + `derive_messages()` + JSONL + 时间旅行 fork |
| `nanobot-tools` | `ocad-tools` 的通用核 | `ToolRegistry` + 三道瀑布 + `ApprovalPolicy` |
| `nanobot-agent` | `ocad-agent` 去掉 `EngineExecutor` | `Agent` / `Inbox` / turn-step 驱动 / `Plan` 状态机 |
| `nanobot-skills` | `ocad-skills` 去掉 CAD builtins | skill manifest → 监听器组合（fiber 语义） |
| `nanobot-boot` | 新写（装配）+ 项目容器 | `Harness` 装配 + CLI 模式 + 自检 + 项目容器 |

壳层（gpui / wgpu）：

| crate | 来源 | 职责 |
|---|---|---|
| `nanobot-gpu` | 从 `ocad-viewport` 抽机制 | wgpu↔gpui 桥：共享设备 + 离屏目标 + 零拷贝合成；**渲染槽**（见 §4.9） |
| `nanobot-rhai` | 照 `workbench-rhai` 再造 | Rhai 脚本插件宿主：manifest → 命令/面板/视图/工具栏（见 §4.8） |
| `nanobot-ui` | 从 `ocad-shell` 抽通用壳 | gpui 工作台：会话工作区 + 对话流 + plan 审查 + 审批面 + 停靠面板（见 §4.8） |

产品侧（留在 rcad-pro，前缀 `ocad-` 不变）：

| crate | 说明 |
|---|---|
| `ocad-engine` | CAD 引擎传输边界，**100% 产品** |
| `ocad-render` | CAD 渲染器（`Mesh`/`Camera`/`WgpuRenderer`），**产品**（从 rcad-render 视角） |
| `ocad-shell` | 从 `ocad-shell` 留下的 CAD 面板（特征树/属性/检查/材料/规则/diff） |
| `ocad-verify` | 写入门与验证器，**产品**（机制考虑提升为框架能力，见 §4.4） |
| `ocad-sketch-host` | AI-1/AI-3 草图工具后端，**产品** |
| `ocad-tools` | 从 `ocad-tools` 拆出的 CAD 工具体 + `sketch_ai/` |
| `ocad-skills` | 从 `ocad-skills` 拆出的 CAD builtins |
| `ocad-project` | 项目 manifest 的 CAD 扩展（见 §4.7） |
| `apps/ocad` | 产品二进制（装配 + 壳 + CLI） |
| `apps/rcad-agent-server` | MCP server，内核的第二个消费者 |

**依赖方向（禁止成环）**：

```
nanobot-cordis ─► nanobot-llm ─► nanobot-session ─► nanobot-tools ─► nanobot-agent ─► nanobot-boot
                                        └─► nanobot-engine（工具的执行基座，产品侧）        │
nanobot-skills ─►（被 boot 装配，监听 agent/tools 事件）◄────────────────────────────────┘

nanobot-gpu ─► nanobot-ui
nanobot-rhai ─► nanobot-ui
nanobot-boot ─► nanobot-ui（装配产物 = 内核 + 壳）

apps/ocad ─► nanobot-boot + ocad-project + ocad-engine + ocad-shell + ocad-gpu
rcad-agent-server ─► nanobot-session + nanobot-tools + ocad-sketch-host
```

**内核层不得依赖壳层**：`nanobot-cordis`/`llm`/`session`/`tools`/`agent`/`skills`/`boot`
的依赖树里不允许出现 `nanobot-ui` / `nanobot-gpu` / `nanobot-rhai` / `gpui*` / `wgpu`。

### 3.2 CI 硬约束（对标 R8）

用脚本检查，不靠自觉：

1. `nanobot-*` **内核** crate 的依赖里**不得出现** `rcad-*` / `ocad-*` / `gpui*` / `wgpu`。
2. 内核 crate 的依赖里**不得出现** tokio 的 `rt-multi-thread` 或运行时入口。
3. `ocad-shell` / `ocad-agent` 类的产品 crate **不得**被内核 crate 反向依赖。
4. provider 类型只能出现在 `nanobot-llm` 的适配器实现内部；
   `nanobot-session` / plan / 锚定不得出现 provider 类型。
5. **`nanobot-gpu` 的共享设备是唯一的 wgpu 入口**：产品与插件的自定义渲染必须经
   `nanobot_gpu::shared_gpu()` 拿设备，不得自建设备（Windows/Web 上自建无法零拷贝合成，
   见 §4.9）。
6. wgpu 版本在 rcad-pro 根 workspace 单一 pin（当前 29.0.3）；nanobot 与 workbench-rs
   的 vendor 必须指向同一提交（见 §3.3）。

### 3.3 vendor 与 wgpu 版本（裁决点 F）

nanobot 的壳层需要三个 vendored crate，与 workbench-rs 完全相同：

| crate | 用途 | workbench-rs 位置 |
|---|---|---|
| `gpui-ce` | UI 框架（含 `gpui_wgpu` 渲染器） | `.vendor/gpui-ce` |
| `gpui-component` | DockArea / Panel / 组件库 | `.vendor/gpui-component` |
| `gpui-rhai` | Rhai → 声明式 UiNode 树运行时 | `.vendor/gpui-rhai` |

**风险**：rcad-pro 同时依赖 workbench-rs（creator 基座）与 nanobot（ocad 基座）。
两处各自 vendor `gpui-ce`，若提交不一致，**同一个构建里会出现两个 `gpui-ce`**——
类型不互通，且 wgpu 版本错位会让零拷贝合成静默失败。

**建议（裁决点 F）**：nanobot **自持 vendor，但钉死与 workbench-rs 相同的提交**；
rcad-pro 根 `[patch.crates-io]` 只保留**一份** `gpui-ce` / `gpui-component` /
`gpui-rhai` 的 patch 源（当前已镜像 workbench-rs 的表，加上 nanobot 时保持同源）。
**若两份 vendor 将来漂移，escape hatch 是抽 `cad-rs/gpui-kit`** 作为共享 vendor 仓
（workbench-rs 与 nanobot 共同依赖）——那是独立的一次重构，不在本设计范围。

---

## 四、产品契约

这是本设计最核心的部分。workbench-rs 之所以好用，是因为产品作者要实现的契约
非常清楚（`Workbench` trait + `Registry` + `product.toml` + CLI 模式）。
nanobot 需要同构的一份。**一个 design harness 产品要提供 10 样东西。**

### 4.1 身份描述符 `harness.toml`

对标 `workbench-api/src/product.rs` 的 `ProductProfile`：`#[serde(default)]` 全字段可选，
只有 `id` 和 `name` 必填；加载失败**永不致命**（回退内置默认 + stderr 告警 + 落到下一个搜索目录）。

```toml
[harness]
id = "com.rcad.ocad"
name = "ocad"
version = "0.1.0"              # 缺省取 CARGO_PKG_VERSION
backend = "gpui"               # 诊断标签

[defaults]
locale = "zh-CN"
approval = "auto"              # auto | deny-all | ui

[provider]                     # 可选：真实推理；缺省用 ScriptedAdapter
kind = "openai-compatible"
endpoint = "https://..."
model = "gpt-6"
```

框架内置 `nanobot_boot::harness_toml::{default_search_dirs, load_from_dirs}`，
语义照抄 workbench-rs 的 `product.rs`（exe 旁 → cwd → 产品自己追加的目录）。

### 4.2 工具后端：泛化后的 `ToolExecutor`

**现状是最需要先修的一个点**：`ocad-agent/src/agent.rs` 的 `ToolExecutor` trait
有 6 个方法，其中 4 个的默认实现直接漏出 CAD 类型：

```rust
fn apply(&self, ...)   -> ocad_verify::ContractedWrite;
fn snapshot(&self)     -> Option<ocad_engine::DocumentSnapshot>;
fn point_inside(&self, points: &[[f64; 3]]) -> Option<Vec<bool>>;
fn checkpoint(&self)   -> Option<ocad_engine::DocumentCheckpoint>;
fn restore(&self, checkpoint: ocad_engine::DocumentCheckpoint);
```

泛化后的形状（**裁决点 A**，见 §6）：

```rust
/// 领域引擎的执行后端。agent 记录 tool/call 后调用它，返回的字符串被记录为
/// tool/result。默认实现把「回滚」「快照」「验证」全部降级为「不支持」。
pub trait ToolExecutor: Send + Sync {
    fn execute(&self, name: &str, arguments: &Value) -> String;

    /// 能否回滚一次写入。false 时写入门直接跳过（没有东西可恢复）。
    fn can_rollback(&self) -> bool { false }

    /// 在写入门下执行工具：执行 → 验证 → 违约则回滚。
    /// 默认实现 = execute + 未验证。
    fn apply(&self, name: &str, arguments: &Value, verifier: &dyn WriteVerifier) -> WriteOutcome {
        let reply = self.execute(name, arguments);
        WriteOutcome { reply, verified: false, report: Value::Null }
    }

    /// 捕获检查点，供 plan 或违约写回滚。None = 无引擎后端。
    fn checkpoint(&self) -> Option<Box<dyn RestorePoint>> { None }
    fn restore(&self, checkpoint: Box<dyn RestorePoint>) {}
}

/// 类型擦除的回滚点：产品侧 downcast 回自己的检查点类型。
pub trait RestorePoint: Send + Sync {}

/// 产品提供的验证器：写入是否兑现了 agent 的承诺。
pub trait WriteVerifier: Send + Sync {
    fn verify(&self, before: &dyn RestorePoint, after: &dyn RestorePoint) -> Result<(), Value>;
}

pub struct WriteOutcome {
    pub reply: String,
    pub verified: bool,
    pub report: Value,          // 产品结构化验证报告
}
```

用 `Box<dyn RestorePoint>` 而不是泛型参数，是为了保持 `Arc<dyn ToolExecutor>`
的对象安全（泛型关联类型会让 trait 失去 dyn 兼容）。

**要搬回产品的**：`EngineExecutor`（`ocad-agent/src/agent.rs:104-157`）——
它是 `ocad-engine::Engine` 的薄包装，100% 产品代码。

### 4.3 事件词汇：基词汇 + 开放 `Extension`

**现状**：`ocad-session/src/event.rs` 的 `SessionEvent` 有 25 个变体
（其 lib 文档自述「24 种」，实测 25——文档略滞后）。判断如下：

- **harness 通用**（留在框架）：`turn/start|end`、`step/start|end`、`user/message`、
  `assistant/chunk|message`、`tool/call|result`、`plan/proposed|accepted|rejected|
  contingent|clarify`、`approval/request|decision`、`check/run|result`、
  `geometry/revision`、`session/forked`、`request/header`。
- **CAD 专属**（退回产品）：`SketchProposalCreated/Decided/Committed`、
  `SketchRepairProposed`（连同 `event.rs` 里的 `*Record` 类型与
  `proposal.rs`(229 行) 的 `replay_sketch_proposals`）。

`check/*` 与 `geometry/revision` 是**通用**的：验证与修订通知是任何 design harness
都需要的（`ocad-verify` 的检查面板、引擎侧修订跨线到 UI），所以留在基词汇。

开放变体（**裁决点 B**，见 §6）：

```rust
/// 产品定义的事件。`domain` 给产品命名空间，`kind` 与 `payload` 由产品解释。
/// 日志自描述（读日志的人知道这是哪个产品的事件），代价是失去静态类型——
/// 产品侧用一个强类型 helper 把 payload 反序列化回自己的类型即可补回。
Extension {
    domain: String,
    kind: String,
    payload: serde_json::Value,
},
```

`derive_messages()` 的投影规则相应改为：基词汇按现有规则投影；`Extension` 事件
询问产品注册的 projector（**没有 projector 就不进模型历史**，只服务 UI 回放与审计）。
这条规则沿用参考实现的 "Model-visible means logged"：**模型能看到的必然已落日志**。

这样 `ocad-session` 的 4 个变体 + 4 个 `*Record` + `proposal.rs` 整体退回产品，
`nanobot-session` 彻底干净，`replay_sketch_proposals` 搬进 `ocad-sketch-host`。

### 4.4 工具契约：`ToolContract` 泛化 + 写入门提升

**现状**：`ocad-tools/src/contract.rs` 的
`ToolContract { ReadOnly, AddsOneFeature, RebuildsSolid, SetsParameter, ResetsDocument, Mutates }`
——「加一个特征 / 重建实体」是 CAD 名词。

**判断**：`check_contract`（只读工具不得改变字节形态）是**通用机制**，
`ReadOnly | Mutating` 是它需要的最小语义；「加一个特征」这类是产品的领域语义。

裁决点 C（§6）：框架只保留最小契约 + 一个开放位：

```rust
pub enum ToolContract {
    ReadOnly,
    Mutating,
    /// 产品的领域契约（如 CAD 的 `adds_one_feature` / `rebuilds_solid`）。
    /// 框架不解释它，只透传给产品的验证器。
    Custom(&'static str),
}
```

**写入门提升为框架能力（裁决点 E）**：`ocad-verify` 的
`apply_verified`（checkpoint → 写 → 快照 → 验证 → 失败回滚）与 `check_contract`
现在是 CAD 类型绑定的，但**模式是通用的**：「agent 说要做 X，验证它真做了 X，
否则回滚」对任何 design harness 都成立。框架在 `nanobot-tools` 的 post-execute
钩子提供这个机制，验证器（`WriteVerifier`）由产品给。

### 4.5 装配：`Harness` 构建器

把 `apps/ocad/src/agent_host.rs::assemble()` 上升为框架能力。形状对标
`workbench-core/src/app.rs` 的 `WorkbenchAppBuilder`：

```rust
let harness = nanobot::Harness::builder()
    .with_harness_profile(profile)                 // 来自 harness.toml
    .with_llm_runtime(llm)                         // 产品选 provider
    .with_tool_registry(registry)                 // 产品注册工具集
    .with_tool_executor(Arc::new(EngineExecutor::new(engine)))  // 产品引擎
    .with_approval_policy(Arc::new(AutoApprovalPolicy))         // 产品策略
    .with_skill_runner(skills)                     // 产品 skill 包
    .with_project_paths(paths)                     // 产品项目布局
    .with_prompt_assembler(|sp| ...)               // 桥到 cordis 瀑布
    .build()?;

harness.agent.followup("把这块板改成 3 mm 厚");
harness.agent.run().await;
```

**装配契约的关键性质**（照抄 workbench-rs 的经验）：

- 每个 `with_*` 的输入都是**产品自己的类型**，框架的类型里不出现产品名字；
- `build()` 内部用 `catch_unwind` 隔离每个插件 fiber 的失败——一个插件 panic
  不掀翻装配（workbench-rs 验收 #19）；
- 装配产物 `Harness` 暴露查询/驱动面：`agent()` / `session()` / `snapshot()` /
  `state()` / `frame_tick(dt)` / `undo()` / `redo()` / `save_container()`；
- `Harness` 的 `Drop` 保存设置并写会话结束标记（对标 workbench-rs 的
  autosave/clean-marker 语义）。

### 4.6 CLI 模式与自检

照抄 workbench-rs 的 `cli.rs` + `selfcheck.rs`，这是 ocad 现在缺的最明显的一块
（ocad 的 CLI 在 `apps/ocad/src/main.rs` 里硬编码，产品身份写死在代码里）。

```rust
pub enum CliMode { Run, Selfcheck, CrashProbe }

pub fn from_env() -> Self;                      // --selfcheck / --selfcheck-crash / --smoke
pub fn exit_with(harness: &mut Harness) -> !;   // 无头自检 + 退出码
pub fn crash_probe_exit(make: impl Fn() -> Harness) -> i32;
```

- **probe 式 SKIP 语义**：每条检查是 `PASS` / `FAIL` / `SKIP`；前置条件不满足就 SKIP，
  任何产品都能跑自检，无关项自动跳过（workbench-rs 的经验：gate 不能绑在示例
  模块的命令上，否则真实产品永远 SKIP）。
- **崩溃探针**：三阶段子进程（A 不跑 Drop 模拟崩溃 → B 恢复并逐字节校验内容 →
  C 验证干净标记抑制恢复），子进程带超时（未实现该模式的产品判 FAIL 而不是挂起）。
  这套已经在 workbench-rs 落地并验证过，直接移植。

### 4.7 项目容器：通用核 + 产品扩展

`ocad-boot` 的 `ProjectPaths`（纯路径算术）与 `Project`/`SessionIndex`/CRC32
校验是**通用**的；`ProjectManifest` 的字段有 CAD 名词：

```rust
pub documents: Vec<MemberDocument { id, path, kind: DocumentKind }>
pub enum DocumentKind { Part, Assembly, Drawing, Manufacturing }   // CAD
pub defaults: ProjectDefaults { units: String, materials: Vec<String> }  // mm
pub checks: CheckConfig                                          // CAD 检查
```

裁决点 D（§6）：框架的项目 manifest 用中性名词（`kind: String` 或产品提供的枚举），
CAD 字段由产品通过 manifest 扩展携带。`ocad-boot` 拆成
`nanobot-boot`（通用容器）+ `ocad-project`（CAD manifest 扩展）。

### 4.8 壳与插件化界面（新增）

**目标**：产品通过插件提供自定义交互界面，而不是改产品代码。

**分层**：

```
nanobot-ui（壳）
  ├─ 内置面（框架默认 bundle）：会话列表 / 会话工作区 / 对话流 / plan 审查 / 审批面
  └─ 插件面（由 nanobot-rhai 装载的脚本面板 / 视图 / 工具栏项）
```

**脚本插件契约**——照 `workbench-rhai/src/host.rs` 的 Host API v1 形状，
但注册的不是 ribbon 而是 harness 面：

```rhai
// plugin.rhai
fn register() {
    register_panel("acme.chart", "温度曲线", "chart.rhai", "right");
    register_view("acme.viewport2", "热力学视图", "thermal.rhai");
    register_command("acme.analyze", "分析", "analyze");
}
fn analyze(args) { "分析完成" }
```

框架注入的全局函数（`nanobot-rhai`）：

| 全局函数 | 作用 |
|---|---|
| `register_panel(id, title, script, area)` | 停靠面板（gpui-rhai 渲染 UiNode 树） |
| `register_view(id, title, script)` | 中央视图（同上） |
| `register_command(id, title, callback)` | 同步命令 |
| `register_background_command(id, title, callback)` | 后台任务（进度 + 协作式取消） |
| `add_toolbar_item(group, command, label)` | 工具栏项 |
| `log_info(msg)` | 日志 |

**挂载机制**（照 workbench-ui 的做法）：`nanobot-ui` 持有一个进程级
`ScriptViewHost`；面板/视图首次显示时 `FileScriptView::new(path)` +
`mount(ScriptViewConfig::new(id), host, window, cx)`，之后每次渲染取
`handle.element(id)` 得到一个 gpui `AnyElement`。

**线程模型**：Rhai 的 `Engine`/`AST` 不是 `Send`，所以命令回调不持有 Engine，
只持有脚本源码（`Arc<String>`，Send），调用时在当前线程现场构建 Engine 并编译
（脚本很小，毫秒级）——照抄 workbench-rhai 的做法。

**契约要点**：

- 插件**只贡献界面与命令**，不贡献领域能力——领域能力走编译期 Rust 模块
  （`nanobot-boot` 的模块解析器，对标 workbench-rs 的 `ModuleLoader`）；
- 插件 manifest（`plugin.toml`）带 `api_version` 与 `permissions`，
  装载失败进 `plugins_rejected` 而**不中断装配**（workbench-rs 验收 #19/#20）；
- 脚本面板/视图是**声明式 UiNode 树**，不是任意代码——信任边界与
  `design-harness.md` §5/§20 一致。

### 4.9 GPU 渲染槽（新增）

**目标**：用户/插件可以自定义 wgpu 渲染，而渲染机制归框架。

**为什么机制通用、内容不是**：`ocad-viewport` 的价值在三件事——
(1) 从 gpui 渲染器拿共享设备（`WgpuContextHandle::from_window`）；
(2) 建离屏颜色目标（`WgpuRenderTarget`）；
(3) 零拷贝合成（`window.paint_surface(SurfaceSource::Texture)`）。
这三件与渲染什么**完全无关**。而 `Mesh`/`Camera`/`SceneObjectUpload`/`WgpuRenderer`
是 CAD 的（`rcad-render`），属于产品。

**框架提供的槽**（照 `workbench-ui/src/gpu_view/mod.rs` 的 `shared_gpu`）：

```rust
/// 进程级共享 GPU 上下文。首次调用从 `window` 初始化。
/// 产品与插件的自定义 gpui Element 在 `paint` 里调它，在返回的设备上建自己的
/// 管线、纹理与渲染目标。
///
/// 共享不是可选的：Windows / Web 上 `paint_surface` 合成的是 gpui 渲染器拥有的
/// 纹理，该纹理必须活在那个渲染器的设备上。
pub fn shared_gpu(window: &Window) -> Option<Rc<Gpu>>;

/// 一帧零拷贝合成：把离屏目标画进 `bounds`。
pub fn paint_surface(window: &mut Window, target: &WgpuRenderTarget, bounds: Bounds<Pixels>);
```

`Gpu` 暴露 `device()` / `queue()` / `target_format()`，以及 `gpu_requirements()`
（告诉宿主在开窗前声明额外 limits——CAD 渲染器的 cull compute 需要 5 个
storage buffer，而 gpui 设备基线只给 4）。

**产品契约**：产品提供**渲染器**（一个在共享设备上建管线、吃自己的场景类型、
调 `paint_surface` 的 gpui `Element`）；框架提供**槽**。

```rust
// 产品侧（ocad-gpu 或 ocad-shell 的视口元素）
impl Element for CadViewportElement {
    fn paint(&mut self, .., window: &mut Window, ..) {
        let Some(gpu) = nanobot_gpu::shared_gpu(window) else { return; };  // 渲染器未就绪，下帧重试
        self.renderer.render(&gpu, self.target.view(), self.camera, ..);
        nanobot_gpu::paint_surface(window, &self.target, bounds);
    }
}
```

**插件能自定义渲染吗？** Rhai 脚本**不能**建 wgpu 管线（没有代码）。所以
「插件自定义渲染」的实际形态是：**插件声明一个视图，产品在渲染该视图时调用
插件提供的渲染钩子**（一个编译期的 `RenderPlugin` trait，或一个场景贡献
`SceneContribution`）。裁决点 G（§6）——本文建议先做「产品渲染器 + 共享槽」，
把「插件渲染贡献」留到有第二个消费者时再定形状。

---

## 五、迁移路径

四步半，每步都有独立可验证的出口。

### 第 0 步：定契约（本文）

产出：本文档。**验收**：§6 七个裁决点定案。

### 第 1 步：搬纯的，验证 submodule 接线

建 `cad-rs/nanobot`，搬 `ocad-cordis` → `nanobot-cordis`、`ocad-llm` → `nanobot-llm`；
rcad-pro 以 git submodule 消费（照 workbench-rs：workspace `exclude` + path 依赖
+ `[patch.crates-io]` 镜像 + CI 的 `submodules: recursive`）。

**验收**：rcad-pro 在引入 submodule 的情况下 `cargo check` 通过；
`nanobot-cordis`/`nanobot-llm` 的依赖树里没有任何 ocad/rcad 名字。
**这一步零风险，但把最麻烦的仓库接线问题提前暴露。**

### 第 2 步：挖出三个混合 crate 的通用核

- `ocad-session` → `nanobot-session`（去 sketch 变体 + `proposal.rs`，加 `Extension`）
- `ocad-tools` → `nanobot-tools`（去 `sketch_ai/` + `builtin.rs`，泛化 `ToolContract`）
- `ocad-skills` → `nanobot-skills`（去 CAD builtins）
- `ocad-agent` → `nanobot-agent`（去 `EngineExecutor`）

**验收**：`cargo check -p nanobot-*` 通过；内核 crate 依赖树零 CAD 零 gpui；
ocad 侧 `use nanobot_*::...` 后功能不回退；事件回放测试（记录/回放同一会话日志，
`derive_messages()` 与最终文档字节一致）。

### 第 3 步：装配 + CLI/自检 + 项目容器

- 写 `nanobot-boot`：`Harness` 构建器、`harness.toml`、CLI 模式、自检。
- `assemble()` 从 `apps/ocad` 升到框架。
- `ocad-boot` 拆成 `nanobot-boot` 的通用容器 + `ocad-project` 的 CAD 扩展。
- `ocad-boot/src/config.rs` 按 §2.2 删除。

**验收**：ocad 的 `--selfcheck` 跑 nanobot 的自检套件并全绿（probe 式 SKIP 生效）；
产品 `main` 收敛到「装配 + CLI 转发」几十行（对标 creator 的 `main.rs`）。

### 第 3.5 步：壳层与插件化界面（新增）

- `nanobot-gpu`：从 `ocad-viewport` 抽机制（共享设备 + 离屏目标 + 零拷贝），
  产品渲染器留在 ocad；`gpu_requirements()` 随槽走。
- `nanobot-rhai`：照 workbench-rhai 造宿主（manifest → 面板/视图/命令/工具栏）。
- `nanobot-ui`：从 `ocad-shell` 抽通用壳（会话列表 + 会话工作区 + 对话流 +
  plan 审查 + 审批面 + 停靠面板）；CAD 领域面板（特征树/属性/检查/材料/规则）
  留在 `ocad-shell`。
- 插件目录约定 + `plugin.toml` manifest + `api_version` 校验 + 装载隔离。

**验收**：ocad 装一个第三方插件，插件贡献的面板/视图出现在界面上并可交互；
一个插件携带的自定义渲染视图能画出来（经 `nanobot-gpu` 共享槽）；
内核层 `cargo check` 不出现 gpui/wgpu。

### 第 4 步（可选）：插件渲染贡献

只有在出现第二个真实 harness 产品、且它需要「插件提供渲染内容」时才做
（见 §4.9 与裁决点 G）。

---

## 六、裁决点

以下七项在第 0 步需要拍板。本文给出建议，标注为**待确认**。

| # | 问题 | 建议 | 理由 |
|---|---|---|---|
| A | `ToolExecutor` 的回滚点用 `Box<dyn RestorePoint>` 还是泛型关联类型？ | **type-erased** | 泛型关联类型会让 trait 失去 dyn 兼容，而 `Agent` 内部就是 `Arc<dyn ToolExecutor>`；产品侧 downcast 一次即可恢复静态类型 |
| B | 事件词汇用开放 `Extension` 变体，还是把 `Session` 做成泛型？ | **`Extension`** | 泛型会让每个产品重定义 turn/step/user/assistant/tool 基事件，重复且易漂移；`Extension` 保持 `Session` 非泛型、日志自描述 |
| C | `ToolContract` 的领域语义（`adds_one_feature` 等）进不进框架？ | **不进**，只留 `ReadOnly`/`Mutating` + `Custom(&'static str)` | 「加一个特征」是 CAD 名词；框架只保留写入门真正需要的最小语义 |
| D | `ocad-boot` 的项目层归谁？`config.rs` 怎么办？ | 通用容器进 nanobot，CAD 字段由产品扩展；**`config.rs` 删掉** | 容器是通用机制；`config.rs` 无生产调用者，抽它等于抽空壳 |
| E | 写入门（checkpoint/验证/回滚）要不要提升为框架能力？ | **要** | 「agent 说要做 X，验证它真做了 X，否则回滚」是任何 design harness 的通用性质；现在只是被 CAD 类型绑住了 |
| F | vendor：nanobot 自持还是依赖 workbench-rs？ | **自持 vendor，钉同一提交；rcad-pro patch 表保持单一源** | 单一 gpui-ce 才能保证零拷贝合成与类型互通；漂移时 escape hatch 是抽 `cad-rs/gpui-kit` |
| G | 「插件自定义渲染」的形状？ | **第 3.5 步先做产品渲染器 + 共享槽**；插件渲染贡献留到第 4 步 | Rhia 脚本不能建 wgpu 管线；先给共享槽（Rust 插件/产品可用），脚本插件的渲染形状待第二个消费者出现再定 |

A/B/C/D 改变第 2、3 步的形状；E 影响第 3 步范围；F 影响第 1、3.5 步；G 只影响第 4 步。

---

## 七、风险与开放问题

| # | 风险 | 等级 | 缓解 |
|---|---|---|---|
| NR1 | **过度抽取**：只有 ocad 一个消费者时，配置组合、通用壳这类「看起来通用」的东西会退化成空壳 | 中 | 每一步以「是否有第二个真实消费者」为门槛；`config.rs` 直接删 |
| NR2 | **仓库接线**：`[patch.crates-io]` 镜像、workspace `exclude`、CI submodule 递归 | 中 | 第 1 步就验证；workbench-rs 已踩过一遍，照抄 |
| NR3 | **`ToolExecutor` 的泛化破坏对象安全** | 中 | 裁决点 A 已给出 type-erased 方案；第 2 步 `cargo check` 兜底 |
| NR4 | **wasm 双形态纪律只写在文档里** | 中 | 第 3.3 节的 CI 检查；`nanobot-llm` 的 wasm 构建纳入 gate 脚本 |
| NR5 | **事件 `Extension` 丢失静态类型** | 低 | 产品侧强类型 helper；日志自描述，回放仍可校验 |
| NR6 | **ocad 回归**：拆分后 ocad 的测试基线漂移 | 中 | 第 2 步的验收含事件回放测试；ocad 的 e2e（`apps/ocad/tests/e2e/`）必须全绿 |
| NR7 | **两份 vendor 的 gpui-ce 漂移**（新增） | **高** | 单一 patch 源（裁决点 F）；把 wgpu 版本与 vendor 提交纳入 CI 检查；逃逸口是 `cad-rs/gpui-kit` |
| NR8 | **壳层重复造轮子**（新增）：nanobot-ui 与 workbench-ui 都实现 dock/面板 | 中 | 只抽 harness 特有的部分（会话工作区）；停靠/面板直接用 `gpui-component`（两边同源），不自己实现 dock |
| NR9 | **Rhai 脚本能力边界的误判**（新增） | 中 | 脚本只做声明式界面 + 薄命令；领域能力一律编译期模块。若脚本需求超出 UiNode 表达力，先扩 UiNode 而不是放代码进去 |

**开放问题**：

- **NQ-1**：`nanobot` 是否需要自己的 `docs/design.md`（对标 workbench-rs 的设计文档）？
  建议是——本文档在框架仓库落地时同步迁移过去，rcad-pro 侧留一个指针。
- **NQ-2**：`rcad-agent-server` 是否改为消费 `nanobot-*`？建议是——它是内核的
  第二个真实消费者，改过去能反过来验证「产品契约」是否够用。
- **NQ-3**：`nanobot-shell`（通用 gpui 壳）触发条件怎么量化？建议：出现第二个
  harness 产品、且其壳有 ≥3 个产品都需要的部件时。
- **NQ-4**（新增）：插件 manifest 的 `permissions` 语义——workbench-rs 的
  `Permissions` 是**审计声明而非沙箱**（`design-harness.md` §5 的信任边界）。
  nanobot 沿用同一语义，还是引入更细的 UI 能力位（如「允许注册视图」）？建议沿用，
  把更细的能力位留作后续扩展。

---

## 附：关键源文件索引

| 内容 | 位置 |
|---|---|
| 插件内核 | `libs/ocad-cordis/src/{context,events,fiber,disposable}.rs` |
| LLM 缝 | `libs/ocad-llm/src/{adapter,assembler,chunk,runtime,openai,scripted}.rs` |
| 事件日志 | `libs/ocad-session/src/{event,session,persistence,projection,proposal}.rs` |
| 工具瀑布 | `libs/ocad-tools/src/{registry,waterfall,approval,contract,error,builtin}.rs` |
| 工具 CAD 核 | `libs/ocad-tools/src/sketch_ai/`（22 文件） |
| 循环与 plan | `libs/ocad-agent/src/{agent,inbox,plan,cancellation}.rs` |
| 装配点（现居产品） | `apps/ocad/src/agent_host.rs::assemble()` |
| 产品壳 | `libs/ocad-shell/src/{shell,session,conversation,workspace}.rs` |
| 产品视口 | `libs/ocad-viewport/src/lib.rs`（552 行；CAD 场景类型来自 `rcad-render`） |
| 项目层 | `libs/ocad-boot/src/{project,manifest,canonical,config}.rs` |
| 写入门 | `libs/ocad-verify/src/{check,enforce,contract,registry}.rs` |
| **参照：GPU 槽** | `workbench-rs/crates/workbench-ui/src/gpu_view/{mod,gpu,scene,mesh,camera}.rs`（`shared_gpu`） |
| **参照：脚本插件宿主** | `workbench-rs/crates/workbench-rhai/src/host.rs` + `.vendor/gpui-rhai/` |
| **参照：脚本界面挂载** | `workbench-rs/crates/workbench-ui/src/dock_panel.rs`（`SharedScripts::mount`） |
| 框架/产品契约参照 | `workbench-rs/README.md`「Integration guide」+ `crates/workbench-core/src/{cli,app,product}.rs` |
| 参考架构 | `docs/design-harness-architecture.md` |

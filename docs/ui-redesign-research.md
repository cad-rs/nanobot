# 十余款 coding harness 的界面收敛出 nanobot 重构蓝图

2026 年的开源 coding harness 在界面上已经收敛到同一副骨架：**左侧会话列表 | 单列对话流 | 贴着输入框的"待决动作区" | 输入框下方一行环境状态 | 右侧可开合的工作区窗格**，而 nanobot 现有的"会话列表 | 会话工作区 | 产品视口 | 面板 dock"恰好就是这副骨架把"右侧工作区窗格"换成 3D 视口的版本，因此重构的重点不是换布局范式，而是把四件事做对：工具调用的两级密度（只读操作压成一行、有副作用的才成卡片）、离输入光标最近的审批与排队区、显式的回合状态机与会话状态徽标、以及随宽度折叠的断点规则。Zed 的 agent panel 是最接近 nanobot 技术栈的参照，它在 2026-04 把线程历史与导航整体移出面板、放进 Threads Sidebar，面板只剩"工具栏 → 虚拟列表 → 活动栏 → 输入框"四层（[parallel-agents.md](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/docs/src/ai/parallel-agents.md)）；但 Zed 的 `agent_ui`、`ui`、`markdown`、`editor`、`terminal_view` 全部是 GPL-3.0-or-later，只有 `gpui` 是 Apache-2.0（[agent_ui/Cargo.toml](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/crates/agent_ui/Cargo.toml)、[gpui/Cargo.toml](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/crates/gpui/Cargo.toml)），MIT 许可的 nanobot 只能照搬其规格与交互，不能拷代码。真正的分歧集中在三处：忙时发消息默认是排队还是插话（steer）、审批是逐条确认还是"模式拨盘 + 风险分级"乃至完全不问、回退是"编辑旧消息"还是"代码与对话可分别恢复的检查点"。本报告对每个分歧给出 nanobot 的取舍建议，并把所有建议明确标为本报告的推断；调研没查到的内容单列在"未覆盖 / 待确认"一节，没有补写。需要提前说明的一点是：所有被调研对象都以代码仓库和 git 为语境，**没有任何一款面向 CAD 这类"设计类"产品**，因此视口并排、原生 surface 遮挡、非 git 的文档级检查点这三块没有现成先例，只能由相邻模式推导。

## 版本基线：三个被点名的项目在 2026 年已经停更或换了形态

本次调研日期为 2026-11-10。开源项目均固定到具体 commit 读源码，闭源项目只依据官方文档与 changelog；笔记作者未能查看任何截图，所有外观描述来自源码中的字符串、样式类与快照文本。下表是各对象的依据版本与 2026 年已发生的变动，后文所有结论都以此为准。

| 对象 | 依据的版本 / 来源 | 2026 年已发生的变动 |
|---|---|---|
| Zed agent panel | `zed-industries/zed` main `f0d896f`（2026-11-09），源码 + `docs/src/ai/*` | 2026-04-22 发布 parallel agents：线程历史与导航移到 Threads Sidebar，新增 Terminal Threads，默认布局改为"Agentic"；text threads 已移除；旧代 `AcpThreadView`、`ActiveThread`、面板内历史/配置视图已不存在（[blog](https://zed.dev/blog/parallel-agents)、[text-threads.md](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/docs/src/ai/text-threads.md)） |
| OpenAI Codex CLI | `openai/codex` main `c875667`（2026-11-10），`codex-rs/tui` 源码与 insta 快照 | 官方文档站 `developers.openai.com/codex` 已重定向到 `learn.chatgpt.com`，TUI 布局无官方文档，结论全部来自源码 |
| Claude Code | `code.claude.com/docs` + `CHANGELOG.md`（最新条目 2.1.322）；闭源 | fullscreen 渲染器成为新用户默认（文档称 2026-05-06 之后首次使用者默认全屏，此为笔记推断段落中的转述）；`/vim` 在 v2.1.92 移除（[commands](https://code.claude.com/docs/en/commands)） |
| pi | `badlogic/pi-mono` 现显示为 `earendil-works/pi`，main 分支文档与源码 | 仓库归属迁移；作者 2026-04 加入 Earendil（仅来自搜索摘要，未读原文，[链接](https://mariozechner.at/posts/2026-04-08-ive-sold-out/)）；fullscreen 成为默认模式，已有自动压缩与 `/mcp`，与 2025-11 博文不一致 |
| DeepSeek 官方 harness | `deepseek-ai/deepseek-harness` README | developer preview；发布日期 2026-08-13 仅见二手报道（[The New Stack](https://thenewstack.io/deepseek-harness-open-source-plugins/)），未从仓库页确认 |
| Codewhale | `codewhale-hq/Codewhale`（原 `Hmbown/DeepSeek-TUI`），README 对应 0.10.2 开发版 | 改名并从"DeepSeek 专用"转为"多 provider、默认 DeepSeek" |
| opencode | `sst/opencode` dev `a010a427`（2026-11-10） | 仓库重组为 V2，TUI 移到 `packages/tui`；文档站 V1/V2 混排（`/docs/tui`、`/docs/keybinds` 是 V1，`/docs/permissions`、`/docs/agents`、`/docs/themes` 是 V2）；会话分享在源码中标注已移除 |
| Crush | `charmbracelet/crush` main `1b37350c`（2026-11-09） | 无归档或改版记录；UI 注释显示已有 client/server 模式 |
| Gemini CLI | `google-gemini/gemini-cli` main `1930f253`（2026-11-09） | 渲染架构处于可切换实验期（alternate buffer、terminal buffer 等） |
| Aider | `Aider-AI/aider` main 源码与 aider.chat 文档 | 最新版本号与发布日期未能确认 |
| Goose | `aaif-goose/goose` main + goose-docs.ai | 仓库迁到 aaif-goose，文档迁到 goose-docs.ai |
| Cline | `cline/cline` `e1f499a`（2026-11-09） | 重构为 monorepo；移除任务时间线、Focus Chain、逐请求成本行、检查点行（[deprecations.mdx](https://github.com/cline/cline/blob/e1f499a/docs/resources/deprecations.mdx)） |
| Roo Code | `RooCodeInc/Roo-Code` `b867ec9`（2026-05-15，最后一次提交） | **仓库已归档**（[GitHub API](https://api.github.com/repos/RooCodeInc/Roo-Code)）；最后一版公告标题为 "The last Roo Code release"；二手报道称团队 2026-04-21 前后宣布转向 Roomote（[The New Stack](https://thenewstack.io/roo-code-cloud-ides-ai-coding/)） |
| Kilo Code | `Kilo-Org/kilocode` `b6a49d0`（2026-11-07） | VS Code 扩展整体重写、跑在 OpenCode server 上，2026-04-02 GA（[Kilo 博客](https://blog.kilo.ai/p/new-kilo-for-vs-code-is-live)）；旧 Roo 系界面不再是当前形态 |
| Continue | `continuedev/continue` `5522c6f`（2026-07-20） | README 自述 "no longer actively maintained and is read-only"，有 "Final 2.0.0 Release"（[README](https://github.com/continuedev/continue/blob/5522c6f/README.md)）；但 GitHub API 显示 `archived: false`、最后 push 为 2026-11-05，两者如何并存未核实；"Cursor 2026-06 收购 Continue"仅见二手报道，未找到官方公告（[The New Stack](https://thenewstack.io/cursor-acquires-continue-coding/)） |
| Amp | ampcode.com 文档与 news；闭源 | 2026-01 删除 TODO 列表；编辑器扩展 2026-03-05 下线；重写后默认不审批；2026-07 用 Dial 取代 smart/rush/deep；2026-08-27 移除 TUI 侧栏；默认发送语义改为 steer；2026-06-02 取消公开 thread |
| OpenHands | docs.openhands.dev + 产品页 | 主 GUI 改名 Agent Canvas；旧 CLI 标注不再积极维护；架构从"每会话一个容器"变为单一 agent-server |
| 多会话桌面应用 | Codex app、Claude Code desktop、Conductor（闭源，仅文档）；Vibe Kanban、opcode、opencode desktop（开源）；Nimbalyst（开源状态未核实） | Crystal 自 2026-02 弃用，README 指向 Nimbalyst（[stravu/crystal](https://github.com/stravu/crystal)） |

## 逐工具调研记录：从四层面板到五档拨盘

### Zed 把面板削成四层，导航整体外移到 Threads Sidebar

**布局。** 当前一代 Agent Panel 默认停靠在左侧，`flexible: true` 按比例定宽，`default_width` 640，内容 `max_content_width` 850 超宽后居中；Threads Sidebar 默认在左、宽 300、`auto_open`（[default.json](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/assets/settings/default.json)）。代码层面只允许左右停靠，`position_is_valid` 拒绝 Bottom，而设置注释仍写可以是 bottom，两者冲突；最小宽度 `MIN_PANEL_WIDTH` 为 300px（[agent_panel.rs](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/crates/agent_ui/src/agent_panel.rs#L5053)）。面板自上而下是工具栏、虚拟化消息列表、活动栏、消息编辑器，整体包在 `WithRemSize` 里按 `agent_ui_font_size` 独立缩放（[agent_panel.rs](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/crates/agent_ui/src/agent_panel.rs#L6617)）。活动栏把 plan 摘要、已编辑文件、消息队列、待授权四块用分隔线堆在编辑器上方（[thread_view.rs](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/crates/agent_ui/src/conversation_view/thread_view.rs#L4000)）；等待授权的工具调用还会以 `ToolCallLayout::Floating` 浮在编辑器上方并限高滚动，源码注释的理由是"so the row can never grow to consume the entire panel and squeeze the conversation list out of view"（[thread_view.rs](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/crates/agent_ui/src/conversation_view/thread_view.rs#L864)）。空线程没有欢迎页，编辑器直接撑满面板；展开态编辑器高度为视口的 80%（[thread_view.rs](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/crates/agent_ui/src/conversation_view/thread_view.rs#L5550)）。Threads Sidebar 按项目分组列出线程与 Terminal Threads，worktree 隔离在标题栏的 worktree picker 里操作，历史视图按 Today / Yesterday / This Week / Past Week / Older 分桶（[parallel-agents.md](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/docs/src/ai/parallel-agents.md)、[threads_archive_view.rs](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/crates/agent_ui/src/threads_archive_view.rs#L141)）。配置界面已并入全局 Settings 的 AI 页（[agent_panel.rs](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/crates/agent_ui/src/agent_panel.rs#L3748)）。

**元素外观。** 用户消息是带 1px 边框、`rounded_md`、`editor_background` 底、不透明窗口下带 `shadow_md` 的小卡片，字号 `text_xs`，用 buffer 字体（`agent_buffer_font_size` 12）；助手消息是无框 markdown，用 UI 字体（[thread_view.rs](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/crates/agent_ui/src/conversation_view/thread_view.rs#L7440)、[default.json](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/assets/settings/default.json)）。**决定视觉密度的规则只有一行**：`use_card_layout = needs_confirmation || is_edit || is_terminal_tool`，其余读、搜、抓取都是"图标 + 标签"的一行（[thread_view.rs](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/crates/agent_ui/src/conversation_view/thread_view.rs#L9729)）。卡片失败、取消、被拒时边框变虚线；取消且无可见输出的调用整条不渲染。Thinking 块是"图标 + Thinking"一行，chevron 仅 hover 可见，展开内容左竖线缩进、限高 `max_h_64` 并叠渐隐遮罩（[thread_view.rs](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/crates/agent_ui/src/conversation_view/thread_view.rs#L8815)）。卡内 diff 是关掉 gutter 与滚动条的只读 `Editor`；独立的 Review tab（`AgentDiffPane`）提供逐 hunk 的 Keep / Reject（[agent_diff.rs](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/crates/agent_ui/src/agent_diff.rs#L41)）。生成中用点状 spinner，等待确认时换成沙漏 spinner 加 "Awaiting Confirmation"（[thread_view.rs](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/crates/agent_ui/src/conversation_view/thread_view.rs#L8661)）。窗口非激活时的通知是一个独立的 450×72px `WindowKind::PopUp` 小窗，位于屏幕右上角、不抢焦点（[agent_notification.rs](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/crates/agent_ui/src/ui/agent_notification.rs#L37)）。流式文本经 `StreamingTextBuffer` 以 16ms 一个 tick 匀速揭示（[acp_thread.rs](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/crates/acp_thread/src/acp_thread.rs#L3849)）。

**人机交互。** 输入框是完整的 `Editor`，`@` 可提及 File、Symbol、Fetch、Thread、Skill、Diagnostics、BranchDiff，以 crease 折叠块内嵌（[completion_provider.rs](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/crates/agent_ui/src/completion_provider.rs#L158)）。生成中发送默认排队；对排队项开 Steer 则在 agent 下一步时送达（仅 Zed Agent 支持），Send Now 立即发送（[agent-panel 文档](https://zed.dev/docs/ai/agent-panel)）。发送按钮随状态三变：空闲为 Send，生成中有输入为 QueueMessage（tooltip 两行 "Queue and Send" / "Send Immediately"），生成中无输入为红色 Stop（[thread_view.rs](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/crates/agent_ui/src/conversation_view/thread_view.rs#L6709)）。授权按钮在工具卡底部：原生 agent 是 Allow / Deny 加一个粒度下拉（"Only this time"、"Always for {tool}"、"Always for selected commands"），外部 agent 按 ACP 的 `allow_once / allow_always / reject_once / reject_always` 逐个画按钮（[thread_view.rs](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/crates/agent_ui/src/conversation_view/thread_view.rs#L11025)、[ACP tool-calls](https://agentclientprotocol.com/protocol/tool-calls)）。回退是"点击已发送的用户消息卡片 → 编辑 → 从该点重启线程"，编辑期间其后条目被 0.8 透明度遮罩盖住。模型、profile、模式切换在线程非 Idle 时直接忽略。几乎每个按钮都通过 `KeyBinding::for_action_in` 把快捷键画在按钮或 tooltip 上，这是系统性做法（笔记推断）。Windows 键位表全部来自 [default-windows.json](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/assets/keymaps/default-windows.json)；macOS 键位未读取。

**实现要点（gpui）。** 消息列表用 gpui 的变高虚拟列表 `list` + `ListState::new(0, ListAlignment::Top, px(2048.0))`，并 `set_follow_mode(FollowMode::Tail)`（[conversation_view.rs](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/crates/agent_ui/src/conversation_view.rs#L1595)）。数据模型 `AcpThread` 发索引级增量事件，视图只做 `splice` 与 `remeasure_items`：`NewEntry` 对应 `splice_focusable`，`EntryUpdated(i)` 对应 `remeasure_items(i..i+1)`，`EntriesRemoved(range)` 对应 `splice(range, 0)`（[conversation_view.rs](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/crates/agent_ui/src/conversation_view.rs#L1975)）。"生成中"指示是 splice 进列表的虚拟尾项。工具内容视图惰性物化，只有可见或展开的才创建 `Editor` / `TerminalView`（[entry_view_state.rs](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/crates/agent_ui/src/entry_view_state.rs#L46)）。滚动位置随线程保存。`FollowMode`、`pause_following_tail`、`splice_focusable` 看起来是为 agent 面板新增到 gpui 的能力（笔记推断，未查提交历史），**nanobot 所用的 gpui-fast 后端是否包含这些 API 未验证**。

**作者公开的理由。** parallel agents 博文称新布局 "works better for agentic work, keeping agent threads front and center"，并说 "As the Threads Sidebar became our primary way of navigating a project, we reconsidered which panels should sit where"；老用户不会被自动切换（[blog](https://zed.dev/blog/parallel-agents)）。ACP 的定位是 "switch between multiple agents without switching your editor"，类比 LSP（[blog](https://zed.dev/blog/bring-your-own-agent-to-zed)）。"Zed 1.0 于 2026-04-29 发布"仅见于二手来源，未在 zed.dev 核实。

### Codex CLI 用动词时态和一个圆点表达全部状态

**布局。** 单列，无常驻侧栏：上方滚动转录，底部自上而下严格堆叠"活动行 → 排队消息预览 → 输入框 → 一行页脚"，所有次级界面（审批、选择器、斜杠菜单、帮助）在底部区域就地展开或替换输入框（[bottom_pane/mod.rs](https://github.com/openai/codex/blob/main/codex-rs/tui/src/bottom_pane/mod.rs)）。页脚在"指令态"（下一步怎么做）与"环境态"（状态行）之间切换，左放提示、右放上下文指标，并有一套写在注释里的宽度折叠优先级，注释自述这些规则 "were built out of trial and error"（[footer.rs](https://github.com/openai/codex/blob/main/codex-rs/tui/src/bottom_pane/footer.rs)）。状态行条目可勾选，枚举 `StatusLineItem` 含模型、推理档、目录、git 分支、PR 号、权限、上下文剩余、5 小时与周限额、任务进度等三十余项（[status_line_setup.rs](https://github.com/openai/codex/blob/main/codex-rs/tui/src/bottom_pane/status_line_setup.rs)）。`/agents` 打开全屏的 Agent command center，列表-详情布局，过滤标签为 "All / Needs you / Working / Ready / Inactive"（[快照](https://github.com/openai/codex/blob/main/codex-rs/tui/src/app/snapshots/codex_tui__app__agents_overview__tests__command_center__live_center_custom_footer.snap)）。

**元素外观。** 几乎所有条目都是"`•` 圆点 + 粗体动词标题 + `└` 缩进细节"。状态靠时态表达：`Running` → `Ran`、`Calling` → `Called`、`Exploring` → `Explored`，圆点成功为绿、失败为红（[exec_cell/render.rs](https://github.com/openai/codex/blob/main/codex-rs/tui/src/exec_cell/render.rs)）。模型发起的工具输出最多显示 5 行，超出显示 `+N lines`；只读探索类调用跨多次合并为一个 `• Explored` 单元并对文件名去重（[快照](https://github.com/openai/codex/blob/main/codex-rs/tui/src/history_cell/snapshots/codex_tui__history_cell__tests__coalesces_reads_across_multiple_calls.snap)）。用户消息以 `›` 为前缀并带一层由终端底色混合出的背景。Diff 是 unified 格式，行号槽加 `+`/`-`，按 hunk 整块做语法高亮，单元标题形如 `• Edited 2 files (+2 -1)`（[diff_render.rs](https://github.com/openai/codex/blob/main/codex-rs/tui/src/diff_render.rs)）。推理正文默认只进转录视图，其标题被提到活动行当动词。计划用 `✔` / `□`，紧凑形态标题带 `1/4 complete`。颜色纪律写在仓库内风格指南：只用默认前景加 dim/bold，成功绿、失败红、品牌品红，避免蓝黄与黑白前景，强调色为 `#63A8F8`（[styles.md](https://github.com/openai/codex/blob/main/codex-rs/tui/styles.md)、[style.rs](https://github.com/openai/codex/blob/main/codex-rs/tui/src/style.rs)）。动效只有一条每 2 秒扫过文字的 shimmer 高光带（[shimmer.rs](https://github.com/openai/codex/blob/main/codex-rs/tui/src/shimmer.rs)）。

**人机交互。** `/` 命令、`@` 文件、`!` shell、`Shift+Tab` 切模式、`Esc` 打断、`Ctrl+G` 外部编辑器、`Ctrl+R` 历史搜索；`?` 打开三栏快捷键速查面板（[快照](https://github.com/openai/codex/blob/main/codex-rs/tui/src/bottom_pane/snapshots/codex_tui__bottom_pane__chat_composer__tests__footer_mode_shortcut_overlay.snap)）。斜杠命令按使用频率而非字母排序，枚举注释写明 "DO NOT ALPHA-SORT"（[slash_command.rs](https://github.com/openai/codex/blob/main/codex-rs/tui/src/slash_command.rs)）。命令审批取代输入框，是编号列表加字母快捷键：`Yes, proceed (y)`、``Yes, and don't ask again for commands that start with `…` (p)``、`No, and tell Codex what to do differently (esc)`（[快照](https://github.com/openai/codex/blob/main/codex-rs/tui/src/chatwidget/snapshots/codex_tui__chatwidget__tests__approval_modal_exec.snap)）；网络审批分"仅此一次 / 本会话 / 以后都允许 / 拒绝"四档。运行中的输入分三级队列显示在输入框上方："Messages to be submitted after next tool call"、"Messages to be submitted at end of turn"、"Queued follow-up inputs"（[快照](https://github.com/openai/codex/blob/main/codex-rs/tui/src/bottom_pane/snapshots/codex_tui__bottom_pane__pending_input_preview__tests__render_pending_steers_above_queued_messages.snap)）。"`Tab` 排队、`Enter` 插话"是笔记由快捷键面板推断的，Enter 在运行中具体进入哪一级队列未逐行核实。回溯是状态机：`Esc` 预备、再 `Esc` 进入转录浏览并高亮最近一条用户 prompt、`Enter` 回退到该 prompt 之前并把它放回输入框（[app_backtrack.rs](https://github.com/openai/codex/blob/main/codex-rs/tui/src/app_backtrack.rs)）。Plan 完成后弹出 `Implement this plan?` 三选项（实现 / 清空上下文后实现 / 留在 Plan 模式）（[快照](https://github.com/openai/codex/blob/main/codex-rs/tui/src/chatwidget/snapshots/codex_tui__chatwidget__tests__plan_implementation_popup.snap)）。

**作者公开的理由。** 活动行把时钟、打断提示和内联上下文放在一行，是为了 "avoids vertical layout churn in the bottom pane"（[status_indicator_widget.rs](https://github.com/openai/codex/blob/main/codex-rs/tui/src/status_indicator_widget.rs)）；黄色只在已知深色背景使用，因为 "Yellow can disappear on light themes"；输入路由分层是因为 "This split matters for Ctrl+C/Ctrl+D"。未找到 OpenAI 专门解释 TUI 设计的文章，理由全部来自源码注释。

### Claude Code 把状态行交给用户脚本，把检查点拆成三个动作

**布局。** 同样是单列转录加底部交互区。状态行是用户配置的 shell 脚本输出，"渲染在内建页脚徽标之上的独立一行"，事件驱动、300ms 去抖；配置后多数页脚按键提示不再显示（[statusline](https://code.claude.com/docs/en/statusline)）。权限模式以固定字符串显示在状态栏，如 `⏸ plan mode on`、`⏵⏵ accept edits on`（[permission-modes](https://code.claude.com/docs/en/permission-modes)）。fullscreen 渲染器下输入框固定在屏幕底部，向上滚动时顶部有一行暗色表头显示已滚出的最近一条 prompt，底边浮动 `Jump to bottom` 并带新消息计数；`/diff` 在终端 ≥110 列时作为侧边面板打开，≥144 列时 Claude 开始改文件会自动打开（[fullscreen](https://code.claude.com/docs/en/fullscreen)、[interactive-mode](https://code.claude.com/docs/en/interactive-mode)）。任务清单最多显示 5 条，子代理面板在 prompt 下方最多 5 行。

**元素外观。** 用户历史消息前有 `❯`，工具结果行用 `⎿` 前缀；工具输出与思考默认折叠（`Thought for 4s`、`Called slack 3 times`），`Ctrl+O` 的转录视图展开全部；spinner 带随机动词，10 秒后变琥珀色，权限检查卡住时变红；回合结束行形如 `✻ Sautéed for 23s`（[CHANGELOG](https://github.com/anthropics/claude-code/blob/main/CHANGELOG.md)）。主题有 auto、light/dark、色盲友好、ANSI 与自定义 JSON。选中行加 `❯` 指针的理由是 "so you can see it without color"。助手消息的前缀字形在文档中没有文字描述，笔记未将其列为事实。

**人机交互。** Bash 权限提示四个选项：`Yes`、`Yes, and don't ask again for: npm test *`、`Yes, and switch to auto mode`、`No`，页脚 `Esc to cancel` 与 `Tab to amend`（[permissions](https://code.claude.com/docs/en/permissions)）。在 Yes 或 No 上按 `Tab` 可附评语：带评语的 No 作为拒绝理由且 Claude 继续工作，不带评语的 No 停止本回合。"不再询问"的持久范围随工具类型不同（Bash 按仓库加命令永久、文件修改到会话结束）。`Shift+Tab` 在 default → acceptEdits → plan 间循环。运行中按 `Enter` 排队，消息在当前工具调用结束后、同一回合内交给模型；`Ctrl+Enter` 立即发送；在输入框首行按 `Up` 取回排队内容（[interactive-mode](https://code.claude.com/docs/en/interactive-mode)）。Rewind 菜单（`/rewind` 或空输入时 `Esc Esc`）提供 **Restore code and conversation / Restore conversation / Restore code / Summarize from here / Summarize up to here**，保留最近 100 个检查点；仅由 Bash 改动的文件不恢复（[checkpointing](https://code.claude.com/docs/en/checkpointing)）。

**作者公开的理由。** fullscreen 渲染器的动机是消除闪烁、让长对话内存恒定、获得鼠标支持，代价是放弃原生 scrollback，因此补了转录内搜索、写回 scrollback、外部编辑器三条逃生通道（[fullscreen](https://code.claude.com/docs/en/fullscreen)）。权限粒度的理由是 "so a rule you save from a prompt covers only what its option named"。检查点定位为 "quick, session-level recovery"，不替代版本控制。

### pi 默认不问权限，把 footer 做成信息密度最高的三行

**布局。** 四段：启动 header、transcript、editor（边框颜色表示 thinking level）、footer；无侧栏（[usage.md](https://raw.githubusercontent.com/badlogic/pi-mono/main/packages/coding-agent/docs/usage.md)）。Footer 第一行是 cwd、分支、会话名；第二行左侧依次为 `↑input ↓output R缓存读 W缓存写 CH命中率 成本 上下文%/窗口`，右对齐模型与 thinking 档；上下文百分比超过 70% 用 warning 色、超过 90% 用 error 色；第三行只在扩展设置了状态时出现（[footer.ts](https://raw.githubusercontent.com/badlogic/pi-mono/main/packages/coding-agent/src/modes/interactive/components/footer.ts)）。

**元素外观。** 工具调用用三种底色表达状态：pending、success、error（[tool-execution.ts](https://raw.githubusercontent.com/badlogic/pi-mono/main/packages/coding-agent/src/modes/interactive/components/tool-execution.ts)）。bash 折叠时保留末尾 5 行，完整输出落盘到文件（[renderers/bash.ts](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/src/core/tools/renderers/bash.ts)）。edit 在参数收齐后、执行前就渲染 diff 预览，结果与预览相同时不重复显示（[renderers/edit.ts](https://raw.githubusercontent.com/earendil-works/pi/main/packages/coding-agent/src/core/tools/renderers/edit.ts)）。主题是严格 schema 的 JSON，token 分通用、消息、工具、markdown、diff、语法几组，支持 hex、OKLCH、OKHSL（[themes.md](https://raw.githubusercontent.com/badlogic/pi-mono/main/packages/coding-agent/docs/themes.md)）。内置无 plan 与 todo 显示。

**人机交互。** 运行中 `Enter` 是 steering（当前回复与工具调用结束后引导下一次回复），`Alt+Enter` 是 follow-up（等任务全部结束），`Alt+Up` 取回排队消息，`Escape` 停止且排队消息回到 editor（[usage.md](https://raw.githubusercontent.com/badlogic/pi-mono/main/packages/coding-agent/docs/usage.md)）。会话以树结构存储，`/tree` 选中用户消息会把原文放回 editor、编辑后提交即产生新分支（[sessions.md](https://raw.githubusercontent.com/badlogic/pi-mono/main/packages/coding-agent/docs/sessions.md)）。扩展面 `ctx.ui` 提供 `select / confirm / input / notify / setStatus / setWidget / setFooter / setHeader / setEditorComponent` 以及工具渲染器注册，等于一份 harness UI 插槽清单（[extensions.md](https://raw.githubusercontent.com/badlogic/pi-mono/main/packages/coding-agent/docs/extensions.md)）。

**作者公开的理由。** Mario Zechner 的总原则是 "if I don't need it, it won't be built"；权限弹窗是 "security theater"；内置 todo "generally confuse models more than they help"；子代理是 "a black box within a black box"（[博客 2025-11-30](https://mariozechner.at/posts/2025-11-30-pi-coding-agent/)）。需要注意 2026 年的 pi 已与该博文有出入：现在有自动压缩、fullscreen 为默认、内置 `/mcp`；未找到作者解释这些转向的文章。

### DeepSeek 系：官方 harness 是 Web UI，Codewhale 才是可参照的终端界面

`deepseek-ai/deepseek-harness`（命令 `dsh`）真实存在，主张 "Everything is a Plugin"，入口 `npx @deepseek-ai/dsh web` 在 `http://127.0.0.1:3080` 启动本地 Web UI，README 未描述任何界面布局、审批模型或快捷键（[GitHub](https://github.com/deepseek-ai/deepseek-harness)）。**官方 harness 的界面在本次调研中没有任何一手描述**，其文档站只抓到标题。

Codewhale（原 DeepSeek-TUI，Rust + ratatui）是这一系里界面最重的。**布局**上除 transcript、composer、footer 外还有 workbar（Tasks / Agents / Context / Files 四个面板，`Alt-!`/`@`/`#`/`$` 切换）、八槽位 hotbar、命令面板 `Ctrl-K`、各类 sheet 与 toast（[KEYBINDINGS.md](https://raw.githubusercontent.com/Hmbown/DeepSeek-TUI/main/docs/KEYBINDINGS.md)）。**交互**的核心是 `Shift+Tab` 五档权限拨盘：Ask → Accept Edits（新安装默认）→ Plan → Auto-Review → Full Access；footer 常显当前档位，档位变更在引擎下一步才生效、期间 status line 显示 pending，transcript 同时留一行 "Permissions changed to …"（[MODES.md](https://raw.githubusercontent.com/Hmbown/DeepSeek-TUI/main/docs/MODES.md)）。审批卡按键为 `y` 批准一次、`a` 全部批准、`n`/`Esc` 拒绝、`e` 先编辑输入再执行。Composer 的 `Enter` 空闲时发送、忙时排队，`Ctrl-Enter` steer 进当前轮；`Esc Esc` 只回退对话不回退文件，另有 `/undo` 与 `/restore`。文档明文规定"可打印字符永远进入 composer，transcript 没有裸字母快捷键"，以及 "`Tab` 只做补全、从不切模式"。Codewhale 的卡片颜色、状态字形、diff 画法与主题文件格式，已读文档未说明；未找到作者撰写的界面设计理由文章。

### opencode V2 是多面板工作台，Crush 用 120×30 双断点折叠侧栏

**opencode（V2 源码）布局。** 全屏应用加右侧 42 列 sidebar（最大 72），会话内容最小宽 44、首选 64；总宽不足 108 列时"面板接管全屏"（[layout.ts](https://github.com/sst/opencode/blob/a010a427e63c1f6406cef313041768a2381d3f2b/packages/tui/src/ui/layout.ts)）。Sidebar 内容全部是插槽注入的 feature plugin：Context（tokens、已用百分比、花费）、Limit、MCP、Todo、Footer（[sidebar.tsx](https://github.com/sst/opencode/blob/a010a427e63c1f6406cef313041768a2381d3f2b/packages/tui/src/routes/session/sidebar.tsx)）。V2 新增会话标签页（横向或竖排 rail）、终端面板、全屏 diff viewer、可拖拽 resize handle。"可用宽度 ≤120 时 sidebar 以浮层出现"是笔记未读完上下文的推断。**外观。** 用户消息是左侧粗竖线 `┃`（颜色取当前 agent 色）加抬升背景；工具分 `InlineTool`（一行：图标加文本，Read `→`、Write `←`、Glob/Grep `✱`）与 `BlockTool`（抬升背景块，用于 shell、edit、patch）；diff 在宽度 >120 时 split、否则 unified；被用户拒绝单独识别为 denied 态而非 error，等待权限时转 warning 色（[index.tsx](https://github.com/sst/opencode/blob/a010a427e63c1f6406cef313041768a2381d3f2b/packages/tui/src/routes/session/index.tsx)）。33 个内置主题，V2 主题格式是完整 token 树加色相调色板与 `$hovered` 等交互态后缀（[themes](https://opencode.ai/docs/themes/)）。**交互。** leader 键 `ctrl+x`，命令面板 `ctrl+p`，全量可配置键位；V2 的 agent 循环是 `shift+tab`，`tab` 让给补全，与 V1 时代"Tab 切 agent"不同（笔记推断其原因）；`escape` 需连按两次才打断；`<leader>return` 入队（[keybind.ts](https://github.com/sst/opencode/blob/a010a427e63c1f6406cef313041768a2381d3f2b/packages/tui/src/config/keybind.ts)）。权限提示为 **Allow once / Allow always / Reject**，选 always 先进二次确认页，选 reject 进入 "Tell OpenCode what to do differently" 输入框（[permission.tsx](https://github.com/sst/opencode/blob/a010a427e63c1f6406cef313041768a2381d3f2b/packages/tui/src/routes/session/permission.tsx)、[permissions](https://opencode.ai/docs/permissions/)）。`/undo` 移除最后一条用户消息及其文件改动，依赖 Git（V1 文档；V2 是否仍依赖 Git 未核实）。**理由。** TUI 只是 server 的一个 client（[server](https://opencode.ai/docs/server/)）；1.0 以性能与能力为由把 Go/Bubble Tea 换成自研 OpenTUI，此条来自搜索摘要、未逐句核对原文（[release notes 镜像](https://newreleases.io/project/github/sst/opencode/release/v1.0.0)）。

**Crush 布局。** 全屏加右侧 32 列 sidebar；`compactModeWidthBreakpoint = 120`、`compactModeHeightBreakpoint = 30`，**宽小于 120 或高小于 30 时 sidebar 折叠成顶部 header**（[ui.go](https://github.com/charmbracelet/crush/blob/1b37350c8afb8d905cfa67a941ba495d554d4141/internal/ui/model/ui.go)）。Sidebar 自上而下是 logo、会话标题、cwd 与分支、模型信息、subagents、修改过的文件、LSPs、MCPs、Skills（[sidebar.go](https://github.com/charmbracelet/crush/blob/1b37350c8afb8d905cfa67a941ba495d554d4141/internal/ui/model/sidebar.go)）。编辑器上方是 pills 区：todo 进度 pill 与 queue pill，可展开成列表（[pills.go](https://github.com/charmbracelet/crush/blob/1b37350c8afb8d905cfa67a941ba495d554d4141/internal/ui/model/pills.go)）。**外观。** 工具头部固定为"状态图标 + 工具名 + 关键参数"，状态图标 pending `●`、success `✓`、error `×`；正文默认 10 行，尾部提示 `… (N lines hidden) [click or space to expand]`（[tools.go](https://github.com/charmbracelet/crush/blob/1b37350c8afb8d905cfa67a941ba495d554d4141/internal/ui/chat/tools.go)）。Thinking 三态循环：折叠 → 尾部窗口 → 全文。语义色 token 含专门的 `yolo` 与 `plan` 色（[quickstyle.go](https://github.com/charmbracelet/crush/blob/1b37350c8afb8d905cfa67a941ba495d554d4141/internal/ui/styles/quickstyle.go)）。**交互。** 权限对话框三个按钮 **Allow / Allow for Session / Deny**，键 `a` / `s` / `d`；含 diff 时宽度 ≥140 默认 split（[permissions.go](https://github.com/charmbracelet/crush/blob/1b37350c8afb8d905cfa67a941ba495d554d4141/internal/ui/dialog/permissions.go)）。输入模式（code / plan）与权限模式（default / yolo / sysadmin）**正交**，状态栏徽章为反色粗体 `PLAN MODE` / `YOLO MODE`，默认模式不显示徽章（[status.go](https://github.com/charmbracelet/crush/blob/1b37350c8afb8d905cfa67a941ba495d554d4141/internal/ui/model/status.go)）。未发现 undo、checkpoint 相关代码或文档，但笔记未做穷尽搜索，不能断言不存在。**理由**写在仓库内的 UI 工程准则：聊天列表惰性渲染、每条目自行缓存、已完成条目可冻结、"Never do IO or expensive work in `Update`"（[AGENTS.md](https://github.com/charmbracelet/crush/blob/1b37350c8afb8d905cfa67a941ba495d554d4141/internal/ui/AGENTS.md)）。

### Gemini CLI 让对话框直接替换输入区，审批选项文案最细

**布局。** 默认内联模式：历史进终端 scrollback，底部 Composer 加一行可配置 footer，无 sidebar；对话框出现时**直接替换输入区**而非浮层（[DefaultAppLayout.tsx](https://github.com/google-gemini/gemini-cli/blob/1930f25394c4b6338b64ffdb43083fca41614798/packages/cli/src/ui/layouts/DefaultAppLayout.tsx)）。Footer 条目、标签与各区域可逐项隐藏（[configuration.md](https://github.com/google-gemini/gemini-cli/blob/1930f25394c4b6338b64ffdb43083fca41614798/docs/reference/configuration.md)）。**外观。** 用户 `> `、模型 `✦ `；工具状态符号 `✓` success、`o` pending、`⊷` executing、`?` confirming、`-` canceled、`x` error（[constants.ts](https://github.com/google-gemini/gemini-cli/blob/1930f25394c4b6338b64ffdb43083fca41614798/packages/cli/src/ui/constants.ts)）；同一轮工具调用包在一个圆角边框组里，边框色随组状态变化；终端内 diff 只有 unified（笔记由源码未见 split 分支推断）。**交互。** `Shift+Tab` 循环 Default → Auto-Edit → Plan，`Ctrl+Y` 切 YOLO；忙时 `Tab` 排队，排队区最多列 3 条（[keyboard-shortcuts.md](https://github.com/google-gemini/gemini-cli/blob/1930f25394c4b6338b64ffdb43083fca41614798/docs/reference/keyboard-shortcuts.md)）。编辑类确认选项为 `Allow once` / `Allow for this session` / `Allow for this file in all future sessions` / `Modify with external editor` / `No, suggest changes (esc)`（[ToolConfirmationMessage.tsx](https://github.com/google-gemini/gemini-cli/blob/1930f25394c4b6338b64ffdb43083fca41614798/packages/cli/src/ui/components/messages/ToolConfirmationMessage.tsx)）。Rewind（`/rewind` 或双击 `Esc`）三选一：回退对话并撤销代码 / 只回退对话 / 只撤销代码（[rewind.md](https://github.com/google-gemini/gemini-cli/blob/1930f25394c4b6338b64ffdb43083fca41614798/docs/cli/rewind.md)）；另有默认关闭的 checkpointing，用影子 Git 仓库快照，`/restore` 恢复后会重新提出原 tool call（[checkpointing.md](https://github.com/google-gemini/gemini-cli/blob/1930f25394c4b6338b64ffdb43083fca41614798/docs/cli/checkpointing.md)）。Plan Mode 审批选项为 "Yes, automatically accept edits" / "Yes, manually accept edits"（[plan-mode.md](https://github.com/google-gemini/gemini-cli/blob/1930f25394c4b6338b64ffdb43083fca41614798/docs/cli/plan-mode.md)）。未找到 Gemini CLI 的正式 UI 设计指南。Qwen Code 仅调研了审批模式差异：多一档由 LLM 分类器把关的 `auto`，连续 3 次被阻后回退人工审批（[approval-mode 文档](https://github.com/QwenLM/qwen-code/blob/main/docs/users/features/approval-mode.md)）。

### Aider 没有状态栏，用 git 提交代替审批

Aider 没有常驻状态栏、没有工具卡片、没有折叠：信息靠启动公告行、每轮后的 `Tokens: … Cost: …` 行和提示符前缀（如 `ask>`、`architect>`）承载，每次输入前在规则线上方列出聊天中的文件（[io.py](https://raw.githubusercontent.com/Aider-AI/aider/main/aider/io.py)、[base_coder.py](https://raw.githubusercontent.com/Aider-AI/aider/main/aider/coders/base_coder.py)）。对已加入聊天的文件的编辑不逐次确认，而是直接落盘并自动 commit，`/undo` 撤销上一个 aider 提交；shell 命令等副作用用行内 `(Y)es/(N)o` 确认，成组时加 `(A)ll`、`(S)kip all`、`(D)on't ask again`。没有排队，只能 `Ctrl-C` 打断；没有会话树或恢复选择器。公开理由是 git 即安全网："you never lose your work if aider makes an inappropriate change"（[git 文档](https://aider.chat/docs/git.html)）。对 nanobot 的参考价值主要是反面的：它证明"无审批"必须以可靠的撤销机制为前提。

### Cline、Roo、Kilo、Continue 收敛到同一副侧栏骨架，但只有 Cline 仍在迭代

**共同布局。** 顶部导航 → 任务头（成本与上下文进度）→ 虚拟化消息流 → 贴在输入框上方的待决动作区 → 带模式、模型、自动批准控件的输入框。Cline 的 footer 自上而下是 AutoApproveBar、ActionButtons、QueuedPrompts、InputSection；Settings、History 等是覆盖层，ChatView 始终挂载、只用 `display:none` 隐藏，因此草稿不丢（[ChatView.tsx](https://github.com/cline/cline/blob/e1f499a/apps/vscode/webview-ui/src/components/chat/ChatView.tsx)、[App.tsx](https://github.com/cline/cline/blob/e1f499a/apps/vscode/webview-ui/src/App.tsx)）。待决动作区的形态是分歧所在：Cline 与 Roo 是全宽主次双按钮行，Continue 是输入框顶部的 "Lump" 凸起小条，Kilo 新版是独立的 Permission Dock 卡片。

**Cline（当前版）。** 审批按钮文案随动作类型变化，全部出自一张配置表：读类 "Approve / Reject"，编辑类 **"Save / Reject"**，命令 **"Run Command / Reject"**，流式中只有 "Cancel"（[buttonConfig.ts](https://github.com/cline/cline/blob/e1f499a/apps/vscode/webview-ui/src/components/chat/chat-view/shared/buttonConfig.ts)）。界面状态由后端下发的 `TurnPhase` 驱动：idle / streaming / awaiting_approval / awaiting_followup / completed / error / resumable，旧的"看最后一条消息"逻辑只作回退（[ExtensionMessage.ts](https://github.com/cline/cline/blob/e1f499a/apps/vscode/src/shared/ExtensionMessage.ts)）。工具行标题是成对句式 "Cline wants to edit this file:"；读、列、搜等低风险工具合并为一行摘要 "Cline read 3 files, 1 folder, performed 2 searches:"（[ToolGroupRenderer.tsx](https://github.com/cline/cline/blob/e1f499a/apps/vscode/webview-ui/src/components/chat/chat-view/components/messages/ToolGroupRenderer.tsx)）。Plan/Act 是输入区右下角的滑动胶囊，textarea 描边跟随模式色。批准、拒绝会把输入框里的草稿一并发出，所以"拒绝并附反馈"不需要单独对话框（[useMessageHandlers.ts](https://github.com/cline/cline/blob/e1f499a/apps/vscode/webview-ui/src/components/chat/chat-view/hooks/useMessageHandlers.ts)）。排队卡片区分 `queue` 与 `steer` 两种投递。检查点不再有独立行，恢复入口并入"编辑用户消息"：**Reset Chat**（回退对话、保留代码）与 **Reset Code**（回退对话并重置代码）（[UserMessage.tsx](https://github.com/cline/cline/blob/e1f499a/apps/vscode/webview-ui/src/components/chat/UserMessage.tsx)）。注意 Cline 的线上文档与代码不一致：自动批准文档列九项与 YOLO，代码只有五个复选框；检查点文档的按钮文案与代码也不同。公开理由是 "frontload context, cultivate mutual understanding, and then empower the agent to act"（[Cline 博客](https://cline.bot/blog/plan-smarter-code-faster-clines-plan-act-is-the-paradigm-for-agentic-coding)），检查点让出错成本 "nearly zero"（[Checkpoints 文档](https://docs.cline.bot/features/checkpoints)）。

**Roo Code（最终版，已归档）。** 保留了最完整的"逐类呈现"：API 请求行（完成后降到 40% 不透明度）、带每秒计时的推理块、表格式 DiffView、命令块底部可编辑的允许 / 拒绝前缀列表、消息流里的检查点标记加 "Restore Files" / "Restore Files & Task" 菜单（[ChatRow.tsx](https://github.com/RooCodeInc/Roo-Code/blob/b867ec9/webview-ui/src/components/chat/ChatRow.tsx)、[CheckpointMenu.tsx](https://github.com/RooCodeInc/Roo-Code/blob/b867ec9/webview-ui/src/components/chat/checkpoints/CheckpointMenu.tsx)）。拒绝按钮叫 "Deny"，命令主按钮叫 "Run"。上下文进度条是 4px 三段：已用、为输出预留、可用（[ContextWindowProgress.tsx](https://github.com/RooCodeInc/Roo-Code/blob/b867ec9/webview-ui/src/components/chat/ContextWindowProgress.tsx)）。五个内置模式（Architect、Code、Ask、Debug、Orchestrator），`Ctrl+.` 循环。**一个需要避开的坑**：处理排队消息时会隐式批准下一个待决动作，即使自动批准是关的（[Message Queueing](https://roocodeinc.github.io/Roo-Code/features/message-queueing)）。

**Kilo Code（新版）。** 权限卡片 PermissionDock 内含可展开的规则管理区，点拒绝后才出现反馈输入框，占位 "Tell Kilo what to do differently"、提示 "Enter to reject, Esc to cancel"（[PermissionDock.tsx](https://github.com/Kilo-Org/kilocode/blob/b6a49d0/packages/kilo-vscode/webview-ui/src/components/chat/PermissionDock.tsx)）。回退以整条用户消息为粒度，回退后底部出现 "N messages reverted / Redo / Redo All" 横幅，发送新消息才使之永久（[Kilo Checkpoints](https://kilo.ai/docs/code-with-ai/features/checkpoints)）。Agent Manager 以完整编辑器 tab 打开，左侧 worktree 列表，可对同一 prompt 并行跑最多 4 个版本（[Agent Manager](https://kilo.ai/docs/automate/agent-manager)）。两个主按钮的实际英文文案未在所读文件中找到，文档称为 "Run" / "Deny"。

**Continue（停止维护）。** 没有任务头；历史中每条用户消息本身就是可编辑输入框。Lump 同一时刻只显示一种工具条，按优先级切换；待批工具调用的按钮是 "⌘⌫ Reject" / "⌘⏎ Accept"（[LumpToolbar.tsx](https://github.com/continuedev/continue/blob/5522c6f/gui/src/components/mainInput/Lump/LumpToolbar/LumpToolbar.tsx)）。工具状态句是五态模板：will / wants to / is / tried to / 无前缀（[ToolCallStatusMessage.tsx](https://github.com/continuedev/continue/blob/5522c6f/gui/src/pages/gui/ToolCallDiv/ToolCallStatusMessage.tsx)）。上下文用量是一个只在 ≥60% 时才出现的 7×14px 竖向"电池"。工具策略三档：Automatic / Ask First / Excluded。未找到 Continue 对这些界面决策的设计说明。

### Goose 给出了最可照搬的工具卡片规格

**布局。** Electron 桌面端：可开合侧栏（Home/Chat、Session History、Recipes、Scheduler、Extensions、Settings），Chat 区只列最近 10 个会话；没有对话旁的大型 workspace 窗格（[Session Management](https://goose-docs.ai/docs/guides/sessions/session-management)）。Composer 底栏从左到右是模型选择器、目录切换器加 git 分支、费用、context 指示器、扩展选择器、附件、语音、Send/Stop；**宽度低于 480px 时只保留模型选择器、语音、麦克风与 Send/Stop**（[ChatInput.tsx](https://raw.githubusercontent.com/aaif-goose/goose/main/ui/desktop/src/components/ChatInput.tsx)）。**外观。** 工具卡片是 `rounded-lg border` 容器，折叠标题为"工具名 + 第一个参数的 key: value"（超 80 字符截断），**待审批时边框变琥珀色，审批区在卡片内部以分隔线隔开**；默认展开规则是"有进行中的活动才展开，用户手动切换后以用户选择为准"；运行中日志限高 4rem、完成后 20rem（[ToolCallWithResponse.tsx](https://raw.githubusercontent.com/aaif-goose/goose/main/ui/desktop/src/components/ToolCallWithResponse.tsx)）。会话状态标记三种：蓝色旋转为处理中，绿点为"你在看别的会话时它完成了"，红点为出错。**交互。** 四种权限模式：Completely Autonomous（默认）、Manual Approval、Smart Approval、Chat Only，每个会话有自己的模式（[Permissions](https://goose-docs.ai/docs/guides/managing-tools/goose-permissions)）。忙时 Enter 入队，点 Send 则立即发送并打断；队列项可编辑、拖拽排序、立即发送、删除（[In-Session Actions](https://goose-docs.ai/docs/guides/sessions/in-session-actions)）。编辑历史消息二选一：Edit in Place（覆盖后续历史）或 Fork Session。"按工具权限三档 Always allow / Ask before / Never allow"仅来自第三方摘要，未核实。未找到 Goose 桌面端的官方设计说明。

### Amp 靠删功能表态，OpenHands 把右侧做成固定 tab

**Amp（闭源）。** 侧栏排序为置顶 → 活动 thread 按 project 分组 → 折叠的 "Inactive Last 72h" → Snoozed 与 Archived；thread 右侧窗格有 Changes、Portals、Files、Terminal、Huddle（[Threads](https://ampcode.com/docs/threads)）。2026 年的改版几乎都是删减并附带公开理由：TODO 列表删除，因为模型已能自行追踪且清单占 UI 空间（[TODOs Are Done](https://ampcode.com/news/todos-are-done)）；默认不审批，因为静态检查给人 "false sense of security"（[Amp, Rebuilt](https://ampcode.com/news/neo)）；编辑器扩展下线，因为 "By keeping these new models in an editor sidebar, we restrict them"（[The Coding Agent Is Dead](https://ampcode.com/news/the-coding-agent-is-dead)）；TUI 侧栏移除，多 thread 管理交给 web 与原生 app（[So Long, TUI Sidebar](https://ampcode.com/news/so-long-tui-sidebar)）；Dial 用 low / medium / high / ultra 取代按模型选择，且**只能在 thread 第一条消息前选定**，因为中途切换会让 prompt cache 失效（[The Dial](https://ampcode.com/docs/the-dial)）；忙时 `Enter` 默认 steer，`Ctrl+Enter` 才排队（[Steer, Don't Queue](https://ampcode.com/news/steer-dont-queue)）；command palette 取代 slash 菜单，因为它 "allows you to run commands without editing your prompt"（[Command Palette](https://ampcode.com/news/command-palette)）。编辑或恢复消息时的文件回滚也在重写中被移除。Amp web 与桌面 app 中工具调用和 diff 的外观没有文字资料。

**OpenHands / Agent Canvas。** 经典 GUI 是左侧 Chat Panel 加右侧固定 tab：Changes、VS Code、Terminal、App、Browser（[Key Features](https://docs.openhands.dev/openhands/usage/key-features)）。Agent Canvas 侧栏可拖拽调宽 **240–560px 且不超过窗口 40%**，会话按 workspace 文件夹分组，支持 pin 与 archive（[Conversations](https://docs.openhands.dev/openhands/usage/agent-canvas/conversations.md)）。确认策略三种：AlwaysConfirm、NeverConfirm、ConfirmRisky，风险等级 LOW / MEDIUM / HIGH / UNKNOWN，UNKNOWN 不视为安全（[SDK Security](https://docs.openhands.dev/sdk/guides/security)）。"Branch from here" 从自己的消息分支时该消息回填 composer。Agent Canvas 中审批按钮文案与确认模式的开关位置未找到文档。

### 多会话桌面应用把"会话即工作区、分支即集成"当作数据模型

Claude Code desktop 的 pane 可拖动、调宽、最小化、弹出为独立窗口，右侧 "session rail" 是卡片堆（环境、Changes、后台任务、Context 百分比），Plan 与 Tasks 是独立 pane（[Claude Code Desktop](https://code.claude.com/docs/en/desktop)）。Vibe Kanban 是四栏：Workspace Sidebar | Conversation | Context Panel（Changes / Logs / Preview 三选一）| Details Sidebar，顶部 navbar 有每个面板的开关，**布局按 workspace 保存并恢复**；侧栏状态分 Running、Idle、Needs Attention、Pinned（[Vibe Kanban Interface](https://www.vibekanban.com/docs/workspaces/interface.md)）。Nimbalyst 的注意力列表分 "Awaiting input / Running / Unread"，每轮结束显示 "Finished in 6m 57s, 3 files +45 -12" 之类的统计（[Nimbalyst](https://nimbalyst.com/docs/session-management/agent-window-and-session-management/)）。Conductor 的组织原则是 "The workspace is the unit of delegation. The branch and pull request are the unit of integration."（[Workflow](https://www.conductor.build/docs/concepts/workflow)）。Codex app 默认保留最近 15 个托管 worktree，删除前存快照（[Git worktrees](https://learn.chatgpt.com/docs/environments/git-worktrees)）。opencode desktop 的会话以 tab 打开，并有汇总未读会话的 Inbox（[changelog](https://opencode.ai/changelog?area=desktop)）。

## 横向对比：十一个维度上的收敛与分叉

下面三张表把上节拆成可横向扫读的维度。表内内容均出自上节已引用的来源；"未查到"表示笔记明确记录为缺口，"—"表示该产品没有此概念。

### 布局形态、状态信息与会话并行

| 对象 | 布局形态 | 状态信息放哪、放什么 | 会话管理与并行 |
|---|---|---|---|
| Zed | 左 dock 面板（四层）+ 面板外的 Threads Sidebar | 输入框底部控件行：token 用量、profile、模式、模型；无独立状态栏 | Sidebar 按项目分组；`ctrl-tab` 线程切换器；每线程可选 agent 与 worktree；后台保活 5 个线程视图 |
| Codex CLI | 单列 TUI，底部区域就地展开 | 输入框下一行，可勾选三十余项；右对齐模式 | `/resume` 选择器（未读用 `●`）；`/agents` 全屏指挥中心；`/fork`、`/worktree` |
| Claude Code | 单列 TUI；宽屏可出侧边 diff 面板 | 用户脚本状态行 + 内建页脚徽标（权限模式、PR 徽标） | `/resume`、`/branch`、`/fork`；子代理面板；desktop 版有侧栏与 split view |
| pi | 单列四段，无侧栏 | 三行 footer：路径分支 / token 缓存成本上下文% / 扩展状态 | 树状会话，`/tree`、`/fork`、`/clone`；作者有意不做子代理 |
| Codewhale | TUI + workbar 四面板 + hotbar | footer 常显权限档位与成本；status chips | 会话选择器 `Ctrl-R`；子代理审批冒泡到父 UI |
| opencode V2 | 全屏 + 右侧 42 列插槽 sidebar + 会话 tab + 终端面板 | 输入框下一行：上下文用量、子代理数、键位提示；sidebar 放 Context/MCP/Todo | 会话标签页、子会话导航（上下左右键）、fork、worktree 对话框 |
| Crush | 全屏 + 右侧 32 列 sidebar，<120×30 折叠为 header | 底部 status bar：模式徽章 + 键位提示；sidebar 放模型、上下文、cost、改动文件 | session picker；同 cwd 多客户端共享 workspace 与权限队列 |
| Gemini CLI | 内联 scrollback + Composer + footer | 可配置 footer（cwd、沙箱、模型、上下文%） | Session Browser；`/resume save` 手动存档 |
| Aider | 行式 REPL | 无常驻状态；每轮后打印 token 与成本 | 无会话管理，仅 `/save` `/load` |
| Cline | IDE 侧栏 webview | 任务头：成本胶囊 + 上下文进度条 | 历史视图；Kanban（worktree）与 Desktop 做并行 |
| Roo（归档） | IDE 侧栏 webview | 任务头：圆环进度 + % + 成本 | 父子任务树；无并行 |
| Kilo | IDE 侧栏 + Agent Manager 编辑器 tab | 任务头时间线 + 三段上下文条 | 会话标签条；Agent Manager 多 worktree、多版本 |
| Continue（停维） | IDE 侧栏 webview | 输入工具条：模式、模型、≥60% 才出现的上下文电池 | 可选会话标签；历史页 |
| Goose | 桌面：侧栏 + 对话 | composer 底栏：模型、目录、分支、费用、context | 侧栏最近 10 个会话，三色状态点；多窗口 |
| Amp | web/桌面：侧栏 + thread + 右侧窗格 | thread 标题栏：project、mode | pin / snooze / archive / labels；每 thread 一个云沙箱 |
| OpenHands | web：侧栏 + 对话 + 右侧 tab/抽屉 | 会话头部 overview 面板；context 进度条 | 按 workspace 文件夹分组；每 agent 一个 worktree；子会话 |

### 工具调用、diff、审批与模式

| 对象 | 工具调用呈现 | diff 呈现 | 审批 / 权限模型与选项措辞 | 模式切换 |
|---|---|---|---|---|
| Zed | 读搜抓取为一行；需确认 / 编辑 / 终端才成卡片 | 卡内只读编辑器；Review tab 逐 hunk Keep / Reject | 卡底 `Allow` / `Deny` + 粒度下拉（Only this time / Always for {tool} / Always for selected commands）；规则 confirm / allow / deny | profile `ctrl-i`；模式 `shift-tab`；非 Idle 时忽略 |
| Codex CLI | `•` + 动词时态；输出限 5 行；探索类合并 | unified，行号 + 符号，按 hunk 高亮 | 取代输入框的编号列表：`Yes, proceed (y)` / `Yes, and don't ask again for … (p)` / `No, and tell Codex what to do differently (esc)`；预设 Read Only / Ask for approval / Full Access | `⇧tab` 循环；`/plan` |
| Claude Code | 默认折叠为一行，`Ctrl+O` 展开 | 行号 + `+/-`；宽屏侧边面板 | `Yes` / `Yes, and don't ask again for: …` / `Yes, and switch to auto mode` / `No`；`Tab` 附评语 | `Shift+Tab`：default → acceptEdits → plan |
| pi | 三态底色块；bash 留尾 5 行 | 执行前即预览 | 默认无审批；权限门由扩展实现 | 无模式；`shift+tab` 调 thinking 档 |
| Codewhale | 卡片 + details pager | `/diff`；画法未查到 | 五档拨盘 Ask → Accept Edits → Plan → Auto-Review → Full Access；审批卡 `y` / `a` / `n` / `e` | `Shift+Tab` 拨盘；`Tab` 从不切模式 |
| opencode V2 | Inline 一行 / Block 背景块两级 | 宽 >120 split，否则 unified；独立 diff viewer | `Allow once` / `Allow always`（二次确认）/ `Reject`（进入反馈输入）；规则 allow / ask / deny | agent 循环 `shift+tab`（build / plan） |
| Crush | 图标 + 名称 + 参数头部，正文 10 行 | 权限框 ≥140 列 split | `Allow` / `Allow for Session` / `Deny`（`a` / `s` / `d`） | `shift+tab` code / plan；权限模式正交 |
| Gemini CLI | 状态符号列 + 圆角边框分组 | 终端内 unified；并排交给 IDE | `Allow once` / `Allow for this session` / `Allow … in all future sessions` / `Modify with external editor` / `No, suggest changes (esc)` | `Shift+Tab`：Default → Auto-Edit → Plan；`Ctrl+Y` YOLO |
| Aider | 文本行，无卡片 | 外包给 git | `(Y)es/(N)o/(A)ll/(S)kip all/(D)on't ask again`；编辑不问、自动 commit | `/ask` `/code` `/architect` |
| Cline | "wants to …" 标题行；低风险合并摘要 | 每文件折叠块，4px 色条 + 行号 | 全宽双按钮，文案随动作变：`Approve` / `Save` / `Run Command` 对 `Reject`；五项自动批准复选 | Plan/Act 滑动胶囊 |
| Roo（归档） | 逐类呈现，含 API 请求行 | 表格式双列行号 | `Approve` / `Save` / `Run` 对 `Deny`；七项自动批准 + 次数 / 成本上限 | 五模式，`Ctrl+.` 循环 |
| Kilo | 未逐文件分析 | Agent Manager diff 面板 | Permission Dock 卡片；每工具 Allow / Ask / Deny；拒绝后展开反馈框 | Agents（原 Modes） |
| Continue（停维） | 五态状态句 + 状态图标，多调用分组 | 4px 色条行；编辑器内联 diff | Lump 内 `Reject` / `Accept`；工具策略 Automatic / Ask First / Excluded | Chat / Plan / Agent 胶囊，`Ctrl+.` |
| Goose | 折叠卡片，待审批琥珀边框 | 未查到 | 卡内 `Allow` / `Deny`；四模式 Autonomous / Manual / Smart / Chat Only | `/mode`；每会话独立 |
| Amp | 默认折叠，`Alt+T` 展开 | 重复块检测；Changes 窗格 | 默认不审批；权限降为可选插件 | Dial low / medium / high / ultra，首条消息前锁定 |
| OpenHands | 阶段分组标题收敛为摘要；样式未查到 | Commits 抽屉合并历史与未提交改动 | AlwaysConfirm / NeverConfirm / ConfirmRisky + 风险分级；按钮文案未查到 | Agent Profile 按会话 |

### 排队与插话、回退、快捷键约定、主题

| 对象 | 忙时发消息：queue vs steer | 回退 / 检查点 | 快捷键约定 | 主题 |
|---|---|---|---|---|
| Zed | 默认排队；逐条 Steer 开关；Send Now | 编辑已发送消息 → 从该点重启；checkpoint 字样在当前源码未找到 | 每层 key context；键位画在按钮与 tooltip 上 | 复用编辑器语义 token，无面板专属色板 |
| Codex CLI | 三级队列；`Tab` 排队（`Enter` 插话为推断） | `Esc Esc` 选旧 prompt → 回退并回填输入框 | `?` 速查面板；`/keymap` 可改 | `/theme` 只选语法高亮主题；颜色跟随终端 |
| Claude Code | `Enter` 排队（当前工具调用后送达）；`Ctrl+Enter` 立即 | Rewind 菜单：代码与对话可分别恢复，另有摘要 | `/keybindings`；`Shift+Tab`、`Esc Esc` | auto / light / dark / 色盲 / ANSI / 自定义 JSON |
| pi | `Enter` steer；`Alt+Enter` follow-up | `/tree` 分支，不回滚文件 | `keybindings.json`，`/hotkeys` | `system` / dark / light，严格 schema JSON |
| Codewhale | `Enter` 排队；`Ctrl-Enter` steer | `Esc Esc` 只回退对话；`/undo`、`/restore` | 可打印字符永远进 composer | `/color` 设会话 composer 色；主题格式未查到 |
| opencode V2 | `<leader>return` 入队 | `/undo` `/redo`（消息 + 文件，V1 文档称依赖 Git） | leader `ctrl+x` + `ctrl+p` 面板，全量可配 | 33 个内置，V2 token 树 |
| Crush | 运行中提交即入队，queue pill 计数 | 未发现（未穷尽搜索） | 固定键位，`ctrl+p` 面板 | 内置 charmtone-panther（默认）、gruvbox-dark、keanu 等 + JSON 继承，实时预览编辑器 |
| Gemini CLI | `Tab` 排队；steering 为实验项 | `/rewind` 三选一；`/restore` 影子 Git | `keybindings.json` | 17 个内置 + 自定义 JSON |
| Aider | 无，只能 `Ctrl-C` | `/undo` 撤销上一个 aider 提交 | Emacs / vi 行编辑 | 五个颜色选项 |
| Cline | queue 与 steer 两种投递 | 编辑用户消息：Reset Chat / Reset Code | `Esc` 取消；`Ctrl+'` 聚焦 | 跟随 VS Code 主题变量 |
| Roo（归档） | FIFO 队列，处理时隐式批准 | 检查点行 + Restore Files / Restore Files & Task | `Ctrl+.` 模式，`Ctrl+Alt+A` 自动批准 | 跟随 VS Code |
| Kilo | 未查到 | 按用户消息 Revert + Redo 横幅 | `Ctrl+Shift+M` Agent Manager | 跟随 VS Code |
| Continue（停维） | 发新消息会取消待批调用 | 历史消息即输入框，改完重跑（语义为推断） | `Ctrl+L`、`Ctrl+I`、`Ctrl+.` | 跟随 IDE |
| Goose | `Enter` 入队；点 Send 打断；关键词打断 | Edit in Place / Fork Session | `Ctrl+N/T/O`，可在设置改 | ThemeSelector 存在，内容未读 |
| Amp | `Enter` 默认 steer；`Ctrl+Enter` 排队 | 文件回滚已在重写中移除 | `Ctrl+O` 命令面板为核心 | 自定义主题已移除 |
| OpenHands | `/goal` 运行时发消息即中断 | Branch from here | `Ctrl+P` 面板（旧 CLI） | 未查到 |

## 共性模式已经稳定，分歧集中在"控制权交给谁"

把十余款工具叠在一起看，有七条模式稳定到可以当作 nanobot 的默认假设。第一，**消息流占主位且放弃聊天气泡**，用左侧竖条、前缀符号或背景层级区分角色；Zed 的用户消息是小卡片、助手消息无框，opencode 与 Crush 用左边线，Codex 与 Gemini 用前缀字形。第二，**工具调用分两级密度**：只读与检索压成一行，有副作用或需要决策的才占版面，Zed 的 `use_card_layout` 判定、opencode 的 Inline 与 Block、Cline 的 ToolGroupRenderer、Codex 的 `• Explored` 合并是同一个规则的四种实现。第三，**需要用户立刻处理的东西紧贴输入光标**：Codex 与 Gemini 直接替换输入框，Cline 与 Roo 放按钮行，Zed 放活动栏与浮动工具卡，Crush 放 pills；环境信息则被压到输入框下方最不显眼的一行。第四，**推理正文默认隐藏**，只留"在做什么"的一行标题，完整内容按需展开。第五，**拒绝是输入口而不是终点**：Codex 的 "No, and tell Codex what to do differently"、opencode 的同名输入框、Gemini 的 "No, suggest changes"、Claude Code 的 `Tab` 评语、Cline 的草稿随按钮发出，都把拒绝与反馈合并成一步。第六，**审批选项遵循同一梯度**：一次 → 本会话或同前缀 → 永久 → 拒绝并说明。第七，**用户拒绝、执行出错、被取消是三种不同的视觉状态**，opencode、Crush、Gemini 与 Zed（虚线边框）都明确区分。

真正的分歧有五处，每一处背后都是"控制权交给谁"的不同判断。**排队还是插话**分成两派：Zed、Goose、Codewhale、Gemini、Claude Code 默认排队并把插话作为显式动作；pi 与 Amp 默认 steer，Amp 的理由是 agent 能更早按反馈行动。值得注意的是"排队"一词在各家含义不同，Claude Code 的排队消息在当前工具调用结束后就送达，实质上接近别家的 steer；而 Roo 的队列会隐式批准待决动作，说明不区分投递语义会制造安全漏洞。**审批的重心**从逐条确认滑向"模式拨盘加风险分级"：Codewhale 五档、Goose 四档、Claude Code 的 auto、Qwen 的分类器、OpenHands 的 ConfirmRisky 都在减少弹窗数量；pi 与 Amp 走到尽头直接不问，但两者的前提分别是"用户自己装扩展把关"和"云沙箱隔离"，Aider 的前提是 git 自动提交。**回退**有三种心智模型：编辑旧消息并从该点重跑（Zed、Codex、Continue）、代码与对话可分别恢复的检查点（Claude Code、Gemini、Cline 的 Reset Chat / Reset Code、Roo）、会话树或分叉（pi、Goose、OpenHands）；趋势是检查点粒度从"每次工具调用一行"收敛到"每条用户消息"，Cline 与 Kilo 都这么改了，笔记推断这说明逐工具粒度在实际使用中过细。**计划与待办**的分化最大：Amp 整个删掉，pi 拒绝内置，Cline 弃用 Focus Chain；而 Zed 留在活动栏、Crush 与 opencode 做成 pill、Claude Code desktop 做成独立 pane。**并排工作区窗格**的形态在固定 tab（OpenHands、Amp、Vibe Kanban）与可自由拖拽弹出的 pane（Claude Code desktop）之间二分。

还有一条贯穿 2026 年的暗线：**界面在做减法**。Cline 删了任务时间线、逐请求成本行与检查点行，Amp 删了 TODO、审批与 TUI 侧栏，Zed 把面板内的历史与配置视图外移。笔记对 Cline 的归纳是"新 harness 之上这些独立层不再值得维护"（推断，综合弃用页与源码注释）。但这条暗线对 nanobot 只能有保留地借鉴，原因在下一节说明。

## nanobot 重构建议：保留四区骨架，重做密度、状态机与断点

本节全部是本报告基于上述调研作出的设计推断，不是任何来源的原文；每条建议后给出它所依据的调研证据。有三条前提来自任务说明而非调研：nanobot 基于 gpui-fast 与 gpui-component 0.9（GPUI Kit）、MIT 许可；产品视口是 wgpu 3D 视图，在 Windows 上是盖在 gpui 内容之上的原生 surface；gpui 的浮层只有 deferred 的才能盖在它上面。这三条本次调研没有独立验证，其中 GPUI Kit 的版本号还与笔记读到的 README 安装片段 `gpui-kit = "0.7"` 不一致（[gpui-component README](https://github.com/longbridge/gpui-component)），crate 名迁移与版本对应关系未核实。

> **入库时的核对（非调研结论）**：本仓库实际钉的是 crates.io 上的 `gpui-component = "0.9.0"`（见根 `Cargo.toml` 与 `Cargo.lock`），外加 `.vendor/gpui-pre-compat` 垫片指向 gpui-fast；上面提到的 `gpui-kit = "0.7"` 是调研读到的 README 安装片段，两者的 crate 名与版本对应关系仍未核实。Windows 上视口为原生 DirectComposition surface、只有 `deferred` 浮层能盖住它，这两条来自本仓库 `crates/nanobot-gpu/src/present.rs` 的实现，同样不是调研得出的。

### 已确认的设计决策

本小节记录调研之后由项目负责人拍板的事项，优先级高于下文的建议。

- **2026-11-10：会话侧栏支持收起，收起后完全隐藏，不保留图标栏。** 依据是 Zed 的做法（整块隐藏，`Ctrl+Alt+J` 开合）。由此带来三条约束：
  1. 侧栏隐藏时，"等待确认 / 出错 / 完成未读"的会话必须在别处可见，否则用户会错过审批请求。汇总徽标（"N 个等待处理"）放在**会话工作区顶部工具栏**（2026-11-10 确认，不放标题栏），点击即展开侧栏并定位到对应会话。
  2. 开合要有快捷键和可点击的入口；隐藏状态下入口不能随侧栏一起消失，因此开合按钮同样放在会话工作区顶部工具栏，与汇总徽标相邻。
  3. 开合状态与宽度需要持久化；窗口过窄时自动隐藏，但自动隐藏不应覆盖用户手动设定的偏好。
  下文"窄窗口断点"第一级中的"收成图标 rail 或隐藏"据此收敛为"隐藏"。

### 布局方案：四区不变，视口旁的会话列按 Zed 的四层重排

现有的"会话列表 | 会话工作区 | 产品视口 | 面板 dock"与行业收敛形态同构，建议保留，不做范式迁移。可类比的先例是 Vibe Kanban 的四栏与 OpenHands 的 App tab：3D 视口本质上是"agent 产出的可交互预览"（笔记推断），只是在 nanobot 里它是主角而不是附属 tab。会话工作区内部建议改成 Zed 的四层：顶部一行工具栏（会话标题可就地编辑、新建、选项菜单、全屏切换）、虚拟化对话流、活动栏、输入框。**计划审阅、审批 / 澄清条、消息队列三者合并进一个活动栏**，按 Zed 的做法用分隔线纵向堆叠在输入框上方，各自可折叠；等待审批的那一项限高并可滚动，避免把对话流挤出视野，这正是 Zed 源码注释给出的理由。输入框下方只留一行环境状态，建议内容参照 Goose 与 pi：模型、权限档位、上下文百分比（70% 与 90% 两级变色，pi 的阈值）、可选的成本。

宽度分配建议以下列数字为起点，再在真实窗口上调参。会话列表 240–300px，Zed 的默认是 300，OpenHands 的可调范围是 240–560 且不超过窗口 40%。会话工作区最小 300px（Zed 的 `MIN_PANEL_WIDTH`），对话内容最大宽度 850px 后居中（Zed 的 `max_content_width`），避免超宽屏上行长失控。视口吃掉剩余空间。面板 dock 沿用 GPUI Kit 的 Dock。

**窄窗口断点建议写成常量并分三级折叠**，依据是 Crush 把断点写成 120×30 常量、opencode 的"总宽不足则面板接管全屏"、Claude Code 的 110/144 列阈值以及 Goose 在 480px 以下只保留核心控件。第一级，窗口不足以同时容纳四区时，会话列表先收成图标 rail 或隐藏，以快捷键唤出；第二级，右侧 / 底部面板 dock 收起为可切换的抽屉；第三级，会话工作区与视口不再并排，改为二选一切换，并在视口模式下保留一个可唤出的最小输入条。高度方向同样设一个断点：窗口过矮时活动栏默认折叠成单行摘要（Crush 的 pills 形态）。输入框底部控件行按 Goose 的规则在窄宽度下只留模型、发送 / 停止。布局开合与宽度建议**按会话或按工作区持久化**，这是 Vibe Kanban 与 OpenHands 的共同做法。具体像素阈值没有可直接套用的来源，因为被调研的断点多以终端列为单位。

**原生 surface 与浮层的约束决定了弹出物的摆放规则。** 没有任何被调研对象面对"一块原生 surface 压在 UI 之上"的问题，以下是由约束本身推出的规则。其一，所有从会话工作区发起的弹层（`@` 补全、斜杠菜单、模型与权限选择器、审批粒度下拉、tooltip）默认向会话列自身内部展开：补全与菜单向上弹出并左对齐输入框，宽度不超过会话列，这与 Codex "所有补全建议渲染在输入框上方"的做法一致。其二，凡是位置上可能越过视口边界的弹层，一律走 deferred 渲染路径，并为它建立一个统一封装，禁止业务代码直接用非 deferred 的浮层。其三，需要全局居中的模态（设置、会话历史、命令面板）要么 deferred，要么参照 Gemini 与 Codex 的"就地替换"思路，直接替换会话工作区内容而不浮在视口上。其四，应用内 toast 不要落在视口上方；窗口非激活时的提醒可以参照 Zed，用独立的 `WindowKind::PopUp` 小窗（450×72px、屏幕右上角、不抢焦点），独立窗口天然不受视口 surface 遮挡，但这一点在 gpui-fast 与 Windows 上的行为未验证。其五，Zed 的审批以浮动工具卡形式贴在输入框上方，nanobot 应让这块浮动区域始终位于会话列的矩形之内。拖拽调整分隔条时，原生 surface 的重排是否会闪烁或滞后，本次调研没有任何依据，需要实测。

### 组件清单：GPUI Kit 能覆盖骨架，输入框与流式 markdown 是风险点

下表列出重构需要的组件、可借鉴的规格来源、以及在 GPUI Kit 中的对应物。"GPUI Kit 对应"一列来自组件目录页（[gpui-kit.com/component](https://gpui-kit.com/component)），**笔记没有读过这些组件的源码或文档，实际 API 与成熟度全部待验证**；难度判断是笔记的推断。

| nanobot 组件 | 规格来源（借鉴设计） | GPUI Kit 对应（待验证） | 难度与风险（推断） |
|---|---|---|---|
| 会话列表项（标题、状态徽标、时间、增删统计） | Zed `ThreadItem` 的五态；Goose 三色点 | Sidebar、List、Badge、Spinner | 易 |
| 对话流虚拟列表（尾部跟随、增量 splice） | Zed `list` + `FollowMode::Tail` | gpui 自带 `list`；Kit 的 VirtualList、MessageScroller | 易，但需确认 gpui-fast 是否含 `FollowMode`、`splice_focusable`、`pause_following_tail` |
| 用户消息卡片（可点击编辑） | Zed 用户消息卡片规格 | Message、Bubble、Textarea | 中 |
| 助手 markdown（流式追加） | Zed `Markdown::append` + 16ms 节流 | TextView | 中：流式追加性能、跨块选择、代码块复制、链接回调需验证 |
| 行内工具行（图标 + 标签，可点开） | Zed 行内标签；opencode InlineTool；Cline 合并摘要 | Label、Icon、Collapsible | 易 |
| 工具卡片（头部、状态、折叠、审批区） | Zed 卡片；Goose `ToolCallWithResponse` | Collapsible、GroupBox、Button | 易到中 |
| Thinking 块（折叠、限高、渐隐） | Zed thinking 块；Crush 三态 | Collapsible、Scrollable | 易 |
| Diff 卡片 | Zed 卡内只读 diff；Codex 行号 + 符号 | Diff | 中；逐 hunk 接受 / 拒绝需自建 |
| 命令 / 脚本输出块 | pi 尾部 5 行 + 完整输出落盘；Codex 5 行 | TextView 或等宽 Label | Kit 无终端组件，真实终端内嵌为难；建议降级为 ANSI 文本块 |
| 活动栏（计划摘要、改动、队列、待审批） | Zed `render_activity_bar`；Crush pills | Collapsible、Divider、Stepper | 中 |
| 审批条 | Zed 权限按钮 + 粒度下拉；Codex 编号选项 | Button、DropdownButton、Kbd | 易 |
| 澄清 / 提问表单 | Crush question 对话框族；Codewhale question sheet | Questionnaire、Radio、Checkbox | Questionnaire 是否对应此用途为推断，未读文档 |
| 输入框（`@` 内联 chip、`/` 补全、粘贴图片） | Zed MessageEditor；Codex 原子元素 | Input、Textarea、Editor、Attachment | **难**：内联不可分割 chip 加补全需在 Kit 输入组件上自建 |
| 模型 / 权限 / 模式选择器 | Zed `Picker::list` | Combobox、Select、Command、Popover | 易 |
| 键位提示 | Zed `KeyBinding::for_action_in` | Kbd、Tooltip | 中：需自封"action → 当前上下文键位"查询 |
| 上下文用量指示 | Roo 三段条；pi 阈值变色；Continue ≥60% 才显示 | Progress | 易 |
| 回合结束统计行 | Nimbalyst；Zed 回合控件行 | Label | 易 |
| 加载与流式指示 | Zed 点状 / 沙漏 spinner；Codex shimmer | Spinner、Skeleton、Shimmer | 易 |
| 错误提示 | Zed `Callout` 分类错误 | Alert | 易 |
| 窗口外通知 | Zed PopUp 小窗 | gpui `open_window`；Kit Notification 为应用内 toast | 易，Windows 行为待测 |
| 面板 dock 与分隔条 | Zed Dock；Vibe Kanban 布局持久化 | Dock、Resizable | 易；与原生 surface 的配合待测 |

实现架构上建议直接采用 Zed 的两条取舍。一是**会话模型发索引级增量事件**（`NewEntry`、`EntryUpdated(i)`、`EntriesRemoved(range)`），视图只做 splice 与单项重测，而不是每次全量 diff；笔记推断若 nanobot 的 thread model 以此为核心，视图同步逻辑的结构可以照着设计。二是**回合状态用显式状态机而不是"看最后一条消息"推导**，Cline 已经为此从推导改为后端下发 `TurnPhase`，其七个状态可直接作为 nanobot 的起点。Crush 的工程准则补充了第三条：每个消息条目自行缓存、完成后冻结，重型内容（diff、输出块）惰性物化。

### 交互规范：排队优先、三选项审批、按用户消息回退

**按键表。** 下表是建议的默认键位，尽量与 Zed 的 Windows 键位和跨工具通行约定一致，以降低迁移成本。总规则采纳 Codewhale 的明文约定：可打印字符永远进入输入框，全局快捷键全部带修饰键。所有键位应可配置，并像 Zed 那样把键位画在对应按钮与 tooltip 上。需要注意 CAD 视口自身通常占用大量裸字母与鼠标组合键，视口聚焦时的键位冲突没有调研依据，需单独设计 key context。

| 动作 | 建议键位 | 依据 |
|---|---|---|
| 发送 / 忙时排队 | `Enter` | Zed、Goose、Codewhale、Claude Code |
| 换行 | `Shift+Enter` | 几乎全部对象 |
| 立即发送（插话 / 打断） | `Ctrl+Shift+Enter` | Zed `SendImmediately` |
| 停止生成 | `Esc`（无弹层、无编辑态时） | Zed、Codex、Claude Code |
| 切换权限档位 | `Shift+Tab` | Codex、Claude Code、Gemini、Crush、Codewhale、Zed 的模式循环 |
| 允许一次 / 始终允许 / 拒绝 | `Shift+Alt+A` / `Shift+Alt+Q` / `Shift+Alt+X` | Zed Windows 键位 |
| 打开审批粒度下拉 | `Ctrl+Alt+A` | Zed |
| 队列首项切换 Steer / 编辑 / 移除 / 清空 | `Ctrl+Alt+S` / `Ctrl+Alt+E` / `Ctrl+Shift+Backspace` / `Ctrl+Alt+Backspace` | Zed |
| 新会话 | `Ctrl+N` | Zed、Goose、Crush |
| 会话切换器 | `Ctrl+Tab` / `Ctrl+Shift+Tab` | Zed、opencode |
| 聚焦 / 开合会话列表 | `Ctrl+Alt+;` / `Ctrl+Alt+J` | Zed |
| 模型选择器 | `Shift+Alt+/` | Zed |
| 命令面板 | `Ctrl+P` 或 `Ctrl+K` | opencode、Crush、OpenHands 用 `Ctrl+P`；Codewhale、Vibe Kanban 用 `Ctrl+K` |
| 添加上下文菜单 | `Ctrl+;` | Zed |
| 会话内搜索 | `Ctrl+F`，`F3` / `Shift+F3` | Zed |
| 展开输入框 | `Shift+Alt+Esc` | Zed |
| 展开 / 折叠工具输出 | `Ctrl+O` | Claude Code、pi、Gemini |
| 回退到旧消息 | 空输入时 `Esc Esc`，或点击用户消息卡片 | Codex、Claude Code、Gemini、Codewhale；Zed 的点击编辑 |
| 对话流滚动 | `PageUp/PageDown`、`Ctrl+Home/End`；`Shift+PageUp/PageDown` 跳上 / 下一条消息 | Zed（pi 与 Claude Code 另有"跳到上 / 下一条用户 prompt"的键位） |

**审批流。** 建议审批位置采用"卡片内加活动栏"双锚定：待批的工具卡片边框变为警示色（Goose 的琥珀边框），同时活动栏出现同一请求的操作按钮；笔记的推断是这样能把审批与具体动作绑定，又不要求用户滚动去找。选项固定为三个加一个下拉：**允许一次、始终允许（下拉选择范围：本会话 / 此类操作 / 此前缀）、拒绝**，这是 Zed、opencode、Crush、Gemini 的最大公约数。主按钮文案随动作类型变化，沿用 Cline 的配置表思路；CAD 语境下的具体动词（例如"应用修改""执行脚本"）需要 nanobot 自己定义，调研没有对应先例。拒绝后就地展开反馈输入（opencode 与 Kilo 的做法），`Enter` 提交、`Esc` 取消；同时保留 Cline 的隐式通道，即审批等待期间直接在输入框打字发送等同于"拒绝并附反馈"。"始终允许"先进一步确认，展示规则实际覆盖的范围，对应 opencode 的二次确认页与 Claude Code "规则只覆盖选项所写明的内容"的原则。权限档位建议做成常显的拨盘而非埋在设置里，三到四档即可（逐项询问 / 自动接受低风险 / 只规划不执行 / 完全放行），档位显示在输入框下方状态行，变更在对话流留一行痕迹，这是 Codewhale 的做法。档位与"计划模式"是否正交，Crush 选择正交、Claude Code 与 Codewhale 选择合并在同一循环里，两者都可行。Amp 与 pi 的"完全不问"路线不建议作为默认：笔记的判断是它依赖云沙箱隔离或用户自担，而 nanobot 是直接操作本地设计文档的桌面壳。

**排队与插话。** 建议默认排队，逐条提供 Steer 开关与"立即发送"，即 Zed 的模型，再加上 Goose 的队列项编辑、重排与删除。选排队而非 Amp 式默认 steer 的理由是推断性的：设计类操作往往改动模型状态，中途改变方向的代价可能高于代码场景，这一点没有调研证据直接支持。无论选哪边，必须把两种投递语义在数据模型里分开（Cline 的 `delivery: queue | steer`），并明确规定**处理排队消息不得隐式批准任何待决审批**，Roo 的反例已经说明后果。发送按钮按 Zed 的三态切换图标：空闲为发送、忙且有输入为排队、忙且无输入为停止。

**回退。** 建议以用户消息为粒度，入口就是点击已发送的用户消息卡片，进入编辑态时其后内容盖一层半透明遮罩（Zed），确认时给出两到三个选项：只回退对话、回退对话并恢复产品状态、分叉为新会话（Cline 的 Reset Chat / Reset Code，加 Goose 的 Fork Session）。回退后保留一条可重做的横幅直到发出新消息（Kilo）。**这里有一个调研无法回答的问题**：所有被调研的检查点实现都建立在 git 或影子 git 仓库之上，nanobot 面向的 CAD 文档如何做快照、Rhai 脚本插件造成的副作用能否撤销，没有任何先例；可以确定的只有 Claude Code 与 Gemini 的教训，即必须在界面上诚实标注哪些改动不在检查点覆盖范围内（两者都不恢复由 shell 造成的改动）。

**会话状态徽标。** 建议五态，直接对应 Zed 的 `AgentThreadStatus`：运行中（旋转指示）、等待确认（警示色，最高优先级）、完成未读（强调色圆点）、出错（错误色圆点）、已读完成（无徽标）。笔记的归纳是会话级状态至少要区分"运行中 / 等用户 / 完成未读 / 出错"四类，"完成但未读"最容易漏掉；Goose、Vibe Kanban、Nimbalyst 三家的分类基本一致。分组标题上汇总"N 个等待处理"，对应 Zed 的 `waiting_thread_count` 与 Codex 指挥中心的 "Needs you" 过滤。后台会话的视图实体建议像 Zed 那样有限保活（Zed 的默认是 5 个）。

### 视觉规范：少量语义色，两种字体角色，四种动效

**颜色角色。** Zed 没有为 agent 面板单独定义色板，Codex 只允许绿、红、品红加一个强调蓝，两者都指向"语义色越少越好"。建议 nanobot 定义以下角色并映射到 GPUI Kit 的主题 token（Kit 的 token 命名与 Zed 不同，需要一层映射；具体对应关系未读 Kit 文档）：三层表面色（面板底、抬升的卡片与输入框底、卡片头部的微弱叠色）；一种边框色配 60% 与 80% 两档透明度，外加聚焦边框色；主文本与弱化文本两级；五种状态色，即强调（进行中、选中）、成功、警告（等待审批）、错误、弱化（取消）。失败、取消、被拒的卡片用虚线边框而不只靠颜色，这是 Zed 的做法，也呼应 Claude Code "不靠颜色也能看出来"的无障碍理由。权限档位可各配一个辨识色（Crush 有专门的 `yolo` 与 `plan` 色，Cline 的输入框描边跟随模式色），但高风险档位的颜色应与警告或错误色同族。

**间距。** Zed 线程视图的实测规格可作基线：用户消息卡片内边距约上下 12px、左右 8px，外层左右 8px；助手消息左右 20px、上下 6px、块间距 12px；缩进内容用左侧 1px 竖线加约 14–20px 内缩；圆角统一为中等一档（[thread_view.rs](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/crates/agent_ui/src/conversation_view/thread_view.rs#L7440)）。上述像素值由 Zed 的 tailwind 式刻度类名换算而来，属于本报告的换算，未在运行界面上量过。

**字体层级。** 建议两种字体角色：UI 字体用于助手正文与一切控件，等宽字体用于用户消息、命令、路径、代码与输出，这是 Zed 的分工。字号四级：正文 14px、面板内次级标签与工具名 13px、用户消息与输出 12px、角标 10px，对应 Zed 的 `TextSize` 与 `agent_buffer_font_size`（[typography.rs](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/crates/ui/src/styles/typography.rs#L94)）。会话工作区建议支持独立于全局的字号缩放；Zed 用 `WithRemSize` 改写子树 rem 基准实现，该工具在 GPL 的 `ui` crate 内，nanobot 需自行实现等价物或改用主题字号。

**图标与动效。** 图标按工具种类而非工具名映射，可直接采用 ACP 的十类 `ToolKind`（read、edit、delete、move、search、execute、think、fetch、switch_mode、other）作为分类基准（[ACP tool-calls](https://agentclientprotocol.com/protocol/tool-calls)），再为 CAD 操作补充自己的类别。图标资源不能取自 Zed。动效限制在四种，全部有明确语义：生成中的点状 spinner、等待确认的另一种 spinner（Zed 用沙漏变体区分两者）、标题或占位内容生成中的透明度脉冲或 shimmer、运行中工具的旋转图标。流式文本按固定节拍匀速揭示（Zed 为 16ms，Codex 在积压时切换到追赶模式）。应提供"减少动效"开关，Claude Code 与 opencode 都有。Claude Code 的"spinner 10 秒后变琥珀色、权限卡住时变红"是低成本的状态补充，可选。

### 许可边界：借规格不借代码，GPUI Kit 可用但逐项验收

Zed 一侧的边界由各 crate 的 `license` 字段决定：`agent_ui`、`ui`、`markdown`、`editor`、`terminal_view` 均为 `GPL-3.0-or-later`，`gpui` 为 `Apache-2.0`（[agent_ui](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/crates/agent_ui/Cargo.toml)、[ui](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/crates/ui/Cargo.toml)、[markdown](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/crates/markdown/Cargo.toml)、[editor](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/crates/editor/Cargo.toml)、[terminal_view](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/crates/terminal_view/Cargo.toml)、[gpui](https://github.com/zed-industries/zed/blob/f0d896f2c18a543335eb155f729d36b624695591/crates/gpui/Cargo.toml)）。对 MIT 许可的 nanobot，本报告的建议是：**可以借鉴**的是布局层级、尺寸与间距数值、交互规则、状态机划分、事件模型的形状、键位约定与文案思路；**不能拷贝**的是上述 GPL crate 中的任何源码，包括 `ui` crate 的组件实现、`WithRemSize`、`Markdown` 实体、`ThreadItem`、图标 SVG 资源，也不应把这些 crate 作为依赖引入。gpui 自身的 API（`list`、`ListState`、`Animation`、focus 与 action 机制）属于 Apache-2.0，可直接使用。这是工程层面的保守做法，不构成法律意见；"照搬规格而不是代码更现实"本身也是笔记的推断。其余被调研项目的许可在笔记中只记录了少数几个（pi 相关未记录、Codewhale 与 DeepSeek harness 为 MIT、opcode 为 AGPL-3.0），同样只借设计。

GPUI Kit 一侧，README 声明 Apache-2.0、75 个以上组件、可选 `gpui-fast` feature，并列出 Dock、VirtualList、TextView、Editor、Diff、Message、MessageScroller、Bubble、Attachment、Questionnaire、Shimmer 等与 agent 界面直接相关的组件（[README](https://github.com/longbridge/gpui-component)、[组件目录](https://gpui-kit.com/component)）。这些组件可以直接用，但**成熟度全部待验证**，建议在重构开工前对四项做最小验收：TextView 在持续流式追加下的帧耗时与文本选择；`list` 或 MessageScroller 的尾部跟随与用户上滚后的暂停；Input / Editor 上能否实现内联不可分割 chip 与补全弹层；Dock 与 Resizable 在原生视口 surface 旁拖拽时的表现。

## 未覆盖 / 待确认：调研没有查到的内容

以下内容在笔记中被明确记录为缺口、推断或仅来自搜索摘要，本报告没有补写。

| 范围 | 未覆盖或待确认的内容 |
|---|---|
| 整体方法 | 没有查看任何截图；像素级外观、实际配色与字体观感均未确认。闭源产品只依据文档 |
| nanobot 自身 | gpui-fast 是否包含 `FollowMode`、`splice_focusable`、`pause_following_tail` 等列表 API；GPUI Kit 各组件的实际 API 与成熟度；`gpui-kit` 与 `gpui-component` 的版本对应；原生 surface 与 deferred 浮层、PopUp 窗口、分隔条拖拽的实际行为；CAD 文档级检查点；视口聚焦时的键位冲突。这些都没有任何调研对象可参照 |
| Zed | checkpoint / "Restore checkpoint" 在当前源码中未找到，旧按钮是否已移除、rewind 是否回滚文件未验证；macOS 键位未读；工具栏高度、字体家族默认值、主题色 token 全集未读；`Markdown::append` 是否真正增量解析未读实现；Threads Sidebar 宿主 `MultiWorkspace` 未读；Dock 的 resize 与比例尺寸算法未读；subagent 卡片完整布局未细读；"Zed 1.0 于 2026-04-29 发布"仅见二手来源；Claude Code / Codex / ACP Registry / Terminal Threads 几篇后续博文仅取自搜索摘要 |
| Codex CLI | 官方文档无 TUI 布局描述；`Enter` 在运行中进入哪一级队列未核实；沙箱与审批策略的命令行参数未能从官方文档核实；大段粘贴是否折叠未确认；`/keymap` 默认键位全表未读；`sidebar_*`、`pets`、`daybreak` 等新功能未读；PR 历史未检索 |
| Claude Code | 助手消息与工具标题行的确切字形；diff 是否有并排或词级高亮；Markdown 渲染规则；文件编辑权限提示的确切选项文案；`/resume` 选择器与 `/permissions` 对话框细节；内建页脚的完整默认内容；桌面通知配置 |
| pi | 启动 header 排版；diff 具体画法与 `toolDiff*` token 名；用户与助手消息的具体外观；spinner 默认帧；`@` 选择器排序与外观；`/settings` 面板；未找到解释"改为 fullscreen 默认"或"加入 `/mcp`"的文章；2025 年回顾与加入 Earendil 两篇仅来自搜索摘要 |
| DeepSeek 系 | 官方 harness 的 Web UI 布局、审批模型、快捷键完全未取得；其发布日期、版本号未从仓库确认；Codewhale 的 header、侧栏、footer 其余字段、卡片配色、状态字形、diff 画法、thinking 默认状态、主题文件格式均未说明；两者都没有找到作者的界面设计理由 |
| opencode | Desktop / Web 客户端的具体布局未读源码；V2 斜杠命令清单未从源码核对；`/undo` 在 V2 是否仍依赖 Git 未核实；会话分享移除的原因与时间无官方公告；"宽度 ≤120 时 sidebar 为浮层"为推断；1.0 迁移到 OpenTUI 的说明仅来自搜索摘要；早期桌面版是否为 Tauri 未找到一手来源 |
| Crush | 无用户可配置键位的文档；undo / checkpoint、会话分享、vim 模式未发现但未穷尽搜索；`!` shell 模式无用户文档；compact 模式 header 文案未核对；未找到 Charm 讲界面设计的文章 |
| Gemini CLI / Qwen Code | footer 默认条目完整列表；启动 banner 与 Tips；`/model` 与 `/settings` 外观；无正式 UI 设计指南；Qwen Code 除审批模式外未调研 |
| Aider | 浏览器模式的布局与框架；语音转写服务；图片与网页相关文档；最新版本号；作者为何不做面板与状态栏的表述 |
| Cline / Roo / Kilo / Continue | 子代理给出的源码行号未逐条复核；Cline 的 JetBrains 面板、Desktop、cline-hub、CLI TUI 源码未读，通知与声音设置位置未找到，Plan/Act 快捷键为推断；Roo 的 Marketplace 与 Cloud 历史界面未调研，Roomote 状态各来源冲突；Kilo 新版 TaskHeader、PromptInput 外观与权限卡主按钮英文文案未确认；Continue 各工具文案模板未下载，README "只读"与 API `archived: false` 的矛盾未核实，被 Cursor 收购一说无官方公告；四者均未检索 PR / Issue 讨论 |
| Goose | 侧栏源码抓取失败，条目顺序与右键菜单未确认；模式选择器在源码中的位置与文档不一致；thinking 与逐消息 token 统计外观未读；按工具权限三档名称未核实；lead/worker 模型的界面呈现；无官方设计说明 |
| OpenHands | 经典 GUI 的 tab 源码抓取失败，是否仍有 Jupyter tab 未确认；工具卡片与风险等级的具体样式；审批按钮文案与确认模式开关位置；源码结构仅来自第三方导读；无界面决策的官方博文 |
| Amp | web 与桌面 app 的工具调用、diff 外观无文字资料；旧权限规则语法未获取 |
| 多会话桌面应用 | Codex app 的 diff 面板、审批流、steer / queue、通知、快捷键；Conductor 的侧栏分组、checkpoints、多 chat tab；Nimbalyst 的开源状态；Codex app 与 Claude Code desktop 的设计理由未检索 |

## Conclusion

这次调研改变的认识是：nanobot 的界面问题不在骨架。十余款工具各自演化两年后停在了几乎相同的四区布局上，nanobot 已经站在那里；差距在骨架之内的三件小事，即哪些工具调用值得占一张卡片、待决动作离输入光标有多近、回合与会话的状态是否被显式建模。这三件事 Zed 都给出了可以逐项对照的规格，而 Cline、Codex、Goose 分别在状态机、信息折叠优先级和卡片细节上提供了第二意见。

更值得警惕的是另一个方向的启发。2026 年的主流叙事是"模型变强，脚手架该删"，Amp 删掉审批与待办，Cline 删掉时间线与检查点行。这条路的每一步都以代码场景的两个安全网为前提：git 能撤销，沙箱能隔离。设计类产品暂时两者都没有现成答案，所以 nanobot 应当借走这些工具的信息架构和降噪手法，却把审批拨盘、计划审阅和按消息回退留下来，并把"产品状态的检查点"当作重构中唯一需要原创设计的部分优先立项。在动手之前，最该先花一两天验证的不是视觉，而是四个技术假设：gpui-fast 的列表跟随 API、GPUI Kit 的流式 markdown、带内联 chip 的输入框、以及原生视口旁的 deferred 浮层与分隔条拖拽。

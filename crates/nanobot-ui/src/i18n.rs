//! Shell chrome localization: the shell's user-visible strings in the two
//! locales the framework ships (zh-CN, en).
//!
//! A flat message table behind a locale. [`t`] returns the localized text,
//! falling back to the other locale and then to the key itself — a missing
//! translation is visible in the UI, never a blank.
//!
//! The locale is a user setting (`ui.locale`) and defaults from the harness
//! profile's `locale`; the live one lives on [`crate::shell::HarnessShell`],
//! which offers `tr`/`trf` so call sites stay short.

/// A UI locale. The shell ships two; anything unrecognised reads as the
/// default (a hand-edited settings file must not blank the UI).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Locale {
    #[default]
    ZhCn,
    En,
}

impl Locale {
    /// Parses a stored locale code (`zh-CN`, `en`, `en-US`, …).
    pub fn from_code(code: &str) -> Self {
        match code.to_ascii_lowercase().as_str() {
            "en" | "en-us" | "en-gb" => Self::En,
            _ => Self::ZhCn,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::ZhCn => "zh-CN",
            Self::En => "en",
        }
    }
}

/// `(key, zh-CN, en)`.
type Msg = (&'static str, &'static str, &'static str);

const MESSAGES: &[Msg] = &[
    // -- shell chrome --
    ("shell.diagnostics", "诊断", "Diagnostics"),
    ("shell.settings", "设置", "Settings"),
    ("shell.plugins", "插件", "Plugins"),
    ("shell.projects", "项目", "Projects"),
    ("shell.no_sessions", "（无会话）", "(no sessions)"),
    (
        "shell.viewport_caption",
        "计划 {done}/{total} · 会话工作区 · 视口 · 面板",
        "Plan {done}/{total} · Session workspace · Viewport · Panels",
    ),
    ("shell.undo", "撤销", "Undo"),
    ("shell.redo", "重做", "Redo"),
    ("shell.save", "存档", "Save"),
    ("shell.load", "开档", "Open"),
    ("shell.import", "导入", "Import"),
    (
        "shell.sessions",
        "会话 · {count}（{total} 个）",
        "Sessions · {count} ({total})",
    ),
    ("shell.guest_signin", "访客 · 点击登录", "Guest · click to sign in"),
    ("shell.plan_progress", "计划 {done}/{total}", "Plan {done}/{total}"),
    ("shell.plan_state", "plan 状态", "Plan state"),
    (
        "shell.compute_progress",
        "计算进度 {done}/{total}",
        "Compute progress {done}/{total}",
    ),
    ("shell.conversation", "对话流", "Conversation"),
    (
        "shell.clarify_prompt",
        "AI 需要补充信息，请回答以下问题（通过输入框提交）：",
        "The AI needs more information — answer below (submitted through the input):",
    ),
    (
        "shell.plan_rejected",
        "plan {index}：验证未通过，等你决定",
        "plan {index}: verification failed, your call",
    ),
    ("shell.accept", "接受", "Accept"),
    ("shell.reject", "回滚", "Roll back"),
    ("shell.modify", "修改", "Modify"),
    ("shell.itemized", "逐项: ", "Itemized: "),
    (
        "shell.no_sessions_in_project",
        "工程「{name}」还没有会话",
        "Project \"{name}\" has no sessions yet",
    ),
    (
        "shell.sessions_note",
        "会话的创建与事件日志随会话内核落地；项目层已把工程目录与清单读进来了",
        "Session creation and event logs come with the session kernel; the project layer has read the project directory and manifest.",
    ),
    // -- composer --
    (
        "composer.placeholder",
        "描述你想做的改动，回车发送…",
        "Describe the change you want, Enter to send…",
    ),
    // -- status / plan / block vocabulary (model.rs) --
    ("status.active", "活动", "Active"),
    ("status.running", "运行中", "Running"),
    ("status.awaiting_review", "等待审查", "Awaiting review"),
    ("status.archived", "已归档", "Archived"),
    ("step.pending", "待办", "Pending"),
    ("step.in_progress", "进行中", "In progress"),
    ("step.done", "完成", "Done"),
    ("plan.none", "无", "None"),
    ("plan.proposed", "已提议", "Proposed"),
    ("plan.executing", "执行中", "Executing"),
    ("plan.verifying", "验证中", "Verifying"),
    ("plan.settled", "已接受", "Settled"),
    ("plan.contingent", "待审查", "Contingent"),
    ("plan.rejected", "已回滚", "Rejected"),
    ("plan.clarify", "待补充", "Clarify"),
    ("block.user", "你", "You"),
    ("block.assistant", "AI", "AI"),
    ("block.tool", "工具", "Tool"),
    ("block.plan", "计划", "Plan"),
    ("block.check", "检查", "Check"),
    ("tool.running", "运行中", "Running"),
    ("tool.done", "完成", "Done"),
    ("tool.failed", "失败", "Failed"),
    // -- plugins panel --
    ("plugins.enabled", "启用", "Enabled"),
    ("plugins.disabled", "已禁用", "Disabled"),
    ("plugins.disable", "禁用", "Disable"),
    (
        "plugins.not_shared",
        "未共享插件装载结果",
        "Plugin load result not shared",
    ),
    ("plugins.rejected", "{name} 拒绝装载", "{name} rejected"),
    // -- diagnostics panel --
    ("diagnostics.product", "产品", "Product"),
    ("diagnostics.theme", "主题", "Theme"),
    ("diagnostics.projects", "项目数", "Projects"),
    ("diagnostics.sessions", "会话数", "Sessions"),
    (
        "diagnostics.sessions_in_project",
        "{count}（打开项目 {name}）",
        "{count} (open project {name})",
    ),
    ("diagnostics.panels", "面板数", "Panels"),
    ("diagnostics.script_panels", "脚本面板", "Script panels"),
    (
        "diagnostics.mounted_errors",
        "已挂载 {mounted}，错误 {errors}",
        "mounted {mounted}, errors {errors}",
    ),
    ("diagnostics.viewport_frames", "视口帧", "Viewport frames"),
    ("diagnostics.settings", "设置", "Settings"),
    ("diagnostics.settings_unconnected", "未连接", "not connected"),
    ("diagnostics.account", "账号", "Account"),
    ("diagnostics.guest", "访客", "Guest"),
    // -- settings panel --
    ("settings.theme", "主题", "Theme"),
    ("settings.font", "字体", "Font"),
    ("settings.font.small", "小", "Small"),
    ("settings.font.normal", "标准", "Normal"),
    ("settings.font.large", "大", "Large"),
    ("settings.font.family", "字体族", "Family"),
    (
        "settings.font.placeholder",
        "字体族名称，回车应用",
        "Font family name, Enter to apply",
    ),
    ("settings.locale", "语言", "Language"),
    ("settings.locale.zh", "中文", "Chinese"),
    ("settings.locale.en", "英文", "English"),
    ("settings.theme_dark", "深色", "Dark"),
    ("settings.theme_light", "浅色", "Light"),
    (
        "settings.memory_mode",
        "内存模式（未连接设置存储）",
        "In-memory (no settings store connected)",
    ),
];

/// The localized text for `key`. Falls back to the other locale, then to the
/// key itself so a gap is visible rather than silent. The result lives as
/// long as `key` (known messages are `'static`).
pub fn t(locale: Locale, key: &str) -> &str {
    match MESSAGES.iter().find(|(k, _, _)| *k == key) {
        Some((_, zh, en)) => match locale {
            Locale::ZhCn => zh,
            Locale::En => en,
        },
        None => key,
    }
}

/// [`t`] with `{name}` substitution: `args` carries bare names, so a
/// message reads `"Plan {done}/{total}"` and the call passes `"done"`.
pub fn t_replace(locale: Locale, key: &str, args: &[(&str, &str)]) -> String {
    let mut out = t(locale, key).to_string();
    for (name, value) in args {
        out = out.replace(&format!("{{{name}}}"), value);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_locale_resolves_every_key() {
        let keys: Vec<&str> = MESSAGES.iter().map(|(k, _, _)| *k).collect();
        for key in &keys {
            for locale in [Locale::ZhCn, Locale::En] {
                let text = t(locale, key);
                assert_ne!(text, *key, "{key} is missing its translation");
                assert!(!text.is_empty(), "{key} is empty");
            }
        }
    }

    #[test]
    fn an_unknown_key_reads_as_the_key() {
        assert_eq!(t(Locale::En, "no.such.key"), "no.such.key");
    }

    #[test]
    fn locale_codes_round_trip_and_unknown_reads_as_default() {
        assert_eq!(Locale::from_code("zh-CN"), Locale::ZhCn);
        assert_eq!(Locale::from_code("en-US"), Locale::En);
        assert_eq!(Locale::from_code("fr"), Locale::ZhCn);
        assert_eq!(Locale::ZhCn.code(), "zh-CN");
        assert_eq!(Locale::En.code(), "en");
    }

    #[test]
    fn replacement_fills_every_placeholder() {
        let out = t_replace(
            Locale::ZhCn,
            "shell.sessions",
            &[("count", "3"), ("total", "12")],
        );
        assert_eq!(out, "会话 · 3（12 个）");
        let out = t_replace(Locale::En, "shell.sessions", &[("count", "3"), ("total", "12")]);
        assert_eq!(out, "Sessions · 3 (12)");
    }
}

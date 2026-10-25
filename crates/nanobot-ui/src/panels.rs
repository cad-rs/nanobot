//! The panel host: where product panels and script-plugin panels live
//! (design 4.8).
//!
//! The shell owns one [`PanelHost`]. The product registers its domain panels
//! (feature tree, properties, checks) as [`PanelSource::Product`] factories; the
//! script host registers plugin panels as [`PanelSource::Script`] paths, which
//! are mounted lazily through `gpui-rhai` the first time they are shown — the
//! same mount-once discipline workbench-ui uses.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{AnyElement, App, Context, IntoElement, ParentElement as _, Styled as _, Window};

use crate::model::SessionView;
use crate::shell::HarnessShell;

/// A dock region. Panels registered for a region are rendered there.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PanelArea {
    Left,
    Right,
    Bottom,
    Top,
}

impl PanelArea {
    pub fn parse(area: &str) -> Option<Self> {
        match area.to_ascii_lowercase().as_str() {
            "left" => Some(PanelArea::Left),
            "right" => Some(PanelArea::Right),
            "bottom" => Some(PanelArea::Bottom),
            "top" => Some(PanelArea::Top),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            PanelArea::Left => "left",
            PanelArea::Right => "right",
            PanelArea::Bottom => "bottom",
            PanelArea::Top => "top",
        }
    }
}

/// What a panel renders.
pub enum PanelSource<S: SessionView> {
    /// A product-registered element factory. It receives the shell (for shared
    /// selection state) and the active session's workspace.
    Product(Rc<
        dyn Fn(
            &HarnessShell<S>,
            &S::Workspace,
            &mut Window,
            &mut Context<HarnessShell<S>>,
        ) -> AnyElement,
    >),
    /// A script panel: a `gpui-rhai` view mounted on first show.
    Script(PathBuf),
}

/// One registered panel.
pub struct PanelDef<S: SessionView> {
    pub id: String,
    pub title: String,
    pub area: PanelArea,
    pub order: i32,
    pub source: PanelSource<S>,
    /// The plugin that contributed this panel, when a script plugin did.
    /// The plugins panel uses it to disable a plugin at runtime (removing
    /// exactly its panels); product panels have none.
    pub source_plugin: Option<String>,
}

impl<S: SessionView> Clone for PanelDef<S> {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            title: self.title.clone(),
            area: self.area,
            order: self.order,
            source: match &self.source {
                PanelSource::Product(render) => PanelSource::Product(render.clone()),
                PanelSource::Script(path) => PanelSource::Script(path.clone()),
            },
            source_plugin: self.source_plugin.clone(),
        }
    }
}

impl<S: SessionView> PanelDef<S> {
    pub fn product(
        id: impl Into<String>,
        title: impl Into<String>,
        area: PanelArea,
        order: i32,
        render: Rc<
            dyn Fn(
                &HarnessShell<S>,
                &S::Workspace,
                &mut Window,
                &mut Context<HarnessShell<S>>,
            ) -> AnyElement,
        >,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            area,
            order,
            source: PanelSource::Product(render),
            source_plugin: None,
        }
    }

    pub fn script(
        id: impl Into<String>,
        title: impl Into<String>,
        area: PanelArea,
        order: i32,
        script: PathBuf,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            area,
            order,
            source: PanelSource::Script(script),
            source_plugin: None,
        }
    }

    /// Names the plugin this panel came from (the plugins panel's disable
    /// removes exactly these).
    pub fn with_plugin(mut self, plugin: impl Into<String>) -> Self {
        self.source_plugin = Some(plugin.into());
        self
    }
}

/// Script views mounted by [`PanelHost`]. One host per process, one view per
/// panel id; a failure is remembered so it is not retried every frame.
#[derive(Default)]
pub struct ScriptRuntime {
    host: Option<gpui_rhai::ScriptViewHost>,
    views: HashMap<String, gpui_rhai::ScriptViewHandle>,
    errors: HashMap<String, String>,
}

impl ScriptRuntime {
    /// Mounts `script` under `id` once. Safe to call on every frame.
    pub fn mount(&mut self, id: &str, script: &Path, window: &mut Window, cx: &mut App) {
        if self.views.contains_key(id) || self.errors.contains_key(id) {
            return;
        }
        if self.host.is_none() {
            match gpui_rhai::ScriptViewHost::new("nanobot-main", cx) {
                Ok(host) => self.host = Some(host),
                Err(e) => {
                    self.errors.insert(id.to_string(), format!("ScriptViewHost: {e}"));
                    return;
                }
            }
        }
        let host = self.host.clone().expect("host just created");
        // gpui-rhai view ids allow only ASCII alphanumerics, '-' and '_'.
        let view_id: String = id
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            })
            .collect();
        let mounted = gpui_rhai::FileScriptView::new(script.to_path_buf())
            .prepare()
            .and_then(|prepared| {
                prepared.mount(gpui_rhai::ScriptViewConfig::new(view_id), host, window, cx)
            });
        match mounted {
            Ok(handle) => {
                self.views.insert(id.to_string(), handle);
            }
            Err(e) => {
                self.errors.insert(id.to_string(), e.to_string());
            }
        }
    }

    /// The panel's element, or an error placeholder. Empty while not mounted.
    pub fn element(&self, id: &str) -> AnyElement {
        if let Some(error) = self.errors.get(id) {
            return error_box(format!("脚本面板加载失败：{error}"));
        }
        let Some(handle) = self.views.get(id) else {
            return gpui::div().into_any_element();
        };
        match handle.flex_item() {
            Ok(item) => match &self.host {
                Some(host) => host.container(item),
                None => item,
            },
            Err(e) => error_box(format!("脚本视图错误：{e}")),
        }
    }
}

fn error_box(message: String) -> AnyElement {
    gpui::div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .size_full()
        .p_2()
        .text_xs()
        .text_color(gpui::red())
        .child(message)
        .into_any_element()
}

/// The registry of product and script panels, plus the script runtime.
pub struct PanelHost<S: SessionView> {
    panels: Vec<PanelDef<S>>,
    scripts: Rc<RefCell<ScriptRuntime>>,
}

impl<S: SessionView> Default for PanelHost<S> {
    fn default() -> Self {
        Self {
            panels: Vec::new(),
            scripts: Rc::new(RefCell::new(ScriptRuntime::default())),
        }
    }
}

impl<S: SessionView> PanelHost<S> {
    pub fn register(&mut self, panel: PanelDef<S>) {
        self.panels.push(panel);
    }

    /// Panels of one region, in registration order.
    pub fn panels_in(&self, area: PanelArea) -> Vec<&PanelDef<S>> {
        let mut found: Vec<&PanelDef<S>> = self
            .panels
            .iter()
            .filter(|p| p.area == area)
            .collect();
        found.sort_by_key(|p| p.order);
        found
    }

    pub fn panels(&self) -> &[PanelDef<S>] {
        &self.panels
    }

    /// The number of registered panels (selfcheck / smoke reporting).
    pub fn len(&self) -> usize {
        self.panels.len()
    }

    pub fn is_empty(&self) -> bool {
        self.panels.is_empty()
    }

    /// Script panels mounted so far (for selfcheck / smoke reporting).
    pub fn mounted_scripts(&self) -> usize {
        self.scripts.borrow().views.len()
    }

    /// Script panels that failed to mount, with the reason.
    pub fn script_errors(&self) -> Vec<(String, String)> {
        self.scripts
            .borrow()
            .errors
            .iter()
            .map(|(id, reason)| (id.clone(), reason.clone()))
            .collect()
    }

    /// Registers every panel a script plugin declared.
    pub fn attach_plugins(&mut self, panels: &[nanobot_rhai::PanelRegistration]) {
        let mut order = self.panels.len() as i32;
        for panel in panels {
            let Some(area) = PanelArea::parse(&panel.area) else {
                continue;
            };
            self.register(
                PanelDef::script(
                    panel.id.clone(),
                    panel.title.clone(),
                    area,
                    order,
                    panel.script.clone(),
                )
                .with_plugin(&panel.plugin),
            );
            order += 1;
        }
    }

    /// Disables a plugin at runtime: removes exactly the panels it
    /// contributed. Returns how many went away (the plugins panel shows
    /// this; zero for an already-disabled or unknown plugin).
    pub fn remove_plugin(&mut self, plugin: &str) -> usize {
        let before = self.panels.len();
        self.panels
            .retain(|panel| panel.source_plugin.as_deref() != Some(plugin));
        before - self.panels.len()
    }

    /// Mounts the script panel and returns its element.
    pub fn script_element(
        &self,
        id: &str,
        script: &Path,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        self.scripts.borrow_mut().mount(id, script, window, cx);
        self.scripts.borrow().element(id)
    }

    /// Renders a product panel.
    pub fn product_element(
        &self,
        panel: &PanelDef<S>,
        shell: &HarnessShell<S>,
        window: &mut Window,
        cx: &mut Context<HarnessShell<S>>,
    ) -> AnyElement {
        let PanelSource::Product(render) = &panel.source else {
            return gpui::div().into_any_element();
        };
        let Some(ws) = shell.workspace() else {
            return gpui::div().into_any_element();
        };
        render(shell, ws, window, cx)
    }
}

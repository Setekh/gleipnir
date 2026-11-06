//! Languages & SDKs: which toolchain each language uses here, and the state of its tools.

use gleipnir_core::{PackageToolchain, ToolStatus, ToolchainEvent, Toolchains};
use gleipnir_packages::ToolSource;
use gleipnir_ui::Theme;
use gleipnir_workspace::{ModalView, Workspace};
use gpui::{
    AnyElement, App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    KeyBinding, PathPromptOptions, PromptLevel, Render, SharedString, Subscription, Window,
    actions, div, prelude::*, px,
};
use std::path::PathBuf;

actions!(settings, [OpenLanguages, Close]);

pub fn bind_keys(cx: &mut App) {
    // IntelliJ's Project Structure, which is where SDKs live there too.
    #[cfg(target_os = "macos")]
    cx.bind_keys([KeyBinding::new("cmd-;", OpenLanguages, None)]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([KeyBinding::new("ctrl-alt-shift-s", OpenLanguages, None)]);
    cx.bind_keys([KeyBinding::new("escape", Close, Some("LanguageSettings"))]);
}

pub fn add_to_workspace(workspace: &mut Workspace, _: &mut Window, _: &mut Context<Workspace>) {
    workspace.register_action(|workspace, _: &OpenLanguages, window, cx| {
        let toolchains = workspace.toolchains().clone();
        workspace.toggle_modal::<LanguageSettings>(window, cx, move |_, cx| {
            LanguageSettings::new(toolchains, cx)
        });
    });
}

pub struct LanguageSettings {
    focus_handle: FocusHandle,
    toolchains: Entity<Toolchains>,
    /// A problem with the last thing tried, like adding a folder that isn't an SDK.
    message: Option<(String, SharedString)>,
    _subscription: Subscription,
}

impl EventEmitter<DismissEvent> for LanguageSettings {}
impl ModalView for LanguageSettings {}

impl Focusable for LanguageSettings {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl LanguageSettings {
    fn new(toolchains: Entity<Toolchains>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&toolchains, |_, _, _: &ToolchainEvent, cx| cx.notify());
        Self {
            focus_handle: cx.focus_handle(),
            toolchains,
            message: None,
            _subscription: subscription,
        }
    }

    fn choose(&mut self, kind: String, root: PathBuf, cx: &mut Context<Self>) {
        self.message = None;
        self.toolchains
            .update(cx, |t, cx| t.choose_sdk(&kind, root, cx));
    }

    fn add_sdk(
        &mut self,
        package_id: String,
        kind: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Use as SDK".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = prompt.await else {
                return;
            };
            let Some(root) = paths.into_iter().next() else {
                return;
            };
            this.update(cx, |this, cx| {
                let result = this
                    .toolchains
                    .update(cx, |t, cx| t.add_sdk(&kind, root, cx));
                this.message = result.err().map(|e| (package_id, format!("{e:#}").into()));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Installing runs other people's code on this machine, so it's always confirmed first.
    fn install(
        &mut self,
        package_id: String,
        tool: ToolStatus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let version = tool
            .version
            .clone()
            .map(|v| format!(" {v}"))
            .unwrap_or_default();
        let detail = format!(
            "This will {}.\nIt goes into {}, and nothing else on your system changes.",
            tool.install_description,
            gleipnir_packages::managed_tools_dir().display()
        );
        let answer = window.prompt(
            PromptLevel::Info,
            &format!("Install {}{version}?", tool.name),
            Some(&detail),
            &["Install", "Cancel"],
            cx,
        );
        let toolchains = self.toolchains.clone();
        cx.spawn(async move |_, cx| {
            if answer.await != Ok(0) {
                return;
            }
            let task = toolchains.update(cx, |t, cx| t.install(&package_id, &tool.name, cx));
            // Failures show on the tool's row, so there's nothing more to do with the result.
            task.await.ok();
        })
        .detach();
    }

    fn render_sdk(
        &self,
        toolchain: &PackageToolchain,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (Some(kind), Some(sdk_name)) = (toolchain.sdk_kind.clone(), toolchain.sdk_name.clone())
        else {
            return div().into_any_element();
        };
        let chosen_root = toolchain.chosen.as_ref().map(|s| s.root.clone());
        let mut list = div().flex().flex_col().gap(px(2.));
        if toolchain.candidates.is_empty() {
            list = list.child(
                div()
                    .text_color(theme.warning)
                    .child(format!("No {sdk_name} found on this machine")),
            );
        }
        for (index, sdk) in toolchain.candidates.iter().enumerate() {
            let selected = chosen_root.as_ref() == Some(&sdk.root);
            let (kind, root) = (kind.clone(), sdk.root.clone());
            let source = match &sdk.source {
                gleipnir_packages::sdk::SdkSource::User => "added by you".to_string(),
                gleipnir_packages::sdk::SdkSource::EnvVar(var) => format!("from ${var}"),
                gleipnir_packages::sdk::SdkSource::Path => "on your PATH".to_string(),
                gleipnir_packages::sdk::SdkSource::Known => String::new(),
            };
            list = list.child(
                div()
                    .id(SharedString::from(format!(
                        "sdk-{}-{index}",
                        toolchain.package_id
                    )))
                    .debug_selector(|| format!("sdk-{}-{index}", toolchain.package_id))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .px(px(8.))
                    .py(px(3.))
                    .rounded(px(4.))
                    .when(selected, |r| r.bg(theme.selection))
                    .when(!selected, |r| r.hover(|r| r.bg(theme.hover)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.choose(kind.clone(), root.clone(), cx)
                    }))
                    .child(
                        div()
                            .size(px(8.))
                            .rounded(px(4.))
                            .border_1()
                            .border_color(theme.text_muted)
                            .when(selected, |d| d.bg(theme.accent).border_color(theme.accent)),
                    )
                    .child(
                        div()
                            .w(px(110.))
                            .flex_none()
                            .child(sdk.version.clone().unwrap_or_else(|| "unknown".into())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.text_muted)
                            .child(sdk.root.display().to_string()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(11.))
                            .text_color(theme.text_faint)
                            .child(source),
                    ),
            );
        }
        let package_id = toolchain.package_id.clone();
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .items_center()
                    .child(div().text_color(theme.text_muted).child(sdk_name))
                    .child(
                        div()
                            .id(SharedString::from(format!("add-sdk-{package_id}")))
                            .px(px(8.))
                            .rounded(px(4.))
                            .text_size(px(12.))
                            .text_color(theme.text_muted)
                            .hover(|b| b.bg(theme.hover).text_color(theme.text))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.add_sdk(package_id.clone(), kind.clone(), window, cx);
                            }))
                            .child("Add…"),
                    ),
            )
            .child(list)
            .into_any_element()
    }

    fn render_tool(
        &self,
        package_id: &str,
        tool: &ToolStatus,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (status, color): (String, _) = match &tool.resolution.found {
            Some((path, source)) => {
                let how = match source {
                    ToolSource::Path | ToolSource::System => source.describe().to_string(),
                    other => other.describe().to_string(),
                };
                (format!("{} · {how}", path.display()), theme.text_muted)
            }
            None if tool.installing => ("Installing…".to_string(), theme.accent),
            None => match &tool.outdated {
                Some(old) => (
                    format!("{old} is installed, but this version of Gleipnir uses a newer one"),
                    theme.warning,
                ),
                None => ("Not found".to_string(), theme.warning),
            },
        };
        let action: Option<AnyElement> = if tool.installing || tool.resolution.found.is_some() {
            None
        } else if tool.can_install {
            let (package_id, tool) = (package_id.to_string(), tool.clone());
            let label = match (&tool.version, &tool.outdated) {
                (Some(version), Some(_)) => format!("Update to {version}"),
                (Some(version), None) => format!("Install {version}"),
                (None, _) => "Install".to_string(),
            };
            Some(
                div()
                    .id(SharedString::from(format!("install-{}", tool.name)))
                    .debug_selector(|| format!("install-{}", tool.name))
                    .flex_none()
                    .px(px(8.))
                    .py(px(2.))
                    .rounded(px(4.))
                    .bg(theme.surface_raised)
                    .border_1()
                    .border_color(theme.border)
                    .hover(|b| b.bg(theme.selection))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.install(package_id.clone(), tool.clone(), window, cx);
                    }))
                    .child(label)
                    .into_any_element(),
            )
        } else {
            Some(
                div()
                    .flex_none()
                    .text_size(px(11.))
                    .text_color(theme.text_faint)
                    .child(tool.install_description.clone())
                    .into_any_element(),
            )
        };
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.))
                    .py(px(3.))
                    .child(div().w(px(200.)).flex_none().child(tool.name.clone()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(color)
                            .child(status),
                    )
                    .children(action),
            )
            .children(tool.error.clone().map(|error| {
                div()
                    .pl(px(210.))
                    .text_size(px(11.))
                    .text_color(theme.error)
                    .child(error)
            }))
            .into_any_element()
    }
}

impl Render for LanguageSettings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let toolchains = self.toolchains.read(cx);
        let detecting = toolchains.is_detecting() && toolchains.packages().is_empty();
        let packages = toolchains.packages().to_vec();

        let mut sections: Vec<AnyElement> = Vec::new();
        for toolchain in &packages {
            let message = self
                .message
                .as_ref()
                .filter(|(id, _)| *id == toolchain.package_id)
                .map(|(_, m)| m.clone());
            let tools: Vec<AnyElement> = toolchain
                .tools
                .iter()
                .map(|tool| self.render_tool(&toolchain.package_id, tool, &theme, cx))
                .collect();
            sections.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .p(px(14.))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(toolchain.name.clone()),
                    )
                    .child(self.render_sdk(toolchain, &theme, cx))
                    .children(
                        message.map(|m| div().text_size(px(12.)).text_color(theme.error).child(m)),
                    )
                    .child(div().flex().flex_col().children(tools))
                    .into_any_element(),
            );
        }

        div()
            .key_context("LanguageSettings")
            .track_focus(&self.focus_handle)
            .occlude()
            .w(px(820.))
            .max_h(px(620.))
            .flex()
            .flex_col()
            .bg(theme.surface_raised)
            .border_1()
            .border_color(theme.border)
            .rounded(px(8.))
            .shadow_lg()
            .overflow_hidden()
            .text_size(px(13.))
            .text_color(theme.text)
            .on_action(cx.listener(|_, _: &Close, _, cx| cx.emit(DismissEvent)))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .items_center()
                    .px(px(14.))
                    .py(px(10.))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Languages & SDKs"),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(theme.text_faint)
                            .child("Choices apply to this project"),
                    ),
            )
            .child(
                div()
                    .id("language-settings-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .when(detecting, |b| {
                        b.child(
                            div()
                                .p(px(20.))
                                .text_color(theme.text_muted)
                                .child("Looking for SDKs…"),
                        )
                    })
                    .children(sections),
            )
    }
}

#[cfg(test)]
mod tests;

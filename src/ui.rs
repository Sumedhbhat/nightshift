use gpui::{
    App, Bounds, Context, FocusHandle, KeyBinding, PathPromptOptions, Role, Task, TitlebarOptions,
    Window, WindowBounds, WindowOptions, actions, div, prelude::*, px, rgb, size, uniform_list,
};
use nightshift::{
    domain::Project,
    github::{self, GitHub, Issue, Remote},
    storage::{self, Store},
};
use tokio::runtime::Handle;

pub type Startup = tokio::sync::oneshot::Receiver<storage::Result<(Store, Vec<Project>)>>;
const BG: u32 = 0x18212c;
const PANEL: u32 = 0x202c39;
const LINE: u32 = 0x354352;
const TEXT: u32 = 0xe3eaf1;
const MUTED: u32 = 0x9badbf;
const ACCENT: u32 = 0x9ec5f8;

actions!(
    nightshift,
    [
        Quit,
        Close,
        AddProject,
        SearchIssues,
        ToggleFilter,
        NextFocus,
        PreviousFocus
    ]
);

struct IssueRow {
    project_id: i64,
    repository: String,
    issue: Issue,
}
struct Nightshift {
    focus: FocusHandle,
    runtime: Handle,
    store: Option<Store>,
    projects: Vec<Project>,
    selected: Option<i64>,
    issues: Vec<IssueRow>,
    remotes: Vec<Remote>,
    ready_only: bool,
    busy: bool,
    searched: bool,
    status: String,
    errors: Vec<String>,
    _startup: Task<()>,
    _picker: Option<Task<()>>,
    _save: Option<Task<()>>,
    _search: Option<Task<()>>,
}

impl Nightshift {
    fn new(window: &mut Window, runtime: Handle, startup: Startup, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let task = cx.spawn(async move |this, cx| {
            let result = startup.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(Ok((store, projects))) => {
                        this.store = Some(store);
                        this.projects = projects;
                        this.status = if this.projects.is_empty() {
                            "Add a project to get started."
                        } else {
                            "Projects loaded. Click Search issues to fetch from GitHub."
                        }
                        .into();
                    }
                    Ok(Err(error)) => this.status = format!("Could not load projects: {error}"),
                    Err(_) => {
                        this.status =
                            "Project storage did not start. Quit and reopen Nightshift.".into()
                    }
                }
                cx.notify();
            });
        });
        Self {
            focus,
            runtime,
            store: None,
            projects: vec![],
            selected: None,
            issues: vec![],
            remotes: vec![],
            ready_only: false,
            busy: true,
            searched: false,
            status: "Loading projects…".into(),
            errors: vec![],
            _startup: task,
            _picker: None,
            _save: None,
            _search: None,
        }
    }

    fn add_project(&mut self, _: &AddProject, _: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.store.is_none() {
            return;
        }
        self.busy = true;
        self.remotes.clear();
        self.status = "Choose a local Git repository linked to a public GitHub repo.".into();
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add project".into()),
        });
        let runtime = self.runtime.clone();
        self._picker = Some(cx.spawn(async move |this, cx| {
            let path = match picker.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Ok(None)) => None,
                error => {
                    let _ = this.update(cx, |this, cx| {
                        this.busy = false;
                        this.status = format!("Could not open the folder picker: {error:?}");
                        cx.notify();
                    });
                    return;
                }
            };
            let Some(path) = path else {
                let _ = this.update(cx, |this, cx| {
                    this.busy = false;
                    this.status = "No project added.".into();
                    cx.notify();
                });
                return;
            };
            let result = runtime.spawn(github::inspect_checkout(path)).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(Ok(mut remotes)) if remotes.len() == 1 => {
                        this.save_remote(remotes.remove(0), cx)
                    }
                    Ok(Ok(remotes)) => {
                        this.remotes = remotes;
                        this.status =
                            "Choose which GitHub repository this project should use.".into();
                    }
                    Ok(Err(error)) => this.status = error,
                    Err(error) => this.status = format!("Repository inspection failed: {error}"),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn save_remote(&mut self, remote: Remote, cx: &mut Context<Self>) {
        let Some(store) = self.store.clone() else {
            return;
        };
        if let Some(existing) = self
            .projects
            .iter()
            .find(|p| p.checkout_path == remote.checkout)
        {
            self.selected = Some(existing.id);
            self.remotes.clear();
            self.status = "This project is already added.".into();
            cx.notify();
            return;
        }
        self.busy = true;
        self.remotes.clear();
        self.status = format!(
            "Checking public repository {}/{}…",
            remote.owner, remote.repo
        );
        let work = self.runtime.spawn(async move {
            let project = GitHub::new()?.verify(remote).await?;
            store.add_project(project).await.map_err(|e| e.to_string())
        });
        self._save = Some(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(Ok(project)) => {
                        this.selected = Some(project.id);
                        this.status = format!(
                            "Added {}/{}. Click Search issues to fetch its open issues.",
                            project.owner, project.name
                        );
                        this.projects.push(project);
                        this.searched = false;
                    }
                    Ok(Err(error)) => this.status = error,
                    Err(error) => this.status = format!("Could not add project: {error}"),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn toggle_filter(&mut self, _: &ToggleFilter, window: &mut Window, cx: &mut Context<Self>) {
        if !self.busy {
            self.ready_only = !self.ready_only;
            self.search(&SearchIssues, window, cx);
            cx.notify();
        }
    }

    fn search(&mut self, _: &SearchIssues, _: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.projects.is_empty() {
            return;
        }
        self.busy = true;
        self.errors.clear();
        self.issues.clear();
        self.searched = false;
        self.status = "Fetching open issues from GitHub…".into();
        let projects = self.projects.clone();
        let ready = self.ready_only;
        let work = self.runtime.spawn(async move {
            let github = GitHub::new()?;
            let mut rows = Vec::new();
            let mut errors = Vec::new();
            for project in projects {
                let result = github.issues(&project, ready).await;
                let repository = format!("{}/{}", project.owner, project.name);
                if let Some(error) = result.error {
                    errors.push(format!("{repository}: {error}"));
                }
                rows.extend(result.issues.into_iter().map(|issue| IssueRow {
                    project_id: project.id,
                    repository: repository.clone(),
                    issue,
                }));
            }
            Ok::<_, String>((rows, errors))
        });
        self._search = Some(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false; this.searched = true;
                match result {
                    Ok(Ok((rows, errors))) => {
                        this.status = if errors.is_empty() { format!("Fetched {} open issues. Click an issue to open it on GitHub.", rows.len()) } else { format!("Fetched {} issues. Some results are incomplete; see the errors below.", rows.len()) };
                        this.issues = rows; this.errors = errors;
                    }
                    Ok(Err(error)) => this.status = error,
                    Err(error) => this.status = format!("Search failed: {error}"),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
}

fn button(id: &'static str, label: &'static str, enabled: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label)
        .focusable()
        .tab_stop(true)
        .px_4()
        .py_2()
        .rounded_md()
        .border_1()
        .border_color(rgb(if enabled { ACCENT } else { LINE }))
        .text_color(rgb(if enabled { ACCENT } else { MUTED }))
        .when(enabled, |style| {
            style.cursor_pointer().hover(|s| s.bg(rgb(LINE)))
        })
        .focus(|style| style.border_color(rgb(TEXT)))
        .child(label)
}

impl Render for Nightshift {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let visible: Vec<usize> = self
            .issues
            .iter()
            .enumerate()
            .filter(|(_, row)| self.selected.is_none_or(|id| id == row.project_id))
            .map(|(index, _)| index)
            .collect();
        let count = visible.len();
        let heading = self
            .projects
            .iter()
            .find(|p| Some(p.id) == self.selected)
            .map(|p| format!("{}/{}", p.owner, p.name))
            .unwrap_or_else(|| "All projects".into());
        let empty = if self.projects.is_empty() {
            "Add your first project\nChoose a local Git repository with a public github.com remote."
        } else if self.busy {
            "Fetching issues…"
        } else if !self.searched {
            "Ready to search\nClick Search issues to fetch open issues from your public repositories."
        } else if !self.errors.is_empty() {
            "No issues loaded for this project. Check the error message below and try again."
        } else if self.ready_only {
            "No open issues labeled ready-for-agent were found."
        } else {
            "No open issues were found for this project."
        };
        div()
            .id("nightshift")
            .role(Role::Application)
            .aria_label("Nightshift")
            .track_focus(&self.focus)
            .key_context("Nightshift")
            .on_action(cx.listener(Self::add_project))
            .on_action(cx.listener(Self::search))
            .on_action(cx.listener(Self::toggle_filter))
            .on_action(|_: &Close, window, _| window.remove_window())
            .on_action(|_: &NextFocus, window, cx| window.focus_next(cx))
            .on_action(|_: &PreviousFocus, window, cx| window.focus_prev(cx))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .font_family(".AppleSystemUIFont")
            .text_sm()
            .child(
                div()
                    .px_6()
                    .h_20()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(rgb(LINE))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .font_family("Avenir Next")
                                    .text_2xl()
                                    .child("Nightshift"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(MUTED))
                                    .child("Projects and public GitHub issues"),
                            ),
                    )
                    .child(
                        button(
                            "add-project",
                            "Add project",
                            !self.busy && self.store.is_some(),
                        )
                        .key_context("AddButton")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.add_project(&AddProject, window, cx)
                        })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .w(px(280.))
                            .flex_none()
                            .flex()
                            .flex_col()
                            .border_r_1()
                            .border_color(rgb(LINE))
                            .child(div().px_5().py_4().text_color(rgb(MUTED)).child("PROJECTS"))
                            .child(
                                div()
                                    .id("all-projects")
                                    .role(Role::Button)
                                    .aria_label("All projects")
                                    .px_5()
                                    .py_3()
                                    .cursor_pointer()
                                    .bg(rgb(if self.selected.is_none() { LINE } else { BG }))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.selected = None;
                                        cx.notify();
                                    }))
                                    .child(format!("All projects ({})", self.projects.len())),
                            )
                            .child(
                                div()
                                    .id("projects")
                                    .overflow_y_scroll()
                                    .flex_1()
                                    .min_h_0()
                                    .children(self.projects.iter().map(|project| {
                                        let id = project.id;
                                        div()
                                            .id(("project", id as usize))
                                            .role(Role::Button)
                                            .aria_label(format!(
                                                "{}/{}",
                                                project.owner, project.name
                                            ))
                                            .px_5()
                                            .py_3()
                                            .cursor_pointer()
                                            .bg(rgb(if self.selected == Some(id) {
                                                LINE
                                            } else {
                                                BG
                                            }))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.selected = Some(id);
                                                cx.notify();
                                            }))
                                            .child(format!("{}/{}", project.owner, project.name))
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .bg(rgb(PANEL))
                            .child(
                                div()
                                    .p_5()
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_3()
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap_1()
                                            .child(div().text_xl().child(heading))
                                            .child(
                                                div()
                                                    .text_color(rgb(MUTED))
                                                    .child(format!("{count} open issues")),
                                            ),
                                    )
                                    .child(
                                        button(
                                            "search-issues",
                                            if self.busy {
                                                "Working…"
                                            } else {
                                                "Search issues"
                                            },
                                            !self.busy && !self.projects.is_empty(),
                                        )
                                        .key_context("SearchButton")
                                        .on_click(
                                            cx.listener(|this, _, window, cx| {
                                                this.search(&SearchIssues, window, cx)
                                            }),
                                        ),
                                    ),
                            )
                            .child(
                                div().px_5().pb_4().child(
                                    button(
                                        "filter",
                                        if self.ready_only {
                                            "Filter: ready-for-agent"
                                        } else {
                                            "Filter: all open issues"
                                        },
                                        !self.busy,
                                    )
                                    .key_context("FilterButton")
                                    .on_click(cx.listener(
                                        |this, _, window, cx| {
                                            this.toggle_filter(&ToggleFilter, window, cx)
                                        },
                                    )),
                                ),
                            )
                            .children(self.remotes.iter().cloned().enumerate().map(
                                |(index, remote)| {
                                    div()
                                        .id(("remote", index))
                                        .role(Role::Button)
                                        .aria_label(format!(
                                            "Use {}: {}/{}",
                                            remote.name, remote.owner, remote.repo
                                        ))
                                        .mx_5()
                                        .p_3()
                                        .mb_2()
                                        .bg(rgb(LINE))
                                        .cursor_pointer()
                                        .child(format!(
                                            "Use {} → {}/{}",
                                            remote.name, remote.owner, remote.repo
                                        ))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if !this.busy {
                                                this.save_remote(remote.clone(), cx);
                                            }
                                        }))
                                },
                            ))
                            .child(if count == 0 {
                                div()
                                    .flex_1()
                                    .p_6()
                                    .text_color(rgb(MUTED))
                                    .child(empty)
                                    .into_any_element()
                            } else {
                                uniform_list(
                                    "issues",
                                    count,
                                    cx.processor(
                                        move |this, range: std::ops::Range<usize>, _, _| {
                                            range
                                                .map(|index| {
                                                    let row = &this.issues[visible[index]];
                                                    let url = row.issue.url.clone();
                                                    div()
                                                        .id(index)
                                                        .w_full()
                                                        .h(px(88.))
                                                        .px_5()
                                                        .flex()
                                                        .flex_col()
                                                        .justify_center()
                                                        .gap_1()
                                                        .border_t_1()
                                                        .border_color(rgb(LINE))
                                                        .cursor_pointer()
                                                        .hover(|style| style.bg(rgb(LINE)))
                                                        .role(Role::Link)
                                                        .aria_label(format!(
                                                            "{} #{} {}",
                                                            row.repository,
                                                            row.issue.number,
                                                            row.issue.title
                                                        ))
                                                        .on_click(move |_, _, cx| cx.open_url(&url))
                                                        .child(
                                                            div()
                                                                .text_xs()
                                                                .text_color(rgb(MUTED))
                                                                .child(format!(
                                                                    "{} · #{}",
                                                                    row.repository,
                                                                    row.issue.number
                                                                )),
                                                        )
                                                        .child(
                                                            div()
                                                                .truncate()
                                                                .child(row.issue.title.clone()),
                                                        )
                                                        .child(
                                                            div()
                                                                .text_xs()
                                                                .text_color(rgb(ACCENT))
                                                                .truncate()
                                                                .child(
                                                                    row.issue.labels.join(" · "),
                                                                ),
                                                        )
                                                })
                                                .collect()
                                        },
                                    ),
                                )
                                .w_full()
                                .flex_1()
                                .min_h_0()
                                .into_any_element()
                            })
                            .children(self.errors.iter().map(|error| {
                                div()
                                    .px_5()
                                    .py_2()
                                    .text_color(rgb(0xf2b8a5))
                                    .child(error.clone())
                            })),
                    ),
            )
            .child(
                div()
                    .id("status")
                    .role(Role::Status)
                    .aria_label(self.status.clone())
                    .px_5()
                    .py_3()
                    .flex_none()
                    .border_t_1()
                    .border_color(rgb(LINE))
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(self.status.clone()),
            )
    }
}

pub fn run(runtime: Handle, startup: Startup) {
    gpui_platform::application().run(move |cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("enter", ToggleFilter, Some("FilterButton")),
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-w", Close, Some("Nightshift")),
            KeyBinding::new("cmd-o", AddProject, Some("Nightshift")),
            KeyBinding::new("cmd-r", SearchIssues, Some("Nightshift")),
            KeyBinding::new("enter", AddProject, Some("AddButton")),
            KeyBinding::new("enter", SearchIssues, Some("SearchButton")),
            KeyBinding::new("tab", NextFocus, Some("Nightshift")),
            KeyBinding::new("shift-tab", PreviousFocus, Some("Nightshift")),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1080.), px(720.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(840.), px(520.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Nightshift".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| Nightshift::new(window, runtime, startup, cx)),
        )
        .expect("open Nightshift window");
        cx.activate(true);
    });
}

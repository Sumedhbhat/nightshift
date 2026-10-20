mod perf_view;
mod text_input;

pub use perf_view::{PerfConfig, run as run_perf};

use gpui::{
    App, Bounds, Context, FocusHandle, Focusable, KeyBinding, PathPromptOptions, Role, Task,
    TitlebarOptions, Window, WindowBounds, WindowOptions, actions, div, prelude::*, px, rgb, size,
    uniform_list,
};
use nightshift::{
    domain::Project,
    github::{self, GitHub, Issue, Remote},
    storage::{self, Store},
};
use std::collections::HashMap;
use text_input::TextInput;
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
        LoadMore,
        FocusSearch,
        FocusResults,
        NextFocus,
        PreviousFocus,
        Activate,
        NextIssue,
        PreviousIssue,
        OpenIssue
    ]
);

#[derive(Clone, PartialEq, gpui::Action)]
#[action(no_json)]
struct SelectProject(usize);

/// Label for GPUI's `secondary` modifier: Command on macOS, Control elsewhere.
const MODIFIER: &str = if cfg!(target_os = "macos") {
    "⌘"
} else {
    "Ctrl+"
};
const MODIFIER_NAME: &str = if cfg!(target_os = "macos") {
    "Command"
} else {
    "Control"
};

fn project_shortcut(index: usize) -> Option<String> {
    (index < 9).then(|| format!("{MODIFIER}{}", index + 1))
}

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
    active_issue: Option<usize>,
    issue_scroll: gpui::UniformListScrollHandle,
    search_input: gpui::Entity<TextInput>,
    query_text: String,
    query: Result<nightshift::issue_query::IssueQuery, String>,
    _input_subscription: gpui::Subscription,
    busy: bool,
    pages: HashMap<i64, Option<String>>,
    status: String,
    errors: Vec<String>,
    _startup: Task<()>,
    _picker: Option<Task<()>>,
    _save: Option<Task<()>>,
    _search: Option<Task<()>>,
    startup_report: Option<StartupReport>,
}

/// Written when `NIGHTSHIFT_STARTUP_REPORT` names a file; the app then quits after its first
/// frame and project load. Used by `scripts/perf.sh` for repeated launch measurements.
struct StartupReport {
    path: std::path::PathBuf,
    main_started: std::time::Instant,
    first_render_ms: Option<f64>,
    first_frame_ms: Option<f64>,
    first_frame_unix_ms: Option<u128>,
    projects_loaded_ms: Option<f64>,
    written: bool,
}

impl Nightshift {
    fn new(
        window: &mut Window,
        runtime: Handle,
        startup: Startup,
        main_started: std::time::Instant,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let search_input = cx.new(TextInput::new);
        let input_subscription = cx.observe(&search_input, |this, input, cx| {
            let text = input.read(cx).text();
            if this.query_text != text {
                this.query_text = text.to_owned();
                this.query = nightshift::issue_query::IssueQuery::parse(text);
                this.active_issue = None;
                this.issue_scroll
                    .scroll_to_item(0, gpui::ScrollStrategy::Top);
            }
            cx.notify();
        });
        cx.on_focus(&search_input.focus_handle(cx), window, |_, _, cx| {
            cx.notify()
        })
        .detach();
        cx.on_blur(&search_input.focus_handle(cx), window, |_, _, cx| {
            cx.notify()
        })
        .detach();
        let task = cx.spawn(async move |this, cx| {
            let result = startup.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                if let Some(report) = &mut this.startup_report {
                    report.projects_loaded_ms =
                        Some(nightshift::perf::elapsed_ms(report.main_started));
                }
                match result {
                    Ok(Ok((store, projects))) => {
                        this.store = Some(store);
                        this.projects = projects;
                        this.status = if this.projects.is_empty() {
                            "Add a project to get started."
                        } else {
                            "Projects loaded. Press / to search, then Enter to fetch from GitHub."
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
            active_issue: None,
            issue_scroll: gpui::UniformListScrollHandle::new(),
            search_input,
            query_text: String::new(),
            query: Ok(Default::default()),
            _input_subscription: input_subscription,
            busy: true,
            pages: HashMap::new(),
            status: "Loading projects…".into(),
            errors: vec![],
            _startup: task,
            _picker: None,
            _save: None,
            _search: None,
            startup_report: std::env::var_os("NIGHTSHIFT_STARTUP_REPORT").map(|path| {
                StartupReport {
                    path: path.into(),
                    main_started,
                    first_render_ms: None,
                    first_frame_ms: None,
                    first_frame_unix_ms: None,
                    projects_loaded_ms: None,
                    written: false,
                }
            }),
        }
    }

    fn record_startup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(report) = &mut self.startup_report else {
            return;
        };
        if report.first_render_ms.is_none() {
            report.first_render_ms = Some(nightshift::perf::elapsed_ms(report.main_started));
            let entity = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| {
                let _ = entity.update(cx, |this, cx| {
                    if let Some(report) = &mut this.startup_report {
                        report.first_frame_ms =
                            Some(nightshift::perf::elapsed_ms(report.main_started));
                        report.first_frame_unix_ms =
                            Some(nightshift::perf::unix_ms(std::time::SystemTime::now()));
                    }
                    cx.notify();
                });
            });
        }
        if report.written || report.first_frame_ms.is_none() || report.projects_loaded_ms.is_none()
        {
            return;
        }
        report.written = true;
        let json = serde_json::json!({
            "version": nightshift::VERSION,
            "os": std::env::consts::OS,
            "main_to_first_render_ms": report.first_render_ms,
            "main_to_first_frame_ms": report.first_frame_ms,
            "main_to_projects_loaded_ms": report.projects_loaded_ms,
            "first_frame_unix_ms": report.first_frame_unix_ms,
        });
        if let Err(error) = std::fs::write(&report.path, json.to_string()) {
            eprintln!("nightshift: could not write startup report: {error}");
        }
        cx.defer(|cx| cx.quit());
    }

    fn select_project(
        &mut self,
        action: &SelectProject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let selected = if action.0 == 0 {
            None
        } else if let Some(project) = self.projects.get(action.0 - 1) {
            Some(project.id)
        } else {
            return;
        };
        self.selected = selected;
        self.focus.focus(window, cx);
        self.active_issue = None;
        self.issue_scroll
            .scroll_to_item(0, gpui::ScrollStrategy::Top);
        cx.notify();
    }

    fn move_issue(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let visible = self.visible_issues();
        if visible.is_empty() {
            return;
        }
        let current = visible
            .iter()
            .position(|index| Some(*index) == self.active_issue);
        let next = match current {
            Some(index) if forward => (index + 1).min(visible.len() - 1),
            Some(index) => index.saturating_sub(1),
            None if forward => 0,
            None => visible.len() - 1,
        };
        self.focus.focus(window, cx);
        self.active_issue = Some(visible[next]);
        self.issue_scroll
            .scroll_to_item(next, gpui::ScrollStrategy::Center);
        cx.notify();
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
            self.active_issue = None;
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
                        this.active_issue = None;
                        this.status = format!(
                            "Added {}/{}. Press {MODIFIER}R to fetch open issues.",
                            project.owner, project.name
                        );
                        this.projects.push(project);
                    }
                    Ok(Err(error)) => this.status = error,
                    Err(error) => this.status = format!("Could not add project: {error}"),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn visible_issues(&self) -> Vec<usize> {
        let Ok(query) = &self.query else {
            return vec![];
        };
        self.issues
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                self.selected.is_none_or(|id| id == row.project_id)
                    && query.matches(&row.repository, &row.issue)
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn search(&mut self, _: &SearchIssues, _: &mut Window, cx: &mut Context<Self>) {
        self.fetch_issues(false, cx);
    }

    fn load_more(&mut self, _: &LoadMore, _: &mut Window, cx: &mut Context<Self>) {
        self.fetch_issues(true, cx);
    }

    fn fetch_issues(&mut self, append: bool, cx: &mut Context<Self>) {
        if self.busy || self.query.is_err() {
            return;
        }
        let Some(project) = self
            .projects
            .iter()
            .find(|project| Some(project.id) == self.selected)
            .cloned()
        else {
            self.status = "Select a project to fetch its issues. All projects shows issues already fetched this session.".into();
            cx.notify();
            return;
        };
        let cursor = if append {
            let Some(cursor) = self.pages.get(&project.id).and_then(Clone::clone) else {
                return;
            };
            Some(cursor)
        } else {
            None
        };
        self.busy = true;
        self.errors.clear();
        self.active_issue = None;
        if !append {
            self.issues.retain(|row| row.project_id != project.id);
            self.pages.remove(&project.id);
        }
        let repository = format!("{}/{}", project.owner, project.name);
        self.status = if append {
            format!("Loading 10 more open issues for {repository}…")
        } else {
            format!("Fetching the first 10 open issues for {repository}…")
        };
        let project_id = project.id;
        let work = self.runtime.spawn(async move {
            let github = GitHub::new()?;
            Ok::<_, String>(github.issue_page(&project, false, cursor.as_deref()).await)
        });
        self._search = Some(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(Ok(page)) => {
                        if let Some(error) = page.error {
                            this.status = format!("Could not fetch issues for {repository}.");
                            this.errors.push(format!("{repository}: {error}"));
                        } else {
                            let received = page.issues.len();
                            for issue in page.issues {
                                if !this.issues.iter().any(|row| {
                                    row.project_id == project_id && row.issue.number == issue.number
                                }) {
                                    this.issues.push(IssueRow {
                                        project_id,
                                        repository: repository.clone(),
                                        issue,
                                    });
                                }
                            }
                            let has_more = page.next.is_some();
                            this.pages.insert(project_id, page.next);
                            let cached = this
                                .issues
                                .iter()
                                .filter(|row| row.project_id == project_id)
                                .count();
                            this.status = format!(
                                "{} {received} open issues for {repository}; {cached} cached{}.",
                                if append { "Loaded" } else { "Fetched" },
                                if has_more { " · more available" } else { "" }
                            );
                        }
                    }
                    Ok(Err(error)) => this.status = error,
                    Err(error) => this.status = format!("Issue fetch failed: {error}"),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
}

fn button(
    id: &'static str,
    label: impl Into<gpui::SharedString>,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    let label = label.into();
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label.clone())
        .focusable()
        .tab_stop(true)
        .px_4()
        .py_2()
        .rounded_md()
        .border_1()
        .border_color(rgb(LINE))
        .text_color(rgb(if enabled { TEXT } else { MUTED }))
        .when(enabled, |style| {
            style.cursor_pointer().hover(|s| s.bg(rgb(LINE)))
        })
        .focus(|style| style.border_color(rgb(ACCENT)))
        .child(label)
}

impl Render for Nightshift {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.record_startup(window, cx);
        let visible = self.visible_issues();
        let count = visible.len();
        let scope_fetched = self.selected.map_or(self.issues.len(), |id| {
            self.issues
                .iter()
                .filter(|row| row.project_id == id)
                .count()
        });
        let scope_searched = self
            .selected
            .map_or(!self.pages.is_empty(), |id| self.pages.contains_key(&id));
        let has_more = self
            .selected
            .is_some_and(|id| self.pages.get(&id).is_some_and(Option::is_some));
        let heading = self
            .projects
            .iter()
            .find(|p| Some(p.id) == self.selected)
            .map(|p| format!("{}/{}", p.owner, p.name))
            .unwrap_or_else(|| "All projects".into());
        let searching = self.search_input.focus_handle(cx).is_focused(window);
        let empty: String = if self.projects.is_empty() {
            format!("Add a local project with {MODIFIER}O to get started.")
        } else if self.query.is_err() {
            "Correct the search above to see matching issues.".into()
        } else if self.busy {
            "Fetching open issues from GitHub…".into()
        } else if self.selected.is_none() && self.issues.is_empty() {
            "Select a project to fetch its first 10 open issues.".into()
        } else if !scope_searched {
            format!(
                "Press Enter in search or {MODIFIER}R to fetch this project's first 10 open issues."
            )
        } else if !self.errors.is_empty() {
            format!(
                "No matching issues loaded. Review the errors below, then refresh with {MODIFIER}R."
            )
        } else {
            format!(
                "No matching open issues. Edit the search or use {MODIFIER}0 to check all projects."
            )
        };
        div().id("nightshift").role(Role::Application).aria_label("Nightshift")
            .track_focus(&self.focus).key_context("Nightshift")
            .on_action(cx.listener(Self::add_project))
            .on_action(cx.listener(Self::search))
            .on_action(cx.listener(Self::load_more))
            .on_action(cx.listener(Self::select_project))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                this.search_input.focus_handle(cx).focus(window, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &FocusResults, window, cx| {
                this.focus.focus(window, cx);
                if this.active_issue.is_none() { this.move_issue(true, window, cx); }
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &NextIssue, window, cx| this.move_issue(true, window, cx)))
            .on_action(cx.listener(|this, _: &PreviousIssue, window, cx| this.move_issue(false, window, cx)))
            .on_action(cx.listener(|this, _: &OpenIssue, _, cx| {
                if let Some(row) = this.active_issue.and_then(|index| this.issues.get(index)) {
                    cx.open_url(&row.issue.url);
                }
            }))
            .on_action(|_: &Close, window, _| window.remove_window())
            .on_action(|_: &NextFocus, window, cx| window.focus_next(cx))
            .on_action(|_: &PreviousFocus, window, cx| window.focus_prev(cx))
            .size_full().flex().flex_col().bg(rgb(BG)).text_color(rgb(TEXT))
            .font_family(".AppleSystemUIFont").text_sm()
            .child(div().flex().flex_1().min_h_0()
                .child(div().w(px(248.)).flex_none().flex().flex_col().border_r_1().border_color(rgb(LINE))
                    .child(div().px_5().py_5().flex().items_baseline().gap_2()
                        .child(div().font_family("Avenir Next").text_xl().child("Nightshift"))
                        .child(div().text_xs().text_color(rgb(MUTED)).child(nightshift::VERSION)))
                    .child(div().px_5().pb_3().text_xs().text_color(rgb(MUTED)).child("PROJECTS"))
                    .child(div().id("all-projects").role(Role::Button).aria_label(format!("All projects, {MODIFIER_NAME} 0"))
                        .focusable().tab_stop(true).key_context("Selectable")
                        .border_l_2().border_color(rgb(if self.selected.is_none() { ACCENT } else { BG }))
                        .focus(|s| s.border_color(rgb(TEXT)))
                        .px_4().py_3().flex().items_center().justify_between().cursor_pointer()
                        .bg(rgb(if self.selected.is_none() { LINE } else { BG }))
                        .on_action(cx.listener(|this, _: &Activate, window, cx| this.select_project(&SelectProject(0), window, cx)))
                        .on_click(cx.listener(|this, _, window, cx| this.select_project(&SelectProject(0), window, cx)))
                        .child("All projects").child(shortcut(format!("{MODIFIER}0"))))
                    .child(div().id("projects").overflow_y_scroll().flex_1().min_h_0()
                        .children(self.projects.iter().enumerate().map(|(index, project)| {
                            let selected = self.selected == Some(project.id);
                            div().id(("project", project.id as usize)).role(Role::Button)
                                .aria_label(format!("{}/{} {}", project.owner, project.name, project_shortcut(index).unwrap_or_default()))
                                .focusable().tab_stop(true).key_context("Selectable")
                                .border_l_2().border_color(rgb(if selected { ACCENT } else { BG }))
                                .focus(|s| s.border_color(rgb(TEXT)))
                                .px_4().py_3().flex().items_center().justify_between().gap_2().cursor_pointer()
                                .bg(rgb(if selected { LINE } else { BG }))
                                .on_action(cx.listener(move |this, _: &Activate, window, cx| this.select_project(&SelectProject(index + 1), window, cx)))
                                .on_click(cx.listener(move |this, _, window, cx| this.select_project(&SelectProject(index + 1), window, cx)))
                                .child(div().min_w_0().flex_1().flex().flex_col()
                                    .child(div().truncate().child(project.name.clone()))
                                    .child(div().truncate().text_xs().text_color(rgb(MUTED)).child(project.owner.clone())))
                                .children(project_shortcut(index).map(shortcut))
                        })))
                    .child(div().p_4().child(button("add-project", format!("Add project  {MODIFIER}O"), !self.busy && self.store.is_some())
                        .w_full().key_context("AddButton")
                        .on_click(cx.listener(|this, _, window, cx| this.add_project(&AddProject, window, cx))))))
                .child(div().flex_1().min_w_0().flex().flex_col().bg(rgb(PANEL))
                    .child(div().p_5().flex_none().flex().flex_col().gap_3()
                        .child(div().flex().items_center().justify_between().gap_3()
                            .child(div().min_w_0().truncate().text_lg().child(heading))
                            .child(div().flex_none().text_xs().text_color(rgb(MUTED)).child("OPEN ISSUES")))
                        .child(div().flex().items_center().gap_2()
                            .child(div().flex_1().min_w_0().child(self.search_input.clone()))
                            .children(has_more.then(|| button("load-more", "Load more", !self.busy)
                                .key_context("LoadMoreButton")
                                .on_click(cx.listener(|this, _, window, cx| this.load_more(&LoadMore, window, cx)))))
                            .child(button("refresh", if self.busy { "Fetching…".into() } else { format!("Refresh  {MODIFIER}R") }, !self.busy && self.selected.is_some())
                                .key_context("SearchButton")
                                .on_click(cx.listener(|this, _, window, cx| this.search(&SearchIssues, window, cx)))))
                        .child(div().text_xs().text_color(rgb(MUTED))
                            .child("Title text  ·  label:ready-for-agent  ·  -label:bug  ·  repo:owner/name  ·  label:\"help wanted\""))
                        .children(self.query.as_ref().err().map(|error| div().text_color(rgb(0xf2b8a5)).child(error.clone())))
                        .child(div().flex().justify_between().gap_2().text_xs().text_color(rgb(MUTED))
                            .child(format!("{count} matches · {scope_fetched} fetched"))
                            .child(if searching { "Enter refreshes · Esc or ↓ goes to results" } else { "/ to search · ↑ ↓ or j k to browse · Enter to open" })))
                    .children(self.remotes.iter().cloned().enumerate().map(|(index, remote)| {
                        let keyboard_remote = remote.clone();
                        div().id(("remote", index)).role(Role::Button)
                            .aria_label(format!("Use {}: {}/{}", remote.name, remote.owner, remote.repo))
                            .focusable().tab_stop(true).key_context("Selectable")
                            .border_1().border_color(rgb(LINE)).focus(|s| s.border_color(rgb(ACCENT)))
                            .mx_5().p_3().mb_2().cursor_pointer()
                            .child(format!("Use {} → {}/{}", remote.name, remote.owner, remote.repo))
                            .on_action(cx.listener(move |this, _: &Activate, _, cx| {
                                if !this.busy { this.save_remote(keyboard_remote.clone(), cx); }
                            }))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.busy { this.save_remote(remote.clone(), cx); }
                            }))
                    }))
                    .child(if count == 0 {
                        div().flex_1().p_6().text_color(rgb(MUTED)).child(empty).into_any_element()
                    } else {
                        uniform_list("issues", count, cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                            range.map(|index| {
                                let issue_index = visible[index];
                                let row = &this.issues[issue_index];
                                let url = row.issue.url.clone();
                                let keyboard_url = url.clone();
                                let selected = this.active_issue == Some(issue_index);
                                div().id(("issue", issue_index)).role(Role::Link)
                                    .aria_label(format!("{} #{} {}", row.repository, row.issue.number, row.issue.title))
                                    .focusable().tab_stop(true).key_context("Selectable")
                                    .border_l_2().border_color(rgb(if selected { ACCENT } else { PANEL }))
                                    .focus(|s| s.border_color(rgb(TEXT)).bg(rgb(LINE)))
                                    .bg(rgb(if selected { LINE } else { PANEL }))
                                    .w_full().h(px(76.)).px_5().flex().flex_col().justify_center().gap_1()
                                    .cursor_pointer().hover(|s| s.bg(rgb(LINE)))
                                    .on_action(move |_: &Activate, _, cx| cx.open_url(&keyboard_url))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.active_issue = Some(issue_index);
                                        this.focus.focus(window, cx);
                                        cx.open_url(&url); cx.notify();
                                    }))
                                    .child(div().flex().items_center().gap_3()
                                        .child(div().flex_none().text_xs().text_color(rgb(MUTED)).child(format!("#{}", row.issue.number)))
                                        .child(div().min_w_0().truncate().child(row.issue.title.clone())))
                                    .child(div().truncate().text_xs().text_color(rgb(MUTED))
                                        .child(format!("{}  ·  {}", row.repository, row.issue.labels.join(" · "))))
                            }).collect()
                        })).track_scroll(&self.issue_scroll).w_full().flex_1().min_h_0().into_any_element()
                    })
                    .children(self.errors.iter().map(|error| div().px_5().py_2().text_color(rgb(0xf2b8a5)).child(error.clone())))))
            .child(div().id("status").role(Role::Status).aria_label(self.status.clone())
                .px_5().py_2().flex_none().border_t_1().border_color(rgb(LINE)).text_xs().text_color(rgb(MUTED))
                .flex().justify_between().gap_3()
                .child(div().flex_1().min_w_0().truncate().child(self.status.clone()))
                .child(div().flex_none().child(format!("/ Search   {MODIFIER}0 All   {MODIFIER}1–9 Project   Tab Focus"))))
    }
}

fn shortcut(label: impl Into<gpui::SharedString>) -> impl IntoElement {
    div()
        .flex_none()
        .font_family("Menlo")
        .text_xs()
        .text_color(rgb(MUTED))
        .child(label.into())
}

pub fn run(runtime: Handle, startup: Startup, main_started: std::time::Instant) {
    gpui_platform::application().run(move |cx: &mut App| {
        for number in 0..=9 {
            cx.bind_keys([KeyBinding::new(
                &format!("secondary-{number}"),
                SelectProject(number),
                Some("Nightshift"),
            )]);
        }
        cx.bind_keys([
            KeyBinding::new("enter", Activate, Some("Selectable")),
            KeyBinding::new("space", Activate, Some("Selectable")),
            KeyBinding::new("j", NextIssue, Some("Nightshift")),
            KeyBinding::new("k", PreviousIssue, Some("Nightshift")),
            KeyBinding::new("enter", OpenIssue, Some("Nightshift")),
            KeyBinding::new("secondary-f", FocusSearch, Some("Nightshift")),
            KeyBinding::new("escape", FocusResults, Some("Nightshift")),
            KeyBinding::new("down", NextIssue, Some("Nightshift")),
            KeyBinding::new("up", PreviousIssue, Some("Nightshift")),
            KeyBinding::new("down", FocusResults, Some("TextInput")),
            KeyBinding::new("up", gpui::NoAction, Some("TextInput")),
            KeyBinding::new("enter", SearchIssues, Some("TextInput")),
            KeyBinding::new("secondary-q", Quit, None),
            KeyBinding::new("secondary-w", Close, Some("Nightshift")),
            KeyBinding::new("secondary-o", AddProject, Some("Nightshift")),
            KeyBinding::new("secondary-r", SearchIssues, Some("Nightshift")),
            KeyBinding::new("/", FocusSearch, Some("Nightshift")),
            KeyBinding::new("enter", AddProject, Some("AddButton")),
            KeyBinding::new("enter", SearchIssues, Some("SearchButton")),
            KeyBinding::new("enter", LoadMore, Some("LoadMoreButton")),
            KeyBinding::new("tab", NextFocus, Some("Nightshift")),
            KeyBinding::new("shift-tab", PreviousFocus, Some("Nightshift")),
        ]);
        text_input::bind_keys(cx);
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
                // Matches the desktop entry name so Linux launchers show the app icon.
                app_id: Some("dev.nightshift.app".into()),
                titlebar: Some(TitlebarOptions {
                    title: Some("Nightshift".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| Nightshift::new(window, runtime, startup, main_started, cx)),
        )
        .expect("open Nightshift window");
        cx.activate(true);
    });
}

mod executions;
mod issues;
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
    update::{self, Updater},
};
use std::collections::HashMap;
use text_input::TextInput;
use tokio::runtime::Handle;

/// What startup hands the window once storage and the orchestrator are running.
pub struct Started {
    pub store: Store,
    pub projects: Vec<Project>,
    pub run: nightshift::application::run::Handle,
    pub updates: tokio::sync::mpsc::Receiver<nightshift::application::run::Update>,
    pub environment_warning: Option<String>,
}

pub type Startup = tokio::sync::oneshot::Receiver<storage::Result<Started>>;
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
        OpenIssue,
        ShowExecutions,
        ToggleRun,
        RetryWorking,
        RecheckPublication,
        DecreaseLimit,
        IncreaseLimit,
        FocusSetup,
        InstallUpdate,
        LeaveInput,
        NewIssue,
        SubmitIssue,
        EditIssue,
        CloseIssue,
        CloseNotPlanned,
        ToggleReady,
        ToggleAssign,
        FocusComment,
        OpenOnGitHub
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
    executions: executions::Executions,
    setup_input: gpui::Entity<TextInput>,
    editing_setup: bool,
    _updates: Option<Task<()>>,
    _output_poll: Option<Task<()>>,
    update: UpdateState,
    issue: issues::IssueUi,
}

/// A release downloaded in the background, installed when the owner restarts.
#[derive(Default)]
struct UpdateState {
    updater: Option<Updater>,
    ready: Option<update::Prepared>,
    installing: bool,
    /// Set after warning that restarting stops active work; the next request restarts.
    confirmed: bool,
    _check: Option<Task<()>>,
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
        let setup_input = cx.new(|cx| {
            TextInput::new(cx)
                .with_placeholder("Optional setup command run in each new worktree, e.g. npm ci")
        });
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
                    Ok(Ok(started)) => {
                        this.store = Some(started.store);
                        this.projects = started.projects;
                        this.listen(started.run, started.updates, cx);
                        this.watch_for_updates(cx);
                        this.status = if this.projects.is_empty() {
                            "Add a project to get started."
                        } else {
                            "Ready"
                        }
                        .into();
                        if let Some(warning) = started.environment_warning {
                            this.status.push_str(&format!(" {warning}"));
                        }
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
            executions: Default::default(),
            setup_input,
            editing_setup: false,
            _updates: None,
            _output_poll: None,
            update: Default::default(),
            issue: issues::IssueUi::new(cx),
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

    /// Checks for a newer release now and then hourly, until one is downloaded and ready.
    fn watch_for_updates(&mut self, cx: &mut Context<Self>) {
        let Some(updater) = Updater::detect() else {
            return;
        };
        self.update.updater = Some(updater.clone());
        let runtime = self.runtime.clone();
        self.update._check = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(update::FIRST_CHECK_DELAY)
                .await;
            loop {
                let check = updater.clone();
                match runtime
                    .spawn(async move { check.prepare_newest().await })
                    .await
                {
                    Ok(Ok(Some(prepared))) => {
                        let _ = this.update(cx, |this, cx| {
                            this.status = format!(
                                "Nightshift {} is ready. Press {MODIFIER}⇧U to restart and update.",
                                prepared.version
                            );
                            this.update.ready = Some(prepared);
                            cx.notify();
                        });
                        return;
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => eprintln!("nightshift: update check failed: {error}"),
                    Err(error) => eprintln!("nightshift: update check stopped: {error}"),
                }
                cx.background_executor().timer(update::CHECK_INTERVAL).await;
            }
        }));
    }

    fn install_update(&mut self, cx: &mut Context<Self>) {
        let (Some(updater), Some(prepared)) =
            (self.update.updater.clone(), self.update.ready.clone())
        else {
            return;
        };
        if self.update.installing {
            return;
        }
        let active = self
            .executions
            .list
            .iter()
            .filter(|e| e.holds_slot())
            .count();
        let run_active = self.executions.status.owner.is_some();
        if (active > 0 || run_active) && !self.update.confirmed {
            self.update.confirmed = true;
            self.status = format!(
                "Restarting ends the active run and interrupts {active} active execution{}. Press {MODIFIER}⇧U again to restart and update.",
                if active == 1 { "" } else { "s" }
            );
            cx.notify();
            return;
        }
        self.update.confirmed = false;
        self.update.installing = true;
        self.status = if cfg!(target_os = "linux") {
            format!(
                "Installing Nightshift {}. Enter your password if asked.",
                prepared.version
            )
        } else {
            format!("Installing Nightshift {}…", prepared.version)
        };
        let work = self
            .runtime
            .spawn(async move { updater.install(&prepared).await });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.update.installing = false;
                match result {
                    // main relaunches the new version after storage closes.
                    Ok(Ok(())) => cx.quit(),
                    Ok(Err(error)) => {
                        this.status = format!("Could not install the update: {error}")
                    }
                    Err(error) => this.status = format!("Could not install the update: {error}"),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
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
        self.editing_setup = false;
        self.close_issue_panels();
        self.executions.view = executions::View::Issues;
        let setup = self
            .projects
            .iter()
            .find(|p| Some(p.id) == selected)
            .and_then(|p| p.setup_command.clone())
            .unwrap_or_default();
        self.setup_input
            .update(cx, |input, cx| input.set_text(&setup, cx));
        self.focus.focus(window, cx);
        self.active_issue = None;
        self.issue_scroll
            .scroll_to_item(0, gpui::ScrollStrategy::Top);
        cx.notify();
    }

    fn move_issue(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.executions.view == executions::View::Executions {
            self.focus.focus(window, cx);
            self.move_execution(forward, cx);
            return;
        }
        if self.issue.compose.is_some() {
            return;
        }
        if self.issue.viewing.is_some() {
            self.scroll_issue(forward, cx);
            return;
        }
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
        self.close_issue_panels();
        self.status = "Choose a local Git repository with a GitHub remote.".into();
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
        self.status = format!("Checking {}/{} with GitHub CLI…", remote.owner, remote.repo);
        let work = self.runtime.spawn(async move {
            let project = GitHub::from_env().verify(remote).await?;
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
                        this.status = format!("Added {}/{}", project.owner, project.name);
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

    fn edit_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = self.projects.iter().find(|p| Some(p.id) == self.selected) else {
            return;
        };
        let setup = project.setup_command.clone().unwrap_or_default();
        self.setup_input
            .update(cx, |input, cx| input.set_text(&setup, cx));
        self.editing_setup = true;
        self.executions.view = executions::View::Issues;
        self.setup_input.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn save_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(store), Some(id)) = (self.store.clone(), self.selected) else {
            return;
        };
        let text = self.setup_input.read(cx).text().trim().to_owned();
        let command = (!text.is_empty()).then_some(text);
        let saved = command.clone();
        let work = self
            .runtime
            .spawn(async move { store.set_setup_command(id, saved).await });
        self.focus.focus(window, cx);
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(Ok(())) => {
                        if let Some(project) = this.projects.iter_mut().find(|p| p.id == id) {
                            project.setup_command = command.clone();
                        }
                        if this.selected == Some(id) {
                            this.editing_setup = false;
                        }
                        this.status = match command {
                            Some(_) => "Setup command saved".into(),
                            None => "Setup command cleared".into(),
                        };
                    }
                    Ok(Err(error)) => {
                        this.status = format!("Could not save the setup command: {error}")
                    }
                    Err(error) => {
                        this.status = format!("Could not save the setup command: {error}")
                    }
                }
                cx.notify();
            });
        })
        .detach();
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

    fn search(&mut self, _: &SearchIssues, window: &mut Window, cx: &mut Context<Self>) {
        // Enter in the setup field saves it; Enter in search or Cmd-R refreshes issues.
        if self.setup_input.focus_handle(cx).is_focused(window) {
            self.save_setup(window, cx);
            return;
        }
        if self.executions.view == executions::View::Executions {
            self.refresh_run(cx);
            return;
        }
        // Enter in the issue title moves to the description; Cmd-R reloads the open issue.
        if self.issue.compose.is_some() {
            if self.issue.title.focus_handle(cx).is_focused(window) {
                self.issue.body.focus_handle(cx).focus(window, cx);
            }
            return;
        }
        if self.issue.viewing.is_some() {
            self.load_issue(cx);
            return;
        }
        self.fetch_issues(false, cx);
    }

    fn load_more(&mut self, _: &LoadMore, _: &mut Window, cx: &mut Context<Self>) {
        self.fetch_issues(true, cx);
    }

    fn render_load_more(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // The scrollable footer uses the same height as an issue for uniform_list.
        div().h(px(76.)).px_5().flex().items_center().child(
            button("load-more", "Load more", !self.busy)
                .key_context("LoadMoreButton")
                .on_click(cx.listener(|this, _, window, cx| this.load_more(&LoadMore, window, cx))),
        )
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
            self.status = "Select a project to refresh its issues.".into();
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
            "Loading more issues…".into()
        } else {
            "Fetching issues…".into()
        };
        let project_id = project.id;
        let work = self.runtime.spawn(async move {
            let github = GitHub::from_env();
            Ok::<_, String>(github.issue_page(&project, cursor.as_deref()).await)
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
                            this.pages.insert(project_id, page.next);
                            let cached = this
                                .issues
                                .iter()
                                .filter(|row| row.project_id == project_id)
                                .count();
                            this.status = format!("{cached} issues loaded");
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
        let in_executions = self.executions.view == executions::View::Executions;
        let run_bar = self.render_run_bar(cx).into_any_element();
        let executions_view = in_executions.then(|| self.render_executions(cx));
        let issue_panel =
            (!in_executions && self.issue.open()).then(|| self.render_issue_panel(cx));
        let in_list = !in_executions && issue_panel.is_none();
        let setup_row = (self.selected.is_some() && self.editing_setup).then(|| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_xs()
                .child(div().flex_none().text_color(rgb(MUTED)).child("Setup"))
                .child(div().flex_1().min_w_0().child(self.setup_input.clone()))
                .child(
                    div()
                        .flex_none()
                        .text_color(rgb(MUTED))
                        .child("Enter saves · Esc cancels"),
                )
        });
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
                if this.executions.view == executions::View::Issues { this.close_issue_panels(); }
                this.search_input.focus_handle(cx).focus(window, cx);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &LeaveInput, window, cx| {
                if this.issue.title.focus_handle(cx).is_focused(window) {
                    this.issue.body.focus_handle(cx).focus(window, cx);
                } else {
                    window.dispatch_action(Box::new(FocusResults), cx);
                }
            }))
            .on_action(cx.listener(|this, _: &FocusResults, window, cx| {
                // Esc in the comment field only leaves the field; elsewhere it closes the form, then the issue.
                if this.issue.compose.is_none() && this.issue.comment.focus_handle(cx).is_focused(window) {
                    this.focus.focus(window, cx);
                    cx.notify();
                    return;
                }
                if this.executions.view == executions::View::Issues && this.issue_back(window, cx) { return; }
                this.editing_setup = false;
                this.focus.focus(window, cx);
                if this.active_issue.is_none() { this.move_issue(true, window, cx); }
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &NextIssue, window, cx| this.move_issue(true, window, cx)))
            .on_action(cx.listener(|this, _: &PreviousIssue, window, cx| this.move_issue(false, window, cx)))
            .on_action(cx.listener(|this, _: &OpenIssue, window, cx| {
                if this.executions.view == executions::View::Executions {
                    this.open_pull_request(cx);
                } else if !this.issue.open() && let Some(index) = this.active_issue {
                    this.view_issue(index, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ShowExecutions, window, cx| this.show_executions(window, cx)))
            .on_action(cx.listener(|this, _: &ToggleRun, _, cx| this.toggle_run(cx)))
            .on_action(cx.listener(|this, _: &RetryWorking, _, cx| this.retry_working(cx)))
            .on_action(cx.listener(|this, _: &RecheckPublication, _, cx| this.recheck_publication(cx)))
            .on_action(cx.listener(|this, _: &DecreaseLimit, _, cx| this.change_limit(-1, cx)))
            .on_action(cx.listener(|this, _: &IncreaseLimit, _, cx| this.change_limit(1, cx)))
            .on_action(cx.listener(|this, _: &FocusSetup, window, cx| this.edit_setup(window, cx)))
            .on_action(cx.listener(|this, _: &InstallUpdate, _, cx| this.install_update(cx)))
            .on_action(cx.listener(|this, _: &NewIssue, window, cx| this.new_issue(window, cx)))
            .on_action(cx.listener(move |this, _: &SubmitIssue, window, cx| if !in_executions { this.submit_issue(window, cx) }))
            .on_action(cx.listener(move |this, _: &EditIssue, window, cx| if !in_executions && this.issue.compose.is_none() { this.edit_issue(window, cx) }))
            .on_action(cx.listener(move |this, _: &CloseIssue, _, cx| if !in_executions { this.toggle_closed(false, cx) }))
            .on_action(cx.listener(move |this, _: &CloseNotPlanned, _, cx| if !in_executions { this.toggle_closed(true, cx) }))
            .on_action(cx.listener(move |this, _: &ToggleReady, _, cx| if !in_executions { this.toggle_ready(cx) }))
            .on_action(cx.listener(move |this, _: &ToggleAssign, _, cx| if !in_executions { this.toggle_assign(cx) }))
            .on_action(cx.listener(move |this, _: &FocusComment, window, cx| if !in_executions { this.focus_comment(window, cx) }))
            .on_action(cx.listener(move |this, _: &OpenOnGitHub, _, cx| if !in_executions { this.open_issue_on_github(cx) }))
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
                    .child(div().id("show-executions").role(Role::Button).aria_label(format!("Executions, {MODIFIER_NAME} E"))
                        .focusable().tab_stop(true).key_context("Selectable")
                        .border_l_2().border_color(rgb(if in_executions { ACCENT } else { BG }))
                        .focus(|s| s.border_color(rgb(TEXT)))
                        .px_4().py_3().mb_3().flex().items_center().justify_between().cursor_pointer()
                        .bg(rgb(if in_executions { LINE } else { BG }))
                        .on_action(cx.listener(|this, _: &Activate, window, cx| this.show_executions(window, cx)))
                        .on_click(cx.listener(|this, _, window, cx| this.show_executions(window, cx)))
                        .child(format!("Executions · {} active", self.executions.list.iter().filter(|e| e.holds_slot()).count()))
                        .child(shortcut(format!("{MODIFIER}E"))))
                    .child(div().px_5().pb_3().text_xs().text_color(rgb(MUTED)).child("PROJECTS"))
                    .child(div().id("all-projects").role(Role::Button).aria_label(format!("All projects, {MODIFIER_NAME} 0"))
                        .focusable().tab_stop(true).key_context("Selectable")
                        .border_l_2().border_color(rgb(if !in_executions && self.selected.is_none() { ACCENT } else { BG }))
                        .focus(|s| s.border_color(rgb(TEXT)))
                        .px_4().py_3().flex().items_center().justify_between().cursor_pointer()
                        .bg(rgb(if !in_executions && self.selected.is_none() { LINE } else { BG }))
                        .on_action(cx.listener(|this, _: &Activate, window, cx| this.select_project(&SelectProject(0), window, cx)))
                        .on_click(cx.listener(|this, _, window, cx| this.select_project(&SelectProject(0), window, cx)))
                        .child("All projects").child(shortcut(format!("{MODIFIER}0"))))
                    .child(div().id("projects").overflow_y_scroll().flex_1().min_h_0()
                        .children(self.projects.iter().enumerate().map(|(index, project)| {
                            let selected = !in_executions && self.selected == Some(project.id);
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
                    .children(self.update.ready.as_ref().map(|ready| div().px_4().pt_4().flex().flex_col().gap_2()
                        .child(div().text_xs().text_color(rgb(MUTED)).child(format!("Nightshift {} is ready", ready.version)))
                        .child(button("install-update", if self.update.installing { "Installing update…".into() } else { format!("Restart to update  {MODIFIER}⇧U") }, !self.update.installing)
                            .w_full().border_color(rgb(ACCENT)).key_context("UpdateButton")
                            .aria_label(format!("Restart to update to Nightshift {}, {MODIFIER_NAME} Shift U", ready.version))
                            .on_click(cx.listener(|this, _, _, cx| this.install_update(cx))))))
                    .child(div().p_4().child(button("add-project", format!("Add project  {MODIFIER}O"), !self.busy && self.store.is_some())
                        .w_full().key_context("AddButton")
                        .on_click(cx.listener(|this, _, window, cx| this.add_project(&AddProject, window, cx))))))
                .child(div().flex_1().min_w_0().flex().flex_col().bg(rgb(PANEL))
                    .child(run_bar)
                    .children(executions_view)
                    .children(issue_panel)
                    .when(in_list, |panel| panel
                    .child(div().p_5().flex_none().flex().flex_col().gap_3()
                        .child(div().flex().items_center().justify_between().gap_3()
                            .child(div().min_w_0().truncate().text_lg().child(heading))
                            .children(self.selected.map(|_| div().flex_none().flex().gap_2()
                                .child(button("new-issue", format!("New issue  {MODIFIER}N"), self.store.is_some())
                                    .key_context("Selectable")
                                    .on_action(cx.listener(|this, _: &Activate, window, cx| this.new_issue(window, cx)))
                                    .on_click(cx.listener(|this, _, window, cx| this.new_issue(window, cx))))
                                .child(button("edit-setup", format!("Setup  {MODIFIER}⇧S"), true)
                                    .key_context("Selectable")
                                    .on_action(cx.listener(|this, _: &Activate, window, cx| this.edit_setup(window, cx)))
                                    .on_click(cx.listener(|this, _, window, cx| this.edit_setup(window, cx)))))))
                        .child(div().flex().items_center().gap_2()
                            .child(div().flex_1().min_w_0().child(self.search_input.clone()))
                            .child(button("refresh", if self.busy { "Fetching…".into() } else { format!("Refresh  {MODIFIER}R") }, !self.busy && self.selected.is_some())
                                .key_context("SearchButton")
                                .on_click(cx.listener(|this, _, window, cx| this.search(&SearchIssues, window, cx)))))
                        .children(self.query.as_ref().err().map(|error| div().text_color(rgb(0xf2b8a5)).child(error.clone())))
                        .children(setup_row)
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
                        div().flex_1().flex().flex_col()
                            .child(div().flex_1().p_6().text_color(rgb(MUTED)).child(empty))
                            .children(has_more.then(|| self.render_load_more(cx)))
                            .into_any_element()
                    } else {
                        uniform_list("issues", count + usize::from(has_more), cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                            range.map(|index| {
                                if index == count {
                                    return this.render_load_more(cx).into_any_element();
                                }
                                let issue_index = visible[index];
                                let row = &this.issues[issue_index];
                                let selected = this.active_issue == Some(issue_index);
                                div().id(("issue", issue_index)).role(Role::Link)
                                    .aria_label(format!("{} #{} {}", row.repository, row.issue.number, row.issue.title))
                                    .focusable().tab_stop(true).key_context("Selectable")
                                    .border_l_2().border_color(rgb(if selected { ACCENT } else { PANEL }))
                                    .focus(|s| s.border_color(rgb(TEXT)).bg(rgb(LINE)))
                                    .bg(rgb(if selected { LINE } else { PANEL }))
                                    .w_full().h(px(76.)).px_5().flex().flex_col().justify_center().gap_1()
                                    .cursor_pointer().hover(|s| s.bg(rgb(LINE)))
                                    .on_action(cx.listener(move |this, _: &Activate, window, cx| this.view_issue(issue_index, window, cx)))
                                    .on_click(cx.listener(move |this, _, window, cx| this.view_issue(issue_index, window, cx)))
                                    .child(div().flex().items_center().gap_3()
                                        .child(div().flex_none().text_xs().text_color(rgb(MUTED)).child(format!("#{}", row.issue.number)))
                                        .child(div().min_w_0().truncate().child(row.issue.title.clone())))
                                    .child(div().truncate().text_xs().text_color(rgb(MUTED))
                                        .child(format!("{}  ·  {}", row.repository, row.issue.labels.join(" · "))))
                                    .into_any_element()
                            }).collect()
                        })).track_scroll(&self.issue_scroll).w_full().flex_1().min_h_0().into_any_element()
                    })
                    .children(self.errors.iter().map(|error| div().px_5().py_2().text_color(rgb(0xf2b8a5)).child(error.clone()))))))
            .child(div().id("status").role(Role::Status).aria_label(self.status.clone())
                .px_3().py_1().flex_none().border_t_1().border_color(rgb(LINE)).text_xs().text_color(rgb(MUTED))
                .flex().justify_between().gap_3()
                .child(div().flex_1().min_w_0().truncate().child(self.status.clone()))
                .children(in_executions.then(|| div().flex_none().child(format!("↑ ↓ Select · {MODIFIER}R Refresh")))))
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
            KeyBinding::new("down", LeaveInput, Some("TextInput")),
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
            KeyBinding::new("secondary-e", ShowExecutions, Some("Nightshift")),
            KeyBinding::new("secondary-shift-r", ToggleRun, Some("Nightshift")),
            KeyBinding::new("secondary-shift-t", RetryWorking, Some("Nightshift")),
            KeyBinding::new("secondary-shift-p", RecheckPublication, Some("Nightshift")),
            KeyBinding::new("secondary-[", DecreaseLimit, Some("Nightshift")),
            KeyBinding::new("secondary-]", IncreaseLimit, Some("Nightshift")),
            KeyBinding::new("secondary-shift-s", FocusSetup, Some("Nightshift")),
            KeyBinding::new("secondary-shift-u", InstallUpdate, Some("Nightshift")),
            KeyBinding::new("enter", InstallUpdate, Some("UpdateButton")),
            KeyBinding::new("enter", ToggleRun, Some("ToggleRunButton")),
            KeyBinding::new("enter", DecreaseLimit, Some("LimitDownButton")),
            KeyBinding::new("enter", IncreaseLimit, Some("LimitUpButton")),
            KeyBinding::new("enter", RetryWorking, Some("RetryWorkingButton")),
            KeyBinding::new("enter", RecheckPublication, Some("RecheckButton")),
            KeyBinding::new("enter", OpenIssue, Some("OpenPullRequestButton")),
            KeyBinding::new("secondary-n", NewIssue, Some("Nightshift")),
            KeyBinding::new("secondary-enter", SubmitIssue, Some("Nightshift")),
            KeyBinding::new("secondary-shift-e", EditIssue, Some("Nightshift")),
            KeyBinding::new("secondary-shift-c", CloseIssue, Some("Nightshift")),
            KeyBinding::new("secondary-shift-x", CloseNotPlanned, Some("Nightshift")),
            KeyBinding::new("secondary-shift-l", ToggleReady, Some("Nightshift")),
            KeyBinding::new("secondary-shift-a", ToggleAssign, Some("Nightshift")),
            KeyBinding::new("secondary-shift-m", FocusComment, Some("Nightshift")),
            KeyBinding::new("secondary-shift-o", OpenOnGitHub, Some("Nightshift")),
            KeyBinding::new("tab", NextFocus, Some("Nightshift")),
            KeyBinding::new("shift-tab", PreviousFocus, Some("Nightshift")),
        ]);
        text_input::bind_keys(cx);
        cx.on_action(|_: &Quit, cx| cx.quit());
        // AppKit needs a native menu action when no GPUI window is handling keys.
        #[cfg(target_os = "macos")]
        cx.set_menus([
            gpui::Menu::new("Nightshift").items([gpui::MenuItem::action("Quit Nightshift", Quit)])
        ]);
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

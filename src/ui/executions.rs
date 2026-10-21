//! Run controls, execution history, details, and paged output.
use super::*;
use nightshift::{
    application::run::{Command, RunStatus, Update},
    domain::{Execution, MAX_CONCURRENCY_LIMIT, Status, Step, StoredOutput},
};
use std::collections::{HashSet, VecDeque};

const OUTPUT_PAGE: usize = 100;
/// Output pages kept in memory for the selected execution (decision D7).
const OUTPUT_PAGES: usize = 24;
const OUTPUT_ROW: f32 = 20.;
const ERROR: u32 = 0xf2b8a5;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum View {
    #[default]
    Issues,
    Executions,
}

#[derive(Default)]
pub(super) struct Output {
    execution: Option<i64>,
    total: usize,
    pages: HashMap<usize, Vec<StoredOutput>>,
    recency: VecDeque<usize>,
    loading: HashSet<usize>,
    follow: bool,
}

#[derive(Default)]
pub(super) struct Executions {
    pub(super) view: View,
    pub(super) run: Option<nightshift::application::run::Handle>,
    pub(super) status: RunStatus,
    pub(super) list: Vec<Execution>,
    pub(super) active: Option<i64>,
    pub(super) list_scroll: gpui::UniformListScrollHandle,
    pub(super) output: Output,
    pub(super) output_scroll: gpui::UniformListScrollHandle,
}

fn step_label(execution: &Execution) -> String {
    let status = match execution.status {
        Status::Pending if execution.step == Step::Checking => "waiting for a slot",
        Status::Pending if execution.working.work_finished_at.is_some() => "awaiting publication",
        Status::Pending => "pending",
        Status::Running => "in progress",
        Status::Error => "error",
        Status::Interrupted => "interrupted",
        Status::Ended => match execution.outcome {
            Some(nightshift::domain::Outcome::PublicationSkipped) => "issue closed; not published",
            Some(nightshift::domain::Outcome::Legacy) => "earlier history",
            _ => "done",
        },
    };
    format!("{} · {status}", execution.step.label())
}

/// Whether the owner may continue local work, per the stored state.
fn can_retry_working(execution: &Execution) -> bool {
    execution.step == Step::Working
        && matches!(execution.status, Status::Error | Status::Interrupted)
        && execution.working.work_finished_at.is_none()
        && !execution.legacy
}

/// Whether publication stopped with an error, or local work finished without publishing yet.
fn can_recheck_publication(execution: &Execution) -> bool {
    match (execution.step, execution.status) {
        (Step::Working, Status::Error | Status::Pending) => {
            execution.working.work_finished_at.is_some() && !execution.holds_slot()
        }
        (Step::BranchPublished, Status::Error | Status::Pending) => true,
        _ => false,
    }
}

impl Nightshift {
    pub(super) fn listen(
        &mut self,
        run: nightshift::application::run::Handle,
        mut updates: tokio::sync::mpsc::Receiver<Update>,
        cx: &mut Context<Self>,
    ) {
        self.executions.run = Some(run);
        self._updates = Some(cx.spawn(async move |this, cx| {
            while let Some(update) = updates.recv().await {
                let Ok(()) = this.update(cx, |this, cx| {
                    match update {
                        Update::Run(status) => this.executions.status = status,
                        Update::Executions => this.reload_executions(cx),
                        Update::Message(message) => this.status = message,
                    }
                    cx.notify();
                }) else {
                    return;
                };
            }
        }));
        // New output for the selected execution; storage is the source of truth.
        self._output_poll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(500))
                    .await;
                let Ok(()) = this.update(cx, |this, cx| this.poll_output(cx)) else {
                    return;
                };
            }
        }));
        self.reload_executions(cx);
    }

    pub(super) fn reload_executions(&mut self, cx: &mut Context<Self>) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let load = self.runtime.spawn(async move { store.executions().await });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(list)) = load.await {
                let _ = this.update(cx, |this, cx| {
                    this.executions.list = list;
                    if this.executions.active.is_none() {
                        this.executions.active = this.executions.list.first().map(|e| e.id);
                    }
                    this.poll_output(cx);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn poll_output(&mut self, cx: &mut Context<Self>) {
        if self.executions.view != View::Executions {
            return;
        }
        let (Some(store), Some(id)) = (self.store.clone(), self.executions.active) else {
            return;
        };
        let count = self
            .runtime
            .spawn(async move { store.output_count(id).await });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(total)) = count.await {
                let _ = this.update(cx, |this, cx| {
                    let output = &mut this.executions.output;
                    if output.execution != Some(id) {
                        *output = Output {
                            execution: Some(id),
                            follow: true,
                            ..Default::default()
                        };
                    }
                    let total = total as usize;
                    if total != output.total {
                        // The previous last page may have been partial.
                        output.pages.remove(&(output.total / OUTPUT_PAGE));
                        output.total = total;
                        if output.follow && total > 0 {
                            this.executions
                                .output_scroll
                                .scroll_to_item(total - 1, gpui::ScrollStrategy::Bottom);
                        }
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    fn output_row(&mut self, index: usize, cx: &mut Context<Self>) -> (String, bool) {
        let page = index / OUTPUT_PAGE;
        let output = &mut self.executions.output;
        if let Some(rows) = output.pages.get(&page) {
            if let Some(position) = output.recency.iter().position(|p| *p == page) {
                output.recency.remove(position);
            }
            output.recency.push_back(page);
            return rows
                .get(index % OUTPUT_PAGE)
                .map(|row| (row.payload.replace('\n', " ⏎ "), row.source == "stderr"))
                .unwrap_or_default();
        }
        let (Some(store), Some(id)) = (self.store.clone(), output.execution) else {
            return Default::default();
        };
        if output.loading.insert(page) {
            let load = self
                .runtime
                .spawn(async move { store.output(id, (page * OUTPUT_PAGE) as i64).await });
            cx.spawn(async move |this, cx| {
                let result = load.await;
                let _ = this.update(cx, |this, cx| {
                    let output = &mut this.executions.output;
                    output.loading.remove(&page);
                    if output.execution != Some(id) {
                        return;
                    }
                    if let Ok(Ok(rows)) = result {
                        output.pages.insert(page, rows);
                        output.recency.push_back(page);
                        while output.pages.len() > OUTPUT_PAGES {
                            let Some(oldest) = output.recency.pop_front() else {
                                break;
                            };
                            if !output.recency.contains(&oldest) {
                                output.pages.remove(&oldest);
                            }
                        }
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        ("…".into(), false)
    }

    pub(super) fn show_executions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.executions.view = View::Executions;
        self.focus.focus(window, cx);
        self.reload_executions(cx);
        cx.notify();
    }

    pub(super) fn move_execution(&mut self, forward: bool, cx: &mut Context<Self>) {
        let list = &self.executions.list;
        if list.is_empty() {
            return;
        }
        let current = list
            .iter()
            .position(|e| Some(e.id) == self.executions.active);
        let next = match current {
            Some(i) if forward => (i + 1).min(list.len() - 1),
            Some(i) => i.saturating_sub(1),
            None => 0,
        };
        self.executions.active = Some(list[next].id);
        self.executions
            .list_scroll
            .scroll_to_item(next, gpui::ScrollStrategy::Center);
        self.poll_output(cx);
        cx.notify();
    }

    fn send(&mut self, command: Command, cx: &mut Context<Self>) {
        match &self.executions.run {
            Some(run) if run.send(command) => {}
            Some(_) => self.status = "Nightshift is busy; try again.".into(),
            None => self.status = "Storage is still loading.".into(),
        }
        cx.notify();
    }

    pub(super) fn toggle_run(&mut self, cx: &mut Context<Self>) {
        let command = if self.executions.status.owner.is_some() {
            Command::EndRun
        } else {
            Command::StartRun
        };
        self.status = match command {
            Command::StartRun => "Starting a run: checking GitHub CLI…".into(),
            _ => "Ending the run…".into(),
        };
        self.send(command, cx);
    }

    pub(super) fn refresh_run(&mut self, cx: &mut Context<Self>) {
        self.send(Command::Refresh, cx);
    }

    pub(super) fn change_limit(&mut self, delta: i32, cx: &mut Context<Self>) {
        let current = self.executions.status.concurrency_limit.max(1) as i32;
        let limit = (current + delta).clamp(1, MAX_CONCURRENCY_LIMIT as i32) as u32;
        if limit as i32 != current {
            self.executions.status.concurrency_limit = limit;
            self.send(Command::SetConcurrencyLimit(limit), cx);
        }
    }

    fn active_execution(&self) -> Option<&Execution> {
        self.executions
            .list
            .iter()
            .find(|e| Some(e.id) == self.executions.active)
    }

    pub(super) fn retry_working(&mut self, cx: &mut Context<Self>) {
        match self
            .active_execution()
            .filter(|e| can_retry_working(e))
            .map(|e| e.id)
        {
            Some(id) => {
                self.status = format!("Retrying working for execution {id}…");
                self.send(Command::RetryWorking(id), cx);
            }
            None => {
                self.status =
                    "Retry working applies to an interrupted or failed working step.".into();
                cx.notify();
            }
        }
    }

    pub(super) fn recheck_publication(&mut self, cx: &mut Context<Self>) {
        match self
            .active_execution()
            .filter(|e| can_recheck_publication(e))
            .map(|e| e.id)
        {
            Some(id) => {
                self.status = format!("Rechecking publication for execution {id}…");
                self.send(Command::RecheckPublication(id), cx);
            }
            None => {
                self.status = "Recheck publication applies after local work finished and publication stopped.".into();
                cx.notify();
            }
        }
    }

    pub(super) fn open_pull_request(&mut self, cx: &mut Context<Self>) {
        if let Some(url) = self
            .active_execution()
            .and_then(|e| e.publication.pull_request_url.clone())
        {
            cx.open_url(&url);
        }
    }

    pub(super) fn render_run_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let status = &self.executions.status;
        let active = status.owner.is_some();
        let reserved = self
            .executions
            .list
            .iter()
            .filter(|e| e.holds_slot())
            .count();
        let summary = match &status.owner {
            Some(owner) => {
                let next = status.next_refresh.map(|at| {
                    let seconds = at
                        .saturating_duration_since(std::time::Instant::now())
                        .as_secs();
                    format!(" · next refresh in {}:{:02}", seconds / 60, seconds % 60)
                });
                format!(
                    "Run active as {owner} · {reserved}/{} slots{}{}",
                    status.concurrency_limit,
                    if status.refreshing {
                        " · refreshing…"
                    } else {
                        ""
                    },
                    next.unwrap_or_default()
                )
            }
            None => format!(
                "No active run · {reserved}/{} slots in use",
                status.concurrency_limit
            ),
        };
        div()
            .px_5()
            .py_3()
            .flex_none()
            .flex()
            .flex_col()
            .gap_2()
            .border_b_1()
            .border_color(rgb(LINE))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        button(
                            "toggle-run",
                            if active {
                                format!("End run  {MODIFIER}⇧R")
                            } else {
                                format!("Start run  {MODIFIER}⇧R")
                            },
                            self.executions.run.is_some(),
                        )
                        .key_context("ToggleRunButton")
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_run(cx))),
                    )
                    .child(
                        button(
                            "limit-down",
                            format!("−  {MODIFIER}["),
                            status.concurrency_limit > 1,
                        )
                        .key_context("LimitDownButton")
                        .on_click(cx.listener(|this, _, _, cx| this.change_limit(-1, cx))),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child(format!("Limit {}", status.concurrency_limit)),
                    )
                    .child(
                        button(
                            "limit-up",
                            format!("+  {MODIFIER}]"),
                            status.concurrency_limit < MAX_CONCURRENCY_LIMIT,
                        )
                        .key_context("LimitUpButton")
                        .on_click(cx.listener(|this, _, _, cx| this.change_limit(1, cx))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child(summary),
                    ),
            )
            .children(status.problems.iter().take(3).map(|problem| {
                div()
                    .text_xs()
                    .text_color(rgb(ERROR))
                    .child(problem.clone())
            }))
    }

    pub(super) fn render_executions(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let count = self.executions.list.len();
        let list = if count == 0 {
            div()
                .p_6()
                .text_color(rgb(MUTED))
                .child(format!(
                    "No executions yet. Start a run with {MODIFIER}⇧R to work through open issues assigned to you and labeled ready-for-agent."
                ))
                .into_any_element()
        } else {
            uniform_list(
                "executions",
                count,
                cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                    range
                        .map(|index| {
                            let execution = &this.executions.list[index];
                            let id = execution.id;
                            let selected = this.executions.active == Some(id);
                            let project = this
                                .projects
                                .iter()
                                .find(|p| p.id == execution.project_id)
                                .map(|p| format!("{}/{}", p.owner, p.name))
                                .unwrap_or_default();
                            let title = execution
                                .snapshot
                                .as_ref()
                                .map(|s| s.title.clone())
                                .unwrap_or_else(|| "Not started".into());
                            let errored = execution.status == Status::Error;
                            div()
                                .id(("execution", id as usize))
                                .role(Role::Button)
                                .aria_label(format!(
                                    "{project} #{} {}",
                                    execution.issue.number,
                                    step_label(execution)
                                ))
                                .focusable()
                                .tab_stop(true)
                                .key_context("Selectable")
                                .border_l_2()
                                .border_color(rgb(if selected { ACCENT } else { PANEL }))
                                .focus(|s| s.border_color(rgb(TEXT)).bg(rgb(LINE)))
                                .bg(rgb(if selected { LINE } else { PANEL }))
                                .h(px(64.))
                                .px_4()
                                .flex()
                                .flex_col()
                                .justify_center()
                                .gap_1()
                                .cursor_pointer()
                                .on_action(cx.listener(move |this, _: &Activate, _, cx| {
                                    this.executions.active = Some(id);
                                    this.poll_output(cx);
                                    cx.notify();
                                }))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.executions.active = Some(id);
                                    this.focus.focus(window, cx);
                                    this.poll_output(cx);
                                    cx.notify();
                                }))
                                .child(
                                    div()
                                        .flex()
                                        .gap_2()
                                        .child(
                                            div()
                                                .flex_none()
                                                .text_xs()
                                                .text_color(rgb(MUTED))
                                                .child(format!("#{}", execution.issue.number)),
                                        )
                                        .child(div().min_w_0().truncate().child(title)),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_xs()
                                        .text_color(rgb(if errored { ERROR } else { MUTED }))
                                        .child(format!("{project} · {}", step_label(execution))),
                                )
                        })
                        .collect()
                }),
            )
            .track_scroll(&self.executions.list_scroll)
            .flex_1()
            .min_h_0()
            .into_any_element()
        };
        div()
            .flex()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .w(px(360.))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .border_r_1()
                    .border_color(rgb(LINE))
                    .child(list),
            )
            .child(self.render_detail(cx))
            .into_any_element()
    }

    fn render_detail(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(execution) = self.active_execution().cloned() else {
            return div()
                .flex_1()
                .p_6()
                .text_color(rgb(MUTED))
                .child("Select an execution to see its details.")
                .into_any_element();
        };
        let mut facts: Vec<(String, String)> = vec![("Workflow".into(), step_label(&execution))];
        let w = &execution.working;
        let p = &execution.publication;
        for (label, value) in [
            ("Run owner", Some(execution.run_owner.clone())),
            ("Branch", w.branch.clone()),
            ("Worktree", w.worktree_path.clone()),
            (
                "Base",
                w.base_commit.clone().map(|c| {
                    format!(
                        "{} {}",
                        w.default_branch.clone().unwrap_or_default(),
                        &c[..c.len().min(12)]
                    )
                }),
            ),
            ("Codex thread", w.codex_thread_id.clone()),
            ("Local result", w.work_result.clone()),
            (
                "Pushed",
                p.pushed_commit.clone().map(|c| {
                    format!(
                        "{} {}",
                        p.pushed_ref.clone().unwrap_or_default(),
                        &c[..c.len().min(12)]
                    )
                }),
            ),
            ("Pull request", p.pull_request_url.clone()),
            (
                "Parent",
                execution
                    .parent
                    .as_ref()
                    .map(|p| format!("#{} {}", p.number, p.title)),
            ),
        ] {
            if let Some(value) = value.filter(|v| !v.is_empty()) {
                facts.push((label.into(), value));
            }
        }
        let retry = can_retry_working(&execution);
        let recheck = can_recheck_publication(&execution);
        let total = if self.executions.output.execution == Some(execution.id) {
            self.executions.output.total
        } else {
            0
        };
        let output = if total == 0 {
            div()
                .p_4()
                .text_xs()
                .text_color(rgb(MUTED))
                .child("No output yet.")
                .into_any_element()
        } else {
            uniform_list(
                "output",
                total,
                cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                    range
                        .map(|index| {
                            let (text, stderr) = this.output_row(index, cx);
                            div()
                                .h(px(OUTPUT_ROW))
                                .px_4()
                                .truncate()
                                .font_family("Menlo")
                                .text_xs()
                                .text_color(rgb(if stderr { ERROR } else { TEXT }))
                                .child(text)
                        })
                        .collect()
                }),
            )
            .track_scroll(&self.executions.output_scroll)
            .flex_1()
            .min_h_0()
            .into_any_element()
        };
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .p_5()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_lg().truncate().child(format!(
                            "#{} {}",
                            execution.issue.number,
                            execution.snapshot.as_ref().map(|s| s.title.as_str()).unwrap_or("")
                        )))
                    .children(facts.into_iter().map(|(label, value)| {
                        div()
                            .flex()
                            .gap_2()
                            .text_xs()
                            .child(
                                div()
                                    .w(px(96.))
                                    .flex_none()
                                    .text_color(rgb(MUTED))
                                    .child(label),
                            )
                            .child(div().min_w_0().truncate().child(value))
                    }))
                    .children(execution.error.as_ref().map(|error| {
                        div().pt_2().text_xs().text_color(rgb(ERROR)).child(format!(
                            "{} → {}: {}{}",
                            execution.step.label(),
                            error.transition.label(),
                            error.message,
                            error
                                .detail
                                .as_ref()
                                .map(|d| format!(" ({d})"))
                                .unwrap_or_default()
                        ))
                    }))
                    .child(
                        div()
                            .pt_2()
                            .flex()
                            .gap_2()
                            .child(
                                button(
                                    "retry-working",
                                    format!("Retry working  {MODIFIER}⇧T"),
                                    retry,
                                )
                                .key_context("RetryWorkingButton")
                                .on_click(cx.listener(|this, _, _, cx| this.retry_working(cx))),
                            )
                            .child(
                                button(
                                    "recheck-publication",
                                    format!("Recheck publication  {MODIFIER}⇧P"),
                                    recheck,
                                )
                                .key_context("RecheckButton")
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.recheck_publication(cx)),
                                ),
                            )
                            .children(execution.publication.pull_request_url.clone().map(|_| {
                                button("open-pr", "Open pull request  Enter", true)
                                    .key_context("OpenPullRequestButton")
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.open_pull_request(cx)),
                                    )
                            })),
                    ),
            )
            .child(
                div()
                    .px_5()
                    .pb_1()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(format!("OUTPUT · {total} blocks")),
            )
            .child(output)
            .into_any_element()
    }
}

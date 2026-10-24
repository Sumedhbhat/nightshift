//! The issue view and the new-issue and edit form. Every GitHub write here is an owner action.
use super::*;
use nightshift::github::issues::{Change, Draft, IssueThread};

const ERROR: u32 = 0xf2b8a5;
/// Keyboard scrolling step in the issue view.
const SCROLL_STEP: f32 = 64.;

/// The issue open in the main panel.
pub(super) struct Viewing {
    project_id: i64,
    repository: String,
    number: u32,
    thread: Option<IssueThread>,
    error: Option<String>,
}

/// The new-issue or edit form. Its text lives in the form inputs.
pub(super) struct Compose {
    project_id: i64,
    repository: String,
    /// `None` creates a new issue.
    editing: Option<u32>,
    assign_me: bool,
    ready: bool,
    error: Option<String>,
}

/// Issue view state kept on the main window.
pub(super) struct IssueUi {
    pub(super) viewing: Option<Viewing>,
    pub(super) compose: Option<Compose>,
    pub(super) title: gpui::Entity<TextInput>,
    pub(super) body: gpui::Entity<TextInput>,
    pub(super) comment: gpui::Entity<TextInput>,
    /// Which form the title and body inputs currently hold, so a cancelled draft survives.
    draft_for: Option<(i64, Option<u32>)>,
    /// The issue the comment draft belongs to; another issue starts with an empty field.
    comment_for: Option<(i64, u32)>,
    /// A write is in flight; one at a time.
    pub(super) busy: bool,
    loading: bool,
    scroll: gpui::ScrollHandle,
    _task: Option<Task<()>>,
    _load: Option<Task<()>>,
}

impl IssueUi {
    pub(super) fn new(cx: &mut Context<Nightshift>) -> Self {
        Self {
            viewing: None,
            compose: None,
            title: cx.new(|cx| {
                TextInput::new(cx)
                    .with_placeholder("Title")
                    .with_label("Issue title")
            }),
            body: cx.new(|cx| {
                TextInput::new(cx)
                    .with_placeholder("Describe the work. Enter adds a line.")
                    .with_label("Issue description")
                    .multiline(10)
            }),
            comment: cx.new(|cx| {
                TextInput::new(cx)
                    .with_placeholder("Leave a comment")
                    .with_label("New comment")
                    .multiline(4)
            }),
            draft_for: None,
            comment_for: None,
            busy: false,
            loading: false,
            scroll: gpui::ScrollHandle::new(),
            _task: None,
            _load: None,
        }
    }

    /// Whether the issue view or form replaces the issue list.
    pub(super) fn open(&self) -> bool {
        self.viewing.is_some() || self.compose.is_some()
    }
}

fn state_label(thread: &IssueThread) -> String {
    let issue = &thread.issue;
    match (issue.open, issue.state_reason.as_deref()) {
        (true, _) => "Open".into(),
        (false, Some("not_planned")) => "Closed as not planned".into(),
        (false, Some("duplicate")) => "Closed as duplicate".into(),
        (false, _) => "Closed".into(),
    }
}

/// `2026-10-24T10:00:00Z` → `2026-10-24 10:00`.
fn timestamp(value: &str) -> String {
    match (value.get(..10), value.get(11..16)) {
        (Some(date), Some(time)) => format!("{date} {time}"),
        _ => value.to_owned(),
    }
}

fn checkbox(
    id: &'static str,
    label: &str,
    hint: String,
    checked: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .role(Role::CheckBox)
        .aria_label(format!("{label}, {hint}"))
        .aria_toggled(if checked {
            gpui::Toggled::True
        } else {
            gpui::Toggled::False
        })
        .focusable()
        .tab_stop(true)
        .key_context("Selectable")
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .py_2()
        .rounded_md()
        .border_1()
        .border_color(rgb(LINE))
        .focus(|style| style.border_color(rgb(ACCENT)))
        .cursor_pointer()
        .child(
            div()
                .font_family("Menlo")
                .text_color(rgb(if checked { ACCENT } else { MUTED }))
                .child(if checked { "[x]" } else { "[ ]" }),
        )
        .child(label.to_owned())
        .child(shortcut(hint))
}

impl Nightshift {
    fn project(&self, id: i64) -> Option<Project> {
        self.projects.iter().find(|p| p.id == id).cloned()
    }

    /// Open an issue from the list in the issue view.
    pub(super) fn view_issue(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.issues.get(index) else {
            return;
        };
        let (project_id, repository, number) =
            (row.project_id, row.repository.clone(), row.issue.number);
        self.active_issue = Some(index);
        self.own_comment((project_id, number), cx);
        self.issue.compose = None;
        self.issue.viewing = Some(Viewing {
            project_id,
            repository,
            number,
            thread: None,
            error: None,
        });
        self.issue.scroll.set_offset(gpui::point(px(0.), px(0.)));
        self.focus.focus(window, cx);
        self.load_issue(cx);
    }

    /// Re-read the viewed issue and its comments.
    pub(super) fn load_issue(&mut self, cx: &mut Context<Self>) {
        let Some(viewing) = &self.issue.viewing else {
            return;
        };
        let (Some(project), number) = (self.project(viewing.project_id), viewing.number) else {
            return;
        };
        // Replacing the task drops any earlier read; results are also checked against the view.
        self.issue.loading = true;
        self.status = format!("Loading #{number}…");
        let work = self
            .runtime
            .spawn(async move { GitHub::from_env().issue_thread(&project, number).await });
        let key = (viewing.project_id, number);
        self.issue._load = Some(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.issue.loading = false;
                match result {
                    Ok(Ok(thread)) => {
                        this.status = format!("#{} loaded", thread.issue.number);
                        // A refresh only updates a view that is still open.
                        this.apply_thread(key.0, thread, false, cx);
                    }
                    Ok(Err(error)) => this.view_error(key, error.to_string()),
                    Err(error) => this.view_error(key, error.to_string()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn view_error(&mut self, key: (i64, u32), error: String) {
        self.status = format!("GitHub request for #{} failed.", key.1);
        if let Some(viewing) = &mut self.issue.viewing
            && (viewing.project_id, viewing.number) == key
        {
            viewing.error = Some(error);
        }
    }

    /// Show a fresh thread if it is still the viewed issue, and keep the list row in step.
    /// Keep the comment draft only while it belongs to `key`.
    fn own_comment(&mut self, key: (i64, u32), cx: &mut Context<Self>) {
        if self.issue.comment_for != Some(key) {
            self.issue
                .comment
                .update(cx, |input, cx| input.set_text("", cx));
            self.issue.comment_for = Some(key);
        }
    }

    fn apply_thread(
        &mut self,
        project_id: i64,
        thread: IssueThread,
        show: bool,
        cx: &mut Context<Self>,
    ) {
        let number = thread.issue.number;
        let position = self
            .issues
            .iter()
            .position(|row| row.project_id == project_id && row.issue.number == number);
        match (thread.issue.open, position) {
            (true, Some(index)) => self.issues[index].issue = thread.issue.summary(),
            (true, None) => {
                if let Some(project) = self.project(project_id) {
                    // Newest first, matching the browse order.
                    let at = self
                        .issues
                        .iter()
                        .position(|row| row.project_id == project_id)
                        .unwrap_or(self.issues.len());
                    self.issues.insert(
                        at,
                        IssueRow {
                            project_id,
                            repository: format!("{}/{}", project.owner, project.name),
                            issue: thread.issue.summary(),
                        },
                    );
                }
                self.active_issue = None;
            }
            // The list holds open issues only.
            (false, Some(index)) => {
                self.issues.remove(index);
                self.active_issue = None;
            }
            (false, None) => {}
        }
        match &mut self.issue.viewing {
            Some(viewing) if viewing.project_id == project_id && viewing.number == number => {
                viewing.thread = Some(thread);
                viewing.error = None;
            }
            _ if show => {
                if let Some(project) = self.project(project_id) {
                    self.own_comment((project_id, number), cx);
                    self.issue.viewing = Some(Viewing {
                        project_id,
                        repository: format!("{}/{}", project.owner, project.name),
                        number,
                        thread: Some(thread),
                        error: None,
                    });
                }
            }
            _ => {}
        }
    }

    /// Leave the form, then the issue view. Returns whether anything closed.
    pub(super) fn issue_back(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.issue.compose.is_some() {
            self.issue.compose = None;
        } else if let Some(viewing) = self.issue.viewing.take() {
            self.active_issue = self.issues.iter().position(|row| {
                row.project_id == viewing.project_id && row.issue.number == viewing.number
            });
            if let Some(active) = self.active_issue
                && let Some(index) = self.visible_issues().iter().position(|i| *i == active)
            {
                self.issue_scroll
                    .scroll_to_item(index, gpui::ScrollStrategy::Center);
            }
        } else {
            return false;
        }
        self.focus.focus(window, cx);
        cx.notify();
        true
    }

    pub(super) fn close_issue_panels(&mut self) {
        self.issue.compose = None;
        self.issue.viewing = None;
    }

    pub(super) fn scroll_issue(&mut self, forward: bool, cx: &mut Context<Self>) {
        let offset = self.issue.scroll.offset();
        let max = self.issue.scroll.max_offset();
        let step = px(if forward { -SCROLL_STEP } else { SCROLL_STEP });
        let y = (offset.y + step).clamp(-max.y, px(0.));
        self.issue.scroll.set_offset(gpui::point(offset.x, y));
        cx.notify();
    }

    /// Open the new-issue form for the selected project.
    pub(super) fn new_issue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.store.is_none() {
            return;
        }
        let Some(project) = self.project(self.selected.unwrap_or_default()) else {
            self.status = if self.projects.is_empty() {
                format!("Add a project with {MODIFIER}O before creating issues.")
            } else {
                format!("Select a project with {MODIFIER}1–{MODIFIER}9 to create an issue in it.")
            };
            cx.notify();
            return;
        };
        self.executions.view = executions::View::Issues;
        self.editing_setup = false;
        self.open_form(&project, None, "", "", window, cx);
    }

    /// Open the edit form for the viewed issue.
    pub(super) fn edit_issue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(viewing) = &self.issue.viewing else {
            return;
        };
        let (Some(thread), Some(project)) = (&viewing.thread, self.project(viewing.project_id))
        else {
            return;
        };
        let (title, body) = (thread.issue.title.clone(), thread.issue.body.clone());
        let number = thread.issue.number;
        self.open_form(&project, Some(number), &title, &body, window, cx);
    }

    fn open_form(
        &mut self,
        project: &Project,
        editing: Option<u32>,
        title: &str,
        body: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = (project.id, editing);
        if self.issue.draft_for != Some(key) {
            self.issue
                .title
                .update(cx, |input, cx| input.set_text(title, cx));
            self.issue
                .body
                .update(cx, |input, cx| input.set_text(body, cx));
            self.issue.draft_for = Some(key);
        }
        self.issue.compose = Some(Compose {
            project_id: project.id,
            repository: format!("{}/{}", project.owner, project.name),
            editing,
            assign_me: true,
            ready: false,
            error: None,
        });
        self.issue.title.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    pub(super) fn toggle_assign(&mut self, cx: &mut Context<Self>) {
        if let Some(compose) = &mut self.issue.compose {
            if compose.editing.is_none() {
                compose.assign_me = !compose.assign_me;
                cx.notify();
            }
            return;
        }
        let Some(thread) = self.issue.viewing.as_ref().and_then(|v| v.thread.as_ref()) else {
            return;
        };
        let assigned = thread.issue.assigned_to(&thread.viewer);
        self.change_issue(Change::SetAssigned(!assigned), cx);
    }

    pub(super) fn toggle_ready(&mut self, cx: &mut Context<Self>) {
        if let Some(compose) = &mut self.issue.compose {
            if compose.editing.is_none() {
                compose.ready = !compose.ready;
                cx.notify();
            }
            return;
        }
        let Some(thread) = self.issue.viewing.as_ref().and_then(|v| v.thread.as_ref()) else {
            return;
        };
        let ready = thread.issue.ready();
        self.change_issue(Change::SetReady(!ready), cx);
    }

    pub(super) fn toggle_closed(&mut self, not_planned: bool, cx: &mut Context<Self>) {
        if self.issue.compose.is_some() {
            return;
        }
        let Some(thread) = self.issue.viewing.as_ref().and_then(|v| v.thread.as_ref()) else {
            return;
        };
        let change = if thread.issue.open {
            Change::Close { not_planned }
        } else if not_planned {
            return;
        } else {
            Change::Reopen
        };
        self.change_issue(change, cx);
    }

    pub(super) fn focus_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.issue.compose.is_none()
            && self
                .issue
                .viewing
                .as_ref()
                .is_some_and(|v| v.thread.is_some())
        {
            self.issue.comment.focus_handle(cx).focus(window, cx);
            self.issue.scroll.scroll_to_bottom();
            cx.notify();
        }
    }

    pub(super) fn open_issue_on_github(&mut self, cx: &mut Context<Self>) {
        if let Some(thread) = self.issue.viewing.as_ref().and_then(|v| v.thread.as_ref()) {
            cx.open_url(&thread.issue.url);
        }
    }

    /// ⌘Enter: submit the form, or post the comment from the issue view.
    pub(super) fn submit_issue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(compose) = &self.issue.compose {
            let title = self.issue.title.read(cx).text().trim().to_owned();
            let body = self.issue.body.read(cx).text().to_owned();
            if title.is_empty() {
                if let Some(compose) = &mut self.issue.compose {
                    compose.error = Some("Enter a title first.".into());
                }
                self.issue.title.focus_handle(cx).focus(window, cx);
                cx.notify();
                return;
            }
            match compose.editing {
                Some(_) => self.change_issue(Change::Edit { title, body }, cx),
                None => {
                    let draft = Draft {
                        title,
                        body,
                        assign_me: compose.assign_me,
                        ready: compose.ready,
                    };
                    self.create_issue(draft, cx);
                }
            }
            return;
        }
        if self.issue.viewing.is_some() {
            let comment = self.issue.comment.read(cx).text().to_owned();
            if comment.trim().is_empty() {
                self.status = "Write a comment first.".into();
                self.issue.comment.focus_handle(cx).focus(window, cx);
                cx.notify();
                return;
            }
            self.change_issue(Change::Comment(comment), cx);
        }
    }

    fn create_issue(&mut self, draft: Draft, cx: &mut Context<Self>) {
        let Some(compose) = &mut self.issue.compose else {
            return;
        };
        let Some(project) = self
            .projects
            .iter()
            .find(|p| p.id == compose.project_id)
            .cloned()
        else {
            return;
        };
        if self.issue.busy {
            return;
        }
        self.issue.busy = true;
        compose.error = None;
        self.status = format!("Creating an issue in {}…", compose.repository);
        let project_id = project.id;
        let asked = draft.clone();
        let work = self
            .runtime
            .spawn(async move { GitHub::from_env().create_issue(&project, &draft).await });
        self.issue._task = Some(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.issue.busy = false;
                match result {
                    Ok(Ok(thread)) => {
                        let mut status = format!("Created #{}", thread.issue.number);
                        if asked.assign_me && !thread.issue.assigned_to(&thread.viewer) {
                            status.push_str(". GitHub did not assign you; check your access.");
                        } else if asked.ready && !thread.issue.ready() {
                            status.push_str(
                                ". GitHub did not add ready-for-agent; check your access.",
                            );
                        }
                        this.status = status;
                        // The owner may have left or changed the form while GitHub answered;
                        // keep newer text, and open the issue only from the unchanged form.
                        let submitted =
                            this.form_holds(project_id, None, &asked.title, &asked.body, cx);
                        let composing = submitted && this.issue.compose.is_some();
                        if submitted {
                            this.issue.draft_for = None;
                            this.issue
                                .title
                                .update(cx, |input, cx| input.set_text("", cx));
                            this.issue
                                .body
                                .update(cx, |input, cx| input.set_text("", cx));
                        }
                        if composing {
                            this.issue.compose = None;
                            this.issue.viewing = None;
                            this.issue.scroll.set_offset(gpui::point(px(0.), px(0.)));
                            this.focus.focus(window, cx);
                        }
                        this.apply_thread(project_id, thread, composing, cx);
                    }
                    Ok(Err(error)) => this.form_error(error.to_string()),
                    Err(error) => this.form_error(error.to_string()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Whether the form inputs still hold this submission: the same issue (`None` for a new
    /// one) with unchanged text. A closed form counts when its draft was not replaced.
    fn form_holds(
        &self,
        project_id: i64,
        editing: Option<u32>,
        title: &str,
        body: &str,
        cx: &Context<Self>,
    ) -> bool {
        let same_form = match &self.issue.compose {
            Some(compose) => compose.project_id == project_id && compose.editing == editing,
            None => true,
        };
        same_form
            && self.issue.draft_for == Some((project_id, editing))
            && self.issue.title.read(cx).text().trim() == title.trim()
            && self.issue.body.read(cx).text() == body
    }

    fn form_error(&mut self, error: String) {
        self.status = "GitHub did not accept the issue.".into();
        if let Some(compose) = &mut self.issue.compose {
            compose.error = Some(error);
        }
    }

    fn change_issue(&mut self, change: Change, cx: &mut Context<Self>) {
        let (project_id, number) = match (&self.issue.compose, &self.issue.viewing) {
            (Some(compose), _) => match compose.editing {
                Some(number) => (compose.project_id, number),
                None => return,
            },
            (None, Some(viewing)) if viewing.thread.is_some() => {
                (viewing.project_id, viewing.number)
            }
            _ => return,
        };
        let Some(project) = self.project(project_id) else {
            return;
        };
        if self.issue.busy {
            return;
        }
        self.issue.busy = true;
        if let Some(compose) = &mut self.issue.compose {
            compose.error = None;
        }
        self.status = match &change {
            Change::Comment(_) => "Posting comment…".into(),
            _ => format!("Updating #{number}…"),
        };
        let request = change.clone();
        let work = self.runtime.spawn(async move {
            GitHub::from_env()
                .change_issue(&project, number, &request)
                .await
        });
        self.issue._task = Some(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.issue.busy = false;
                let error = match result {
                    Ok(Ok(thread)) => {
                        let mut status = change.done().to_owned();
                        match change {
                            Change::SetAssigned(true)
                                if !thread.issue.assigned_to(&thread.viewer) =>
                            {
                                status = "GitHub did not assign you; check your access.".into();
                            }
                            Change::Edit { title, body }
                                if this.form_holds(project_id, Some(number), &title, &body, cx) =>
                            {
                                this.issue.draft_for = None;
                                if this.issue.compose.is_some() {
                                    this.issue.compose = None;
                                    this.focus.focus(window, cx);
                                }
                            }
                            // Clear the draft only if it is still the posted text on this issue.
                            Change::Comment(posted)
                                if this.issue.comment_for == Some((project_id, number))
                                    && *this.issue.comment.read(cx).text() == posted =>
                            {
                                this.issue
                                    .comment
                                    .update(cx, |input, cx| input.set_text("", cx));
                                this.issue.scroll.scroll_to_bottom();
                            }
                            _ => {}
                        }
                        this.status = format!("#{number}: {status}");
                        // For the written issue, this re-read is newer than a load that started
                        // before the write. A load of another issue stays in flight.
                        let viewed = this.issue.viewing.as_ref().is_some_and(|viewing| {
                            viewing.project_id == project_id && viewing.number == number
                        });
                        if viewed {
                            this.issue._load = None;
                            this.issue.loading = false;
                        }
                        this.apply_thread(project_id, thread, false, cx);
                        None
                    }
                    Ok(Err(error)) => Some(error.to_string()),
                    Err(error) => Some(error.to_string()),
                };
                if let Some(error) = error {
                    match &mut this.issue.compose {
                        Some(compose) => {
                            this.status = "GitHub did not accept the change.".into();
                            compose.error = Some(error);
                        }
                        None => this.view_error((project_id, number), error),
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(super) fn render_issue_panel(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.issue.compose.is_some() {
            self.render_form(cx)
        } else {
            self.render_issue(cx)
        }
    }

    fn render_form(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let compose = self.issue.compose.as_ref().expect("form is open");
        let busy = self.issue.busy;
        let heading = match compose.editing {
            Some(number) => format!("Edit #{number} in {}", compose.repository),
            None => format!("New issue in {}", compose.repository),
        };
        let submit = match (compose.editing, busy) {
            (_, true) => "Saving…".to_owned(),
            (Some(_), false) => format!("Save  {MODIFIER}↵"),
            (None, false) => format!("Create issue  {MODIFIER}↵"),
        };
        let options = compose.editing.is_none().then(|| {
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .child(
                    checkbox(
                        "assign-me",
                        "Assign to me",
                        format!("{MODIFIER}⇧A"),
                        compose.assign_me,
                    )
                    .on_action(cx.listener(|this, _: &Activate, _, cx| this.toggle_assign(cx)))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_assign(cx))),
                )
                .child(
                    checkbox(
                        "mark-ready",
                        "Add ready-for-agent",
                        format!("{MODIFIER}⇧L"),
                        compose.ready,
                    )
                    .on_action(cx.listener(|this, _: &Activate, _, cx| this.toggle_ready(cx)))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_ready(cx))),
                )
        });
        let ready_note =
            (compose.editing.is_none() && compose.ready && compose.assign_me).then(|| {
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child("An active run may start this issue as soon as it is created.")
            });
        div()
            .id("issue-form")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_5()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(div().min_w_0().truncate().text_lg().child(heading))
                    .child(shortcut("Esc cancels")),
            )
            .child(div().text_xs().text_color(rgb(MUTED)).child("Title"))
            .child(self.issue.title.clone())
            .child(div().text_xs().text_color(rgb(MUTED)).child("Description"))
            .child(self.issue.body.clone())
            .children(options)
            .children(ready_note)
            .children(
                compose
                    .error
                    .clone()
                    .map(|error| div().text_color(rgb(ERROR)).child(error)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        button("submit-issue", submit, !busy)
                            .key_context("Selectable")
                            .on_action(cx.listener(|this, _: &Activate, window, cx| {
                                this.submit_issue(window, cx)
                            }))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.submit_issue(window, cx)),
                            ),
                    )
                    .child(
                        button("cancel-issue", "Cancel  Esc", true)
                            .key_context("Selectable")
                            .on_action(cx.listener(|this, _: &Activate, window, cx| {
                                this.issue_back(window, cx);
                            }))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.issue_back(window, cx);
                            })),
                    ),
            )
            .child(div().text_xs().text_color(rgb(MUTED)).child(
                "Enter adds a line · Tab moves between fields · Markdown is sent as written",
            ))
            .into_any_element()
    }

    fn render_issue(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let viewing = self.issue.viewing.as_ref().expect("issue is open");
        let busy = self.issue.busy;
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .child(
                button("issue-back", "← Issues  Esc", true)
                    .key_context("Selectable")
                    .on_action(cx.listener(|this, _: &Activate, window, cx| {
                        this.issue_back(window, cx);
                    }))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.issue_back(window, cx);
                    })),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(rgb(MUTED))
                    .child(format!("{} #{}", viewing.repository, viewing.number)),
            )
            .child(
                button(
                    "issue-github",
                    format!("Open on GitHub  {MODIFIER}⇧O"),
                    viewing.thread.is_some(),
                )
                .key_context("Selectable")
                .on_action(cx.listener(|this, _: &Activate, _, cx| this.open_issue_on_github(cx)))
                .on_click(cx.listener(|this, _, _, cx| this.open_issue_on_github(cx))),
            );
        let error = viewing
            .error
            .clone()
            .map(|error| div().text_color(rgb(ERROR)).child(error));
        let Some(thread) = viewing.thread.as_ref() else {
            return div()
                .flex_1()
                .p_5()
                .flex()
                .flex_col()
                .gap_3()
                .child(header)
                .children(error)
                .child(div().text_color(rgb(MUTED)).child(if self.issue.loading {
                    "Loading the issue from GitHub…".to_owned()
                } else {
                    format!("Press {MODIFIER}R to try again.")
                }))
                .into_any_element();
        };
        let issue = &thread.issue;
        let assigned = issue.assigned_to(&thread.viewer);
        let ready = issue.ready();
        let enabled = !busy;
        let close_label = if issue.open {
            format!("Close  {MODIFIER}⇧C")
        } else {
            format!("Reopen  {MODIFIER}⇧C")
        };
        let actions = div()
            .flex()
            .flex_wrap()
            .gap_2()
            .child(
                button("issue-edit", format!("Edit  {MODIFIER}⇧E"), enabled)
                    .key_context("Selectable")
                    .on_action(
                        cx.listener(|this, _: &Activate, window, cx| this.edit_issue(window, cx)),
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.edit_issue(window, cx))),
            )
            .child(
                button("issue-close", close_label, enabled)
                    .key_context("Selectable")
                    .on_action(
                        cx.listener(|this, _: &Activate, _, cx| this.toggle_closed(false, cx)),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_closed(false, cx))),
            )
            .children(issue.open.then(|| {
                button(
                    "issue-not-planned",
                    format!("Close as not planned  {MODIFIER}⇧X"),
                    enabled,
                )
                .key_context("Selectable")
                .on_action(cx.listener(|this, _: &Activate, _, cx| this.toggle_closed(true, cx)))
                .on_click(cx.listener(|this, _, _, cx| this.toggle_closed(true, cx)))
            }))
            .child(
                button(
                    "issue-ready",
                    if ready {
                        format!("Remove ready-for-agent  {MODIFIER}⇧L")
                    } else {
                        format!("Add ready-for-agent  {MODIFIER}⇧L")
                    },
                    enabled,
                )
                .key_context("Selectable")
                .on_action(cx.listener(|this, _: &Activate, _, cx| this.toggle_ready(cx)))
                .on_click(cx.listener(|this, _, _, cx| this.toggle_ready(cx))),
            )
            .child(
                button(
                    "issue-assign",
                    if assigned {
                        format!("Unassign me  {MODIFIER}⇧A")
                    } else {
                        format!("Assign me  {MODIFIER}⇧A")
                    },
                    enabled,
                )
                .key_context("Selectable")
                .on_action(cx.listener(|this, _: &Activate, _, cx| this.toggle_assign(cx)))
                .on_click(cx.listener(|this, _, _, cx| this.toggle_assign(cx))),
            );
        let people = |label: &str, values: &[String]| {
            format!(
                "{label}: {}",
                if values.is_empty() {
                    "none".to_owned()
                } else {
                    values.join(", ")
                }
            )
        };
        let meta = div()
            .flex()
            .flex_wrap()
            .gap_x_4()
            .gap_y_1()
            .text_xs()
            .text_color(rgb(MUTED))
            .child(
                div()
                    .text_color(rgb(if issue.open { ACCENT } else { MUTED }))
                    .child(state_label(thread)),
            )
            .child(format!(
                "Opened by {} on {}",
                if issue.author.is_empty() {
                    "unknown"
                } else {
                    &issue.author
                },
                timestamp(&issue.created_at)
            ))
            .child(people("Assignees", &issue.assignees))
            .child(people("Labels", &issue.labels));
        let body = if issue.body.trim().is_empty() {
            div()
                .text_color(rgb(MUTED))
                .child("No description provided.")
        } else {
            div().child(issue.body.clone())
        };
        let comments = thread.comments.iter().map(|comment| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .p_3()
                .rounded_md()
                .border_1()
                .border_color(rgb(LINE))
                .child(div().text_xs().text_color(rgb(MUTED)).child(format!(
                    "{} · {}",
                    comment.author,
                    timestamp(&comment.created_at)
                )))
                .child(comment.body.clone())
        });
        div()
            .id("issue-view")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.issue.scroll)
            .p_5()
            .flex()
            .flex_col()
            .gap_3()
            .child(header)
            .child(div().text_lg().child(issue.title.clone()))
            .child(meta)
            .children(error)
            .child(actions)
            .child(
                div()
                    .p_3()
                    .rounded_md()
                    .bg(rgb(BG))
                    .border_1()
                    .border_color(rgb(LINE))
                    .child(body),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(match thread.comments.len() {
                        0 => "No comments".to_owned(),
                        1 => "1 comment".to_owned(),
                        n => format!("{n} comments"),
                    }),
            )
            .children(comments)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .child(format!("Comment  {MODIFIER}⇧M"))
                    .child("Enter adds a line"),
            )
            .child(self.issue.comment.clone())
            .child(
                div().flex().child(
                    button(
                        "issue-comment",
                        if busy {
                            "Working…".to_owned()
                        } else {
                            format!("Comment  {MODIFIER}↵")
                        },
                        enabled,
                    )
                    .key_context("Selectable")
                    .on_action(
                        cx.listener(|this, _: &Activate, window, cx| this.submit_issue(window, cx)),
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.submit_issue(window, cx))),
                ),
            )
            .into_any_element()
    }
}

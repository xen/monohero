use crate::{client, model::Model, tray::UiEvent};
use futures::{
    StreamExt,
    channel::mpsc::{UnboundedReceiver, UnboundedSender},
};
use gpui_kit::component::{
    ActiveTheme, Disableable, Sizable, StyledExt,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    input::{Input, InputState},
    radio::RadioGroup,
};
use gpui_kit::*;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub struct Dashboard {
    pub visible: bool,
    home: PathBuf,
    model: Model,
    busy: bool,
    loading: bool,
    revision: u64,
    form_queue: Option<String>,
    monitor_loaded: bool,
    new_queue: Entity<InputState>,
    grace: Entity<InputState>,
    codex: Entity<InputState>,
    socket: Entity<InputState>,
    interval: Entity<InputState>,
    proxy: bool,
    monitor_enabled: bool,
    sender: UnboundedSender<UiEvent>,
    _fetch: Option<Task<()>>,
    _action: Option<Task<()>>,
    _poll: Task<()>,
    _events: Task<()>,
    #[cfg(target_os = "macos")]
    _tray: Option<tray_icon::TrayIcon>,
}
impl Dashboard {
    pub fn new(
        home: PathBuf,
        hidden: bool,
        sender: UnboundedSender<UiEvent>,
        mut events: UnboundedReceiver<UiEvent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let new_queue = cx.new(|cx| InputState::new(window, cx).placeholder("New queue name"));
        let grace = cx.new(|cx| InputState::new(window, cx).default_value("180"));
        let codex = cx.new(|cx| InputState::new(window, cx).default_value("codex"));
        let socket =
            cx.new(|cx| InputState::new(window, cx).placeholder("Public proxy socket (optional)"));
        let interval = cx.new(|cx| InputState::new(window, cx).default_value("15"));
        let poll = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(5)).await;
                if this
                    .update_in(cx, |this, window, cx| {
                        if this.visible {
                            this.refresh(window, cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let event_task = cx.spawn_in(window, async move |this, cx| {
            while let Some(event) = events.next().await {
                if this
                    .update_in(cx, |this, window, cx| match event {
                        UiEvent::Show => {
                            this.visible = true;
                            cx.activate(true);
                            window.activate_window();
                            this.refresh(window, cx);
                            cx.notify();
                        }
                        UiEvent::Quit => cx.quit(),
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        #[cfg(target_os = "macos")]
        let tray = crate::tray::create(sender.clone());
        let mut model = Model::new();
        #[cfg(target_os = "macos")]
        if let Err(error) = &tray {
            model.error = Some(format!("Menu-bar icon failed: {error}"));
        }
        let mut this = Self {
            visible: !hidden,
            home,
            model,
            busy: false,
            loading: false,
            revision: 0,
            form_queue: None,
            monitor_loaded: false,
            new_queue,
            grace,
            codex,
            socket,
            interval,
            proxy: false,
            monitor_enabled: false,
            sender,
            _fetch: None,
            _action: None,
            _poll: poll,
            _events: event_task,
            #[cfg(target_os = "macos")]
            _tray: tray.ok(),
        };
        this.refresh(window, cx);
        this
    }
    pub fn hide(&mut self, cx: &mut Context<Self>) {
        self.visible = false;
        cx.hide();
        cx.notify();
    }
    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading || self.busy {
            return;
        }
        self.loading = true;
        self.revision += 1;
        let revision = self.revision;
        let home = self.home.clone();
        self._fetch = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_spawn(async move { client::request(&home, json!({"op":"Snapshot"})) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.revision != revision {
                    return;
                }
                this.loading = false;
                this.model.accept(result);
                this.sync_forms(window, cx);
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn sync_forms(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.form_queue != self.model.selected {
            if let Some(queue) = self.model.queue() {
                let seconds = queue["unclaimed_after"].as_u64().unwrap_or(180).to_string();
                self.grace
                    .update(cx, |input, cx| input.set_value(seconds, window, cx));
            }
            self.form_queue = self.model.selected.clone();
        }
        if !self.monitor_loaded {
            let config = &self.model.snapshot["monitor"];
            if !config.is_null() {
                self.monitor_enabled = config["enabled"] == true;
                self.proxy = config["proxy"] == true;
                if let Some(path) = config["codex"].as_str() {
                    let value = path.to_owned();
                    self.codex
                        .update(cx, |input, cx| input.set_value(value, window, cx));
                }
                if let Some(path) = config["socket"].as_str() {
                    let value = path.to_owned();
                    self.socket
                        .update(cx, |input, cx| input.set_value(value, window, cx));
                }
                if let Some(seconds) = config["interval"].as_u64() {
                    self.interval.update(cx, |input, cx| {
                        input.set_value(seconds.to_string(), window, cx)
                    });
                }
                self.monitor_loaded = true;
            }
        }
    }
    fn apply(&mut self, action: Value, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.model.error = None;
        self.model.notice = None;
        self._fetch = None;
        self.loading = false;
        self.revision += 1;
        let home = self.home.clone();
        self._action=Some(cx.spawn_in(window,async move|this,cx|{
            let result=cx.background_spawn(async move{client::request(&home,json!({"op":"Manage","action":action}))}).await;
            let _=this.update_in(cx,|this,window,cx|{this.busy=false;match result{
                Ok(value)=>this.model.notice=Some(if value["state"]=="draining"{"New protected commands are stopped; waiting for existing commands to finish.".into()}else{"Queue updated.".into()}),
                Err(error)=>this.model.error=Some(error),
            }this.refresh(window,cx);cx.notify();});
        }));
        cx.notify();
    }
    fn button(
        &self,
        id: impl Into<SharedString>,
        label: &'static str,
        action: Value,
        cx: &mut Context<Self>,
    ) -> Button {
        Button::new(id.into())
            .label(label)
            .disabled(self.busy)
            .on_click(
                cx.listener(move |this, _, window, cx| this.apply(action.clone(), window, cx)),
            )
    }
    fn confirm_button(
        &self,
        id: &'static str,
        label: &'static str,
        title: &'static str,
        action: Value,
        cx: &mut Context<Self>,
    ) -> Button {
        Button::new(id)
            .label(label)
            .disabled(self.busy)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.model.confirm(title.into(), action.clone());
                cx.notify();
            }))
    }
    fn select_queue(&mut self, queue: String, window: &mut Window, cx: &mut Context<Self>) {
        self.model.selected = Some(queue);
        self.model.confirmation = None;
        self.sync_forms(window, cx);
        cx.notify();
    }
    fn create_queue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.new_queue.read(cx).value().to_string();
        let name = name.trim().to_owned();
        if name.is_empty() {
            self.model.error = Some("Enter a unique project queue name.".into());
            cx.notify();
            return;
        }
        self.model.selected = Some(name.clone());
        self.new_queue
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.apply(
            json!({"type":"policy","queue":name,"blocked":"attention","unclaimed_after":180}),
            window,
            cx,
        );
    }
    fn save_grace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(seconds) = self.grace.read(cx).value().parse::<u64>() else {
            self.model.error = Some("Claim grace must be an integer number of seconds.".into());
            cx.notify();
            return;
        };
        if let Some(queue) = self.model.queue() {
            self.apply(json!({"type":"policy","queue":queue["name"],"blocked":queue["blocked_policy"],"unclaimed_after":seconds}),window,cx);
        }
    }
    fn save_monitor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(interval) = self.interval.read(cx).value().parse::<u64>() else {
            self.model.error = Some("Monitor interval must be a number of seconds.".into());
            cx.notify();
            return;
        };
        let socket = self.socket.read(cx).value().to_string();
        self.apply(json!({"type":"configure_monitor","codex":self.codex.read(cx).value().to_string(),"proxy":self.proxy,"socket":if socket.trim().is_empty(){Value::Null}else{json!(socket)},"interval":interval,"enabled":self.monitor_enabled}),window,cx);
    }
    fn confirmation(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (title, _) = self.model.confirmation.as_ref()?;
        Some(div().v_flex().gap_3().p_4().border_1().border_color(cx.theme().warning).rounded_md()
            .child(div().font_semibold().child(title.clone()))
            .child(div().text_sm().child("Pending work is preserved when clearing completed history. Active work must be stopped before deleting a queue. Job keys and audit records remain to prevent accidental replay."))
            .child(Checkbox::new("effects-inspected").label("I inspected failed/interrupted effects").checked(self.model.inspected).on_change(cx.listener(|this,value,_,cx|{this.model.inspected= *value;cx.notify();})))
            .child(div().h_flex().gap_2()
                .child(Button::new("confirm-operation").label("Confirm").danger().disabled(self.busy).on_click(cx.listener(|this,_,window,cx|{if let Some(action)=this.model.confirmed_action(){this.apply(action,window,cx);}})))
                .child(Button::new("dismiss-operation").label("Keep queue").on_click(cx.listener(|this,_,_,cx|{this.model.confirmation=None;cx.notify();}))))
            .into_any_element())
    }
    fn job(&self, job: Value, queue: &str, order: &[i64], cx: &mut Context<Self>) -> AnyElement {
        let id = job["id"].as_i64().unwrap_or(0);
        let state = job["state"].as_str().unwrap_or("unknown").to_owned();
        let display = if !job["pending_action"].is_null() {
            "draining"
        } else {
            &state
        };
        let mut content = div()
            .v_flex()
            .gap_2()
            .py_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .h_flex()
                    .gap_3()
                    .justify_between()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .font_medium()
                            .child(format!("#{id}  {}", job["label"].as_str().unwrap_or(""))),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_sm()
                            .text_color(if matches!(state.as_str(), "failed" | "interrupted") {
                                cx.theme().danger
                            } else {
                                cx.theme().muted_foreground
                            })
                            .child(display.to_owned()),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(job["cwd"].as_str().unwrap_or("").to_owned()),
            );
        if let Some(reason) = job["hold_reason"].as_str() {
            content = content.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().warning)
                    .child(reason.to_owned()),
            );
        }
        if let Some(observation) = job.get("observation").filter(|value| !value.is_null()) {
            let age = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                .saturating_sub(observation["observed_at"].as_u64().unwrap_or(0));
            content = content.child(div().text_sm().child(format!(
                "Chat: {} — {}, {age}s ago{}",
                observation["state"].as_str().unwrap_or("unknown"),
                observation["source"].as_str().unwrap_or("unavailable"),
                if age > 60 { " (stale)" } else { "" }
            )));
            if let Some(reason) = observation["reason"].as_str() {
                content = content.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(reason.to_owned()),
                );
            }
        }
        if let Some(error) = job["delivery"]["error"].as_str() {
            content = content.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(error.to_owned()),
            );
        }
        if job["protected_failure"] == true {
            content=content.child(div().text_sm().text_color(cx.theme().danger).child("A protected command failed. The owner must inspect effects before yielding this grant."));
        }
        let mut actions = div().h_flex().gap_2().flex_wrap();
        if state == "queued"
            && let Some(index) = order.iter().position(|item| *item == id)
        {
            for (up, label) in [(true, "Move up"), (false, "Move down")] {
                if if up {
                    index > 0
                } else {
                    index + 1 < order.len()
                } {
                    let mut next = order.to_vec();
                    next.swap(index, if up { index - 1 } else { index + 1 });
                    actions = actions.child(
                        self.button(
                            format!("{label}-{id}"),
                            label,
                            json!({"type":"reorder","queue":queue,"ids":next}),
                            cx,
                        )
                        .small(),
                    );
                }
            }
        }
        if matches!(state.as_str(), "running" | "queued") {
            let legacy = state == "running"
                && !job["delivery"]["claimed_at"].is_null()
                && job["delivery"]["managed"] != true;
            let command = state == "running" && job["thread_id"].is_null();
            actions = actions
                .child(
                    self.button(
                        format!("defer-{id}"),
                        "Defer",
                        json!({"type":"defer","id":id,"reason":"Deferred by operator"}),
                        cx,
                    )
                    .small()
                    .disabled(self.busy || legacy || command),
                )
                .child(
                    self.button(
                        format!("cancel-{id}"),
                        "Cancel",
                        json!({"type":"cancel","id":id}),
                        cx,
                    )
                    .small()
                    .danger()
                    .disabled(self.busy || legacy || command),
                );
            if legacy {
                content = content.child(
                    div()
                        .text_xs()
                        .child("Legacy claimed work must be stopped and yielded by its owner."),
                );
            }
        }
        if state == "deferred" || state == "queued" && !job["hold_reason"].is_null() {
            actions = actions.child(
                self.button(
                    format!("resume-{id}"),
                    "Resume at tail",
                    json!({"type":"resume","id":id}),
                    cx,
                )
                .small(),
            );
        }
        if matches!(state.as_str(), "failed" | "interrupted") {
            let action = json!({"type":"resolve","id":id});
            actions = actions.child(
                Button::new(SharedString::from(format!("resolve-{id}")))
                    .label("Resolve after inspection")
                    .small()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.model.confirm(
                            format!("Resolve failure barrier for #{id}?"),
                            action.clone(),
                        );
                        cx.notify();
                    })),
            );
        }
        if let Some(thread) = job["thread_id"].as_str() {
            let url = format!("codex://threads/{thread}");
            actions = actions.child(
                Button::new(SharedString::from(format!("open-chat-{id}")))
                    .label("Open chat")
                    .small()
                    .ghost()
                    .on_click(move |_, _, cx| cx.open_url(&url)),
            );
        }
        content.child(actions).into_any_element()
    }
}
impl Render for Dashboard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut sidebar = div()
            .id("queue-sidebar")
            .overflow_y_scroll()
            .v_flex()
            .gap_3()
            .w(px(250.))
            .h_full()
            .p_4()
            .bg(cx.theme().sidebar)
            .border_r_1()
            .border_color(cx.theme().border)
            .child(div().text_xl().font_semibold().child("Queues"));
        for queue in self.model.queues() {
            let name = queue["name"].as_str().unwrap_or("").to_owned();
            let selected = self.model.selected.as_ref() == Some(&name);
            let button = Button::new(SharedString::from(format!("queue-{name}")))
                .label(name.clone())
                .w_full()
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_queue(name.clone(), window, cx)
                }));
            sidebar = sidebar.child(if selected {
                button.primary()
            } else {
                button.ghost()
            });
        }
        sidebar = sidebar
            .child(div().flex_1())
            .child(Input::new(&self.new_queue))
            .child(
                Button::new("create-queue")
                    .label("Create queue")
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, window, cx| this.create_queue(window, cx))),
            )
            .child(
                self.confirm_button(
                    "clear-all-history",
                    "Clear all completed",
                    "Clear completed history in all queues?",
                    json!({"type":"clear_all","history_only":true}),
                    cx,
                )
                .ghost(),
            )
            .child(
                self.confirm_button(
                    "delete-all-queues",
                    "Delete all queues",
                    "Delete all queues and cancel their pending jobs?",
                    json!({"type":"clear_all","delete":true}),
                    cx,
                )
                .ghost()
                .danger(),
            )
            .child(
                Button::new("quit-dashboard")
                    .label("Quit dashboard")
                    .ghost()
                    .on_click(cx.listener(|this, _, _, _| {
                        let _ = this.sender.unbounded_send(UiEvent::Quit);
                    })),
            );
        let mut body = div()
            .id("queue-content")
            .v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .gap_4()
            .p_6()
            .overflow_y_scroll()
            .child(
                div()
                    .h_flex()
                    .gap_2()
                    .justify_between()
                    .child(
                        div().text_2xl().font_semibold().child(
                            self.model
                                .selected
                                .clone()
                                .unwrap_or_else(|| "Monohero".into()),
                        ),
                    )
                    .child(
                        div()
                            .h_flex()
                            .gap_2()
                            .child(
                                Button::new("refresh")
                                    .label("Refresh")
                                    .disabled(self.loading || self.busy)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.model.error = None;
                                        this.refresh(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("hide")
                                    .label("Hide")
                                    .ghost()
                                    .on_click(cx.listener(|this, _, _, cx| this.hide(cx))),
                            ),
                    ),
            );
        if let Some(error) = &self.model.error {
            body = body.child(
                div()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().danger)
                    .text_color(cx.theme().danger)
                    .child(error.clone()),
            );
        }
        if let Some(notice) = &self.model.notice {
            body = body.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(notice.clone()),
            );
        }
        if let Some(confirmation) = self.confirmation(cx) {
            body = body.child(confirmation);
        }
        if let Some(queue) = self.model.queue().cloned() {
            let name = queue["name"].as_str().unwrap_or("").to_owned();
            let paused = queue["paused"] == true;
            let queue_name = name.clone();
            body=body.child(div().h_flex().gap_4().flex_wrap()
                .child(self.button("pause-queue",if paused{"Resume queue"}else{"Pause queue"},json!({"type":"pause","queue":name,"paused":!paused}),cx))
                .child(div().v_flex().gap_2().child(div().text_sm().child("Blocked chats"))
                    .child(RadioGroup::new("blocked-policy").children(["Attention list","Hold at tail"]).selected_index(Some(if queue["blocked_policy"]=="tail"{1}else{0})).on_change(cx.listener(move|this,index,window,cx|{
                        let grace=this.model.queue().and_then(|q|q["unclaimed_after"].as_u64()).unwrap_or(180);
                        this.apply(json!({"type":"policy","queue":queue_name,"blocked":if *index==1{"tail"}else{"attention"},"unclaimed_after":grace}),window,cx);
                    }))))
                .child(div().v_flex().gap_2().child(div().text_sm().child("Claim grace (seconds)"))
                    .child(div().h_flex().gap_2().child(div().w(px(86.)).child(Input::new(&self.grace))).child(Button::new("save-grace").label("Save").disabled(self.busy).on_click(cx.listener(|this,_,window,cx|this.save_grace(window,cx)))))));
            let jobs = self.model.jobs();
            let order = self.model.queued_order();
            let count = |state: &str| jobs.iter().filter(|job| job["state"] == state).count();
            let attention = jobs
                .iter()
                .filter(|job| {
                    matches!(
                        job["state"].as_str(),
                        Some("deferred" | "failed" | "interrupted")
                    ) || !job["hold_reason"].is_null()
                })
                .count();
            body = body.child(
                div()
                    .h_flex()
                    .gap_6()
                    .text_sm()
                    .child(format!("{} running", count("running")))
                    .child(format!(
                        "{} waiting",
                        jobs.iter()
                            .filter(|job| job["state"] == "queued" && job["hold_reason"].is_null())
                            .count()
                    ))
                    .child(format!("{attention} need attention")),
            );
            for group in ["Running", "Waiting", "Needs attention", "History"] {
                let filtered: Vec<Value> = jobs
                    .iter()
                    .filter(|job| match group {
                        "Running" => job["state"] == "running",
                        "Waiting" => job["state"] == "queued" && job["hold_reason"].is_null(),
                        "Needs attention" => {
                            matches!(
                                job["state"].as_str(),
                                Some("deferred" | "failed" | "interrupted")
                            ) || job["state"] == "queued" && !job["hold_reason"].is_null()
                        }
                        _ => !matches!(
                            job["state"].as_str(),
                            Some("running" | "queued" | "deferred" | "failed" | "interrupted")
                        ),
                    })
                    .cloned()
                    .collect();
                if !filtered.is_empty() {
                    body = body.child(div().text_lg().font_semibold().mt_2().child(group));
                    for job in filtered {
                        body = body.child(self.job(job, &name, &order, cx));
                    }
                }
            }
            body = body.child(
                div()
                    .h_flex()
                    .gap_2()
                    .flex_wrap()
                    .pt_3()
                    .child(self.confirm_button(
                        "clear-history",
                        "Clear completed",
                        "Clear completed history?",
                        json!({"type":"clear","queue":name,"history_only":true}),
                        cx,
                    ))
                    .child(
                        self.confirm_button(
                            "clear-queue",
                            "Clear queue",
                            "Cancel pending jobs and clear this queue?",
                            json!({"type":"clear","queue":name}),
                            cx,
                        )
                        .danger(),
                    )
                    .child(
                        self.confirm_button(
                            "delete-queue",
                            "Delete queue",
                            "Delete this queue and cancel pending jobs?",
                            json!({"type":"clear","queue":name,"delete":true}),
                            cx,
                        )
                        .danger(),
                    ),
            );
        } else {
            body = body.child("Create a queue or submit a job with the Monohero CLI.");
        }
        body=body.child(div().v_flex().gap_3().mt_4().pt_4().border_t_1().border_color(cx.theme().border)
            .child(div().text_lg().font_semibold().child("Chat monitoring"))
            .child(Checkbox::new("monitor-enabled").label("Probe chats automatically without model calls").checked(self.monitor_enabled).on_change(cx.listener(|this,value,window,cx|{this.monitor_enabled= *value;this.save_monitor(window,cx);})))
            .child(div().h_flex().gap_2().child(div().flex_1().child(Input::new(&self.codex))).child(Button::new("save-monitor").label("Save settings").disabled(self.busy).on_click(cx.listener(|this,_,window,cx|this.save_monitor(window,cx)))))
            .child(Checkbox::new("monitor-proxy").label("Use the owning Codex host's public proxy for live approval/input flags").checked(self.proxy).on_change(cx.listener(|this,value,_,cx|{this.proxy= *value;cx.notify();})))
            .child(Input::new(&self.socket))
            .child(div().h_flex().gap_2().child("Probe interval (seconds)").child(div().w(px(86.)).child(Input::new(&self.interval))))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child("History mode reports saved turns and blocked goals. Live approval/input flags require the owning server. Unknown or stale status never releases a legacy claim. Closing this window keeps the queue service running.")));
        div()
            .h_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(sidebar)
            .child(body)
    }
}

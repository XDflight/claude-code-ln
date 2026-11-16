//! The window: a sessions × organizations matrix with a pending-changes bar.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::TimeZone;
use eframe::egui::{self, Color32, RichText, Sense, Stroke, vec2};
use egui_extras::{Column, TableBuilder};

use crate::config::{self, Config};
use crate::ops::{Executor, Op};
use crate::store::{self, LinkState, OrgKey, SessionGroup, Snapshot};
use crate::{i18n, names, t, tf};

const GREEN: Color32 = Color32::from_rgb(46, 160, 67);
const AMBER: Color32 = Color32::from_rgb(214, 148, 0);
const PURPLE: Color32 = Color32::from_rgb(150, 90, 220);
const RED: Color32 = Color32::from_rgb(218, 54, 51);
const BLUE: Color32 = Color32::from_rgb(47, 129, 247);
const GRAY: Color32 = Color32::from_rgb(140, 140, 140);

#[derive(Clone, Debug, PartialEq, Eq)]
enum Pending {
    Link,
    Unlink,
    Sync { from: OrgKey },
}

/// What a matrix cell currently shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cell {
    Absent { tombstone: bool },
    Fresh,
    Stale,
    Diverged,
    Broken,
}

pub struct App {
    root: PathBuf,
    projects: Option<PathBuf>,
    config: Config,
    snapshot: Snapshot,
    scan_error: Option<String>,
    /// Kept current by a background thread so the UI never waits on a child process.
    app_running: Arc<AtomicBool>,
    pending: BTreeMap<(String, OrgKey), Pending>,
    selected: Option<String>,
    filter: String,
    alias_edits: BTreeMap<String, String>,
    /// (success, message), newest last.
    log: Vec<(bool, String)>,
}

fn format_time(ms: i64) -> String {
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "—".into())
}

fn format_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.0} KB", (bytes as f64 / 1024.0).ceil())
    }
}

fn tilde(path: &str) -> String {
    let home = dirs::home_dir().and_then(|h| h.to_str().map(str::to_owned));
    match home.as_deref().and_then(|h| path.strip_prefix(h)) {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => format!("~{rest}"),
        _ => path.to_owned(),
    }
}

impl App {
    pub fn new(root: PathBuf, projects: Option<PathBuf>) -> Self {
        let mut app = Self {
            root,
            projects,
            config: Config::load(),
            snapshot: Snapshot::default(),
            scan_error: None,
            app_running: Arc::new(AtomicBool::new(store::desktop_app_running())),
            pending: BTreeMap::new(),
            selected: None,
            filter: String::new(),
            alias_edits: BTreeMap::new(),
            log: Vec::new(),
        };
        let flag = app.app_running.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(3));
                flag.store(store::desktop_app_running(), Ordering::Relaxed);
            }
        });
        i18n::set_zh(app.config.language == "zh");
        app.rescan();
        app.learn_names();
        if std::env::var_os("CLAUDE_CODE_LN_DEMO").is_some() {
            app.demo_state();
        }
        app
    }

    /// Screenshot aid (see docs/make-demo-store.py): opens the details of a forked
    /// session and queues one change of each kind. Nothing is applied.
    fn demo_state(&mut self) {
        let groups = self.snapshot.groups.clone();
        if let Some(group) = groups.iter().find(|g| g.state() == LinkState::Diverged) {
            self.selected = Some(group.uuid.clone());
            self.queue_sync(&group.uuid, group.newest().org);
        }
        let linked: Vec<_> = groups.iter().filter(|g| g.state() == LinkState::Linked).collect();
        if let Some(group) = linked.first()
            && let Some(org) = (0..self.snapshot.orgs.len()).find(|o| group.copy_in(*o).is_none())
        {
            self.toggle_cell(&group.uuid, org);
        }
        if let Some(group) = linked.last().filter(|_| linked.len() > 1) {
            self.toggle_cell(&group.uuid, group.copies[0].org);
        }
    }

    /// Picks up account e-mails / organization names from the desktop app's local cache.
    fn learn_names(&mut self) {
        let Some(app_data_dir) = self.root.parent() else { return };
        // The cache names many other things by uuid; keep only what the store uses.
        let mut found = names::harvest(app_data_dir);
        found.retain(|uuid, _| {
            self.snapshot.orgs.iter().any(|o| o.key.account == *uuid || o.key.org == *uuid)
        });
        if self.config.learn(found) {
            self.save_config();
        }
    }

    fn save_config(&mut self) {
        if let Err(e) = self.config.save() {
            let message = tf!("Could not save settings: {}", "保存设置失败：{}", e);
            if self.log.last().is_none_or(|(_, last)| *last != message) {
                self.log.push((false, message));
            }
        }
    }

    fn rescan(&mut self) {
        match store::scan(&self.root, self.projects.as_deref()) {
            Ok(snapshot) => {
                self.snapshot = snapshot;
                self.scan_error = None;
            }
            Err(e) => {
                self.snapshot = Snapshot::default();
                self.scan_error = Some(e);
            }
        }
        self.prune_pending();
        if self.selected.as_deref().is_some_and(|u| self.snapshot.group(u).is_none()) {
            self.selected = None;
        }
    }

    fn pending_for(&self, uuid: &str, org: &OrgKey) -> Option<&Pending> {
        self.pending.get(&(uuid.to_owned(), org.clone()))
    }

    /// Copies of `group` that will still exist once the queued changes are applied.
    fn surviving_copies(&self, group: &SessionGroup) -> usize {
        let kept = group
            .copies
            .iter()
            .filter(|c| c.error.is_none())
            .filter(|c| {
                self.pending_for(&group.uuid, &self.snapshot.orgs[c.org].key)
                    != Some(&Pending::Unlink)
            })
            .count();
        let added = self
            .pending
            .iter()
            .filter(|((uuid, _), p)| *uuid == group.uuid && **p == Pending::Link)
            .count();
        kept + added
    }

    /// Drops queued changes that no longer make sense for the current snapshot.
    fn prune_pending(&mut self) {
        let snapshot = &self.snapshot;
        self.pending.retain(|(uuid, org), pending| {
            let (Some(group), Some(index)) = (snapshot.group(uuid), snapshot.org_index(org))
            else {
                return false;
            };
            let present = group.copy_in(index).is_some();
            match pending {
                Pending::Link => !present && group.copies.iter().any(|c| c.error.is_none()),
                Pending::Unlink => present,
                Pending::Sync { from } => {
                    present
                        && snapshot
                            .org_index(from)
                            .and_then(|i| group.copy_in(i))
                            .is_some_and(|c| c.error.is_none())
                }
            }
        });
        // An unlink must never leave a session without any copy.
        loop {
            let orphaning = self.pending.iter().find_map(|((uuid, org), p)| {
                let group = self.snapshot.group(uuid)?;
                (*p == Pending::Unlink && self.surviving_copies(group) == 0)
                    .then(|| (uuid.clone(), org.clone()))
            });
            match orphaning {
                Some(key) => self.pending.remove(&key),
                None => break,
            };
        }
    }

    fn cell_state(&self, group: &SessionGroup, org: usize) -> Cell {
        let Some(copy) = group.copy_in(org) else {
            return Cell::Absent {
                tombstone: self.snapshot.orgs[org].tombstones.contains(&group.uuid),
            };
        };
        if copy.error.is_some() {
            Cell::Broken
        } else if copy.cli_session_id != group.newest().cli_session_id {
            Cell::Diverged
        } else if group.is_stale(copy) {
            Cell::Stale
        } else {
            Cell::Fresh
        }
    }

    fn toggle_cell(&mut self, uuid: &str, org: usize) {
        let key = (uuid.to_owned(), self.snapshot.orgs[org].key.clone());
        if self.pending.remove(&key).is_some() {
            self.prune_pending();
            return;
        }
        let Some(group) = self.snapshot.group(uuid) else { return };
        if group.copy_in(org).is_none() {
            if group.copies.iter().any(|c| c.error.is_none()) {
                self.pending.insert(key, Pending::Link);
            } else {
                self.log.push((false, t!("This session has no readable copy to link from", "该会话没有可读取的副本，无法链接").into()));
            }
        } else {
            self.pending.insert(key.clone(), Pending::Unlink);
            if self.surviving_copies(group) == 0 {
                self.pending.remove(&key);
                self.log.push((
                    false,
                    tf!(
                        "\"{}\" has only this copy, so it cannot be unlinked (link it to another organization first to move it)",
                        "「{}」只剩这一个副本，不能取消链接（先把它链接到其它组织即可实现“移动”）",
                        group.title()
                    ),
                ));
            }
        }
    }

    /// Queues syncing every other copy of `uuid` to the copy in `from`.
    fn queue_sync(&mut self, uuid: &str, from: usize) {
        let Some(group) = self.snapshot.group(uuid) else { return };
        let from_key = self.snapshot.orgs[from].key.clone();
        for copy in &group.copies {
            if copy.org != from {
                let key = (uuid.to_owned(), self.snapshot.orgs[copy.org].key.clone());
                self.pending.insert(key, Pending::Sync { from: from_key.clone() });
            }
        }
        self.pending.remove(&(uuid.to_owned(), from_key));
        self.prune_pending();
    }

    fn build_ops(&self) -> Vec<Op> {
        self.pending
            .iter()
            .filter_map(|((uuid, org), pending)| {
                let group = self.snapshot.group(uuid)?;
                Some(match pending {
                    // A copy the user chose as the sync source is also the link source.
                    Pending::Link => Op::Link {
                        uuid: uuid.clone(),
                        from: self
                            .pending
                            .iter()
                            .find_map(|((u, _), p)| match p {
                                Pending::Sync { from } if u == uuid => Some(from.clone()),
                                _ => None,
                            })
                            .unwrap_or_else(|| self.snapshot.orgs[group.newest().org].key.clone()),
                        to: org.clone(),
                    },
                    Pending::Sync { from } => {
                        Op::Sync { uuid: uuid.clone(), from: from.clone(), to: org.clone() }
                    }
                    Pending::Unlink => Op::Unlink { uuid: uuid.clone(), org: org.clone() },
                })
            })
            .collect()
    }

    fn describe(&self, op: &Op) -> String {
        let title = |uuid: &str| {
            self.snapshot.group(uuid).map_or_else(|| uuid.to_owned(), |g| g.title().to_owned())
        };
        match op {
            Op::Link { uuid, from, to } => tf!(
                "Link \"{}\": {} → {}",
                "链接「{}」：{} → {}",
                title(uuid),
                self.config.org_label(from),
                self.config.org_label(to)
            ),
            Op::Sync { uuid, from, to } => tf!(
                "Sync \"{}\": {} → {}",
                "同步「{}」：{} → {}",
                title(uuid),
                self.config.org_label(from),
                self.config.org_label(to)
            ),
            Op::Unlink { uuid, org } => {
                tf!("Unlink \"{}\": {}", "取消链接「{}」：{}", title(uuid), self.config.org_label(org))
            }
        }
    }

    fn apply(&mut self) {
        // The snapshot may be minutes old: re-read the disk and only proceed if the
        // plan the user confirmed is still exactly the plan that would run.
        let confirmed = self.build_ops();
        self.rescan();
        let ops = self.build_ops();
        if ops != confirmed {
            self.log.push((
                false,
                t!(
                    "Sessions changed on disk since the last scan. Rescanned; review the pending changes and click Apply again.",
                    "磁盘上的会话在上次扫描后发生了变化，已重新扫描；请检查待应用的更改后再点一次“应用更改”"
                )
                .into(),
            ));
            return;
        }
        if ops.is_empty() {
            return;
        }
        let Some(data_dir) = config::data_dir() else {
            self.log.push((false, t!("No data directory available for backups; nothing was changed", "找不到用于存放备份的数据目录，未执行任何更改").into()));
            return;
        };
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        let executor =
            Executor { root: self.root.clone(), backup_dir: data_dir.join("backups").join(stamp) };
        for (op, result) in executor.run(&ops) {
            let what = self.describe(&op);
            self.log.push(match result {
                Ok(msg) => (true, format!("{what} — {msg}")),
                Err(e) => (false, tf!("{} — failed: {}", "{} — 失败：{}", what, e)),
            });
        }
        self.pending.clear();
        self.rescan();
    }

    // ---------------------------------------------------------------- panels

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.heading(t!("Claude Code Session Links", "Claude Code 会话链接"));
            ui.separator();
            if ui.button(t!("⟳ Refresh", "⟳ 刷新")).on_hover_text(t!("Rescan the disk", "重新扫描磁盘")).clicked() {
                self.rescan();
                self.learn_names();
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text(t!("Search title / folder / ID", "搜索标题 / 目录 / ID"))
                    .desired_width(220.0),
            );
            if ui.checkbox(&mut self.config.show_archived, t!("Show archived", "显示已归档")).changed() {
                self.save_config();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button(t!("中文", "English"))
                    .on_hover_text(t!("切换到中文界面", "Switch the interface to English"))
                    .clicked()
                {
                    i18n::set_zh(!i18n::zh());
                    self.config.language = if i18n::zh() { "zh" } else { "en" }.to_owned();
                    self.save_config();
                    self.rescan();
                }
            });
        });
        if self.app_running.load(Ordering::Relaxed) {
            egui::Frame::new()
                .fill(AMBER.gamma_multiply(0.18))
                .corner_radius(4.0)
                .inner_margin(6.0)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        t!(
                            "The Claude desktop app is running. It only reads session files at startup or \
                             when you switch organizations, and it overwrites the files of the current \
                             organization from memory. Quit Claude before applying changes if you can; \
                             otherwise switch organizations once afterwards, and do not change sessions \
                             of the organization you are currently in.",
                            "Claude 桌面端正在运行。它只在启动或切换组织时读取会话文件，并会用内存中的状态覆盖\
                             当前组织里的文件：建议先完全退出 Claude 再应用更改；否则请在应用更改后切换一次组织，\
                             且不要改动当前所在组织的会话。"
                        ),
                    );
                });
        }
        if let Some(error) = &self.scan_error {
            ui.colored_label(RED, error);
        }
        for warning in &self.snapshot.warnings {
            ui.colored_label(AMBER, warning);
        }
        ui.add_space(4.0);
    }

    fn alias_field(&mut self, ui: &mut egui::Ui, uuid: &str, width: f32) {
        let text = self
            .alias_edits
            .entry(uuid.to_owned())
            .or_insert_with(|| self.config.alias(uuid).unwrap_or_default().to_owned());
        let response = ui
            .add(
                egui::TextEdit::singleline(text)
                    .hint_text(
                        self.config.detected(uuid).unwrap_or_else(|| config::short(uuid)).to_owned(),
                    )
                    .desired_width(width),
            )
            .on_hover_text(tf!("{}\nType a name to override the detected one", "{}\n在此输入名称可覆盖自动识别的名称", uuid));
        if response.changed() {
            self.config.set_alias(uuid, text);
            self.save_config();
        }
    }

    fn side_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.strong(t!("Accounts & organizations", "账号与组织"));
        ui.weak(t!(
            "Names are detected from the desktop app's local cache where possible; type to override.",
            "名称会尽量从桌面端的本地缓存中识别；也可在此手动填写。"
        ));
        ui.add_space(4.0);
        let orgs: Vec<(OrgKey, usize)> =
            self.snapshot.orgs.iter().map(|o| (o.key.clone(), o.session_count)).collect();
        let mut last_account = None;
        egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
            for (key, count) in &orgs {
                if last_account.as_ref() != Some(&key.account) {
                    last_account = Some(key.account.clone());
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label(t!("Account", "账号"));
                        self.alias_field(ui, &key.account, 130.0);
                    });
                }
                ui.horizontal(|ui| {
                    ui.add_space(14.0);
                    self.alias_field(ui, &key.org, 130.0);
                    ui.weak(format!("{count}"));
                });
            }
            ui.add_space(12.0);
            ui.separator();
            ui.strong(t!("Legend (click a cell to toggle)", "图例（点击格子切换）"));
            let legend = [
                (Cell::Fresh, None, t!("Present, newest copy", "已存在，且是最新副本")),
                (Cell::Stale, None, t!("Present, metadata older than the newest copy", "已存在，元数据落后于最新副本")),
                (Cell::Diverged, None, t!("Present, but forked (points at another transcript)", "已存在，但对话已分叉（指向不同转录）")),
                (Cell::Broken, None, t!("File is broken or unreadable", "文件损坏或无法读取")),
                (Cell::Absent { tombstone: false }, None, t!("Absent", "不存在")),
                (Cell::Absent { tombstone: false }, Some(Pending::Link), t!("Will be linked", "待链接")),
                (Cell::Fresh, Some(Pending::Unlink), t!("Will be unlinked", "待取消链接")),
                (
                    Cell::Stale,
                    Some(Pending::Sync { from: OrgKey { account: String::new(), org: String::new() } }),
                    t!("Will be synced", "待同步"),
                ),
            ];
            for (cell, pending, text) in legend {
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(vec2(22.0, 18.0), Sense::hover());
                    paint_cell(ui, rect, cell, pending.as_ref());
                    ui.label(text);
                });
            }
            ui.add_space(8.0);
            ui.weak(
                t!(
                    "A link is a copy of the session's metadata in another organization's folder; all \
                     copies share one Claude Code transcript. The desktop app replaces the file on \
                     every save, so a real symlink cannot survive and copies drift apart; use Sync to \
                     realign them.\n\nDo not delete a linked session inside the desktop app (that \
                     cleans up the shared worktree and notes); use Unlink here instead. Files that \
                     are replaced or removed are moved to the backup folder.",
                    "“链接”是把会话元数据复制到另一个组织目录；各副本共享同一份 Claude Code 转录。\
                     桌面端保存时会用新文件替换旧文件，所以真正的符号链接无法存活，副本会逐渐产生差异，\
                     可用“同步”拉齐。\n\n请不要在桌面端里“删除”已链接的会话（那会清理共享的工作树与笔记），\
                     用这里的“取消链接”即可。被替换或移除的文件都会进入备份目录。"
                ),
            );
        });
    }

    fn matrix(&mut self, ui: &mut egui::Ui) {
        let needle = self.filter.trim().to_lowercase();
        let rows: Vec<usize> = self
            .snapshot
            .groups
            .iter()
            .enumerate()
            .filter(|(_, g)| self.config.show_archived || !g.all_archived())
            .filter(|(_, g)| {
                needle.is_empty()
                    || g.title().to_lowercase().contains(&needle)
                    || g.uuid.contains(&needle)
                    || g.copies.iter().any(|c| {
                        c.cwd.as_deref().is_some_and(|d| d.to_lowercase().contains(&needle))
                    })
            })
            .map(|(i, _)| i)
            .collect();

        if self.snapshot.orgs.is_empty() && self.scan_error.is_none() {
            ui.label(tf!("No organization folders found under {}.", "在 {} 下没有找到任何组织目录。", self.root.display()));
            return;
        }
        let mut clicked: Option<(String, usize)> = None;
        let mut sync: Option<(String, usize)> = None;
        let mut select: Option<String> = None;
        let org_count = self.snapshot.orgs.len();

        // Sticky header; drag a column's right edge to resize it.
        egui::ScrollArea::horizontal().auto_shrink([false, false]).show(ui, |ui| {
            let resizable = |initial: f32, least: f32| {
                Column::initial(initial).at_least(least).resizable(true).clip(true)
            };
            TableBuilder::new(ui)
                .id_salt("matrix")
                .striped(true)
                .auto_shrink([false, false])
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(resizable(250.0, 120.0))
                .columns(resizable(96.0, 36.0), org_count)
                .column(resizable(140.0, 60.0))
                .header(40.0, |mut header| {
                    header.col(|ui| {
                        ui.strong(tf!("Sessions ({})", "会话（{}）", rows.len()));
                    });
                    for org in &self.snapshot.orgs {
                        header.col(|ui| {
                            ui.vertical(|ui| {
                                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                                ui.strong(self.config.name(&org.key.org));
                                ui.weak(self.config.name(&org.key.account));
                            })
                            .response
                            .on_hover_text(tf!(
                                "{}\n{}\n\nAccount {}\nOrganization {}\n{}",
                                "{}\n{}\n\n账号 {}\n组织 {}\n{}",
                                self.config.name(&org.key.org),
                                self.config.name(&org.key.account),
                                org.key.account,
                                org.key.org,
                                org.dir.display()
                            ));
                        });
                    }
                    header.col(|ui| {
                        ui.strong(t!("Status", "状态"));
                    });
                })
                .body(|body| {
                    body.rows(44.0, rows.len(), |mut row| {
                        let group = &self.snapshot.groups[rows[row.index()]];
                        let newest = group.newest();
                        row.col(|ui| {
                            ui.vertical(|ui| {
                                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                                let selected =
                                    self.selected.as_deref() == Some(group.uuid.as_str());
                                let mut title = RichText::new(group.title());
                                if group.all_archived() {
                                    title = title.weak();
                                }
                                if ui.selectable_label(selected, title).clicked() {
                                    select = Some(group.uuid.clone());
                                }
                                ui.weak(format!(
                                    "{} · {}",
                                    format_time(newest.freshness()),
                                    newest.cwd.as_deref().map(tilde).unwrap_or_default()
                                ));
                            });
                        });
                        for org in 0..org_count {
                            row.col(|ui| {
                                let cell = self.cell_state(group, org);
                                let pending =
                                    self.pending_for(&group.uuid, &self.snapshot.orgs[org].key);
                                let (rect, response) = ui.allocate_exact_size(
                                    vec2(ui.available_width().min(34.0), 26.0),
                                    Sense::click(),
                                );
                                if response.hovered() {
                                    ui.painter().rect_filled(
                                        rect,
                                        4.0,
                                        ui.visuals().widgets.hovered.weak_bg_fill,
                                    );
                                }
                                paint_cell(ui, rect, cell, pending);
                                let response = response.on_hover_text(format!(
                                    "{}\n{}\n{}",
                                    self.config.org_label(&self.snapshot.orgs[org].key),
                                    group.title(),
                                    cell_hint(cell, pending)
                                ));
                                if response.clicked() {
                                    clicked = Some((group.uuid.clone(), org));
                                }
                            });
                        }
                        row.col(|ui| {
                            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                            let (text, color) = match group.state() {
                                LinkState::Single => (t!("Not linked", "未链接").to_owned(), GRAY),
                                LinkState::Linked => {
                                    (tf!("Linked ×{}", "已链接 ×{}", group.copies.len()), GREEN)
                                }
                                LinkState::Diverged => (t!("Forked", "已分叉").to_owned(), PURPLE),
                                LinkState::Broken => {
                                    (t!("No readable copy", "无可读副本").to_owned(), RED)
                                }
                            };
                            ui.colored_label(color, text);
                            let out_of_sync = group.copies.len() > 1
                                && newest.error.is_none()
                                && group.copies.iter().any(|c| {
                                    c.error.is_some()
                                        || group.is_stale(c)
                                        || c.cli_session_id != newest.cli_session_id
                                });
                            if out_of_sync
                                && ui
                                    .small_button(t!("Sync", "同步"))
                                    .on_hover_text(t!(
                                        "Overwrite the other copies with the newest one (old copies are backed up first)",
                                        "把最新副本覆盖到其它副本（旧副本会先备份）"
                                    ))
                                    .clicked()
                            {
                                sync = Some((group.uuid.clone(), newest.org));
                            }
                        });
                    });
                });
        });

        if let Some(uuid) = select {
            self.selected = if self.selected.as_ref() == Some(&uuid) { None } else { Some(uuid) };
        }
        if let Some((uuid, org)) = clicked {
            self.toggle_cell(&uuid, org);
        }
        if let Some((uuid, from)) = sync {
            self.queue_sync(&uuid, from);
        }
    }

    fn details(&mut self, ui: &mut egui::Ui, uuid: &str) {
        let Some(group) = self.snapshot.group(uuid) else { return };
        let mut sync_from = None;
        ui.horizontal(|ui| {
            ui.strong(group.title());
            ui.weak(format!("local_{}", group.uuid));
        });
        egui::Grid::new("details").striped(true).spacing(vec2(16.0, 4.0)).show(ui, |ui| {
            for header in [
                t!("Organization", "组织"),
                t!("Last activity", "最后活动"),
                t!("Transcript (cliSessionId)", "转录 (cliSessionId)"),
                t!("Transcript file", "转录文件"),
                t!("Metadata", "元数据"),
                "",
            ] {
                ui.weak(header);
            }
            ui.end_row();
            for copy in &group.copies {
                let org = &self.snapshot.orgs[copy.org];
                ui.label(self.config.org_label(&org.key))
                    .on_hover_text(copy.path.display().to_string());
                if let Some(error) = &copy.error {
                    ui.colored_label(RED, error);
                    ui.end_row();
                    continue;
                }
                ui.label(format_time(copy.freshness()));
                let cli = copy.cli_session_id.as_deref().unwrap_or("—");
                ui.monospace(config::short(cli)).on_hover_text(cli);
                match self.snapshot.transcripts.get(cli) {
                    Some(t) => ui
                        .label(format_size(t.size))
                        .on_hover_text(t.path.display().to_string()),
                    None => ui.colored_label(AMBER, t!("not found", "未找到")),
                };
                let mut meta = format_size(copy.size);
                if copy.archived {
                    meta.push_str(t!(" · archived", " · 已归档"));
                }
                if copy.is_symlink {
                    meta.push_str(t!(" · symlink", " · 符号链接"));
                }
                ui.label(meta);
                if group.copies.len() > 1
                    && ui
                        .small_button(t!("Sync from this copy", "以此为准同步"))
                        .on_hover_text(t!("Overwrite every other copy with this one (old copies are backed up first)", "把这个副本覆盖到其它所有副本（旧副本会先备份）"))
                        .clicked()
                {
                    sync_from = Some(copy.org);
                }
                ui.end_row();
            }
        });
        if let Some(from) = sync_from {
            self.queue_sync(uuid, from);
        }
    }

    fn bottom_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        if let Some(uuid) = self.selected.clone() {
            self.details(ui, &uuid);
            ui.separator();
        }
        let ops = self.build_ops();
        if !ops.is_empty() {
            ui.horizontal(|ui| {
                ui.strong(tf!("Pending changes ({})", "待应用的更改（{}）", ops.len()));
                if ui.button(RichText::new(t!("Apply changes", "应用更改")).color(GREEN).strong()).clicked() {
                    self.apply();
                }
                if ui.button(t!("Discard", "放弃")).clicked() {
                    self.pending.clear();
                }
            });
            egui::ScrollArea::vertical().id_salt("ops").max_height(90.0).show(ui, |ui| {
                for op in &ops {
                    ui.label(format!("• {}", self.describe(op)));
                }
            });
        } else if self.log.is_empty() {
            ui.weak(t!("Click cells to queue links and unlinks, then apply them together.", "点击格子来排队“链接 / 取消链接”，确认后再统一应用。"));
        }
        if !self.log.is_empty() {
            ui.horizontal(|ui| {
                ui.weak(t!("Recent results", "最近结果"));
                if ui.small_button(t!("Clear", "清空")).clicked() {
                    self.log.clear();
                }
            });
            egui::ScrollArea::vertical()
                .id_salt("log")
                .max_height(70.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for (ok, message) in &self.log {
                        ui.colored_label(if *ok { GREEN } else { RED }, message);
                    }
                });
        }
        ui.add_space(4.0);
    }
}

fn cell_hint(cell: Cell, pending: Option<&Pending>) -> &'static str {
    match pending {
        Some(Pending::Link) => t!("Will be linked here (click again to cancel)", "待链接到此组织（再次点击取消）"),
        Some(Pending::Unlink) => t!("Will be unlinked from here (click again to cancel)", "待从此组织取消链接（再次点击取消）"),
        Some(Pending::Sync { .. }) => t!("Will be synced (click again to cancel)", "待同步（再次点击取消）"),
        None => match cell {
            Cell::Absent { tombstone: true } => {
                t!(
                    "This session was once deleted in this organization (a tombstone exists; it does not prevent linking). Click to link here",
                    "此组织中曾删除过该会话（存在墓碑文件，不影响链接）。点击：链接到此组织"
                )
            }
            Cell::Absent { tombstone: false } => t!("Click to link here", "点击：链接到此组织"),
            Cell::Fresh => t!("Newest copy. Click to unlink", "最新副本。点击：取消链接"),
            Cell::Stale => t!("Metadata is older than the newest copy. Click to unlink", "元数据落后于最新副本。点击：取消链接"),
            Cell::Diverged => t!("Forked: points at another transcript. Click to unlink", "对话已分叉，指向另一份转录。点击：取消链接"),
            Cell::Broken => t!("File is broken or unreadable. Click to unlink", "文件损坏或无法读取。点击：取消链接"),
        },
    }
}

fn paint_cell(ui: &egui::Ui, rect: egui::Rect, cell: Cell, pending: Option<&Pending>) {
    let painter = ui.painter();
    let center = rect.center();
    let radius = 7.0;
    let arm = vec2(4.0, 0.0);
    match pending {
        Some(Pending::Link) => {
            painter.circle_stroke(center, radius, Stroke::new(2.0, GREEN));
            painter.line_segment([center - arm, center + arm], Stroke::new(2.0, GREEN));
            painter.line_segment([center - arm.yx(), center + arm.yx()], Stroke::new(2.0, GREEN));
            return;
        }
        Some(Pending::Unlink) => {
            painter.circle_stroke(center, radius, Stroke::new(2.0, RED));
            painter.line_segment([center - arm, center + arm], Stroke::new(2.0, RED));
            return;
        }
        _ => {}
    }
    match cell {
        Cell::Absent { tombstone } => {
            painter.circle_stroke(center, radius - 2.0, Stroke::new(1.0, GRAY.gamma_multiply(0.6)));
            if tombstone {
                painter.circle_filled(center, 1.5, GRAY.gamma_multiply(0.6));
            }
        }
        Cell::Fresh => {
            painter.circle_filled(center, radius, GREEN);
        }
        Cell::Stale => {
            painter.circle_filled(center, radius, AMBER);
        }
        Cell::Diverged => {
            painter.circle_filled(center, radius, PURPLE);
        }
        Cell::Broken => {
            painter.circle_filled(center, radius, RED);
            let d = vec2(3.0, 3.0);
            let white = Stroke::new(1.5, Color32::WHITE);
            painter.line_segment([center - d, center + d], white);
            painter.line_segment([center - vec2(3.0, -3.0), center + vec2(3.0, -3.0)], white);
        }
    }
    if matches!(pending, Some(Pending::Sync { .. })) {
        painter.circle_stroke(center, radius + 3.0, Stroke::new(2.0, BLUE));
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.ctx().request_repaint_after(std::time::Duration::from_secs(3));

        egui::Panel::top("top").show(ui, |ui| self.top_bar(ui));
        egui::Panel::bottom("bottom").show(ui, |ui| self.bottom_bar(ui));
        egui::Panel::left("side")
            .default_size(250.0)
            .show(ui, |ui| self.side_panel(ui));
        egui::CentralPanel::default().show(ui, |ui| self.matrix(ui));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const A: &str = "aaaaaaaa-0000-4000-8000-000000000001";
    const O1: &str = "bbbbbbbb-0000-4000-8000-000000000001";
    const O2: &str = "bbbbbbbb-0000-4000-8000-000000000002";
    const S: &str = "cccccccc-0000-4000-8000-000000000001";

    #[test]
    fn pending_queue_never_orphans_a_session() {
        let base = std::env::temp_dir().join(format!("ccln-ui-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        for org in [O1, O2] {
            fs::create_dir_all(base.join(A).join(org)).unwrap();
        }
        fs::write(
            base.join(A).join(O1).join(store::session_file_name(S)),
            format!(r#"{{"sessionId":"local_{S}","cliSessionId":"x","lastActivityAt":5}}"#),
        )
        .unwrap();
        let mut app = App::new(base.clone(), None);
        assert_eq!(app.snapshot.orgs.len(), 2);

        // The only copy cannot be unlinked.
        app.toggle_cell(S, 0);
        assert!(app.pending.is_empty());
        // Link to the second org, then unlinking the first becomes a valid "move".
        app.toggle_cell(S, 1);
        app.toggle_cell(S, 0);
        assert_eq!(app.build_ops().len(), 2);
        // Cancelling the link also cancels the now-orphaning unlink.
        app.toggle_cell(S, 1);
        assert!(app.pending.is_empty());

        let _ = fs::remove_dir_all(&base);
    }
}

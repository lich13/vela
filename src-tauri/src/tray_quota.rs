//! The tray icon's menu: what is left of the quota of every account that is
//! signed in, and Quit. Each time the menu opens the accounts' own services
//! are asked, and the rows change as the answers come, also while the menu is
//! open. Nothing is asked while it is closed; what someone else saves of an
//! Antigravity account meanwhile is listed.
use crate::{
    core,
    gateway::ClientId,
    runtime::Runtime,
    usage::{
        antigravity::quota::{Periods, Without},
        claude_quota, codex_quota, commands, grok_quota, QuotaRound, Remaining,
    },
};
use parking_lot::Mutex;
use serde::Serialize;
use std::sync::Arc;
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    AppHandle, Wry,
};
use tokio::sync::broadcast::error::RecvError;

/// The ID of the menu's Quit.
pub(crate) const QUIT: &str = "quit";
/// What the ID of every row of the quota starts with. One that can be chosen
/// opens the usage page.
pub(crate) const ROW: &str = "quota-";
/// Where the rows of the quota start in the menu: they are its first, and
/// its separator and Quit come after them.
const FIRST: usize = 0;
/// How many cells a bar has where the menu shows text alone.
const CELLS: usize = 8;
/// How many it has where the menu draws the row.
const DRAWN: usize = 12;
const READING: &str = "读取中…";
const NOBODY: &str = "没有已登录的官方账号";
/// What every row under a title starts with.
const INDENT: &str = "    ";

/// The clients, in the order the menu lists them.
#[derive(Clone, Copy)]
enum Client {
    Codex,
    Claude,
    Antigravity,
    Grok,
}

/// What is being done about a client's accounts.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Reading {
    #[default]
    Idle,
    /// They are being listed, and nobody is asked.
    Listing,
    /// They are being listed, and the menu opened meanwhile: once they are,
    /// each is asked.
    ListingThenAsking,
    /// They are being listed and each asked.
    Asking,
}
impl Reading {
    /// What follows when the accounts are to be read, with `ask` each asked
    /// for its quota as well, and whether a reading is to be started for it.
    /// One that is under way is not started again: a listing goes on to ask
    /// where it was asked to, and an asking is left to answer.
    fn begin(self, ask: bool) -> (Self, bool) {
        match (self, ask) {
            (Self::Idle, false) => (Self::Listing, true),
            (Self::Idle, true) => (Self::Asking, true),
            (Self::Listing, true) => (Self::ListingThenAsking, false),
            (under_way, _) => (under_way, false),
        }
    }
    /// What follows when a reading has done what it set out to, and whether
    /// it goes on to ask.
    fn end(self) -> (Self, bool) {
        match self {
            Self::ListingThenAsking => (Self::Asking, true),
            _ => (Self::Idle, false),
        }
    }
}
/// An account as the menu lists it.
#[derive(Clone, Debug, Default, PartialEq)]
struct Account {
    /// What tells it from the other accounts of its client.
    id: String,
    title: Title,
    /// The periods each row under the title shows.
    lines: Vec<Line>,
    /// What is said under them: that the account is being read, or why it
    /// could not be.
    note: Option<String>,
}
/// Whose the rows under it are.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
struct Title {
    client: &'static str,
    name: Option<String>,
    plan: Option<String>,
    /// The account the client is signed in to, where it lists more than one.
    current: bool,
}
impl Title {
    /// `client · name（plan）`, with a mark on the account signed in to.
    fn text(&self) -> String {
        let mut text = self.client.to_owned();
        if let Some(name) = &self.name {
            text.push_str(" · ");
            text.push_str(name);
        }
        if let Some(plan) = &self.plan {
            text.push_str(&format!("（{plan}）"));
        }
        if self.current {
            text.push_str(" ✓");
        }
        text
    }
}
#[derive(Clone, Debug, Default, PartialEq)]
struct Line {
    /// What the periods are of, where an account has more than one row.
    name: Option<&'static str>,
    /// What each period is called and the percentage left of it, where the
    /// service said.
    periods: Vec<(String, Option<f64>)>,
}
impl Line {
    fn of(periods: Vec<Remaining>) -> Self {
        Self {
            name: None,
            periods: periods
                .into_iter()
                .map(|period| (period.label, Some(period.percent)))
                .collect(),
        }
    }
}
impl Account {
    /// The account could not be read, and why is said under what was read of
    /// it before, which still holds. A login that is `over` is the exception:
    /// only that is said of it.
    fn failed(&mut self, why: String, over: bool) {
        if over {
            self.lines.clear();
        }
        self.note = Some(why);
    }
}
/// A row of the menu.
#[derive(Clone, Debug, PartialEq)]
struct Row {
    /// What it says. Where the menu shows text alone this is the row; where
    /// it draws the row from `style`, this is still what is read out and
    /// searched.
    text: String,
    /// A title only says whose the rows under it are, and cannot be chosen.
    enabled: bool,
    style: Style,
}
/// How a row is drawn where the menu draws more than text. The menu is told
/// as JSON: see `native/tray_menu.swift`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum Style {
    Title(Title),
    /// The periods of an account, and what they are of where the account has
    /// more than one row of them.
    Detail {
        name: Option<&'static str>,
        periods: Vec<Gauge>,
    },
    /// Whatever else is said: that an account is being read, why it could
    /// not be, that nobody is signed in. It is drawn as the text it is.
    Plain,
}
/// A period in a row: what it is called and what is left of it, where the
/// service said.
#[derive(Clone, Debug, PartialEq, Serialize)]
struct Gauge {
    label: String,
    left: Option<Left>,
}
/// What is left of a period, as a number and a bar. The number is a whole
/// percentage, and the bar says what the number does: it is empty only where
/// that reads 0% and full only where it reads 100%, and has from one cell to
/// all but one between the two.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
struct Left {
    percent: u8,
    /// How many cells of the bar that is drawn are filled.
    filled: usize,
    tone: Tone,
}
/// What the colour of a bar says of how little is left. It goes by the
/// number, as the bar does.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
enum Tone {
    Normal,
    /// A fifth is left, or less.
    Orange,
    /// A tenth, or less.
    Red,
}
impl Left {
    fn of(percent: f64) -> Self {
        let percent = percent.round().clamp(0.0, 100.0) as u8;
        Self {
            percent,
            filled: filled(percent, DRAWN),
            tone: match percent {
                0..=10 => Tone::Red,
                11..=20 => Tone::Orange,
                _ => Tone::Normal,
            },
        }
    }
    /// The number and then the bar, where the menu shows text alone. The
    /// number is as wide as three digits, so that the bar starts at the same
    /// place in every row.
    fn text(&self) -> String {
        let number = format!("{}%", self.percent);
        let filled = filled(self.percent, CELLS);
        format!(
            "{}{number} {}{}",
            "\u{2007}".repeat(4 - number.chars().count()),
            "▰".repeat(filled),
            "▱".repeat(CELLS - filled),
        )
    }
}
/// How many of `cells` are filled where the number reads `percent`.
fn filled(percent: u8, cells: usize) -> usize {
    match percent {
        0 => 0,
        100.. => cells,
        _ => ((f64::from(percent) / 100.0 * cells as f64).round() as usize).clamp(1, cells - 1),
    }
}
/// A row of periods: what it says, and how it is drawn. Each period is what
/// it is called, the number and the bar, and one the service gave no number
/// for says so. What the periods are of comes last, so that the periods of
/// every row stand under each other.
fn detail(line: &Line) -> (String, Style) {
    let periods: Vec<Gauge> = line
        .periods
        .iter()
        .map(|(label, percent)| Gauge {
            label: label.clone(),
            left: percent.map(Left::of),
        })
        .collect();
    let mut text = periods
        .iter()
        .map(|period| match &period.left {
            Some(left) => format!("{} {}", period.label, left.text()),
            None => format!("{} —", period.label),
        })
        .collect::<Vec<_>>()
        .join("   ");
    if let Some(name) = line.name {
        text.push_str("   ");
        text.push_str(name);
    }
    let style = Style::Detail {
        name: line.name,
        periods,
    };
    (text, style)
}
fn rows(accounts: &[Account]) -> Vec<Row> {
    if accounts.is_empty() {
        return vec![Row {
            text: NOBODY.into(),
            enabled: false,
            style: Style::Plain,
        }];
    }
    let mut rows = Vec::new();
    for account in accounts {
        rows.push(Row {
            text: account.title.text(),
            enabled: false,
            style: Style::Title(account.title.clone()),
        });
        let mut under: Vec<(String, Style)> = account
            .lines
            .iter()
            .filter(|line| !line.periods.is_empty())
            .map(detail)
            .collect();
        under.extend(account.note.clone().map(|note| (note, Style::Plain)));
        if under.is_empty() {
            under.push(("—".into(), Style::Plain));
        }
        rows.extend(under.into_iter().map(|(text, style)| Row {
            text: format!("{INDENT}{text}"),
            enabled: true,
            style,
        }));
    }
    rows
}
/// The title of an account of `client`, with a mark on the account signed in
/// to where the client lists more than one.
fn title(client: ClientId, name: Option<&str>, plan: Option<&str>, current: bool) -> Title {
    Title {
        client: client.name(),
        name: name.map(str::to_owned),
        plan: plan.map(str::to_owned),
        current,
    }
}
/// A plan as Claude Code's login names it, the way the other plans are written.
fn capitalized(plan: &str) -> String {
    let mut letters = plan.chars();
    letters
        .next()
        .map(|first| first.to_uppercase().chain(letters).collect())
        .unwrap_or_default()
}
/// The rows of an Antigravity account. A quota the account does not have is
/// left out.
fn antigravity_lines(periods: &Periods) -> Vec<Line> {
    [
        ("Gemini", "gemini-5h", "gemini-weekly"),
        ("Claude / GPT", "3p-5h", "3p-weekly"),
    ]
    .into_iter()
    .map(|(name, burst, weekly)| Line {
        name: Some(name),
        periods: [("5h", burst), ("7d", weekly)]
            .into_iter()
            .filter_map(|(label, key)| {
                let period = periods
                    .get(key)
                    .filter(|period| period.without != Some(Without::Disabled))?;
                Some((label.to_owned(), period.remaining_percent))
            })
            .collect(),
    })
    .collect()
}
/// The account Grok Build is signed in to after its service was asked, none
/// where Grok holds no official login. `earlier` is the account as it was
/// listed before: what was read with another login is another account's and
/// is not kept. A period the service could not be asked for stays as it was
/// read before, and why it could not is said under them.
fn grok_account(earlier: Option<Account>, quota: Option<grok_quota::Quota>) -> Option<Account> {
    let quota = quota?;
    let mut account = earlier
        .filter(|earlier| earlier.id == quota.login)
        .unwrap_or_else(|| Account {
            id: quota.login.clone(),
            title: title(ClientId::Grok, None, None, false),
            ..Account::default()
        });
    if quota.week.is_none() && quota.month.is_none() {
        if let Some(why) = quota.failed {
            let over = why == grok_quota::OVER;
            account.failed(why, over);
            return Some(account);
        }
    }
    let read = account
        .lines
        .pop()
        .map(|line| line.periods)
        .unwrap_or_default();
    let periods = [("周", quota.week), ("月", quota.month)]
        .into_iter()
        .filter_map(|(label, percent)| match percent {
            Some(percent) => Some((label.to_owned(), Some(percent))),
            // A question was not answered, and this may be what it would
            // have told.
            None if quota.failed.is_some() => read.iter().find(|(was, _)| was == label).cloned(),
            // Both were answered: the account has no such period.
            None => None,
        })
        .collect();
    account.lines = vec![Line {
        name: None,
        periods,
    }];
    account.note = quota.failed;
    Some(account)
}
/// The account with `id` as it was listed before, its title apart: what was
/// read of it stays until it is read again, so that a menu opened again does
/// not go blank first.
fn listed(earlier: &[Account], id: String, title: Title) -> Account {
    let earlier = earlier.iter().find(|account| account.id == id);
    let lines = earlier
        .map(|account| account.lines.clone())
        .unwrap_or_default();
    let note = if lines.iter().all(|line| line.periods.is_empty()) {
        Some(READING.to_owned())
    } else {
        earlier.and_then(|account| account.note.clone())
    };
    Account {
        id,
        title,
        lines,
        note,
    }
}

pub(crate) struct Tray {
    app: AppHandle,
    runtime: Arc<Runtime>,
    menu: Menu<Wry>,
    /// Each client's accounts, the clients in the order they are listed.
    accounts: Mutex<[Vec<Account>; 4]>,
    /// The rows as the menu has them, above its separator. They are touched
    /// on the main thread alone.
    shown: Mutex<Vec<(Row, MenuItem<Wry>)>>,
    /// What is being done about each client's accounts.
    reading: Mutex<[Reading; 4]>,
    /// What the last asking of the Antigravity accounts came to.
    antigravity: Mutex<Option<Result<QuotaRound, String>>>,
    /// One listing of the Antigravity accounts at a time, so that what an
    /// earlier one read does not replace what a later one did.
    listing: tokio::sync::Mutex<()>,
}

impl Tray {
    /// Builds the menu, with the accounts that can be listed without asking
    /// anyone. Called on the main thread.
    pub(crate) fn new(app: &AppHandle, runtime: Arc<Runtime>) -> tauri::Result<Arc<Self>> {
        let menu = Menu::new(app)?;
        menu.append(&PredefinedMenuItem::separator(app)?)?;
        menu.append(&MenuItem::with_id(
            app,
            QUIT,
            "退出 Vela",
            true,
            None::<&str>,
        )?)?;
        let tray = Arc::new(Self {
            app: app.clone(),
            runtime,
            menu,
            accounts: Mutex::default(),
            shown: Mutex::default(),
            reading: Mutex::default(),
            antigravity: Mutex::default(),
            listing: Default::default(),
        });
        tray.draw()?;
        tray.read(false);
        // Whoever asked, the menu shows what is saved: the usage page and the
        // reading of the sessions save an account's periods as well, and an
        // asking saves each account's as it is answered.
        let mut saved = tray.runtime.usage.subscribe_antigravity_quota();
        let listener = tray.clone();
        tauri::async_runtime::spawn(async move {
            while let Ok(()) | Err(RecvError::Lagged(_)) = saved.recv().await {
                listener.antigravity_saved().await;
            }
        });
        Ok(tray)
    }
    pub(crate) fn menu(&self) -> &Menu<Wry> {
        &self.menu
    }
    /// The menu is about to open: every client's accounts are asked.
    pub(crate) fn opened(self: &Arc<Self>) {
        self.read(true);
    }

    /// Lists every client's accounts again and, with `ask`, asks each for its
    /// quota. A client that is still being asked is left to that asking, and
    /// one that is only being listed is asked once it has been.
    fn read(self: &Arc<Self>, ask: bool) {
        for client in [
            Client::Codex,
            Client::Claude,
            Client::Antigravity,
            Client::Grok,
        ] {
            if !self.step(client, |reading| reading.begin(ask)) {
                continue;
            }
            let tray = self.clone();
            tauri::async_runtime::spawn(async move {
                let mut ask = ask;
                loop {
                    match client {
                        Client::Codex => tray.codex(ask).await,
                        Client::Antigravity => tray.antigravity(ask).await,
                        // Whether these two are signed in is only learned by
                        // asking.
                        Client::Claude if ask => tray.claude().await,
                        Client::Grok if ask => tray.grok().await,
                        Client::Claude | Client::Grok => {}
                    }
                    if !tray.step(client, Reading::end) {
                        break;
                    }
                    ask = true;
                }
            });
        }
    }
    /// Moves what is being done about `client` on as `how` says, and tells
    /// what it says to do next. Both are one hold of the lock, so that a menu
    /// opened in between is neither lost nor answered twice.
    fn step(&self, client: Client, how: impl FnOnce(Reading) -> (Reading, bool)) -> bool {
        let mut reading = self.reading.lock();
        let (next, go) = how(reading[client as usize]);
        reading[client as usize] = next;
        go
    }

    fn set(self: &Arc<Self>, client: Client, change: impl FnOnce(&mut Vec<Account>)) {
        change(&mut self.accounts.lock()[client as usize]);
        let tray = self.clone();
        // The main thread takes this while the menu is open as well: see
        // `tray_macos::context_menu`.
        let _ = self.app.run_on_main_thread(move || {
            let _ = tray.draw();
        });
    }

    /// Makes the menu's rows what the accounts are now. An item stays where
    /// there is one, so a menu that is open changes in place.
    fn draw(&self) -> tauri::Result<()> {
        let rows = rows(&self.accounts.lock().concat());
        let mut shown = self.shown.lock();
        for (index, row) in rows.iter().enumerate() {
            let Some((was, item)) = shown.get_mut(index) else {
                let item = MenuItem::with_id(
                    &self.app,
                    format!("{ROW}{index}"),
                    &row.text,
                    row.enabled,
                    None::<&str>,
                )?;
                self.menu.insert(&item, FIRST + index)?;
                shown.push((row.clone(), item));
                continue;
            };
            if was.text != row.text {
                item.set_text(&row.text)?;
                was.text.clone_from(&row.text);
            }
            if was.enabled != row.enabled {
                item.set_enabled(row.enabled)?;
                was.enabled = row.enabled;
            }
            was.style.clone_from(&row.style);
        }
        while shown.len() > rows.len() {
            let Some((_, item)) = shown.pop() else { break };
            self.menu.remove(&item)?;
        }
        Ok(())
    }

    async fn codex(self: &Arc<Self>, ask: bool) {
        let runtime = self.runtime.clone();
        let Ok(logins) =
            tauri::async_runtime::spawn_blocking(move || runtime.core.lock().quota_logins()).await
        else {
            return;
        };
        let named = |login: &core::QuotaLogin, plan: Option<&str>| {
            title(
                ClientId::Codex,
                Some(&login.name),
                plan,
                login.current && logins.len() > 1,
            )
        };
        self.set(Client::Codex, |accounts| {
            *accounts = logins
                .iter()
                .map(|login| {
                    let title = named(login, login.plan.as_deref());
                    listed(accounts, login.key.clone(), title)
                })
                .collect();
        });
        if !ask {
            return;
        }
        futures_util::future::join_all(logins.iter().map(|login| async {
            let answer = codex_quota::quota(login).await;
            self.set(Client::Codex, |accounts| {
                let Some(account) = accounts.iter_mut().find(|account| account.id == login.key)
                else {
                    return;
                };
                match answer {
                    Ok(quota) => {
                        account.title = named(login, quota.plan.as_deref());
                        account.lines = vec![Line::of(quota.periods)];
                        account.note = None;
                    }
                    Err(error) => {
                        let over = error.message == codex_quota::OVER;
                        account.failed(error.message, over);
                    }
                }
            });
        }))
        .await;
    }

    async fn claude(self: &Arc<Self>) {
        let Ok(view) = commands::get_claude_usage_quota(&self.runtime, true).await else {
            return;
        };
        self.set(Client::Claude, |accounts| {
            accounts.clear();
            if view.error.as_deref() == Some(claude_quota::SIGNED_OUT) {
                return;
            }
            let periods = [("five_hour", "5h"), ("seven_day", "7d")]
                .into_iter()
                .filter_map(|(id, label)| {
                    let window = view.windows.iter().find(|window| window.id == id)?;
                    Some((label.to_owned(), Some(100.0 - window.used_percent)))
                })
                .collect();
            accounts.push(Account {
                id: String::new(),
                title: title(
                    ClientId::Claude,
                    view.account.as_deref(),
                    view.plan.as_deref().map(capitalized).as_deref(),
                    false,
                ),
                lines: vec![Line {
                    name: None,
                    periods,
                }],
                note: view.error,
            });
        });
    }

    async fn antigravity(self: &Arc<Self>, ask: bool) {
        self.antigravity_saved().await;
        if !ask {
            return;
        }
        let round = self.runtime.usage.refresh_antigravity_quota().await;
        *self.antigravity.lock() = Some(round.map_err(|error| error.message));
        self.antigravity_saved().await;
    }
    /// Lists the saved accounts with the periods last saved for each, which
    /// the usage page and the session reading save as well.
    async fn antigravity_saved(self: &Arc<Self>) {
        let _one = self.listing.lock().await;
        let usage = self.runtime.usage.clone();
        let Ok(Ok(saved)) =
            tauri::async_runtime::spawn_blocking(move || usage.antigravity_quotas()).await
        else {
            return;
        };
        let round = self.antigravity.lock().clone();
        let several = saved.len() > 1;
        self.set(Client::Antigravity, |accounts| {
            *accounts = saved
                .into_iter()
                .map(|(account, periods)| {
                    let lines = antigravity_lines(&periods);
                    let current = matches!(
                        &round,
                        Some(Ok(round)) if round.current.as_deref() == Some(account.id.as_str())
                    );
                    let note = match &round {
                        None if lines.iter().all(|line| line.periods.is_empty()) => {
                            Some(READING.to_owned())
                        }
                        None => None,
                        Some(Ok(round)) => round.failed.get(&account.id).cloned(),
                        Some(Err(error)) => Some(error.clone()),
                    };
                    Account {
                        title: title(ClientId::Agy, Some(&account.name), None, current && several),
                        id: account.id,
                        lines,
                        note,
                    }
                })
                .collect();
        });
    }

    async fn grok(self: &Arc<Self>) {
        let Ok(home) = self.runtime.home(ClientId::Grok) else {
            return;
        };
        let quota = grok_quota::quota(&home).await;
        self.set(Client::Grok, |accounts| {
            let account = grok_account(accounts.pop(), quota);
            accounts.extend(account);
        });
    }
}

#[cfg(test)]
#[path = "tray_quota_tests.rs"]
mod tests;

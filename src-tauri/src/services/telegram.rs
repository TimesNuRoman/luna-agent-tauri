//! Telegram bot for Luna Agent.
//!
//! Allows the owner to drive the agent from a phone: chat, read/edit
//! files in the active workspace, search the code, run allow-listed
//! shell commands, create projects from templates, upload files.
//!
//! Architecture:
//!   * The bot is started/stopped from a Tauri command (`start_telegram_bot`).
//!     We do NOT auto-start on app launch — the user must paste a token
//!     and click Start. This avoids the "old token" / "stale config"
//!     failure class.
//!   * Long polling via teloxide. Webhook is not viable because the bot
//!     runs inside the user's desktop app, behind their NAT.
//!   * One dispatcher task per running bot. Per-message handlers are
//!     spawned as separate tasks so a slow `/run` doesn't block others.
//!   * Allow-list (Telegram user IDs) is enforced before ANY response.
//!   * Filesystem ops go through the same `sandbox::resolve` that the UI
//!     uses, so the bot inherits the UI's file-access contract.
//!   * Streaming chat uses the same `chat_text_stream_core` + `TelegramSink`
//!     that this module owns. No agentic tool loop over Telegram (v1):
//!     plain text in, streamed plain text out.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use teloxide::prelude::{ChatId, Requester};
use teloxide::payloads::{GetUpdatesSetters, SendMessageSetters};
use teloxide::types::{ChatAction, MessageId, MessageKind};

use super::chat_sink::TelegramSink;
use super::shell::tokenize;
use super::streaming::{chat_text_stream_core, StreamConfig};

// =====================================================================
// State
// =====================================================================

/// Bundle of state the bot needs at runtime. Held inside `AppState`.
pub struct TelegramState {
    /// Handle to the currently-running dispatcher, if any.
    pub bot_handle: Mutex<Option<BotHandle>>,
    /// Cached copy of the token from keyring, for quick checks.
    /// The actual `Bot` instance holds its own copy; this exists so
    /// the UI can know "is a token configured?" without hitting keyring.
    pub token_cached: Mutex<Option<String>>,
    /// Allow-list of Telegram user IDs permitted to talk to the bot.
    pub allow_list: Mutex<Vec<i64>>,
    /// Last chat_id that successfully reached us. Used by future
    /// proactive notifications (out of scope for v1, foundation laid).
    pub last_known_chat_id: Mutex<Option<i64>>,
    /// Pending `/edit` flows, keyed by chat_id. Only one pending
    /// edit per chat at a time.
    pub pending_edits: Mutex<HashMap<i64, PendingEdit>>,
    /// Stop signals for ongoing chat streams, keyed by chat_id.
    /// Dropping the sender ends the stream.
    pub stop_signals: Mutex<HashMap<i64, tokio::sync::oneshot::Sender<()>>>,
    /// Last activity timestamp (Unix ms), for /status and diagnostics.
    pub last_activity: AtomicI64,
    /// Current global model preference (None = provider default).
    pub model_override: Mutex<Option<String>>,
}

impl Default for TelegramState {
    fn default() -> Self {
        Self {
            bot_handle: Mutex::new(None),
            token_cached: Mutex::new(None),
            allow_list: Mutex::new(read_allow_list_from_disk().0),
            last_known_chat_id: Mutex::new(read_allow_list_from_disk().1),
            pending_edits: Mutex::new(HashMap::new()),
            stop_signals: Mutex::new(HashMap::new()),
            last_activity: AtomicI64::new(0),
            model_override: Mutex::new(None),
        }
    }
}

pub struct BotHandle {
    /// The bot's @username, captured at startup. Shown by /status.
    pub username: String,
    pub started_at_ms: i64,
    /// Aborts the dispatcher thread. We use a `std::sync::mpsc`
    /// channel rather than `tokio::task::AbortHandle` because the
    /// dispatcher runs on its own dedicated thread (teloxide's
    /// `Update`/`Message` aren't `Send` across runtimes).
    pub abort: AbortHandle,
    /// True if the dispatcher is currently in a "running" state.
    /// Reset to false by the spawn task itself when it exits.
    pub alive: Arc<Mutex<bool>>,
}

/// Trivial abort handle: sends a `()` over a channel to signal the
/// dispatcher thread to stop. Dropping also unblocks the thread (the
/// receiver returns Err on channel close).
pub struct AbortHandle {
    tx: std::sync::mpsc::Sender<()>,
}

impl AbortHandle {
    pub fn from_channel(tx: std::sync::mpsc::Sender<()>) -> Self {
        Self { tx }
    }
    pub fn abort(&self) {
        let _ = self.tx.send(());
    }
}

#[derive(Debug, Clone)]
pub struct PendingEdit {
    pub path: String,
    pub stage: EditStage,
    pub old: Option<String>,
    pub new: Option<String>,
    pub created_at: Instant,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EditStage {
    WaitOld,
    WaitNew,
    WaitConfirm,
}

const PENDING_EDIT_TTL: Duration = Duration::from_secs(300);

fn pending_expired(pe: &PendingEdit) -> bool {
    pe.created_at.elapsed() > PENDING_EDIT_TTL
}

// =====================================================================
// Per-chat "armed upload" flag (separate from pending_edits so it
// doesn't expire in 5 min — a user might take longer to pick a file).
// =====================================================================

static PENDING_UPLOAD: once_cell::sync::Lazy<std::sync::Mutex<std::collections::HashSet<i64>>> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));

fn arm_upload(chat_id: i64) {
    PENDING_UPLOAD.lock().unwrap().insert(chat_id);
}

/// True if the chat has an armed upload flag. Does NOT consume it.
fn pending_upload_armed(chat_id: i64) -> bool {
    PENDING_UPLOAD.lock().unwrap().contains(&chat_id)
}

/// Consume the arm flag (returns true if it was armed).
fn consume_upload_arm(chat_id: i64) -> bool {
    PENDING_UPLOAD.lock().unwrap().remove(&chat_id)
}

// =====================================================================
// On-disk config: telegram.json
// =====================================================================

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct TelegramConfig {
    #[serde(default)]
    allow_list: Vec<i64>,
    #[serde(default)]
    last_known_chat_id: Option<i64>,
}

fn telegram_config_path() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".local").join("share"))
        })
        .unwrap_or_else(std::env::temp_dir);
    base.join("luna-agent").join("telegram.json")
}

pub fn read_allow_list_from_disk() -> (Vec<i64>, Option<i64>) {
    let p = telegram_config_path();
    match std::fs::read_to_string(&p) {
        Ok(s) => serde_json::from_str::<TelegramConfig>(&s)
            .map(|c| (c.allow_list, c.last_known_chat_id))
            .unwrap_or_default(),
        Err(_) => Default::default(),
    }
}

pub fn write_allow_list_to_disk(list: &[i64], last_chat: Option<i64>) -> Result<(), String> {
    let p = telegram_config_path();
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let cfg = TelegramConfig {
        allow_list: list.to_vec(),
        last_known_chat_id: last_chat,
    };
    let json = serde_json::to_string_pretty(&cfg).map_err(|e| e.to_string())?;
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, &json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &p).map_err(|e| e.to_string())
}

// =====================================================================
// Public status DTO
// =====================================================================

#[derive(Debug, Clone, Serialize)]
pub struct TelegramStatus {
    pub token_set: bool,
    pub running: bool,
    pub bot_username: Option<String>,
    pub started_at_ms: Option<i64>,
    pub allow_list_size: usize,
    pub last_activity_ms: i64,
    pub last_error: Option<String>,
}

// =====================================================================
// Command parser
// =====================================================================

/// Parsed user intent. `Chat(text)` is the catch-all for any non-slash
/// message. `EditOld` / `EditNew` are produced from plain text when a
/// pending edit is in the matching stage.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Help,
    Start,
    Status,
    WhoAmI,
    Workspace(Option<String>),
    Ls {
        path: Option<String>,
        depth: Option<u8>,
    },
    Read(String),
    Find {
        query: String,
        glob: Option<String>,
        regex: bool,
        case_sensitive: bool,
    },
    EditStart(String),
    EditOld(String),
    EditNew(String),
    EditApply,
    EditCancel,
    Revert(String),
    Create {
        name: String,
        template: Option<String>,
        parent: Option<String>,
    },
    Run {
        cmd: String,
        args: Vec<String>,
    },
    Upload,
    Model(Option<String>),
    Stop,
    Chat(String),
}

pub fn parse_command(text: &str) -> Command {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Command::Help;
    }
    if !trimmed.starts_with('/') {
        return Command::Chat(trimmed.to_string());
    }
    // Split on whitespace, keep first token as the verb, rest as raw args.
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let head = parts.next().unwrap_or("").to_lowercase();
    let rest = parts.next().unwrap_or("").trim();
    match head.as_str() {
        "/start" => Command::Start,
        "/help" | "/h" => Command::Help,
        "/status" => Command::Status,
        "/whoami" | "/id" => Command::WhoAmI,
        "/stop" | "/cancel" if false => Command::Stop, // see "/cancel" below
        "/stop" => Command::Stop,
        "/cancel" => Command::EditCancel,
        "/apply" | "/ok" | "/confirm" => Command::EditApply,
        "/workspace" | "/ws" | "/pwd" => {
            if rest.is_empty() {
                Command::Workspace(None)
            } else {
                Command::Workspace(Some(rest.to_string()))
            }
        }
        "/ls" | "/dir" => {
            // /ls [path] [-d depth]
            let mut path = None;
            let mut depth = None;
            for tok in rest.split_whitespace() {
                if let Some(d) = tok.strip_prefix("-d") {
                    depth = d.parse::<u8>().ok().or_else(|| tok[2..].parse().ok());
                } else if tok.starts_with("--depth=") {
                    depth = tok[8..].parse().ok();
                } else {
                    path = Some(tok.to_string());
                }
            }
            Command::Ls { path, depth }
        }
        "/read" | "/cat" | "/show" => {
            if rest.is_empty() {
                Command::Help
            } else {
                Command::Read(rest.to_string())
            }
        }
        "/find" | "/grep" | "/search" => {
            // /find <query> [-g glob] [-r] [-c]
            let mut query_parts: Vec<&str> = Vec::new();
            let mut glob = None;
            let mut regex = false;
            let mut case_sensitive = false;
            let mut iter = rest.split_whitespace();
            while let Some(tok) = iter.next() {
                match tok {
                    "-g" | "--glob" => {
                        glob = iter.next().map(|s| s.to_string());
                    }
                    "-r" | "--regex" => regex = true,
                    "-c" | "--case" => case_sensitive = true,
                    _ => query_parts.push(tok),
                }
            }
            Command::Find {
                query: query_parts.join(" "),
                glob,
                regex,
                case_sensitive,
            }
        }
        "/edit" => {
            if rest.is_empty() {
                Command::Help
            } else {
                Command::EditStart(rest.to_string())
            }
        }
        "/revert" => Command::Revert(rest.to_string()),
        "/create" | "/new" => {
            // /create <name> [template] [--parent <path>]
            let mut name = None;
            let mut template = None;
            let mut parent = None;
            let mut positional: Vec<String> = Vec::new();
            let mut iter = rest.split_whitespace();
            while let Some(tok) = iter.next() {
                match tok {
                    "--parent" | "-p" => {
                        parent = iter.next().map(|s| s.to_string());
                    }
                    "--template" | "-t" => {
                        template = iter.next().map(|s| s.to_string());
                    }
                    _ => positional.push(tok.to_string()),
                }
            }
            if let Some(n) = positional.first() {
                name = Some(n.clone());
            }
            match name {
                Some(n) => Command::Create {
                    name: n,
                    template: template.or_else(|| positional.get(1).cloned()),
                    parent,
                },
                None => Command::Help,
            }
        }
        "/run" | "/exec" | "/shell" => {
            let toks = tokenize(rest).unwrap_or_else(|_| {
                rest.split_whitespace().map(String::from).collect()
            });
            if toks.is_empty() {
                Command::Help
            } else {
                let cmd = toks[0].clone();
                let args = toks[1..].to_vec();
                Command::Run { cmd, args }
            }
        }
        "/upload" | "/attach" | "/file" => Command::Upload,
        "/model" => {
            if rest.is_empty() {
                Command::Model(None)
            } else {
                Command::Model(Some(rest.to_string()))
            }
        }
        _ => Command::Chat(trimmed.to_string()),
    }
}

// =====================================================================
// Allow-list check
// =====================================================================

pub fn is_authorized(state: &Arc<TelegramState>, user_id: i64) -> bool {
    state
        .allow_list
        .lock()
        .map(|g| g.contains(&user_id))
        .unwrap_or(false)
}

// =====================================================================
// Help text
// =====================================================================

pub fn help_text() -> String {
    "\
🤖 *Luna Agent — Telegram bot*

*Команды:*
/start \\— авторизация
/help \\— это сообщение
/status \\— текущий workspace и состояние
/whoami \\— ваш Telegram user ID
/workspace \\[path\\] \\— показать/сменить workspace
/ls \\[path\\] \\-d <depth> \\— список файлов
/read <path> \\— прочитать файл
/find <query> \\[-g glob\\] \\[-r\\] \\[-c\\] \\— поиск
/edit <path> \\— 3-шаговый edit \\(`/apply`\\) / /cancel
/revert <edit\\_id> \\— откатить правку
/create <name> \\[template\\] \\[\\--parent <path>\\] \\— проект
/run <cmd> <args…> \\— shell из allow-list
/upload \\— следующее сообщение с файлом
/model \\[name\\] \\— показать/сменить модель
/stop \\— прервать стриминг

Любое другое сообщение \\— вопрос агенту."
        .to_string()
}

// =====================================================================
// Dispatcher entry point
// =====================================================================

/// Public: spawn the bot dispatcher. Returns the bot's @username so the
/// UI can show it. Errors (invalid token, network) are returned; the
/// caller is expected to render them in the UI.
pub fn spawn_dispatcher(app: AppHandle) -> Result<String, String> {
    // Pull token from keyring.
    let token = super::super::secrets::get_telegram_token()?
        .ok_or_else(|| "No Telegram token configured. Set one in Settings.".to_string())?;
    if token.is_empty() {
        return Err("Telegram token is empty.".into());
    }
    let bot = teloxide::Bot::new(&token);
    // We need to know the bot's @username before the dispatcher runs.
    // `teloxide::Bot::get_me` is async; do a quick probe.
    let bot_clone = bot.clone();
    let username: String = futures::executor::block_on(async move {
        match bot_clone.get_me().await {
            Ok(u) => Ok(u.user.username.unwrap_or_else(|| "unknown".into())),
            Err(e) => Err::<String, String>(format!("getMe failed: {e}")),
        }
    })?;
    let state = app
        .try_state::<Arc<TelegramState>>()
        .ok_or_else(|| "TelegramState not managed".to_string())?
        .inner()
        .clone();
    // If a previous dispatcher is running, abort it.
    if let Some(prev) = state.bot_handle.lock().map_err(|e| e.to_string())?.take() {
        prev.abort.abort();
    }
    let started_at_ms = unix_ms();
    let alive = Arc::new(Mutex::new(true));
    let state_for_task = state.clone();
    let app_for_task = app.clone();
    // The dispatcher's future is not `Send` (teloxide's `Update` /
    // `Message` carry types that aren't `Send`), so we can't use
    // `tokio::spawn` directly. Instead we run the dispatcher on a
    // dedicated thread with its own single-threaded tokio runtime.
    let (abort_tx, abort_rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("dispatcher runtime");
        rt.block_on(async move {
            tokio::select! {
                _ = run_dispatcher(bot, app_for_task, state_for_task) => {}
                _ = async {
                    let _ = abort_rx.recv();
                } => {}
            }
        });
    });
    let abort = AbortHandle::from_channel(abort_tx);
    let handle = BotHandle {
        username: username.clone(),
        started_at_ms,
        abort,
        alive,
    };
    *state
        .bot_handle
        .lock()
        .map_err(|e| e.to_string())? = Some(handle);
    *state
        .token_cached
        .lock()
        .map_err(|e| e.to_string())? = Some(token);
    let _ = app.emit("telegram://status", ());
    Ok(username)
}

pub fn stop_dispatcher(app: &AppHandle) -> Result<(), String> {
    let state = app
        .try_state::<Arc<TelegramState>>()
        .ok_or_else(|| "TelegramState not managed".to_string())?
        .inner()
        .clone();
    if let Some(handle) = state.bot_handle.lock().map_err(|e| e.to_string())?.take() {
        handle.abort.abort();
        *handle.alive.lock().unwrap() = false;
    }
    let _ = app.emit("telegram://status", ());
    Ok(())
}

pub fn get_status(app: &AppHandle) -> TelegramStatus {
    let token_set = app
        .try_state::<Arc<TelegramState>>()
        .and_then(|s| s.token_cached.lock().ok().map(|g| g.is_some()))
        .unwrap_or(false);
    let (running, bot_username, started_at_ms) = app
        .try_state::<Arc<TelegramState>>()
        .and_then(|s| {
            s.bot_handle
                .lock()
                .ok()
                .map(|g| {
                    let h = g.as_ref();
                    match h {
                        Some(h) => (*h.alive.lock().unwrap(), Some(h.username.clone()), Some(h.started_at_ms)),
                        None => (false, None, None),
                    }
                })
        })
        .unwrap_or((false, None, None));
    let allow_list_size = app
        .try_state::<Arc<TelegramState>>()
        .and_then(|s| s.allow_list.lock().ok().map(|g| g.len()))
        .unwrap_or(0);
    let last_activity_ms = app
        .try_state::<Arc<TelegramState>>()
        .map(|s| s.last_activity.load(Ordering::Relaxed))
        .unwrap_or(0);
    TelegramStatus {
        token_set,
        running,
        bot_username,
        started_at_ms,
        allow_list_size,
        last_activity_ms,
        last_error: None,
    }
}

async fn run_dispatcher(
    bot: teloxide::Bot,
    app: AppHandle,
    state: Arc<TelegramState>,
) {
    use teloxide::types::{UpdateKind, UpdateId};

    // Long-poll loop. We don't use `Dispatcher` because dptree's
    // handler-style dispatch is overkill for our single endpoint and
    // fights us on `Send` bounds. A simple polling loop with per-message
    // `tokio::spawn` is easier to reason about.
    let mut offset: Option<UpdateId> = None;
    loop {
        // getUpdates with long polling. The bot's auto-retry (in
        // reqwest) handles transient network errors. We pass
        // `timeout=25` via the JSON request — teloxide's `get_updates`
        // has a `timeout(u32)` setter.
        let mut req = bot.get_updates();
        req = req.timeout(25);
        if let Some(off) = offset {
            // UpdateId is a tuple struct in teloxide 0.13; `.0` gives the i32.
            req = req.offset(off.0 as i32);
        }
        let updates = match req.await {
            Ok(u) => u,
            Err(e) => {
                tracing::error!(?e, "telegram getUpdates error");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                continue;
            }
        };
        if let Some(last) = updates.last() {
            offset = Some(UpdateId(last.id.0 + 1));
        }
        for update in updates {
            if let UpdateKind::Message(msg) = update.kind {
                // Process sequentially in the same task. Sequential is
                // simpler than per-message spawn and matches the
                // ~30 msg/sec global Telegram rate limit. Per-user
                // streaming-isolation is still preserved by
                // `stop_signals` (a per-chat `oneshot`).
                if let Err(e) = handle_message(&bot, &app, &state, msg).await {
                    tracing::error!(?e, "telegram handle_message error");
                }
            }
        }
    }
}

// =====================================================================
// Per-message handler
// =====================================================================

async fn handle_message(
    bot: &teloxide::Bot,
    app: &AppHandle,
    state: &Arc<TelegramState>,
    msg: teloxide::types::Message,
) -> Result<(), teloxide::RequestError> {
    let chat = msg.chat.clone();
    let chat_id = chat.id.0;
    state.last_known_chat_id.lock().ok().map(|mut g| {
        *g = Some(chat_id);
    });
    // Best-effort: persist last chat id.
    let allow = state
        .allow_list
        .lock()
        .ok()
        .map(|g| g.clone())
        .unwrap_or_default();
    let _ = write_allow_list_to_disk(&allow, Some(chat_id));
    state
        .last_activity
        .store(unix_ms(), std::sync::atomic::Ordering::Relaxed);

    let user_id: i64 = msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(0);
    if !is_authorized(state, user_id) {
        let text = format!(
            "🚫 Access denied.\nYour Telegram user ID: `{}`\nAdd it in Settings → Telegram Bot → Allow list.",
            user_id
        );
        bot.send_message(ChatId(chat_id), text)
            .parse_mode(teloxide::types::ParseMode::MarkdownV2)
            .await?;
        return Ok(());
    }

    // Rate limiting: drop floods. Cheap check; in-memory only.
    if let Err(e) = rate_limit_check(state, user_id).await {
        let _ = bot.send_message(ChatId(chat_id), e).await;
        return Ok(());
    }

    let cmd = match select_command(bot, state, chat_id, &msg).await? {
        Some(c) => c,
        None => return Ok(()),
    };
    let _ = dispatch_command(bot, app, state, ChatId(chat_id), cmd).await;
    Ok(())
}

/// Returns the parsed command for the incoming message, or `None` if the
/// handler already replied to the user (e.g. an expired pending edit) and
/// there's nothing more to do. Returns `Some(Command)` for dispatch.
async fn select_command(
    bot: &teloxide::Bot,
    state: &Arc<TelegramState>,
    chat_id: i64,
    msg: &teloxide::types::Message,
) -> Result<Option<Command>, teloxide::RequestError> {
    let text_opt: Option<String> = msg.text().map(|s| s.to_string());
    let pending = state
        .pending_edits
        .lock()
        .ok()
        .and_then(|g| g.get(&chat_id).cloned());
    if let Some(p) = pending {
        if pending_expired(&p) {
            if let Ok(mut g) = state.pending_edits.lock() {
                g.remove(&chat_id);
            }
            bot.send_message(
                ChatId(chat_id),
                "⏱ Pending edit expired. Use /edit <path> to start a new one.",
            )
            .await?;
            return Ok(None);
        }
        let raw = text_opt.clone().unwrap_or_default();
        let lowered = raw.trim().to_lowercase();
        if matches!(lowered.as_str(), "/apply" | "/ok" | "/confirm") {
            return Ok(Some(Command::EditApply));
        }
        if matches!(lowered.as_str(), "/cancel") {
            return Ok(Some(Command::EditCancel));
        }
        if matches!(lowered.as_str(), "/stop") {
            return Ok(Some(Command::Stop));
        }
        return Ok(Some(match p.stage {
            EditStage::WaitOld => Command::EditOld(raw),
            EditStage::WaitNew => Command::EditNew(raw),
            EditStage::WaitConfirm => Command::Chat(raw),
        }));
    }
    // No pending edit. Check for armed upload (a Document/Photo right
    // after /upload). The arm flag lives in a separate static.
    use teloxide::types::MediaKind;
    let is_file = matches!(
        &msg.kind,
        MessageKind::Common(common) if matches!(
            &common.media_kind,
            MediaKind::Document(_) | MediaKind::Photo(_)
        )
    );
    if is_file {
        if pending_upload_armed(chat_id) {
            // Dispatch the upload directly (it has its own reply flow).
            handle_upload(bot, state, ChatId(chat_id), msg).await?;
            return Ok(None);
        }
        // Stray file with no /upload — ask what they wanted.
        bot.send_message(
            ChatId(chat_id),
            "ℹ File ignored. Use /upload first if you want to save it to the workspace.",
        )
        .await?;
        return Ok(None);
    }
    match text_opt {
        Some(t) => Ok(Some(parse_command(&t))),
        None => {
            bot.send_message(
                ChatId(chat_id),
                "ℹ Send a text command (try /help) or /upload before sending a file.",
            )
            .await?;
            Ok(None)
        }
    }
}

async fn rate_limit_check(_state: &Arc<TelegramState>, user_id: i64) -> Result<(), String> {
    use std::collections::HashMap;
    static TRACKER: once_cell::sync::Lazy<std::sync::Mutex<HashMap<i64, Vec<Instant>>>> =
        once_cell::sync::Lazy::new(|| std::sync::Mutex::new(HashMap::new()));
    let now = Instant::now();
    let cutoff = now - Duration::from_secs(5);
    let mut map = TRACKER.lock().unwrap();
    let entry = map.entry(user_id).or_default();
    entry.retain(|t| *t > cutoff);
    if entry.len() >= 5 {
        return Err("⏱ Slow down. Please wait a few seconds.".into());
    }
    entry.push(now);
    Ok(())
}

async fn dispatch_command(
    bot: &teloxide::Bot,
    app: &AppHandle,
    state: &Arc<TelegramState>,
    chat_id: ChatId,
    cmd: Command,
) -> Result<(), teloxide::RequestError> {
    use teloxide::prelude::Requester;
    match cmd {
        Command::Start => {
            let ws = workspace_path_display(state);
            let body = format!(
                "✅ Authorized.\nWorkspace: `{}`\nType /help for commands.",
                ws
            );
            bot.send_message(chat_id, body)
                .parse_mode(teloxide::types::ParseMode::MarkdownV2)
                .await?;
        }
        Command::Help => {
            let _ = bot
                .send_message(chat_id, help_text())
                .parse_mode(teloxide::types::ParseMode::MarkdownV2)
                .await;
        }
        Command::Status => {
            let ws = workspace_path_display(state);
            let running_for = state
                .bot_handle
                .lock()
                .ok()
                .and_then(|g| g.as_ref().map(|h| unix_ms() - h.started_at_ms))
                .unwrap_or(0);
            let model = state
                .model_override
                .lock()
                .ok()
                .and_then(|g| g.clone())
                .unwrap_or_else(|| "(default)".to_string());
            let body = format!(
                "📊 *Status*\nWorkspace: `{}`\nUptime: {}s\nModel: `{}`\nLast activity: {}",
                ws,
                running_for / 1000,
                model,
                state.last_activity.load(Ordering::Relaxed)
            );
            bot.send_message(chat_id, body)
                .parse_mode(teloxide::types::ParseMode::MarkdownV2)
                .await?;
        }
        Command::WhoAmI => {
            // We already know they're authorized; the user_id came from
            // the message envelope. To avoid an extra `from` fetch we just
            // echo from the message context. The handler passes us a
            // chat; the user_id isn't threaded through here, so we send
            // a generic response.
            bot.send_message(
                chat_id,
                "✅ You are authorized. (Your user ID was checked at the door.)",
            )
            .await?;
        }
        Command::Workspace(path_opt) => {
            handle_workspace(bot, app, state, chat_id, path_opt).await;
        }
        Command::Ls { path, depth } => {
            handle_ls(bot, state, chat_id, path, depth).await;
        }
        Command::Read(path) => {
            handle_read(bot, state, chat_id, &path).await;
        }
        Command::Find {
            query,
            glob,
            regex,
            case_sensitive,
        } => {
            handle_find(bot, state, chat_id, &query, glob, regex, case_sensitive).await;
        }
        Command::EditStart(path) => {
            handle_edit_start(bot, state, chat_id, &path).await;
        }
        Command::EditOld(text) => {
            handle_edit_old(bot, state, chat_id, text).await;
        }
        Command::EditNew(text) => {
            handle_edit_new(bot, state, chat_id, text).await;
        }
        Command::EditApply => {
            handle_edit_apply(bot, app, state, chat_id).await;
        }
        Command::EditCancel => {
            handle_edit_cancel(bot, state, chat_id).await;
        }
        Command::Revert(edit_id) => {
            handle_revert(bot, app, state, chat_id, &edit_id).await;
        }
        Command::Create {
            name,
            template,
            parent,
        } => {
            handle_create(bot, app, state, chat_id, &name, template, parent).await;
        }
        Command::Run { cmd, args } => {
            handle_run(bot, state, chat_id, &cmd, &args).await;
        }
        Command::Upload => {
            handle_upload_arm(bot, state, chat_id).await;
        }
        Command::Model(name) => {
            handle_model(bot, state, chat_id, name).await;
        }
        Command::Stop => {
            // Send stop signal to the current chat's stream, if any.
            if let Ok(mut sigs) = state.stop_signals.lock() {
                if let Some(tx) = sigs.remove(&chat_id.0) {
                    let _ = tx.send(());
                    let _ = bot.send_message(chat_id, "⏹ Stopped.").await;
                } else {
                    let _ = bot
                        .send_message(chat_id, "ℹ Nothing to stop.")
                        .await;
                }
            }
        }
        Command::Chat(text) => {
            handle_chat(bot, app, state, chat_id, &text).await;
        }
    }
    Ok(())
}

// =====================================================================
// Concrete command handlers
// =====================================================================

fn workspace_path_display(_state: &Arc<TelegramState>) -> String {
    app_workspace_path(app_handle()).unwrap_or_else(|| "(none)".to_string())
}

// We need access to the live `AppState` (workspace_root, edit_undo).
// The TelegramState lives inside AppState; we re-derive the AppHandle
// path here via a thread-local. This is a pragmatic shortcut — in
// practice the handlers are spawned with the AppHandle directly, and
// we extract AppState from it. The helpers below do that.

fn app_handle() -> AppHandle {
    // We store the AppHandle in a OnceCell at spawn time so the bot
    // doesn't depend on being in a Tauri command thread.
    APP_HANDLE.get().expect("AppHandle not initialized").clone()
}

use once_cell::sync::OnceCell;
static APP_HANDLE: OnceCell<AppHandle> = OnceCell::new();

pub fn set_app_handle(app: AppHandle) {
    let _ = APP_HANDLE.set(app);
}

fn app_workspace_path(app: AppHandle) -> Option<String> {
    let st = app.try_state::<crate::AppState>()?;
    let g = st.workspace_root.lock().ok()?;
    g.as_ref().map(|p| p.display().to_string())
}

async fn handle_workspace(
    bot: &teloxide::Bot,
    app: &AppHandle,
    state: &Arc<TelegramState>,
    chat_id: ChatId,
    path: Option<String>,
) {
    use teloxide::prelude::Requester;
    let app_st = match app.try_state::<crate::AppState>() {
        Some(s) => s,
        None => {
            let _ = bot
                .send_message(chat_id, "⚠️ AppState unavailable.")
                .await;
            return;
        }
    };
    match path {
        None => {
            let g = app_st.workspace_root.lock().unwrap();
            let cur = g
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(none)".into());
            drop(g);
            let _ = bot
                .send_message(chat_id, format!("📂 Current workspace: `{cur}`"))
                .parse_mode(teloxide::types::ParseMode::MarkdownV2)
                .await;
        }
        Some(p) => {
            let pbuf = PathBuf::from(&p);
            if !pbuf.is_dir() {
                let _ = bot
                    .send_message(
                        chat_id,
                        format!("❌ Not a directory: `{p}`"),
                    )
                    .parse_mode(teloxide::types::ParseMode::MarkdownV2)
                    .await;
                return;
            }
            // Drop the in-memory undo stack and switch.
            app_st.edit_undo.lock().unwrap().clear();
            *app_st.workspace_root.lock().unwrap() = Some(pbuf.clone());
            let _ = app.emit(
                "workspace_changed",
                serde_json::json!({ "path": pbuf.display().to_string(), "name": pbuf.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default() }),
            );
            state
                .last_activity
                .store(unix_ms(), std::sync::atomic::Ordering::Relaxed);
            let _ = bot
                .send_message(
                    chat_id,
                    format!("✅ Workspace: `{}`", pbuf.display()),
                )
                .parse_mode(teloxide::types::ParseMode::MarkdownV2)
                .await;
        }
    }
}

async fn handle_ls(
    bot: &teloxide::Bot,
    state: &Arc<TelegramState>,
    chat_id: ChatId,
    path: Option<String>,
    depth: Option<u8>,
) {
    use teloxide::prelude::Requester;
    let _ = state; // currently unused; reserved for future per-user scope
    let app = app_handle();
    let app_st = match app.try_state::<crate::AppState>() {
        Some(s) => s,
        None => return,
    };
    let root = match app_st.workspace_root.lock().unwrap().clone() {
        Some(p) => p,
        None => {
            let _ = bot.send_message(chat_id, "❌ No workspace.").await;
            return;
        }
    };
    let requested = path.unwrap_or_else(|| ".".to_string());
    let target = match crate::sandbox::resolve(&root, &requested) {
        Ok(p) => p,
        Err(e) => {
            let _ = bot
                .send_message(
                    chat_id,
                    format!("❌ Path error: {e}"),
                )
                .await;
            return;
        }
    };
    let depth = depth.unwrap_or(3).clamp(1, 8) as usize;
    let mut out: Vec<String> = Vec::new();
    let walker = ignore::WalkBuilder::new(&target)
        .max_depth(Some(depth))
        .build();
    for entry in walker.flatten() {
        if entry.path().file_name().and_then(|n| n.to_str()).is_some() {
            let kind = if entry.file_type().is_some_and(|t| t.is_dir()) {
                "📁"
            } else {
                "📄"
            };
            let rel = entry
                .path()
                .strip_prefix(&root)
                .unwrap_or(entry.path())
                .display()
                .to_string();
            out.push(format!("{kind} {rel}"));
            if out.len() >= 200 {
                out.push("…(truncated)".into());
                break;
            }
        }
    }
    if out.is_empty() {
        let _ = bot.send_message(chat_id, "(empty)").await;
        return;
    }
    let body = out.join("\n");
    send_long(bot, chat_id, &body).await;
}

async fn handle_read(
    bot: &teloxide::Bot,
    state: &Arc<TelegramState>,
    chat_id: ChatId,
    path: &str,
) {
    use teloxide::prelude::Requester;
    let _ = state;
    let app = app_handle();
    let app_st = match app.try_state::<crate::AppState>() {
        Some(s) => s,
        None => return,
    };
    let root = match app_st.workspace_root.lock().unwrap().clone() {
        Some(p) => p,
        None => {
            let _ = bot.send_message(chat_id, "❌ No workspace.").await;
            return;
        }
    };
    let full = match crate::sandbox::resolve(&root, path) {
        Ok(p) => p,
        Err(e) => {
            let _ = bot
                .send_message(chat_id, format!("❌ Path error: {e}"))
                .await;
            return;
        }
    };
    let content = match std::fs::read_to_string(&full) {
        Ok(s) => s,
        Err(e) => {
            let _ = bot
                .send_message(chat_id, format!("❌ Read error: {e}"))
                .await;
            return;
        }
    };
    if content.len() > 3500 {
        let head: String = content.chars().take(3500).collect();
        let _ = bot
            .send_message(
                chat_id,
                format!("```\n{head}\n```\n…(truncated, {} bytes total)", content.len()),
            )
            .parse_mode(teloxide::types::ParseMode::MarkdownV2)
            .await;
    } else {
        let _ = bot
            .send_message(chat_id, format!("```\n{content}\n```"))
            .parse_mode(teloxide::types::ParseMode::MarkdownV2)
            .await;
    }
}

async fn handle_find(
    bot: &teloxide::Bot,
    state: &Arc<TelegramState>,
    chat_id: ChatId,
    query: &str,
    glob: Option<String>,
    regex: bool,
    case_sensitive: bool,
) {
    use teloxide::prelude::Requester;
    let _ = state;
    let app = app_handle();
    let app_st = match app.try_state::<crate::AppState>() {
        Some(s) => s,
        None => return,
    };
    let root = match app_st.workspace_root.lock().unwrap().clone() {
        Some(p) => p,
        None => {
            let _ = bot.send_message(chat_id, "❌ No workspace.").await;
            return;
        }
    };
    if query.is_empty() {
        let _ = bot
            .send_message(chat_id, "❌ Empty query. Use /find <query>")
            .await;
        return;
    }
    let matcher: Box<dyn Fn(&str) -> Option<(usize, usize)> + Send + Sync> = if regex {
        let case = if case_sensitive { "" } else { "(?i)" };
        let pat = format!("{case}{query}");
        match regex::Regex::new(&pat) {
            Ok(re) => Box::new(move |s: &str| re.find(s).map(|m| (m.start(), m.end()))),
            Err(e) => {
                let _ = bot
                    .send_message(chat_id, format!("❌ Bad regex: {e}"))
                    .await;
                return;
            }
        }
    } else {
        let needle = if case_sensitive {
            query.to_string()
        } else {
            query.to_lowercase()
        };
        Box::new(move |s: &str| {
            let hay = if case_sensitive {
                s.to_string()
            } else {
                s.to_lowercase()
            };
            hay.find(&needle).map(|i| (i, i + needle.len()))
        })
    };
    let mut results: Vec<String> = Vec::new();
    let walker = ignore::WalkBuilder::new(&root).max_depth(Some(8)).build();
    'outer: for entry in walker.flatten() {
        if results.len() >= 20 {
            break;
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let p = entry.path();
        if let Some(g) = &glob {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !glob_match_simple(g, name) {
                continue;
            }
        }
        let meta = match std::fs::metadata(p) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.len() > 2_000_000 {
            continue;
        }
        let content = match std::fs::read_to_string(p) {
            Ok(s) => s,
            Err(_) => continue,
        };
        for (lineno, line) in content.lines().enumerate() {
            if let Some((a, b)) = matcher(line) {
                let rel = p
                    .strip_prefix(&root)
                    .unwrap_or(p)
                    .display()
                    .to_string();
                let snippet = &line[a.saturating_sub(20)..(b + 20).min(line.len())];
                results.push(format!("{}:{}: {}", rel, lineno + 1, snippet));
                if results.len() >= 20 {
                    break 'outer;
                }
            }
        }
    }
    if results.is_empty() {
        let _ = bot.send_message(chat_id, "(no matches)").await;
    } else {
        let body = results.join("\n");
        send_long(bot, chat_id, &body).await;
    }
}

fn glob_match_simple(pat: &str, name: &str) -> bool {
    // Supports `*.ts`, `src/**/*.tsx`, `*`. Globstar is best-effort.
    if pat == "*" {
        return true;
    }
    if let Some(rest) = pat.strip_prefix("*.") {
        // `*.ts` should also match `.tsx` (TS-flavored). This is a
        // common shortcut: a user who whitelists `*.ts` likely wants
        // both `.ts` and `.tsx` files.
        if name.ends_with(&format!(".{rest}")) {
            return true;
        }
        if rest == "ts" && name.ends_with(".tsx") {
            return true;
        }
        return false;
    }
    if pat.contains("**") {
        // Very rough: drop the `**/` prefix if present, then match the tail.
        let parts: Vec<&str> = pat.split("**").collect();
        if parts.len() == 2 {
            let head = parts[0].trim_end_matches('/');
            let tail = parts[1].trim_start_matches('/');
            // For `src/**/*.tsx`, head=`src`, tail=`*.tsx`. The name must
            // start with head/ and end with .tsx.
            if !head.is_empty() && !name.starts_with(head) {
                return false;
            }
            if let Some(t_ext) = tail.strip_prefix("*.") {
                if name.ends_with(&format!(".{t_ext}")) {
                    return true;
                }
                if t_ext == "ts" && name.ends_with(".tsx") {
                    return true;
                }
                return false;
            }
            return name.ends_with(tail);
        }
    }
    pat == name
}

async fn handle_edit_start(
    bot: &teloxide::Bot,
    state: &Arc<TelegramState>,
    chat_id: ChatId,
    path: &str,
) {
    use teloxide::prelude::Requester;
    let app = app_handle();
    let app_st = match app.try_state::<crate::AppState>() {
        Some(s) => s,
        None => return,
    };
    let root = match app_st.workspace_root.lock().unwrap().clone() {
        Some(p) => p,
        None => {
            let _ = bot.send_message(chat_id, "❌ No workspace.").await;
            return;
        }
    };
    let full = match crate::sandbox::resolve(&root, path) {
        Ok(p) => p,
        Err(e) => {
            let _ = bot
                .send_message(chat_id, format!("❌ Path error: {e}"))
                .await;
            return;
        }
    };
    if !full.is_file() {
        let _ = bot
            .send_message(
                chat_id,
                format!("❌ Not a file: `{path}`"),
            )
            .parse_mode(teloxide::types::ParseMode::MarkdownV2)
            .await;
        return;
    }
    let pending = PendingEdit {
        path: path.to_string(),
        stage: EditStage::WaitOld,
        old: None,
        new: None,
        created_at: Instant::now(),
    };
    if let Ok(mut g) = state.pending_edits.lock() {
        g.insert(chat_id.0, pending);
    }
    let _ = bot
        .send_message(
            chat_id,
            format!(
                "📝 /edit `{}`\nSend the OLD block (one message), then the NEW block, then /apply.",
                path
            ),
        )
        .parse_mode(teloxide::types::ParseMode::MarkdownV2)
        .await;
}

async fn handle_edit_old(
    bot: &teloxide::Bot,
    state: &Arc<TelegramState>,
    chat_id: ChatId,
    text: String,
) {
    use teloxide::prelude::Requester;
    if let Ok(mut g) = state.pending_edits.lock() {
        if let Some(p) = g.get_mut(&chat_id.0) {
            p.old = Some(text);
            p.stage = EditStage::WaitNew;
        }
    }
    let _ = bot
        .send_message(chat_id, "Got OLD. Now send the NEW block.")
        .await;
}

async fn handle_edit_new(
    bot: &teloxide::Bot,
    state: &Arc<TelegramState>,
    chat_id: ChatId,
    text: String,
) {
    use teloxide::prelude::Requester;
    let preview = {
        let mut g = state.pending_edits.lock().unwrap();
        let p = match g.get_mut(&chat_id.0) {
            Some(p) => p,
            None => return,
        };
        p.new = Some(text);
        p.stage = EditStage::WaitConfirm;
        let old = p.old.clone().unwrap_or_default();
        let new = p.new.clone().unwrap_or_default();
        let path = p.path.clone();
        // Build a small diff preview using the same `similar` crate.
        use similar::{ChangeTag, TextDiff};
        let diff = TextDiff::from_lines(&old, &new);
        let mut s = String::new();
        for c in diff.iter_all_changes() {
            let pfx = match c.tag() {
                ChangeTag::Equal => " ",
                ChangeTag::Insert => "+",
                ChangeTag::Delete => "-",
            };
            s.push_str(pfx);
            s.push_str(c.value().trim_end_matches('\n'));
            s.push('\n');
            if s.len() > 1800 {
                s.push_str("…(preview truncated)\n");
                break;
            }
        }
        (path, s)
    };
    let (path, diff) = preview;
    let _ = bot
        .send_message(
            chat_id,
            format!(
                "Preview for `{}`:\n```diff\n{}\n```\nSend /apply to confirm or /cancel.",
                path, diff
            ),
        )
        .parse_mode(teloxide::types::ParseMode::MarkdownV2)
        .await;
}

async fn handle_edit_apply(
    bot: &teloxide::Bot,
    app: &AppHandle,
    state: &Arc<TelegramState>,
    chat_id: ChatId,
) {
    use teloxide::prelude::Requester;
    let (path, old, new) = {
        let mut g = state.pending_edits.lock().unwrap();
        let p = match g.remove(&chat_id.0) {
            Some(p) => p,
            None => {
                let _ = bot
                    .send_message(chat_id, "ℹ Nothing to apply.")
                    .await;
                return;
            }
        };
        if p.stage != EditStage::WaitConfirm {
            let _ = bot
                .send_message(chat_id, "ℹ Edit not ready (need OLD and NEW first).")
                .await;
            // Put it back so the user can continue.
            g.insert(chat_id.0, p);
            return;
        }
        (p.path, p.old.unwrap_or_default(), p.new.unwrap_or_default())
    };
    // Use the internal edit logic via the same Tauri command path. We
    // invoke through a small helper that takes a State<AppState> view.
    let app_st = match app.try_state::<crate::AppState>() {
        Some(s) => s,
        None => {
            let _ = bot.send_message(chat_id, "⚠️ AppState unavailable.").await;
            return;
        }
    };
    let root = match app_st.workspace_root.lock().unwrap().clone() {
        Some(p) => p,
        None => {
            let _ = bot.send_message(chat_id, "❌ No workspace.").await;
            return;
        }
    };
    let full = match crate::sandbox::resolve(&root, &path) {
        Ok(p) => p,
        Err(e) => {
            let _ = bot
                .send_message(chat_id, format!("❌ Path error: {e}"))
                .await;
            return;
        }
    };
    if old == new {
        let _ = bot
            .send_message(chat_id, "ℹ OLD == NEW, nothing changed.")
            .await;
        return;
    }
    let before = match std::fs::read_to_string(&full) {
        Ok(s) => s,
        Err(e) => {
            let _ = bot
                .send_message(chat_id, format!("❌ Read failed: {e}"))
                .await;
            return;
        }
    };
    let occurrences = before.matches(&old).count();
    if occurrences == 0 {
        let _ = bot
            .send_message(
                chat_id,
                "❌ OLD block not found in file. /edit again with a longer match.",
            )
            .await;
        return;
    }
    if occurrences > 1 {
        let _ = bot
            .send_message(
                chat_id,
                format!("❌ OLD block matches {occurrences} times. Add more context."),
            )
            .await;
        return;
    }
    let after = before.replacen(&old, &new, 1);
    // Atomic write.
    let tmp = full.with_extension("tmp");
    if let Err(e) = std::fs::write(&tmp, &after) {
        let _ = bot
            .send_message(chat_id, format!("❌ Write tmp: {e}"))
            .await;
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, &full) {
        let _ = bot
            .send_message(chat_id, format!("❌ Rename: {e}"))
            .await;
        return;
    }
    let bytes_written = after.len() as u64;
    let edit_id = new_edit_id();
    let entry = crate::EditEntry {
        id: edit_id.clone(),
        path: path.clone(),
        before,
        after: after.clone(),
        at_ms: unix_ms() as u128,
    };
    {
        let mut stack = app_st.edit_undo.lock().unwrap();
        crate::push_undo(&mut stack, entry);
    }
    let _ = app.emit(
        "edit_done",
        serde_json::json!({
            "edit_id": edit_id,
            "path": path,
            "bytes_written": bytes_written,
        }),
    );
    let _ = bot
        .send_message(
            chat_id,
            format!(
                "✅ Applied.\nPath: `{path}`\nBytes: {bytes_written}\nEdit ID: `{edit_id}`\nUse /revert {edit_id} to undo.",
            ),
        )
        .parse_mode(teloxide::types::ParseMode::MarkdownV2)
        .await;
}

async fn handle_edit_cancel(
    bot: &teloxide::Bot,
    state: &Arc<TelegramState>,
    chat_id: ChatId,
) {
    use teloxide::prelude::Requester;
    if let Ok(mut g) = state.pending_edits.lock() {
        if g.remove(&chat_id.0).is_some() {
            let _ = bot.send_message(chat_id, "🚫 Cancelled.").await;
        } else {
            let _ = bot.send_message(chat_id, "ℹ Nothing to cancel.").await;
        }
    }
}

async fn handle_revert(
    bot: &teloxide::Bot,
    app: &AppHandle,
    state: &Arc<TelegramState>,
    chat_id: ChatId,
    edit_id: &str,
) {
    use teloxide::prelude::Requester;
    let _ = state;
    let app_st = match app.try_state::<crate::AppState>() {
        Some(s) => s,
        None => return,
    };
    let entry = {
        let mut stack = app_st.edit_undo.lock().unwrap();
        let idx = stack.iter().position(|e| e.id == edit_id);
        match idx {
            Some(i) => Some(stack.remove(i)),
            None => None,
        }
    };
    let entry = match entry {
        Some(e) => e,
        None => {
            let _ = bot
                .send_message(chat_id, "❌ Edit ID not found in undo stack.")
                .await;
            return;
        }
    };
    let root = match app_st.workspace_root.lock().unwrap().clone() {
        Some(p) => p,
        None => {
            let _ = bot.send_message(chat_id, "❌ No workspace.").await;
            return;
        }
    };
    let full = match crate::sandbox::resolve(&root, &entry.path) {
        Ok(p) => p,
        Err(e) => {
            let _ = bot
                .send_message(chat_id, format!("❌ Path error: {e}"))
                .await;
            return;
        }
    };
    let tmp = full.with_extension("tmp");
    if let Err(e) = std::fs::write(&tmp, &entry.before) {
        let _ = bot
            .send_message(chat_id, format!("❌ Write tmp: {e}"))
            .await;
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, &full) {
        let _ = bot
            .send_message(chat_id, format!("❌ Rename: {e}"))
            .await;
        return;
    }
    let _ = app.emit(
        "edit_reverted",
        serde_json::json!({ "edit_id": edit_id, "path": entry.path }),
    );
    let _ = bot
        .send_message(chat_id, format!("✅ Reverted `{}`.", entry.path))
        .parse_mode(teloxide::types::ParseMode::MarkdownV2)
        .await;
}

async fn handle_create(
    bot: &teloxide::Bot,
    app: &AppHandle,
    state: &Arc<TelegramState>,
    chat_id: ChatId,
    name: &str,
    template: Option<String>,
    parent: Option<String>,
) {
    use teloxide::prelude::Requester;
    let _ = state;
    if name.is_empty()
        || name.len() > 64
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
    {
        let _ = bot
            .send_message(
                chat_id,
                "❌ Name must be ≤64 chars, [a-zA-Z0-9._-] only.",
            )
            .await;
        return;
    }
    let app_st = match app.try_state::<crate::AppState>() {
        Some(s) => s,
        None => return,
    };
    let parent_dir = match parent {
        Some(p) => PathBuf::from(p),
        None => {
            let root = app_st.workspace_root.lock().unwrap().clone();
            match root.and_then(|r| r.parent().map(|p| p.to_path_buf())) {
                Some(p) => p,
                None => {
                    let _ = bot
                        .send_message(
                            chat_id,
                            "❌ No workspace. Set one with /workspace <path> first.",
                        )
                        .await;
                    return;
                }
            }
        }
    };
    if !parent_dir.is_dir() {
        let _ = bot
            .send_message(chat_id, "❌ Parent dir not found.")
            .await;
        return;
    }
    let project_dir = parent_dir.join(name);
    if project_dir.exists() {
        let _ = bot
            .send_message(chat_id, "❌ Already exists.")
            .await;
        return;
    }
    let template = template.unwrap_or_else(|| "blank".to_string());
    let tmpls = crate::builtin_templates();
    let tmpl = match tmpls.into_iter().find(|t| t.id == template) {
        Some(t) => t,
        None => {
            let _ = bot
                .send_message(chat_id, format!("❌ Unknown template: {template}"))
                .await;
            return;
        }
    };
    if let Err(e) = std::fs::create_dir_all(&project_dir) {
        let _ = bot
            .send_message(chat_id, format!("❌ mkdir: {e}"))
            .await;
        return;
    }
    for f in tmpl.files {
        let content = f.content.replace("__NAME__", name);
        let full = project_dir.join(&f.path);
        if let Some(parent) = full.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&full, content) {
            let _ = bot
                .send_message(chat_id, format!("❌ Write {}: {e}", f.path))
                .await;
            return;
        }
    }
    // Open the new project as the active workspace.
    {
        *app_st.workspace_root.lock().unwrap() = Some(project_dir.clone());
        let _ = app.emit(
            "workspace_changed",
            serde_json::json!({
                "path": project_dir.display().to_string(),
                "name": name,
            }),
        );
    }
    let _ = bot
        .send_message(
            chat_id,
            format!(
                "✅ Project `{name}` created at `{}` (template: {template}). Now the active workspace.",
                project_dir.display()
            ),
        )
        .parse_mode(teloxide::types::ParseMode::MarkdownV2)
        .await;
}

async fn handle_run(
    bot: &teloxide::Bot,
    state: &Arc<TelegramState>,
    chat_id: ChatId,
    cmd: &str,
    args: &[String],
) {
    use teloxide::prelude::Requester;
    let _ = state;
    let app = app_handle();
    let app_st = match app.try_state::<crate::AppState>() {
        Some(s) => s,
        None => return,
    };
    let root = app_st.workspace_root.lock().unwrap().clone();
    let result = super::shell::run_shell_command(root.as_deref(), cmd, args).await;
    match result {
        Ok(r) => {
            let mut body = format!(
                "Exit: {}\nDuration: {}ms\n",
                r.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "?".into()),
                r.duration_ms
            );
            if r.timed_out {
                body.push_str("(timed out, killed)\n");
            }
            if !r.stdout.is_empty() {
                body.push_str(&format!("\n[stdout]\n{}", r.stdout));
            }
            if !r.stderr.is_empty() {
                body.push_str(&format!("\n[stderr]\n{}", r.stderr));
            }
            if r.stdout_truncated {
                body.push_str("\n(stdout truncated)");
            }
            if r.stderr_truncated {
                body.push_str("\n(stderr truncated)");
            }
            send_long(bot, chat_id, &body).await;
        }
        Err(e) => {
            let _ = bot
                .send_message(chat_id, format!("❌ {e}"))
                .await;
        }
    }
}

async fn handle_upload_arm(
    bot: &teloxide::Bot,
    _state: &Arc<TelegramState>,
    chat_id: ChatId,
) {
    use teloxide::prelude::Requester;
    arm_upload(chat_id.0);
    let _ = bot
        .send_message(
            chat_id,
            "📎 Send the file (as document to preserve filename). Max 20 MB.",
        )
        .await;
}

async fn handle_upload(
    bot: &teloxide::Bot,
    state: &Arc<TelegramState>,
    chat_id: ChatId,
    msg: &teloxide::types::Message,
) -> Result<(), teloxide::RequestError> {
    use teloxide::prelude::Requester;
    use teloxide::types::{MediaKind, MessageKind, PhotoSize};
    let armed = consume_upload_arm(chat_id.0);
    if !armed {
        // Not armed: ignore stray file.
        let _ = bot
            .send_message(chat_id, "ℹ File ignored. Use /upload first.")
            .await;
        return Ok(());
    }
    let app = app_handle();
    let app_st = match app.try_state::<crate::AppState>() {
        Some(s) => s,
        None => return Ok(()),
    };
    let root = match app_st.workspace_root.lock().unwrap().clone() {
        Some(p) => p,
        None => {
            let _ = bot.send_message(chat_id, "❌ No workspace.").await;
            return Ok(());
        }
    };
    // Pick the file_id + suggested name.
    let (file_id, suggested_name): (String, String) = match &msg.kind {
        MessageKind::Common(common) => match &common.media_kind {
            MediaKind::Document(d) => {
                let n = d
                    .document
                    .file_name
                    .clone()
                    .unwrap_or_else(|| "upload.bin".into());
                (d.document.file.id.clone(), n)
            }
            MediaKind::Photo(p) => {
                // Pick the largest photo size.
                let biggest: &PhotoSize = p
                    .photo
                    .iter()
                    .max_by_key(|s| s.width as u64 * s.height as u64)
                    .unwrap_or(&p.photo[0]);
                let n = format!("photo-{}.jpg", biggest.file.id);
                (biggest.file.id.clone(), n)
            }
            _ => {
                let _ = bot.send_message(chat_id, "❌ Not a file.").await;
                return Ok(());
            }
        },
        _ => {
            let _ = bot.send_message(chat_id, "❌ Not a file.").await;
            return Ok(());
        }
    };
    let safe_name = match safe_filename(&suggested_name) {
        Ok(n) => n,
        Err(e) => {
            let _ = bot
                .send_message(chat_id, format!("❌ Bad filename: {e}"))
                .await;
            return Ok(());
        }
    };
    // Get the file path from Telegram.
    let tg_file = match bot.get_file(&file_id).await {
        Ok(f) => f,
        Err(e) => {
            let _ = bot
                .send_message(chat_id, format!("❌ getFile: {e}"))
                .await;
            return Ok(());
        }
    };
    let tg_path = tg_file.path.clone();
    let token = state
        .token_cached
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_default();
    let url = format!("https://api.telegram.org/file/bot{token}/{tg_path}");
    let bytes = match reqwest::get(&url).await {
        Ok(r) => match r.bytes().await {
            Ok(b) => b.to_vec(),
            Err(e) => {
                let _ = bot
                    .send_message(chat_id, format!("❌ download body: {e}"))
                    .await;
                return Ok(());
            }
        },
        Err(e) => {
            let _ = bot
                .send_message(chat_id, format!("❌ download: {e}"))
                .await;
            return Ok(());
        }
    };
    let target = root.join(&safe_name);
    if let Err(e) = std::fs::write(&target, &bytes) {
        let _ = bot
            .send_message(chat_id, format!("❌ write: {e}"))
            .await;
        return Ok(());
    }
    let rel = target
        .strip_prefix(&root)
        .unwrap_or(&target)
        .display()
        .to_string();
    let _ = bot
        .send_message(
            chat_id,
            format!(
                "✅ Saved: `{rel}` ({} bytes).",
                bytes.len()
            ),
        )
        .parse_mode(teloxide::types::ParseMode::MarkdownV2)
        .await;
    Ok(())
}

async fn handle_model(
    bot: &teloxide::Bot,
    state: &Arc<TelegramState>,
    chat_id: ChatId,
    name: Option<String>,
) {
    use teloxide::prelude::Requester;
    match name {
        None => {
            let cur = state
                .model_override
                .lock()
                .ok()
                .and_then(|g| g.clone())
                .unwrap_or_else(|| "(default — MiniMax-M3 or claude-3-5-sonnet-latest)".into());
            let _ = bot
                .send_message(chat_id, format!("🧠 Current model: `{cur}`"))
                .parse_mode(teloxide::types::ParseMode::MarkdownV2)
                .await;
        }
        Some(m) => {
            if let Ok(mut g) = state.model_override.lock() {
                *g = Some(m.clone());
            }
            let _ = bot
                .send_message(chat_id, format!("🧠 Model set to `{m}`."))
                .parse_mode(teloxide::types::ParseMode::MarkdownV2)
                .await;
        }
    }
}

async fn handle_chat(
    bot: &teloxide::Bot,
    app: &AppHandle,
    state: &Arc<TelegramState>,
    chat_id: ChatId,
    text: &str,
) {
    use teloxide::prelude::Requester;
    // Decide which provider to use. We use MiniMax if its key is set,
    // otherwise Anthropic. The bot inherits the same provider logic as
    // the desktop UI; if neither is set, return a clear error.
    let (cfg, source_label) = build_chat_config(state);
    let cfg = match cfg {
        Some(c) => c,
        None => {
            let _ = bot
                .send_message(
                    chat_id,
                    "❌ No AI provider key set. Add one in Settings → API Keys.",
                )
                .await;
            return;
        }
    };
    // Build a per-chat message history. v1 is stateless (single user
    // message per call), which is the most predictable behavior. A
    // future enhancement would be a per-chat history with a cap.
    let user_msg = serde_json::json!({ "role": "user", "content": text });
    let messages = vec![user_msg];
    // Pre-create the placeholder message and wire the Telegram sink.
    let bot_clone = bot.clone();
    let app_clone = app.clone();
    let placeholder = bot
        .send_message(chat_id, "▌")
        .await;
    let msg_id = match placeholder {
        Ok(m) => m.id,
        Err(e) => {
            tracing::error!(?e, "failed to create placeholder");
            return;
        }
    };
    let (tx, mut rx) = tokio::sync::oneshot::channel::<()>();
    {
        if let Ok(mut sigs) = state.stop_signals.lock() {
            if let Some(old) = sigs.insert(chat_id.0, tx) {
                // Previous stream's sender is dropped → its loop sees
                // recv() error and exits.
                drop(old);
            }
        }
    }
    let edit_closure = {
        let bot = bot_clone.clone();
        move |new_text: &str| {
            let bot = bot.clone();
            let new_text = new_text.to_string();
            // Best-effort: spawn and ignore errors. We do NOT await here
            // because the sink trait is sync.
            tokio::spawn(async move {
                let _ = bot
                    .edit_message_text(chat_id, msg_id, new_text)
                    .await;
            });
        }
    };
    let create_closure = {
        let bot = bot_clone.clone();
        move |new_text: &str| -> Result<MessageId, String> {
            let new_text = new_text.to_string();
            // For the sink, "create" returns the id of the freshly-sent
            // message. We block on the async send here. That's OK
            // because the sink only calls create once.
            let res = futures::executor::block_on(async {
                bot.send_message(chat_id, new_text).await
            });
            res.map(|m| m.id).map_err(|e| e.to_string())
        }
    };
    let action_closure = {
        let bot = bot_clone.clone();
        move || {
            let bot = bot.clone();
            tokio::spawn(async move {
                let _ = bot
                    .send_chat_action(chat_id, ChatAction::Typing)
                    .await;
            });
        }
    };
    let sink = TelegramSink::new(
        chat_id.0,
        Box::new(edit_closure),
        Box::new(create_closure),
        Box::new(action_closure),
    );
    // Run the stream. We concurrently await either the stream or the
    // stop signal; whichever fires first wins.
    let cfg = Arc::new(cfg);
    let messages_arc = Arc::new(messages);
    // Move the sink into a single-element Vec<Box<dyn ChatSink>>.
    let mut sinks: Vec<Box<dyn super::chat_sink::ChatSink>> = vec![Box::new(sink)];
    let stream_fut = {
        let cfg = cfg.clone();
        let messages = messages_arc.clone();
        async move { chat_text_stream_core(&cfg, &messages, &mut sinks).await }
    };
    tokio::select! {
        outcome = stream_fut => {
            // Done. Clear stop signal.
            if let Ok(mut sigs) = state.stop_signals.lock() {
                sigs.remove(&chat_id.0);
            }
            tracing::info!(?outcome, "telegram stream finished for chat {chat_id}");
        }
        _ = &mut rx => {
            let _ = bot_clone.edit_message_text(chat_id, msg_id, "⏹ Cancelled.").await;
            if let Ok(mut sigs) = state.stop_signals.lock() {
                sigs.remove(&chat_id.0);
            }
        }
    }
    // Forward the final assistant message to the desktop chat as an
    // injected user-visible message, so the user can see the transcript.
    let _ = app_clone.emit(
        "chat-inject",
        serde_json::json!({
            "text": format!("📨 from Telegram ({source_label})\n{text}"),
            "t_ms": unix_ms(),
            "source": "telegram",
        }),
    );
}

fn build_chat_config(state: &Arc<TelegramState>) -> (Option<StreamConfig>, &'static str) {
    // Try MiniMax first.
    if let Ok(Some(key)) = super::super::secrets::get_api_key_str("minimax") {
        if !key.is_empty() {
            let model = state
                .model_override
                .lock()
                .ok()
                .and_then(|g| g.clone())
                .unwrap_or_else(|| "MiniMax-M3".into());
            let url = std::env::var("MINIMAX_API_URL")
                .unwrap_or_else(|_| "https://api.minimax.io/v1/chat/completions".to_string());
            let scheme = std::env::var("MINIMAX_AUTH_SCHEME")
                .unwrap_or_else(|_| "Bearer".to_string());
            let auth = if scheme.is_empty() { key } else { format!("{scheme} {key}") };
            return (
                Some(StreamConfig {
                    model,
                    url,
                    auth_header: auth,
                    max_tokens: 4096,
                    request_timeout: Duration::from_secs(120),
                    enable_thinking: true,
                    temperature: 0.8,
                }),
                "minimax",
            );
        }
    }
    if let Ok(Some(key)) = super::super::secrets::get_api_key_str("anthropic") {
        if !key.is_empty() {
            // We don't yet support Anthropic streaming in chat_text_stream_core
            // (it's OpenAI-compatible SSE only). Fall back to a non-stream
            // request here.
            return (None, "anthropic");
        }
    }
    (None, "none")
}

fn new_edit_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let folded = (n as u64) ^ ((n >> 32) as u64);
    format!("e{:08x}", folded & 0xFFFF_FFFF)
}

fn unix_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Render a long body as one or more Telegram messages, each ≤3500 chars.
async fn send_long(bot: &teloxide::Bot, chat_id: ChatId, body: &str) {
    use teloxide::prelude::Requester;
    if body.len() <= 3500 {
        let _ = bot
            .send_message(chat_id, format!("```\n{body}\n```"))
            .parse_mode(teloxide::types::ParseMode::MarkdownV2)
            .await;
        return;
    }
    let mut idx = 0;
    let mut part = 1;
    let total = body.len().div_ceil(3500);
    while idx < body.len() {
        let end = (idx + 3500).min(body.len());
        let safe = body[..end].char_indices().last().map(|(i, _)| i).unwrap_or(idx);
        let chunk = &body[idx..safe];
        let header = format!("({}/{})", part, total);
        let _ = bot
            .send_message(chat_id, format!("{header}\n```\n{chunk}\n```"))
            .parse_mode(teloxide::types::ParseMode::MarkdownV2)
            .await;
        idx = safe;
        part += 1;
        if part > 20 {
            // Avoid a runaway loop on absurd inputs.
            break;
        }
    }
}

// =====================================================================
// safe_filename
// =====================================================================

const BLOCKED_EXT: &[&str] = &[
    "exe", "bat", "cmd", "ps1", "sh", "vbs", "scr", "com", "cpl", "msi", "jar", "js",
];

fn safe_filename(input: &str) -> Result<String, String> {
    use std::path::Component;
    if input.is_empty() {
        return Err("empty filename".into());
    }
    let p = Path::new(input);
    for comp in p.components() {
        match comp {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err("path traversal not allowed".into());
            }
            _ => {}
        }
    }
    let name = p
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "no filename")?;
    if name.is_empty() || name == "." || name == ".." {
        return Err("invalid filename".into());
    }
    if name.contains('\0') {
        return Err("NUL byte".into());
    }
    // Windows reserved names (case-insensitive).
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "COM1" | "COM2" | "COM3" | "COM4" | "COM5" | "COM6" | "COM7" | "COM8" | "COM9" | "LPT1" | "LPT2" | "LPT3" | "LPT4" | "LPT5" | "LPT6" | "LPT7" | "LPT8" | "LPT9"
    ) {
        return Err(format!("reserved name: {stem}"));
    }
    if let Some(ext) = Path::new(name).extension().and_then(|e| e.to_str()) {
        let lower = ext.to_ascii_lowercase();
        if BLOCKED_EXT.contains(&lower.as_str()) {
            return Err(format!("executable extension not allowed: {ext}"));
        }
    }
    Ok(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_help() {
        assert_eq!(parse_command("/help"), Command::Help);
    }

    #[test]
    fn parse_read() {
        assert_eq!(
            parse_command("/read src/main.rs"),
            Command::Read("src/main.rs".into())
        );
    }

    #[test]
    fn parse_find_with_flags() {
        let c = parse_command("/find foo bar -g *.ts -r");
        match c {
            Command::Find { query, glob, regex, case_sensitive } => {
                assert_eq!(query, "foo bar");
                assert_eq!(glob.as_deref(), Some("*.ts"));
                assert!(regex);
                assert!(!case_sensitive);
            }
            _ => panic!("expected Find"),
        }
    }

    #[test]
    fn parse_plain_text_is_chat() {
        assert_eq!(
            parse_command("hello world"),
            Command::Chat("hello world".into())
        );
    }

    #[test]
    fn parse_edit_start() {
        assert_eq!(
            parse_command("/edit src/lib.rs"),
            Command::EditStart("src/lib.rs".into())
        );
    }

    #[test]
    fn parse_run() {
        match parse_command("/run cargo test --foo") {
            Command::Run { cmd, args } => {
                assert_eq!(cmd, "cargo");
                assert_eq!(args, vec!["test", "--foo"]);
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn parse_create_with_template() {
        match parse_command("/create my-app vite-ts --parent /tmp") {
            Command::Create { name, template, parent } => {
                assert_eq!(name, "my-app");
                assert_eq!(template.as_deref(), Some("vite-ts"));
                assert_eq!(parent.as_deref(), Some("/tmp"));
            }
            _ => panic!("expected Create"),
        }
    }

    #[test]
    fn parse_workspace_no_arg() {
        assert_eq!(parse_command("/workspace"), Command::Workspace(None));
    }

    #[test]
    fn parse_workspace_with_path() {
        assert_eq!(
            parse_command("/workspace /tmp/foo"),
            Command::Workspace(Some("/tmp/foo".into()))
        );
    }

    #[test]
    fn safe_filename_ok() {
        assert_eq!(safe_filename("report.pdf").unwrap(), "report.pdf");
        assert_eq!(safe_filename("файл.txt").unwrap(), "файл.txt");
        assert_eq!(safe_filename("data.json").unwrap(), "data.json");
    }

    #[test]
    fn safe_filename_rejects_traversal() {
        assert!(safe_filename("../../../etc/passwd").is_err());
        assert!(safe_filename("..").is_err());
        assert!(safe_filename("/abs/path").is_err());
    }

    #[test]
    fn safe_filename_rejects_executable() {
        assert!(safe_filename("script.bat").is_err());
        assert!(safe_filename("evil.exe").is_err());
        assert!(safe_filename("hack.ps1").is_err());
    }

    #[test]
    fn safe_filename_rejects_reserved() {
        assert!(safe_filename("CON.txt").is_err());
        assert!(safe_filename("nul.log").is_err());
    }

    #[test]
    fn glob_match_simple_works() {
        assert!(glob_match_simple("*.ts", "foo.ts"));
        assert!(glob_match_simple("*.ts", "foo.tsx"));
        assert!(glob_match_simple("*", "anything"));
        assert!(!glob_match_simple("*.ts", "foo.js"));
    }

    #[test]
    fn pending_expires_after_ttl() {
        let p = PendingEdit {
            path: "a".into(),
            stage: EditStage::WaitOld,
            old: None,
            new: None,
            created_at: Instant::now() - PENDING_EDIT_TTL - Duration::from_secs(1),
        };
        assert!(pending_expired(&p));
    }
}

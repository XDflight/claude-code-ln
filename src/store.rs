//! Read-only model of the Claude desktop app's Claude Code session store.
//!
//! Layout on disk (one metadata file per session, per organization):
//!
//! ```text
//! <root>/<accountUuid>/<orgUuid>/local_<uuid>.json   session metadata
//! <root>/<accountUuid>/<orgUuid>/deleted_<uuid>      tombstone of a deleted session
//! ```
//!
//! The same `local_<uuid>.json` present under several organizations is what this
//! tool calls a "soft link": every copy points (through `cliSessionId`) at a
//! Claude Code transcript in `~/.claude/projects/<slug>/<cliSessionId>.jsonl`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

use crate::{t, tf};

pub const SESSION_PREFIX: &str = "local_";
pub const SESSION_SUFFIX: &str = ".json";
pub const TOMBSTONE_PREFIX: &str = "deleted_";
/// The app skips record files larger than this.
pub const MAX_RECORD_BYTES: u64 = 10 * 1024 * 1024;

/// `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`, hex digits only.
pub fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// Extracts `<uuid>` from a `local_<uuid>.json` file name.
pub fn session_uuid_from_file_name(name: &str) -> Option<&str> {
    let id = name.strip_prefix(SESSION_PREFIX)?.strip_suffix(SESSION_SUFFIX)?;
    is_uuid(id).then_some(id)
}

pub fn session_file_name(uuid: &str) -> String {
    format!("{SESSION_PREFIX}{uuid}{SESSION_SUFFIX}")
}

/// True for a real directory; a symlink to a directory does not count, so an aliased
/// organization can never pass for an independent one.
pub fn is_real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_dir())
}

/// Default location of the desktop app's session store for this platform.
pub fn default_root() -> Option<PathBuf> {
    // Store (MSIX) installs on Windows see a redirected %APPDATA%.
    #[cfg(windows)]
    if let Some(packages) = dirs::data_local_dir().map(|d| d.join("Packages")) {
        for package in fs::read_dir(packages).into_iter().flatten().filter_map(Result::ok) {
            let candidate = package
                .path()
                .join("LocalCache")
                .join("Roaming")
                .join("Claude")
                .join("claude-code-sessions");
            if package.file_name().to_string_lossy().starts_with("Claude_") && candidate.is_dir() {
                return Some(candidate);
            }
        }
    }
    // macOS: ~/Library/Application Support, Windows: %APPDATA%, Linux: ~/.config
    Some(dirs::config_dir()?.join("Claude").join("claude-code-sessions"))
}

/// Directory holding the Claude Code CLI transcripts (`<slug>/<id>.jsonl`).
pub fn default_projects_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir).join("projects"));
    }
    Some(dirs::home_dir()?.join(".claude").join("projects"))
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OrgKey {
    pub account: String,
    pub org: String,
}

#[derive(Clone, Debug)]
pub struct Org {
    pub key: OrgKey,
    pub dir: PathBuf,
    /// Session uuids (without the `local_` prefix) that have a tombstone here.
    pub tombstones: BTreeSet<String>,
    pub session_count: usize,
}

/// One `local_<uuid>.json` file under one organization.
#[derive(Clone, Debug)]
pub struct SessionCopy {
    pub org: usize,
    pub path: PathBuf,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub is_symlink: bool,
    /// Set when the file could not be read or parsed; the fields below are then empty.
    pub error: Option<String>,
    pub cli_session_id: Option<String>,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub created_at: Option<i64>,
    pub last_activity_at: Option<i64>,
    pub archived: bool,
}

impl SessionCopy {
    /// Best available "how recent is this copy" timestamp, in epoch milliseconds.
    pub fn freshness(&self) -> i64 {
        self.last_activity_at.or(self.created_at).unwrap_or_else(|| {
            self.modified
                .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0)
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkState {
    /// Exists under exactly one organization.
    Single,
    /// Several copies, all pointing at the same transcript.
    Linked,
    /// Several copies whose `cliSessionId` differ: the conversations have forked.
    Diverged,
    /// No copy can be read.
    Broken,
}

/// All copies of one desktop session id across organizations.
#[derive(Clone, Debug)]
pub struct SessionGroup {
    /// Uuid without the `local_` prefix.
    pub uuid: String,
    /// Sorted by organization index.
    pub copies: Vec<SessionCopy>,
}

impl SessionGroup {
    pub fn copy_in(&self, org: usize) -> Option<&SessionCopy> {
        self.copies.iter().find(|c| c.org == org)
    }

    /// The most recently active readable copy (falls back to any copy).
    pub fn newest(&self) -> &SessionCopy {
        self.copies
            .iter()
            .filter(|c| c.error.is_none())
            .max_by_key(|c| (c.freshness(), c.modified))
            .unwrap_or(&self.copies[0])
    }

    pub fn state(&self) -> LinkState {
        let mut ids =
            self.copies.iter().filter(|c| c.error.is_none()).map(|c| c.cli_session_id.as_deref());
        let Some(first) = ids.next() else {
            return LinkState::Broken;
        };
        if !ids.all(|id| id == first) {
            LinkState::Diverged
        } else if self.copies.len() < 2 {
            LinkState::Single
        } else {
            LinkState::Linked
        }
    }

    /// True when this copy's metadata is older than the newest copy's.
    pub fn is_stale(&self, copy: &SessionCopy) -> bool {
        copy.freshness() < self.newest().freshness()
    }

    pub fn title(&self) -> &str {
        let newest = self.newest();
        newest
            .title
            .as_deref()
            .filter(|t| !t.trim().is_empty())
            .or_else(|| self.copies.iter().find_map(|c| c.title.as_deref()))
            .unwrap_or(t!("(untitled)", "(无标题)"))
    }

    pub fn all_archived(&self) -> bool {
        self.copies.iter().all(|c| c.archived)
    }
}

#[derive(Clone, Debug)]
pub struct Transcript {
    pub path: PathBuf,
    pub size: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub orgs: Vec<Org>,
    /// Sorted by most recent activity first.
    pub groups: Vec<SessionGroup>,
    /// CLI transcripts by `cliSessionId`.
    pub transcripts: HashMap<String, Transcript>,
    /// Non-fatal problems met while scanning.
    pub warnings: Vec<String>,
}

impl Snapshot {
    pub fn org_index(&self, key: &OrgKey) -> Option<usize> {
        self.orgs.iter().position(|o| &o.key == key)
    }

    pub fn group(&self, uuid: &str) -> Option<&SessionGroup> {
        self.groups.iter().find(|g| g.uuid == uuid)
    }
}

fn sorted_dir(dir: &Path) -> std::io::Result<Vec<fs::DirEntry>> {
    let mut entries: Vec<_> = fs::read_dir(dir)?.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    Ok(entries)
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key)?.as_str().map(str::to_owned)
}

fn int_field(v: &Value, key: &str) -> Option<i64> {
    let n = v.get(key)?;
    n.as_i64().or_else(|| n.as_f64().map(|f| f as i64))
}

fn read_copy(org: usize, path: PathBuf, uuid: &str) -> SessionCopy {
    let link_meta = fs::symlink_metadata(&path).ok();
    let meta = fs::metadata(&path).ok();
    let mut copy = SessionCopy {
        org,
        size: meta.as_ref().map_or(0, |m| m.len()),
        modified: meta.as_ref().and_then(|m| m.modified().ok()),
        is_symlink: link_meta.is_some_and(|m| m.file_type().is_symlink()),
        path,
        error: None,
        cli_session_id: None,
        title: None,
        cwd: None,
        model: None,
        created_at: None,
        last_activity_at: None,
        archived: false,
    };
    if copy.size > MAX_RECORD_BYTES {
        copy.error = Some(t!("larger than 10 MiB; the desktop app ignores it", "超过 10 MiB，桌面端会忽略该文件").into());
        return copy;
    }
    let value: Value = match fs::read(&copy.path)
        .map_err(|e| e.to_string())
        .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|e| e.to_string()))
    {
        Ok(v) => v,
        Err(e) => {
            copy.error = Some(e);
            return copy;
        }
    };
    if !value.is_object() {
        copy.error = Some(t!("not a JSON object", "不是 JSON 对象").into());
        return copy;
    }
    let expected = format!("{SESSION_PREFIX}{uuid}");
    match value.get("sessionId").and_then(Value::as_str) {
        Some(id) if id == expected => {}
        other => {
            copy.error = Some(tf!(
                "sessionId does not match the file name ({})",
                "sessionId 与文件名不一致（{}）",
                other.unwrap_or(t!("missing", "缺失"))
            ));
        }
    }
    copy.cli_session_id = str_field(&value, "cliSessionId");
    copy.title = str_field(&value, "title");
    copy.cwd = str_field(&value, "cwd");
    copy.model = str_field(&value, "model");
    copy.created_at = int_field(&value, "createdAt");
    copy.last_activity_at = int_field(&value, "lastActivityAt");
    copy.archived = value.get("isArchived").and_then(Value::as_bool).unwrap_or(false);
    copy
}

fn scan_transcripts(projects: &Path) -> HashMap<String, Transcript> {
    let mut map = HashMap::new();
    let Ok(projects) = fs::read_dir(projects) else {
        return map;
    };
    for project in projects.filter_map(Result::ok) {
        let Ok(files) = fs::read_dir(project.path()) else {
            continue;
        };
        for file in files.filter_map(Result::ok) {
            let name = file.file_name();
            let Some(id) = name.to_str().and_then(|n| n.strip_suffix(".jsonl")) else {
                continue;
            };
            if !is_uuid(id) {
                continue;
            }
            let size = file.metadata().map_or(0, |m| m.len());
            map.insert(id.to_owned(), Transcript { path: file.path(), size });
        }
    }
    map
}

/// Scans the session store. Only fails when `root` itself cannot be listed.
pub fn scan(root: &Path, projects: Option<&Path>) -> Result<Snapshot, String> {
    let accounts =
        sorted_dir(root).map_err(|e| tf!("Cannot read {}: {}", "无法读取 {}：{}", root.display(), e))?;
    let mut snapshot = Snapshot::default();
    let mut groups: BTreeMap<String, Vec<SessionCopy>> = BTreeMap::new();

    for account in accounts {
        let Some(account_id) = account.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !is_uuid(&account_id) || !is_real_dir(&account.path()) {
            continue;
        }
        let orgs = match sorted_dir(&account.path()) {
            Ok(orgs) => orgs,
            Err(e) => {
                snapshot.warnings.push(tf!("Cannot read account directory {}: {}", "无法读取账号目录 {}：{}", account_id, e));
                continue;
            }
        };
        for org in orgs {
            let Some(org_id) = org.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !is_uuid(&org_id) || !is_real_dir(&org.path()) {
                continue;
            }
            let index = snapshot.orgs.len();
            let mut entry = Org {
                key: OrgKey { account: account_id.clone(), org: org_id.clone() },
                dir: org.path(),
                tombstones: BTreeSet::new(),
                session_count: 0,
            };
            match sorted_dir(&entry.dir) {
                Ok(files) => {
                    for file in files {
                        let name = file.file_name();
                        let Some(name) = name.to_str() else { continue };
                        if let Some(uuid) = session_uuid_from_file_name(name) {
                            entry.session_count += 1;
                            groups
                                .entry(uuid.to_owned())
                                .or_default()
                                .push(read_copy(index, file.path(), uuid));
                        } else if let Some(uuid) =
                            name.strip_prefix(TOMBSTONE_PREFIX).filter(|u| is_uuid(u))
                        {
                            entry.tombstones.insert(uuid.to_owned());
                        }
                    }
                }
                Err(e) => snapshot
                    .warnings
                    .push(tf!("Cannot read organization directory {}/{}: {}", "无法读取组织目录 {}/{}：{}", account_id, org_id, e)),
            }
            snapshot.orgs.push(entry);
        }
    }

    snapshot.groups = groups
        .into_iter()
        .map(|(uuid, copies)| SessionGroup { uuid, copies })
        .collect();
    snapshot
        .groups
        .sort_by(|a, b| b.newest().freshness().cmp(&a.newest().freshness()).then(a.uuid.cmp(&b.uuid)));
    if let Some(projects) = projects {
        snapshot.transcripts = scan_transcripts(projects);
    }
    Ok(snapshot)
}

/// Whether the Claude desktop app appears to be running right now.
pub fn desktop_app_running() -> bool {
    #[cfg(target_os = "macos")]
    {
        // Full executable paths; the bundled CLI (`.../claude.app/Contents/MacOS/claude`)
        // differs in case and does not match.
        let Ok(out) = std::process::Command::new("ps").args(["-axo", "comm="]).output() else {
            return false;
        };
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .any(|line| line.trim().ends_with("/Claude.app/Contents/MacOS/Claude"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // The Electron app runs as `.../claude-desktop` or with an `app.asar` under a Claude dir.
        let Ok(procs) = fs::read_dir("/proc") else { return false };
        procs.filter_map(Result::ok).any(|entry| {
            if !entry.file_name().to_string_lossy().bytes().all(|b| b.is_ascii_digit()) {
                return false;
            }
            let Ok(raw) = fs::read(entry.path().join("cmdline")) else { return false };
            let cmdline = String::from_utf8_lossy(&raw).to_ascii_lowercase();
            let first = cmdline.split('\0').next().unwrap_or_default();
            first.ends_with("/claude-desktop")
                || (cmdline.contains("claude") && cmdline.contains("app.asar"))
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        // Note: the Claude Code CLI is also `claude.exe`, so this can over-report.
        let Ok(out) = std::process::Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq Claude.exe", "/NH", "/FO", "CSV"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        else {
            return false;
        };
        String::from_utf8_lossy(&out.stdout).to_ascii_lowercase().contains("claude.exe")
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

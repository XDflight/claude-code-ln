//! Mutations of the session store: link, sync and unlink.
//!
//! Facts about the desktop app these operations rely on (read from its bundle):
//!
//! * A session shows up under an organization iff `local_<uuid>.json` exists in that
//!   organization's directory; tenancy is derived from the directory, not the record.
//! * The app saves records with temp-file + rename, so a filesystem symlink or hard
//!   link would be replaced by a regular file on its next save. A "soft link" therefore
//!   has to be a real copy, and copies drift apart until they are synced again.
//! * The app promotes stray `local_*.json.tmp` files, so our temp files must not use
//!   that name pattern.
//! * `deleted_<uuid>` tombstones are not consulted when loading records.
//!
//! Nothing here ever touches CLI transcripts, and nothing is destroyed: a file that is
//! replaced or removed is first moved into the tool's backup directory.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::store::{self, OrgKey};
use crate::{t, tf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    /// Copy the record from `from` into `to`, where it must not exist yet.
    Link { uuid: String, from: OrgKey, to: OrgKey },
    /// Overwrite the record in `to` with the one in `from` (the old one is backed up).
    Sync { uuid: String, from: OrgKey, to: OrgKey },
    /// Remove the record from `org`; refused if it is the last copy anywhere.
    Unlink { uuid: String, org: OrgKey },
}

impl Op {
    /// Syncs first (so new links copy the content the user settled on), then links,
    /// then unlinks: a "move" must copy before it removes.
    fn rank(&self) -> u8 {
        match self {
            Op::Sync { .. } => 0,
            Op::Link { .. } => 1,
            Op::Unlink { .. } => 2,
        }
    }
}

pub struct Executor {
    pub root: PathBuf,
    /// Directory receiving the files this run replaces or removes.
    pub backup_dir: PathBuf,
}

fn org_dir(root: &Path, key: &OrgKey) -> Result<PathBuf, String> {
    if !store::is_uuid(&key.account) || !store::is_uuid(&key.org) {
        return Err(t!("account or organization id is not a valid UUID", "账号或组织 ID 不是合法的 UUID").into());
    }
    let account = root.join(&key.account);
    let dir = account.join(&key.org);
    // Never follow a symlinked account / organization directory out of the store.
    if !store::is_real_dir(&account) || !store::is_real_dir(&dir) {
        return Err(tf!("organization directory is missing or is a symlink: {}", "组织目录不存在或是符号链接：{}", dir.display()));
    }
    Ok(dir)
}

fn record_path(root: &Path, key: &OrgKey, uuid: &str) -> Result<PathBuf, String> {
    if !store::is_uuid(uuid) {
        return Err(tf!("session id is not a valid UUID: {}", "会话 ID 不是合法的 UUID：{}", uuid));
    }
    Ok(org_dir(root, key)?.join(store::session_file_name(uuid)))
}

/// Reads a record and checks that the app would accept it under this file name.
fn read_valid_record(path: &Path, uuid: &str) -> Result<Vec<u8>, String> {
    let bytes = fs::read(path).map_err(|e| tf!("cannot read {}: {}", "无法读取 {}：{}", path.display(), e))?;
    if bytes.len() as u64 > store::MAX_RECORD_BYTES {
        return Err(t!("source file is larger than 10 MiB; the desktop app would ignore it", "源文件超过 10 MiB，桌面端会忽略它").into());
    }
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| tf!("source file is not valid JSON: {}", "源文件不是合法 JSON：{}", e))?;
    let expected = format!("{}{uuid}", store::SESSION_PREFIX);
    if value.get("sessionId").and_then(|v| v.as_str()) != Some(expected.as_str()) {
        return Err(t!("source file's sessionId does not match its file name", "源文件的 sessionId 与文件名不一致").into());
    }
    Ok(bytes)
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

static TEMP_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Writes `bytes` to a private, fsynced temp file next to the final destination.
fn write_temp(dir: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    // Deliberately not `local_*.json.tmp`: the app would adopt such a file.
    let temp = dir.join(format!(".claude-code-ln-{}-{n}.tmp", std::process::id()));
    let _ = fs::remove_file(&temp);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = options.open(&temp).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    if let Err(e) = result {
        let _ = fs::remove_file(&temp);
        return Err(tf!("cannot write to {}: {}", "无法写入 {}：{}", dir.display(), e));
    }
    Ok(temp)
}

/// Moves `path` out of the store (rename, or copy + remove across volumes).
fn move_file(path: &Path, dest: &Path) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| tf!("cannot create the backup directory: {}", "无法创建备份目录：{}", e))?;
    }
    if fs::rename(path, dest).is_ok() {
        return Ok(());
    }
    fs::copy(path, dest).map_err(|e| tf!("cannot back up {}: {}", "无法备份 {}：{}", path.display(), e))?;
    fs::remove_file(path).map_err(|e| tf!("cannot remove {}: {}", "无法移除 {}：{}", path.display(), e))
}

impl Executor {
    fn backup_path(&self, key: &OrgKey, uuid: &str) -> PathBuf {
        let dir = self.backup_dir.join(&key.account).join(&key.org);
        let mut path = dir.join(store::session_file_name(uuid));
        let mut n = 1;
        while exists(&path) {
            path = dir.join(format!("{}.{n}", store::session_file_name(uuid)));
            n += 1;
        }
        path
    }

    fn link(&self, uuid: &str, from: &OrgKey, to: &OrgKey) -> Result<String, String> {
        let source = record_path(&self.root, from, uuid)?;
        let dest = record_path(&self.root, to, uuid)?;
        if from == to {
            return Err(t!("source and target organization are the same", "源组织与目标组织相同").into());
        }
        let dir = org_dir(&self.root, to)?;
        if exists(&dest) {
            return Err(t!("the session already exists in the target organization", "目标组织中已存在该会话").into());
        }
        let bytes = read_valid_record(&source, uuid)?;
        let temp = write_temp(&dir, &bytes)?;
        // `hard_link` fails if the destination appeared meanwhile, so nothing is clobbered.
        let placed = fs::hard_link(&temp, &dest);
        let _ = fs::remove_file(&temp);
        placed.map_err(|e| tf!("cannot create {}: {}", "无法创建 {}：{}", dest.display(), e))?;
        Ok(t!("linked", "已链接").into())
    }

    fn sync(&self, uuid: &str, from: &OrgKey, to: &OrgKey) -> Result<String, String> {
        let source = record_path(&self.root, from, uuid)?;
        let dest = record_path(&self.root, to, uuid)?;
        if from == to {
            return Err(t!("source and target organization are the same", "源组织与目标组织相同").into());
        }
        if !exists(&dest) {
            return Err(t!("the session does not exist in the target organization", "目标组织中不存在该会话").into());
        }
        let bytes = read_valid_record(&source, uuid)?;
        if fs::read(&dest).is_ok_and(|current| current == bytes) {
            return Ok(t!("already identical", "内容已一致").into());
        }
        let dir = org_dir(&self.root, to)?;
        let temp = write_temp(&dir, &bytes)?;
        let backup = self.backup_path(to, uuid);
        let backed_up = backup
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| fs::copy(&dest, &backup).map(drop));
        if let Err(e) = backed_up {
            let _ = fs::remove_file(&temp);
            return Err(tf!("cannot back up the old copy: {}", "无法备份旧副本：{}", e));
        }
        if let Err(e) = fs::rename(&temp, &dest) {
            let _ = fs::remove_file(&temp);
            return Err(tf!("cannot replace {}: {}", "无法替换 {}：{}", dest.display(), e));
        }
        Ok(t!("synced", "已同步").into())
    }

    /// Organizations (other than `org`) holding a readable copy of `uuid` that does not
    /// depend on `removing` (a symlink to the file being removed is not a copy).
    fn other_copies(&self, uuid: &str, org: &OrgKey, removing: &Path) -> usize {
        let name = store::session_file_name(uuid);
        let removing_is_file =
            fs::symlink_metadata(removing).is_ok_and(|m| m.file_type().is_file());
        let target = removing_is_file.then(|| fs::canonicalize(removing).ok()).flatten();
        let mut count = 0;
        let Ok(accounts) = fs::read_dir(&self.root) else { return 0 };
        for account in accounts.filter_map(Result::ok) {
            let Ok(orgs) = fs::read_dir(account.path()) else { continue };
            for candidate in orgs.filter_map(Result::ok) {
                let same = account.file_name().to_str() == Some(org.account.as_str())
                    && candidate.file_name().to_str() == Some(org.org.as_str());
                let path = candidate.path().join(&name);
                if same || !store::is_real_dir(&account.path()) || !store::is_real_dir(&candidate.path()) {
                    continue;
                }
                let Ok(real) = fs::canonicalize(&path) else { continue };
                if target.as_deref() != Some(real.as_path()) && read_valid_record(&path, uuid).is_ok() {
                    count += 1;
                }
            }
        }
        count
    }

    fn unlink(&self, uuid: &str, org: &OrgKey) -> Result<String, String> {
        let path = record_path(&self.root, org, uuid)?;
        if !exists(&path) {
            return Err(t!("the session does not exist in this organization", "该组织中不存在该会话").into());
        }
        if self.other_copies(uuid, org, &path) == 0 {
            return Err(t!("this is the only usable copy of the session; refusing to remove it", "这是该会话唯一可用的副本，已拒绝移除").into());
        }
        move_file(&path, &self.backup_path(org, uuid))?;
        Ok(t!("unlinked (copy moved to backups)", "已取消链接（副本已移入备份）").into())
    }

    pub fn run_one(&self, op: &Op) -> Result<String, String> {
        match op {
            Op::Link { uuid, from, to } => self.link(uuid, from, to),
            Op::Sync { uuid, from, to } => self.sync(uuid, from, to),
            Op::Unlink { uuid, org } => self.unlink(uuid, org),
        }
    }

    /// Runs every operation (syncs, then links, then unlinks) and reports each outcome.
    /// If a sync or link of a session fails, that session's unlinks are skipped.
    pub fn run(&self, ops: &[Op]) -> Vec<(Op, Result<String, String>)> {
        let mut ordered: Vec<&Op> = ops.iter().collect();
        ordered.sort_by_key(|op| op.rank());
        let mut failed: Vec<&str> = Vec::new();
        ordered
            .into_iter()
            .map(|op| {
                let result = match op {
                    Op::Unlink { uuid, .. } if failed.contains(&uuid.as_str()) => {
                        Err(t!("skipped: a link or sync of this session did not succeed", "已跳过：该会话的链接或同步未成功").into())
                    }
                    _ => self.run_one(op),
                };
                if let (Err(_), Op::Link { uuid, .. } | Op::Sync { uuid, .. }) = (&result, op) {
                    failed.push(uuid);
                }
                (op.clone(), result)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "aaaaaaaa-0000-4000-8000-000000000001";
    const O1: &str = "bbbbbbbb-0000-4000-8000-000000000001";
    const O2: &str = "bbbbbbbb-0000-4000-8000-000000000002";
    const S: &str = "cccccccc-0000-4000-8000-000000000001";

    struct Fixture {
        base: PathBuf,
        exec: Executor,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    fn key(org: &str) -> OrgKey {
        OrgKey { account: A.into(), org: org.into() }
    }

    fn record(cli: &str, activity: i64) -> String {
        format!(
            r#"{{"sessionId":"local_{S}","cliSessionId":"{cli}","title":"t","lastActivityAt":{activity}}}"#
        )
    }

    fn fixture(name: &str) -> Fixture {
        let base = std::env::temp_dir().join(format!("ccln-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let root = base.join("store");
        for org in [O1, O2] {
            fs::create_dir_all(root.join(A).join(org)).unwrap();
        }
        fs::write(root.join(A).join(O1).join(store::session_file_name(S)), record("x", 1)).unwrap();
        let exec = Executor { root, backup_dir: base.join("backup") };
        Fixture { base, exec }
    }

    fn path(f: &Fixture, org: &str) -> PathBuf {
        f.exec.root.join(A).join(org).join(store::session_file_name(S))
    }

    #[test]
    fn link_copies_and_never_clobbers() {
        let f = fixture("link");
        let op = Op::Link { uuid: S.into(), from: key(O1), to: key(O2) };
        f.exec.run_one(&op).unwrap();
        assert_eq!(fs::read(path(&f, O1)).unwrap(), fs::read(path(&f, O2)).unwrap());
        assert!(f.exec.run_one(&op).is_err(), "second link must refuse to overwrite");
        // No temp files are left behind, and the copy is a regular private file.
        let names: Vec<_> = fs::read_dir(f.exec.root.join(A).join(O2))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let meta = fs::symlink_metadata(path(&f, O2)).unwrap();
            assert!(meta.file_type().is_file());
            assert_eq!(meta.nlink(), 1);
            assert_eq!(meta.mode() & 0o777, 0o600);
        }
        let snapshot = store::scan(&f.exec.root, None).unwrap();
        assert_eq!(snapshot.groups.len(), 1);
        assert_eq!(snapshot.groups[0].state(), store::LinkState::Linked);
    }

    #[test]
    fn link_rejects_bad_source() {
        let f = fixture("badsrc");
        fs::write(path(&f, O1), "{not json").unwrap();
        let op = Op::Link { uuid: S.into(), from: key(O1), to: key(O2) };
        assert!(f.exec.run_one(&op).is_err());
        assert!(!exists(&path(&f, O2)));
        fs::write(path(&f, O1), r#"{"sessionId":"local_other"}"#).unwrap();
        assert!(f.exec.run_one(&op).is_err());
    }

    #[test]
    fn sync_overwrites_with_backup() {
        let f = fixture("sync");
        fs::write(path(&f, O2), record("old", 0)).unwrap();
        let snapshot = store::scan(&f.exec.root, None).unwrap();
        assert_eq!(snapshot.groups[0].state(), store::LinkState::Diverged);
        let op = Op::Sync { uuid: S.into(), from: key(O1), to: key(O2) };
        assert_eq!(f.exec.run_one(&op).unwrap(), "synced");
        assert_eq!(fs::read_to_string(path(&f, O2)).unwrap(), record("x", 1));
        let backup = f.exec.backup_dir.join(A).join(O2).join(store::session_file_name(S));
        assert_eq!(fs::read_to_string(backup).unwrap(), record("old", 0));
        assert_eq!(f.exec.run_one(&op).unwrap(), "already identical");
    }

    #[test]
    fn unlink_refuses_last_copy_and_backs_up() {
        let f = fixture("unlink");
        let unlink1 = Op::Unlink { uuid: S.into(), org: key(O1) };
        assert!(f.exec.run_one(&unlink1).is_err());
        assert!(exists(&path(&f, O1)));
        // A corrupt copy elsewhere does not count as a surviving copy.
        fs::write(path(&f, O2), "garbage").unwrap();
        assert!(f.exec.run_one(&unlink1).is_err());
        fs::remove_file(path(&f, O2)).unwrap();

        // "Move": queued as unlink + link, executed as link then unlink.
        let ops = [unlink1, Op::Link { uuid: S.into(), from: key(O1), to: key(O2) }];
        let results = f.exec.run(&ops);
        assert!(results.iter().all(|(_, r)| r.is_ok()), "{results:?}");
        assert!(!exists(&path(&f, O1)));
        assert!(exists(&path(&f, O2)));
        assert!(exists(&f.exec.backup_dir.join(A).join(O1).join(store::session_file_name(S))));
    }

    #[test]
    fn sync_runs_before_link_and_failed_link_blocks_unlink() {
        const O3: &str = "bbbbbbbb-0000-4000-8000-000000000003";
        let f = fixture("order");
        fs::create_dir_all(f.exec.root.join(A).join(O3)).unwrap();
        fs::write(path(&f, O2), record("wanted", 0)).unwrap();
        let ops = [
            Op::Link { uuid: S.into(), from: key(O2), to: key(O3) },
            Op::Sync { uuid: S.into(), from: key(O2), to: key(O1) },
        ];
        assert!(f.exec.run(&ops).iter().all(|(_, r)| r.is_ok()));
        for org in [O1, O2, O3] {
            assert_eq!(fs::read_to_string(path(&f, org)).unwrap(), record("wanted", 0));
        }
        // The link fails (already present), so the paired unlink must not run.
        let ops = [
            Op::Unlink { uuid: S.into(), org: key(O1) },
            Op::Link { uuid: S.into(), from: key(O1), to: key(O3) },
        ];
        let results = f.exec.run(&ops);
        assert!(results.iter().all(|(_, r)| r.is_err()), "{results:?}");
        assert!(exists(&path(&f, O1)));
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_independent_copies() {
        const O3: &str = "bbbbbbbb-0000-4000-8000-000000000003";
        let f = fixture("symlink");
        let unlink1 = Op::Unlink { uuid: S.into(), org: key(O1) };
        // A record symlink pointing at the file being removed.
        std::os::unix::fs::symlink(path(&f, O1), path(&f, O2)).unwrap();
        assert!(f.exec.run_one(&unlink1).is_err());
        fs::remove_file(path(&f, O2)).unwrap();
        // An organization directory that is an alias of another one.
        std::os::unix::fs::symlink(f.exec.root.join(A).join(O1), f.exec.root.join(A).join(O3))
            .unwrap();
        assert!(f.exec.run_one(&unlink1).is_err());
        assert!(f.exec.run_one(&Op::Link { uuid: S.into(), from: key(O1), to: key(O3) }).is_err());
        assert_eq!(store::scan(&f.exec.root, None).unwrap().orgs.len(), 2);
        assert!(exists(&path(&f, O1)));
    }

    #[test]
    fn rejects_non_uuid_components() {
        let f = fixture("uuid");
        let evil = OrgKey { account: "..".into(), org: "..".into() };
        let op = Op::Link { uuid: S.into(), from: key(O1), to: evil };
        assert!(f.exec.run_one(&op).is_err());
        let op = Op::Unlink { uuid: "../../x".into(), org: key(O1) };
        assert!(f.exec.run_one(&op).is_err());
    }
}

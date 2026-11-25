use super::{Action, Filter, Journal, Reason, Record, Service};
use crate::gateway::ClientId;
use parking_lot::Mutex;
use std::sync::{mpsc, Arc};
use tokio::sync::broadcast;

const PROVIDER: &str = "00000000-0000-0000-0000-000000000001";
const MODEL: &str = "fixture-model";
const QUEUE_FULL: &str = "日志队列已满，部分事件未记录";

#[tokio::test]
async fn raw_appends_and_merges_preserve_index_and_full_attachments_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let service = Service::new(dir.path());
    let mut changes = service.subscribe();
    let body = "fixture error ".repeat(100);
    let mut records = Vec::new();
    for n in 0..3 {
        let mut record = Record::new(
            Some(ClientId::Codex),
            Some(PROVIDER),
            Some(&format!("fixture-{n}")),
            Reason::ProtocolError,
            Action::Returned,
            Some(502),
            None,
        );
        record.details.message = Some(body.clone());
        record.details.raw = Some(Arc::new(crate::event_raw::Raw::bytes(
            body.as_bytes(),
            vec![],
            None,
        )));
        service.emit(record.clone());
        assert!(changes.recv().await.unwrap().error.is_none());
        records.push(record);
    }
    let mut repeated = records[1].clone();
    repeated.id = uuid::Uuid::new_v4().to_string();
    service.emit(repeated);
    let merged = changes.recv().await.unwrap();
    assert!(merged.error.is_none());
    assert_eq!(merged.record.unwrap().count, 2);
    drop(service);
    let reopened = Service::new(dir.path());
    assert_eq!(reopened.query(Filter::default()).total, 3);
    for (n, record) in records.iter().enumerate() {
        let saved = reopened.detail(&record.id).unwrap();
        assert_eq!(saved.count, if n == 1 { 2 } else { 1 });
        assert_eq!(saved.details.message.as_ref().unwrap().chars().count(), 512);
        assert_eq!(reopened.raw(&record.id).unwrap().unwrap().body, body);
    }
}

#[tokio::test]
async fn clear_write_failure_restores_rows_aliases_and_stable_page_order() {
    let dir = tempfile::tempdir().unwrap();
    let service = Service::new(dir.path());
    let mut events = service.subscribe();
    for n in 0..55 {
        let mut record = Record::new(
            Some(ClientId::Codex),
            Some(PROVIDER),
            Some(&format!("fixture-{n}")),
            Reason::ProtocolError,
            Action::Returned,
            Some(502),
            Some(1),
        );
        record.first_at = super::now();
        record.last_at = record.first_at;
        record.details.raw = Some(Arc::new(crate::event_raw::Raw::bytes(
            b"fixture complete error",
            vec![],
            None,
        )));
        service.emit(record);
        events.recv().await.unwrap();
    }
    let before = service.query(Filter::default());
    assert_eq!(before.total, 55);
    let ids: Vec<_> = before.items.iter().map(|r| r.id.clone()).collect();
    let path = dir.path().join("diagnostics.jsonl");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(service.clear(None, None).await.is_err());
    let restored = service.query(Filter::default());
    assert_eq!(restored.total, 55);
    assert_eq!(
        restored
            .items
            .iter()
            .map(|r| r.id.clone())
            .collect::<Vec<_>>(),
        ids
    );
    assert!(restored.error.is_some());
    for id in &ids {
        assert_eq!(
            service.raw(id).unwrap().unwrap().body,
            "fixture complete error"
        );
    }
    std::fs::remove_dir(&path).unwrap();
    service.clear(None, None).await.unwrap();
    assert_eq!(service.query(Filter::default()).total, 0);
    assert_eq!(
        crate::event_raw::stored(&dir.path().join("diagnostic-errors.sqlite")),
        0
    );
}

#[tokio::test]
async fn raw_write_failure_is_visible_and_restart_prunes_expired_attachments() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("diagnostic-errors.sqlite");
    std::fs::write(&path, b"fixture blocker").unwrap();
    let service = Service::new(dir.path());
    let mut changes = service.subscribe();
    let mut record = Record::new(
        Some(ClientId::Codex),
        Some(PROVIDER),
        Some(MODEL),
        Reason::Capacity,
        Action::Waiting,
        Some(200),
        Some(1),
    );
    record.details.raw = Some(Arc::new(crate::event_raw::Raw::bytes(
        b"fixture error",
        vec![],
        None,
    )));
    let id = record.id.clone();
    service.emit(record.clone());
    let change = changes.recv().await.unwrap();
    assert!(change.error.unwrap().contains("原文保存失败"));
    assert!(service
        .detail(&id)
        .unwrap()
        .details
        .raw_error
        .as_ref()
        .unwrap()
        .incomplete
        .is_some());
    assert!(service.raw(&id).is_err());
    drop(service);

    std::fs::remove_file(&path).unwrap();
    let raw = record.details.raw.take().unwrap();
    raw.save(&path, &id).unwrap();
    record.details.raw_error = Some(raw.reference(&id));
    record.last_at = super::now() - super::RETENTION - 1;
    record.first_at = record.last_at;
    std::fs::write(
        dir.path().join("diagnostics.jsonl"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    let service = Service::new(dir.path());
    let mut changes = service.subscribe();
    // A queued write observes completion of startup retention without sleeps.
    service.emit(Record::new(
        None,
        None,
        None,
        Reason::Network,
        Action::Returned,
        None,
        None,
    ));
    changes.recv().await.unwrap();
    assert_eq!(crate::event_raw::stored(&path), 0);
    assert!(service.detail(&id).is_none());
}

#[tokio::test]
async fn errors_kept_as_files_are_read_from_the_store_after_an_upgrade() {
    let dir = tempfile::tempdir().unwrap();
    let files = dir.path().join("diagnostic-errors");
    std::fs::create_dir(&files).unwrap();
    let mut lines = String::new();
    let mut ids = Vec::new();
    for (body, age) in [
        ("fixture current error", 0),
        ("fixture expired error", super::RETENTION + 1),
    ] {
        let mut record = Record::new(
            Some(ClientId::Codex),
            Some(PROVIDER),
            Some(body),
            Reason::ProtocolError,
            Action::Returned,
            Some(502),
            None,
        );
        record.last_at = super::now() - age;
        record.first_at = record.last_at;
        let raw = crate::event_raw::Raw::bytes(body.as_bytes(), vec![], None);
        record.details.raw_error = Some(raw.reference(&record.id));
        std::fs::write(
            files.join(format!("{}.error", record.id)),
            format!("{{\"headers\":[],\"event\":null,\"incomplete\":null}}\n{body}"),
        )
        .unwrap();
        lines.push_str(&serde_json::to_string(&record).unwrap());
        lines.push('\n');
        ids.push(record.id);
    }
    std::fs::write(dir.path().join("diagnostics.jsonl"), lines).unwrap();
    let service = Service::new(dir.path());
    let mut changes = service.subscribe();
    // A queued write observes completion of the startup work without sleeps.
    service.emit(Record::new(
        None,
        None,
        None,
        Reason::Network,
        Action::Returned,
        None,
        None,
    ));
    assert!(changes.recv().await.unwrap().error.is_none());
    assert_eq!(
        service.raw(&ids[0]).unwrap().unwrap().body,
        "fixture current error"
    );
    assert!(service.detail(&ids[1]).is_none());
    assert_eq!(
        crate::event_raw::stored(&dir.path().join("diagnostic-errors.sqlite")),
        1
    );
    assert!(!files.exists());
}

#[test]
fn emit_checked_returns_false_and_reports_a_full_bounded_queue() {
    let (sender, _receiver) = mpsc::sync_channel(1);
    let (events, mut changes) = broadcast::channel(8);
    let service = Service {
        sender,
        journal: Arc::new(Mutex::new(Journal::default())),
        events,
        raw_store: Arc::new(std::env::temp_dir().join("unused-event-fixture")),
        file_cursors: Default::default(),
    };

    let accepted = Record::new(
        Some(ClientId::Codex),
        Some(PROVIDER),
        Some(MODEL),
        Reason::ProtocolError,
        Action::Returned,
        Some(502),
        Some(1),
    );
    assert!(service.emit_checked(accepted));

    let rejected = Record::new(
        Some(ClientId::Codex),
        Some(PROVIDER),
        Some(MODEL),
        Reason::ProtocolError,
        Action::Returned,
        Some(502),
        Some(2),
    );
    assert!(!service.emit_checked(rejected));

    let change = changes
        .try_recv()
        .expect("queue rejection should immediately publish a diagnostic error");
    assert!(change.record.is_none());
    assert_eq!(change.error.as_deref(), Some(QUEUE_FULL));
    assert!(!change.notify);

    let page = service.query(Filter::default());
    assert_eq!(page.error.as_deref(), Some(QUEUE_FULL));
    assert_eq!(
        page.total, 0,
        "the worker was intentionally held off the queue"
    );
}

#[tokio::test]
async fn burst_preserves_each_change_merge_notification_and_clear_barrier() {
    let dir = tempfile::tempdir().unwrap();
    let service = Service::new(dir.path());
    let mut changes = service.subscribe();
    let mut records = Vec::new();
    let (tx, done) = tokio::sync::oneshot::channel();
    {
        // Hold the published snapshot until the complete burst is queued.
        let _snapshot = service.journal.lock();
        for n in 0..70 {
            let record = Record::new(
                Some(ClientId::Codex),
                Some(PROVIDER),
                Some(&format!("burst-{n}")),
                Reason::CircuitOpen,
                Action::Returned,
                Some(503),
                None,
            );
            service
                .sender
                .send(super::Job::Append(Box::new(record.clone())))
                .unwrap();
            records.push(record);
        }
        let mut duplicate = records[69].clone();
        duplicate.id = uuid::Uuid::new_v4().to_string();
        service
            .sender
            .send(super::Job::Append(Box::new(duplicate)))
            .unwrap();
        service
            .sender
            .send(super::Job::NotificationAccepted(records[69].id.clone()))
            .unwrap();
        service
            .sender
            .send(super::Job::Clear(None, None, tx))
            .unwrap();
        let last = Record::new(
            None,
            None,
            Some("after-clear"),
            Reason::ProtocolError,
            Action::Returned,
            Some(502),
            None,
        );
        service
            .sender
            .send(super::Job::Append(Box::new(last)))
            .unwrap();
    }
    for record in &records {
        let change = changes.recv().await.unwrap();
        assert_eq!(change.record.unwrap().id, record.id);
        assert!(change.notify);
    }
    let merged = changes.recv().await.unwrap().record.unwrap();
    assert_eq!(merged.id, records[69].id);
    assert_eq!(merged.count, 2);
    let accepted = changes.recv().await.unwrap();
    assert!(!accepted.notify);
    assert!(accepted.record.unwrap().notified_at > 0);
    assert!(changes.recv().await.unwrap().record.is_none());
    done.await.unwrap().unwrap();
    let last = changes.recv().await.unwrap().record.unwrap();
    assert_eq!(last.model.as_deref(), Some("after-clear"));
    assert_eq!(service.query(Filter::default()).total, 1);
    assert!(service.journal.lock().aliases.is_empty());
    let restored = Service::new(dir.path());
    assert_eq!(restored.query(Filter::default()).items[0].id, last.id);
    restored.clear(None, None).await.unwrap();
}

#[tokio::test]
async fn duplicate_errors_merge_before_attachment_io_and_distinct_bodies_remain_complete() {
    let dir = tempfile::tempdir().unwrap();
    let service = Service::new(dir.path());
    let mut changes = service.subscribe();
    let make = |body: &[u8]| {
        let mut record = Record::new(
            Some(ClientId::Codex),
            Some(PROVIDER),
            Some(MODEL),
            Reason::Capacity,
            Action::Waiting,
            Some(200),
            Some(1),
        );
        record.details.raw = Some(Arc::new(crate::event_raw::Raw::bytes(body, vec![], None)));
        record
    };
    let first = make(b"fixture first complete error");
    let first_id = first.id.clone();
    service.emit(first);
    assert!(changes.recv().await.unwrap().error.is_none());
    let path = dir.path().join("diagnostic-errors.sqlite");
    let retained = dir.path().join("retained-errors");
    std::fs::rename(&path, &retained).unwrap();
    std::fs::write(&path, b"fixture write blocker").unwrap();
    service.emit(make(b"fixture first complete error"));
    let change = changes.recv().await.unwrap();
    assert!(
        change.error.is_none(),
        "a duplicate must not write a new attachment"
    );
    let merged = change.record.unwrap();
    assert_eq!(merged.id, first_id);
    assert_eq!(merged.count, 2);
    std::fs::remove_file(&path).unwrap();
    std::fs::rename(&retained, &path).unwrap();
    let second = make(b"fixture second complete error");
    let second_id = second.id.clone();
    service.emit(second);
    assert!(changes.recv().await.unwrap().error.is_none());
    assert_eq!(service.query(Filter::default()).total, 2);
    assert_eq!(crate::event_raw::stored(&path), 2);
    assert_eq!(
        service.raw(&first_id).unwrap().unwrap().body,
        "fixture first complete error"
    );
    assert_eq!(
        service.raw(&second_id).unwrap().unwrap().body,
        "fixture second complete error"
    );
}

#[test]
fn source_import_during_snapshot_write_remains_pending_until_next_write() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("diagnostics.jsonl");
    let (sender, _receiver) = mpsc::sync_channel(8);
    let service = Service {
        sender,
        journal: Arc::new(Mutex::new(Journal::default())),
        events: broadcast::channel(8).0,
        raw_store: Arc::new(dir.path().join("diagnostic-errors.sqlite")),
        file_cursors: Default::default(),
    };
    let record = |model| {
        Record::new(
            Some(ClientId::Claude),
            None,
            Some(model),
            Reason::Capacity,
            Action::Returned,
            Some(429),
            None,
        )
    };
    service.add_external(vec![record("fixture-before")]);
    let snapshot = service
        .journal
        .lock()
        .prepare_rewrite(&path, false)
        .unwrap();
    assert!(!service.journal.lock().dirty);
    service.add_external(vec![record("fixture-during")]);
    let written = super::write_records_stream(&path, &snapshot);
    service.journal.lock().finish_write(written, Ok(()));
    assert!(service.journal.lock().needs_write(&path));
    assert_eq!(super::load(&path).records.len(), 1);
    let snapshot = service
        .journal
        .lock()
        .prepare_rewrite(&path, false)
        .unwrap();
    service
        .journal
        .lock()
        .finish_write(super::write_records_stream(&path, &snapshot), Ok(()));
    assert!(!service.journal.lock().needs_write(&path));
    assert_eq!(super::load(&path).records.len(), 2);
    let snapshot = service.journal.lock().prepare_rewrite(&path, true).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    service
        .journal
        .lock()
        .finish_write(super::write_records_stream(&path, &snapshot), Ok(()));
    assert!(service.journal.lock().dirty);
    std::fs::remove_dir(&path).unwrap();
    let snapshot = service
        .journal
        .lock()
        .prepare_rewrite(&path, false)
        .unwrap();
    service
        .journal
        .lock()
        .finish_write(super::write_records_stream(&path, &snapshot), Ok(()));
    assert_eq!(super::load(&path).records.len(), 2);
}

fn fixture(model: &str) -> Record {
    Record::new(
        Some(ClientId::Codex),
        Some(PROVIDER),
        Some(model),
        Reason::ProtocolError,
        Action::Returned,
        Some(502),
        Some(1),
    )
}

fn repeat(record: &Record) -> Record {
    let mut repeated = record.clone();
    repeated.id = uuid::Uuid::new_v4().to_string();
    repeated
}

fn file_lines(path: &std::path::Path) -> Vec<serde_json::Value> {
    let text = std::fs::read_to_string(path).unwrap();
    assert!(text.is_empty() || text.ends_with('\n'));
    text.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn rows(journal: &Journal) -> Vec<String> {
    journal
        .records
        .iter()
        .map(|r| serde_json::to_string(&**r).unwrap())
        .collect()
}

#[tokio::test]
async fn appended_merges_and_acknowledgments_reload_as_the_rows_in_memory() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("diagnostics.jsonl");
    let service = Service::new(dir.path());
    let mut changes = service.subscribe();
    let records: Vec<_> = ["fixture-a", "fixture-b", "fixture-c"]
        .into_iter()
        .map(fixture)
        .collect();
    for record in &records {
        service.emit(record.clone());
        changes.recv().await.unwrap();
    }
    for _ in 0..2 {
        service.emit(repeat(&records[1]));
        let merged = changes.recv().await.unwrap().record.unwrap();
        assert_eq!(merged.id, records[1].id);
    }
    service
        .sender
        .send(super::Job::NotificationAccepted(records[0].id.clone()))
        .unwrap();
    assert!(changes.recv().await.unwrap().record.unwrap().notified_at > 0);

    // Every change was added to the end; none replaced the file.
    let lines = file_lines(&path);
    assert_eq!(lines.len(), 6);
    assert_eq!(lines[4]["count"], 3);
    assert_eq!(lines[5]["id"], records[0].id.as_str());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let kept = rows(&service.journal.lock());
    let page = serde_json::to_value(service.query(Filter::default()).items).unwrap();
    assert_eq!(service.journal.lock().superseded, 3);
    drop(service);

    let loaded = super::load(&path);
    assert!(loaded.error.is_none());
    assert!(!loaded.dirty);
    assert_eq!(loaded.superseded, 3);
    assert_eq!(rows(&loaded), kept);
    let reopened = Service::new(dir.path());
    assert_eq!(
        serde_json::to_value(reopened.query(Filter::default()).items).unwrap(),
        page
    );
    assert_eq!(reopened.detail(&records[1].id).unwrap().count, 3);
    assert!(reopened.detail(&records[0].id).unwrap().notified_at > 0);
}

#[tokio::test]
async fn interrupted_append_is_skipped_and_the_next_start_repairs_the_file() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("diagnostics.jsonl");
    let first = fixture("fixture-a");
    let second = fixture("fixture-b");
    let line = serde_json::to_string(&second).unwrap();
    let half = &line[..line.len() / 2];
    let whole = format!("{}\n", serde_json::to_string(&first).unwrap());

    // The write stopped inside a row: the row is lost, the rest is read.
    std::fs::write(&path, format!("{whole}{half}")).unwrap();
    let cut = super::load(&path);
    assert!(cut.error.is_none());
    assert!(cut.dirty);
    assert_eq!(rows(&cut), [serde_json::to_string(&first).unwrap()]);

    // The write stopped before the line end: the row is kept, and the next
    // write replaces the file instead of continuing that line.
    std::fs::write(&path, format!("{whole}{line}")).unwrap();
    let unfinished = super::load(&path);
    assert!(unfinished.error.is_none());
    assert!(unfinished.dirty);
    assert_eq!(unfinished.records.len(), 2);

    // A damaged line that is not the unfinished last one is still reported.
    std::fs::write(&path, format!("{whole}{half}\n")).unwrap();
    assert!(super::load(&path).error.is_some());

    std::fs::write(&path, format!("{whole}{half}")).unwrap();
    let service = Service::new(dir.path());
    let mut changes = service.subscribe();
    // A queued write observes completion of the startup rewrite without sleeps.
    let third = fixture("fixture-c");
    service.emit(third.clone());
    assert!(changes.recv().await.unwrap().error.is_none());
    let lines = file_lines(&path);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["id"], first.id.as_str());
    assert_eq!(lines[1]["id"], third.id.as_str());
    drop(service);
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(half.as_bytes())
        .unwrap();
    let reopened = Service::new(dir.path());
    let page = reopened.query(Filter::default());
    assert!(page.error.is_none());
    assert_eq!(page.total, 2);
}

#[tokio::test]
async fn replaced_lines_past_the_limit_and_rows_long_expired_rewrite_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("diagnostics.jsonl");
    let service = Service::new(dir.path());
    let mut changes = service.subscribe();
    let first = fixture("fixture-a");
    let second = fixture("fixture-b");
    for record in [&first, &second, &repeat(&second)] {
        service.emit(record.clone());
        changes.recv().await.unwrap();
    }
    assert_eq!(file_lines(&path).len(), 3);

    service.journal.lock().superseded = super::SUPERSEDED_LINES;
    service.emit(repeat(&second));
    assert_eq!(changes.recv().await.unwrap().record.unwrap().count, 3);
    let lines = file_lines(&path);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["id"], first.id.as_str());
    assert_eq!(lines[1]["count"], 3);
    assert_eq!(service.journal.lock().superseded, 0);

    // A row just past retention is hidden at once and leaves the file with
    // the next batch, not with the next event.
    let expire = |seconds| {
        let mut journal = service.journal.lock();
        Arc::make_mut(&mut journal.records[0]).last_at = super::now() - seconds;
    };
    expire(super::RETENTION + 1);
    service.emit(fixture("fixture-c"));
    changes.recv().await.unwrap();
    assert_eq!(file_lines(&path).len(), 3);
    assert_eq!(service.query(Filter::default()).total, 2);
    assert!(service.detail(&first.id).is_none());

    expire(super::RETENTION + super::EXPIRY_BATCH + 1);
    service.emit(fixture("fixture-d"));
    changes.recv().await.unwrap();
    let lines = file_lines(&path);
    assert_eq!(lines.len(), 3);
    assert!(lines.iter().all(|line| line["id"] != first.id.as_str()));
    assert_eq!(service.journal.lock().records.len(), 3);
    assert_eq!(service.query(Filter::default()).total, 3);
}

#[tokio::test]
async fn clear_drops_appended_lines_and_restart_drops_expired_rows_with_their_replacements() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("diagnostics.jsonl");
    let service = Service::new(dir.path());
    let mut changes = service.subscribe();
    let first = fixture("fixture-a");
    for record in [&first, &repeat(&first)] {
        service.emit(record.clone());
        changes.recv().await.unwrap();
    }
    assert_eq!(file_lines(&path).len(), 2);
    service.clear(None, None).await.unwrap();
    assert!(changes.recv().await.unwrap().record.is_none());
    let lines = file_lines(&path);
    assert_eq!(lines.len(), 1);
    assert!(lines[0]["cleared"]["at"].is_u64());
    assert_eq!(service.journal.lock().superseded, 0);

    // Rows after a clear follow the header, which later appends leave alone.
    let second = fixture("fixture-b");
    for record in [&second, &repeat(&second)] {
        service.emit(record.clone());
        changes.recv().await.unwrap();
    }
    let lines = file_lines(&path);
    assert_eq!(lines.len(), 3);
    assert!(lines[0]["cleared"]["at"].is_u64());
    let cleared_at = service.journal.lock().cleared.as_ref().unwrap().at;
    drop(service);
    let loaded = super::load(&path);
    assert_eq!(loaded.cleared.as_ref().unwrap().at, cleared_at);
    assert!(!loaded.dirty);
    assert_eq!(loaded.records.len(), 1);
    assert_eq!(loaded.records[0].count, 2);

    // An expired row goes at startup whichever of its lines is read.
    let mut expired = second.clone();
    expired.last_at = super::now() - super::RETENTION - 1;
    expired.first_at = expired.last_at;
    let mut replaced = expired.clone();
    replaced.count = 2;
    let kept = fixture("fixture-c");
    let text: String = [&expired, &kept, &replaced]
        .into_iter()
        .map(|record| format!("{}\n", serde_json::to_string(record).unwrap()))
        .collect();
    std::fs::write(&path, text).unwrap();
    let reopened = Service::new(dir.path());
    let mut changes = reopened.subscribe();
    reopened.emit(fixture("fixture-d"));
    changes.recv().await.unwrap();
    let lines = file_lines(&path);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["id"], kept.id.as_str());
    assert!(reopened.detail(&second.id).is_none());
    assert_eq!(reopened.query(Filter::default()).total, 2);
}

// 覆盖 Antigravity CLI 会话用量导入：计量口径、增量与幂等、账号归属、回溯重跑、估算耗时、模型回退与计价、路径归属、只读，以及与网关记录的去重。
use super::super::super::store::Store;
use super::*;

/// Encodes the few protobuf fields the fixtures need.
mod proto {
    fn varint(mut n: u64, out: &mut Vec<u8>) {
        while n >= 0x80 {
            out.push(n as u8 | 0x80);
            n >>= 7;
        }
        out.push(n as u8);
    }
    pub(super) fn number(field: u64, n: u64) -> Vec<u8> {
        let mut out = Vec::new();
        varint(field << 3, &mut out);
        varint(n, &mut out);
        out
    }
    pub(super) fn bytes(field: u64, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        varint(field << 3 | 2, &mut out);
        varint(body.len() as u64, &mut out);
        out.extend_from_slice(body);
        out
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    store: Store,
    pricing: Pricing,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("db")).unwrap();
        let pricing = Pricing::new(&dir.path().join("db")).unwrap();
        Self {
            dir,
            store,
            pricing,
        }
    }
    fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }
    fn transcript(&self, session: &str) -> PathBuf {
        self.home()
            .join("antigravity-cli/brain")
            .join(session)
            .join(TRANSCRIPT)
    }
    fn write(&self, session: &str, lines: &str) {
        let path = self.transcript(session);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, lines).unwrap();
    }
    fn append(&self, session: &str, text: &str) {
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(self.transcript(session))
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
    }
    /// A conversation database with the two tables the import reads.
    fn database(&self, session: &str) -> Connection {
        let dir = self.home().join("antigravity-cli/conversations");
        std::fs::create_dir_all(&dir).unwrap();
        let db = Connection::open(dir.join(format!("{session}.db"))).unwrap();
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS steps (idx INTEGER PRIMARY KEY, step_type INTEGER, metadata BLOB);
             CREATE TABLE IF NOT EXISTS gen_metadata (idx INTEGER PRIMARY KEY, data BLOB);",
        )
        .unwrap();
        db
    }
    fn sync(&mut self) -> Report {
        let (home, mut report) = (self.home(), Report::default());
        scan(
            &mut self.store,
            &home,
            &self.pricing,
            &Settings::default(),
            &mut report,
            false,
        );
        report
    }
    fn row(&self, session: &str, step: u64) -> Record {
        self.store
            .detail(&safe_id(&format!("antigravity:cli:{session}:step:{step}")))
            .unwrap()
    }
    /// The row of a step run after `rewinds` rewinds.
    fn rewound(&self, session: &str, step: u64, rewinds: u64) -> Record {
        let id = format!("antigravity:cli:{session}:step:{step}:rewind:{rewinds}");
        self.store.detail(&safe_id(&id)).unwrap()
    }
    /// An account vault with the time each account was signed in.
    fn accounts(&self, activations: &[(&str, i64)]) {
        let entries: Vec<_> = activations
            .iter()
            .map(|(id, _)| json!({"id": id, "name": id, "kind": "oauth", "sourceType": "fixture", "payload": "{}", "updatedAt": 0}))
            .collect();
        let activations: Vec<_> = activations
            .iter()
            .map(|(id, since)| json!({"accountId": id, "sinceMs": since}))
            .collect();
        std::fs::write(
            self.store
                .data_dir()
                .unwrap()
                .join("antigravity-accounts.json"),
            json!({"schema": 1, "entries": entries, "activations": activations}).to_string(),
        )
        .unwrap();
    }
    fn cursor_key(&self, session: &str) -> String {
        let path = self.transcript(session).canonicalize().unwrap();
        safe_id(&format!("antigravity:cli:file:{}", path.to_string_lossy()))
    }
    fn requests(&self) -> u64 {
        self.store
            .dashboard(&Filter::default())
            .unwrap()
            .totals
            .requests
    }
}

/// The time of a line written `second` seconds into the fixture session.
fn at(second: u64) -> i64 {
    chrono::DateTime::parse_from_rfc3339(&format!("2026-01-01T00:00:{second:02}Z"))
        .unwrap()
        .timestamp_millis()
}
/// One finished model step as agy writes it, `step` seconds into the session.
fn planner(step: u64, model: Option<&str>, usage: [u64; 3]) -> String {
    planner_at(step, step, model, usage)
}
fn planner_at(
    step: u64,
    second: u64,
    model: Option<&str>,
    [input, cached, output]: [u64; 3],
) -> String {
    let mut line = json!({
        "step_index": step, "source": "MODEL", "type": "PLANNER_RESPONSE", "status": "DONE",
        "created_at": format!("2026-01-01T00:00:{second:02}Z"),
        "input_tokens": input, "cache_read_tokens": cached, "output_tokens": output,
    });
    if let Some(model) = model {
        line["model"] = json!(model);
    }
    format!("{line}\n")
}
/// A line without usage: the user's turn or a tool's result.
fn other(step: u64, kind: &str, second: u64) -> String {
    let line = json!({
        "step_index": step, "source": "SYSTEM", "type": kind, "status": "DONE",
        "created_at": format!("2026-01-01T00:00:{second:02}Z"),
    });
    format!("{line}\n")
}
fn selection(step: u64, change: &str) -> String {
    let content = format!("<USER_SETTINGS_CHANGE>\n{change}\n</USER_SETTINGS_CHANGE>");
    let line = json!({
        "step_index": step, "source": "USER_EXPLICIT", "type": "USER_INPUT", "status": "DONE",
        "created_at": "2026-01-01T00:00:00Z", "content": content,
    });
    format!("{line}\n")
}
fn save_step(db: &Connection, step: u64, response: &str, thinking: u64, generator: u64) {
    let usage = [
        proto::number(2, 1),
        proto::number(9, thinking),
        proto::bytes(11, response.as_bytes()),
    ]
    .concat();
    // Sent at 100 s, first content 2.014 s later, done at 4.032 s.
    let at =
        |seconds: u64, nanos: u64| [proto::number(1, seconds), proto::number(2, nanos)].concat();
    let metadata = [
        proto::bytes(1, &at(100, 0)),
        proto::bytes(6, &at(102, 14_000_000)),
        proto::bytes(7, &at(104, 32_000_000)),
        proto::bytes(9, &usage),
        proto::bytes(20, &proto::number(3, generator)),
    ]
    .concat();
    db.execute(
        "INSERT OR REPLACE INTO steps VALUES (?1, 15, ?2)",
        (step as i64, metadata),
    )
    .unwrap();
}
fn save_generator(db: &Connection, index: u64, key: &str, served: &str, shown: &str) {
    let entry = |key: &str, value: &str| {
        let pair = [
            proto::bytes(1, key.as_bytes()),
            proto::bytes(2, value.as_bytes()),
        ];
        proto::bytes(20, &pair.concat())
    };
    let chat = [
        proto::number(3, 1900),
        proto::bytes(19, served.as_bytes()),
        entry("request_id", "request-fixture"),
        entry("model_enum", key),
        proto::bytes(21, shown.as_bytes()),
    ]
    .concat();
    db.execute(
        "INSERT OR REPLACE INTO gen_metadata VALUES (?1, ?2)",
        (index as i64, proto::bytes(1, &chat)),
    )
    .unwrap();
}
fn model(row: &Record) -> (Option<&str>, Option<&str>, Option<&str>) {
    let a = &row.attempts[0];
    (
        a.response_model.as_deref(),
        a.model_display_name.as_deref(),
        a.reasoning_effort.as_deref(),
    )
}

// 输入不含缓存、输出含思考，按原值入库；重复同步与整文件重读都不新增记录。
#[test]
fn counts_are_stored_as_written_and_syncing_again_adds_nothing() {
    let mut f = Fixture::new();
    f.write(
        "session",
        &planner(1, Some("gemini-3.1-pro-high"), [1000, 200, 80]),
    );
    save_step(&f.database("session"), 1, "response-fixture", 30, 0);
    let report = f.sync();
    assert_eq!(
        (report.errors, report.imported, report.files),
        (0, 1, 1),
        "{:?}",
        report.last_error
    );
    let row = f.row("session", 1);
    assert_eq!(
        (row.client.as_str(), row.source.as_str()),
        ("antigravity", "antigravity")
    );
    assert_eq!(row.session_id, Some(safe_id("session")));
    let a = &row.attempts[0];
    // Timed by the conversation database.
    assert_eq!(
        (a.stream, a.first_token_ms, a.generation_ms, a.duration_ms),
        (true, Some(2014), Some(2018), 4032)
    );
    assert!(a.parse_reasons.is_empty());
    assert_eq!(
        (
            a.tokens.input,
            a.tokens.cache_read,
            a.tokens.cache_write,
            a.tokens.output,
            a.tokens.reasoning,
            a.tokens.inclusive_input
        ),
        (Some(1000), Some(200), None, Some(80), Some(30), Some(1200))
    );
    assert_eq!(a.input_basis.as_deref(), Some("exclusive"));
    assert_eq!(a.transport, "cli_file");
    assert_eq!(a.response_id, Some(safe_id("response-fixture")));
    // No account is known to have been signed in.
    assert_eq!(a.provider, None);
    assert_eq!(model(&row).0, Some("gemini-3.1-pro"));
    assert!(a.price.is_some());
    let totals = f.store.dashboard(&Filter::default()).unwrap().totals;
    assert_eq!(
        (
            totals.requests,
            totals.tokens.input,
            totals.tokens.cache_read,
            totals.tokens.output
        ),
        (1, Some(1000), Some(200), Some(80))
    );

    assert_eq!(f.sync().imported, 0);
    let (home, mut report) = (f.home(), Report::default());
    scan(
        &mut f.store,
        &home,
        &f.pricing,
        &Settings::default(),
        &mut report,
        true,
    );
    assert_eq!(report.errors, 0);
    assert_eq!(f.requests(), 1);
    assert_eq!(
        f.store.dashboard(&Filter::default()).unwrap().totals.cost,
        totals.cost
    );
}

// 尾部未写完的行下次再读；同一步骤取最后写入的一行；已归档的明细不再被改写。
#[test]
fn an_unfinished_tail_is_read_again_and_the_last_line_of_a_step_wins() {
    let mut f = Fixture::new();
    let half = planner(2, Some("gemini-3.1-pro"), [50, 0, 7]);
    let (head, rest) = half.split_at(40);
    f.write(
        "session",
        &[
            &planner(1, Some("gemini-3.1-pro"), [100, 20, 12]),
            &planner(1, Some("gemini-3.1-pro"), [100, 20, 5]),
            head,
        ]
        .concat(),
    );
    let report = f.sync();
    assert_eq!((report.errors, report.imported), (0, 1));
    assert_eq!(f.row("session", 1).attempts[0].tokens.output, Some(5));

    f.append("session", rest);
    assert_eq!(f.sync().imported, 1);
    assert_eq!(f.requests(), 2);
    assert_eq!(f.row("session", 2).attempts[0].tokens.output, Some(7));

    // A finished step written again updates its row instead of adding one,
    // with a smaller count too: every line read is a finished one.
    for output in [15, 9] {
        f.append(
            "session",
            &planner(2, Some("gemini-3.1-pro"), [50, 0, output]),
        );
        f.sync();
        assert_eq!(f.requests(), 2);
        assert_eq!(f.row("session", 2).attempts[0].tokens.output, Some(output));
    }

    // Once its detail is archived the summary stands.
    let archived = f.row("session", 2).id;
    f.store.archive_batch(&[archived]).unwrap();
    f.append("session", &planner(2, Some("gemini-3.1-pro"), [50, 0, 99]));
    assert_eq!(f.sync().errors, 0);
    let totals = f.store.dashboard(&Filter::default()).unwrap().totals;
    assert_eq!((totals.requests, totals.tokens.output), (2, Some(14)));
    assert_eq!(f.store.logs(&Filter::default()).unwrap().total, 1);
}

// agy 与 App 共用登录：记录归到请求时登录的账号并计入该账号的额度窗口，登录之前的请求不归属。
#[test]
fn a_row_belongs_to_the_account_signed_in_at_its_time() {
    let mut f = Fixture::new();
    f.accounts(&[("account-a", at(2)), ("account-b", at(4))]);
    let model = Some("gemini-3.1-pro");
    let lines: Vec<String> = (1..=5)
        .map(|step| planner(step, model, [100, 0, 10]))
        .collect();
    f.write("session", &lines.concat());
    assert_eq!(f.sync().errors, 0);
    let account = |step| f.row("session", step).attempts[0].provider.clone();
    assert_eq!(account(1), None);
    for (step, id) in [(2, "account-a"), (3, "account-a"), (4, "account-b")] {
        assert_eq!(account(step).as_deref(), Some(id), "{step}");
    }

    let periods = quota::parse(&json!({"response": {"groups": [{"buckets": [
        {"bucketId": "gemini-5h", "window": "5h", "resetTime": "2026-01-01T05:00:00Z", "remainingFraction": 0.5},
    ]}]}}));
    f.store
        .save_antigravity_quota("account-a", &periods)
        .unwrap();
    let stats = f
        .store
        .antigravity_stats(&Filter::default(), at(30))
        .unwrap();
    let a = stats.accounts.iter().find(|a| a.id == "account-a").unwrap();
    assert_eq!(
        (a.tracks[0].burst_5h.requests, a.tracks[0].burst_5h.tokens),
        (2, 220)
    );
    let mut groups: Vec<&str> = stats.providers.iter().map(|p| p.id.as_str()).collect();
    groups.sort_unstable();
    assert_eq!(groups, ["account-a", "account-b", "session"]);
}

// 回溯后同一步骤序号再次执行并各自计费：两次执行都计入，首次执行沿用原有记录标识；
// 被回溯覆盖的步骤不取会话库里同序号（已是重跑后）的元数据；整份读取与分两次增量读取结果相同。
#[test]
fn steps_run_again_after_a_rewind_are_counted_beside_the_first_run() {
    let model = Some("gemini-3.1-pro");
    let first = [
        other(0, "USER_INPUT", 0),
        planner(1, model, [100, 0, 10]),
        other(2, "GENERIC", 2),
        planner(3, model, [100, 0, 20]),
    ]
    .concat();
    // The user goes back to step 2; step 3 runs again and is written twice.
    let again = [
        other(2, "USER_INPUT", 12),
        planner_at(3, 13, model, [100, 0, 30]),
        planner_at(3, 13, model, [100, 0, 31]),
        other(4, "GENERIC", 14),
        planner_at(5, 15, model, [100, 0, 40]),
    ]
    .concat();
    let rows = |f: &Fixture| {
        [
            f.row("session", 1),
            f.row("session", 3),
            f.rewound("session", 3, 1),
            f.rewound("session", 5, 1),
        ]
        .map(|row| {
            (
                row.id.clone(),
                row.attempts[0].tokens.output,
                row.duplicate_of.clone(),
            )
        })
    };
    let outputs = |f: &Fixture| {
        let totals = f.store.dashboard(&Filter::default()).unwrap().totals;
        (totals.requests, totals.tokens.output)
    };

    let mut whole = Fixture::new();
    whole.write("session", &[first.as_str(), &again].concat());
    let db = whole.database("session");
    save_step(&db, 1, "response-1", 1, 0);
    save_step(&db, 3, "response-again", 1, 0);
    save_step(&db, 5, "response-5", 1, 0);
    let report = whole.sync();
    assert_eq!((report.errors, report.imported), (0, 4));
    assert_eq!(
        rows(&whole).map(|(_, output, duplicate)| (output, duplicate)),
        [10, 20, 31, 40].map(|output| (Some(output), None))
    );
    assert_eq!(outputs(&whole), (4, Some(101)));
    // Step 3 of the database is the second run.
    let first_run = whole.row("session", 3);
    assert_eq!(first_run.attempts[0].response_id, None);
    assert_eq!(first_run.attempts[0].tokens.reasoning, None);
    assert_eq!(first_run.attempts[0].first_token_ms, None);
    assert_eq!(
        whole.rewound("session", 3, 1).attempts[0].response_id,
        Some(safe_id("response-again"))
    );
    assert_eq!(
        whole.row("session", 1).attempts[0].response_id,
        Some(safe_id("response-1"))
    );

    let mut steps = Fixture::new();
    steps.write("session", &first);
    let db = steps.database("session");
    save_step(&db, 1, "response-1", 1, 0);
    save_step(&db, 3, "response-first", 1, 0);
    assert_eq!(steps.sync().imported, 2);
    save_step(&db, 3, "response-again", 1, 0);
    save_step(&db, 5, "response-5", 1, 0);
    steps.append("session", &again);
    let report = steps.sync();
    assert_eq!((report.errors, report.imported), (0, 2));
    assert_eq!(rows(&steps), rows(&whole));
    assert_eq!(outputs(&steps), (4, Some(101)));
    // Read whole again, the same rows stand.
    let (home, mut report) = (steps.home(), Report::default());
    scan(
        &mut steps.store,
        &home,
        &steps.pricing,
        &Settings::default(),
        &mut report,
        true,
    );
    assert_eq!(report.errors, 0);
    assert_eq!(rows(&steps), rows(&whole));
    assert_eq!(outputs(&steps), (4, Some(101)));
}

// 旧版本保存的同步位置在升级后首次同步时作废：会话整份重读，已有记录补上账号，
// 曾被重跑覆盖的记录恢复为首次执行的数值，重跑另记一条；之后的同步不再重读。
#[test]
fn a_position_saved_by_an_older_reading_starts_over() {
    let mut f = Fixture::new();
    let model = Some("gemini-3.1-pro");
    // What the older reading stored for step 3: its second run.
    f.write("session", &planner_at(3, 13, model, [100, 0, 31]));
    assert_eq!(f.sync().imported, 1);
    f.write(
        "session",
        &[
            planner(3, model, [100, 0, 20]),
            other(2, "USER_INPUT", 12),
            planner_at(3, 13, model, [100, 0, 31]),
        ]
        .concat(),
    );
    f.accounts(&[("account-a", 0)]);
    let bytes = std::fs::read(f.transcript("session")).unwrap();
    let meta = std::fs::metadata(f.transcript("session")).unwrap();
    let read = Position {
        offset: bytes.len() as u64,
        tail: storage::digest(&bytes[bytes.len().saturating_sub(4096)..]),
        identity: file_identity(&meta),
        modified: file_modified(&meta).unwrap(),
        complete: true,
        reading: READING,
        ..Position::default()
    };
    let key = f.cursor_key("session");
    let save = |f: &mut Fixture, position: &Value| {
        f.store
            .write_batch(&[], Some((&key, &position.to_string())))
            .unwrap();
    };
    // Read to its end by this reading, the file is left alone.
    let mut position = serde_json::to_value(&read).unwrap();
    save(&mut f, &position);
    assert_eq!(f.sync().imported, 0);
    assert_eq!(f.row("session", 3).attempts[0].tokens.output, Some(31));

    let fields = position.as_object_mut().unwrap();
    fields.remove("reading");
    fields.remove("walk");
    fields.insert("timed".into(), json!(true));
    save(&mut f, &position);
    let report = f.sync();
    assert_eq!((report.errors, report.imported), (0, 2));
    let first = f.row("session", 3);
    assert_eq!(first.attempts[0].tokens.output, Some(20));
    assert_eq!(first.attempts[0].provider.as_deref(), Some("account-a"));
    assert_eq!(
        f.rewound("session", 3, 1).attempts[0].tokens.output,
        Some(31)
    );
    assert_eq!(f.requests(), 2);
    assert_eq!(f.sync().imported, 0);
}

// 会话库没有计时的步骤，以其后第一个其他步骤的行估算耗时：该行是工具结果时取两行创建时间之差，
// 是用户输入、系统消息或尚无后续行时不估算；会话库已有计时的步骤不估算。估算值不进入耗时与统计。
#[test]
fn a_step_the_database_does_not_time_is_timed_by_the_tool_result_after_it() {
    let mut f = Fixture::new();
    let model = Some("gemini-3.1-pro");
    let usage = [100, 0, 10];
    f.write(
        "session",
        &[
            planner_at(1, 1, model, usage),
            other(2, "GENERIC", 4),
            planner_at(3, 5, model, usage),
            other(4, "USER_INPUT", 9),
            planner_at(5, 10, model, usage),
            other(6, "SYSTEM_MESSAGE", 12),
            planner_at(7, 13, model, usage),
            other(8, "GENERIC", 20),
            // Written again as it finishes; a result in the same second times nothing.
            planner_at(9, 21, model, usage),
            planner_at(9, 21, model, usage),
            other(10, "GENERIC", 21),
            planner_at(11, 30, model, usage),
            planner_at(11, 31, model, usage),
            other(12, "GENERIC", 33),
            planner_at(13, 40, model, usage),
        ]
        .concat(),
    );
    save_step(&f.database("session"), 7, "response-7", 1, 0);
    assert_eq!(f.sync().errors, 0);
    let estimate = |step| f.row("session", step).attempts[0].estimated_duration_ms;
    assert_eq!(
        [1, 3, 5, 7, 9, 11, 13].map(estimate),
        [Some(3000), None, None, None, None, Some(2000), None]
    );
    let estimated = f.row("session", 1);
    let a = &estimated.attempts[0];
    assert_eq!((a.duration_ms, a.measured_duration_ms()), (0, None));
    assert_eq!(a.parse_reasons, ["attempt_time_unreported"]);
    // As on a row with no estimate.
    assert_eq!(a.availability, f.row("session", 3).attempts[0].availability);
    assert_eq!(f.row("session", 7).attempts[0].duration_ms, 4032);
    let totals = f.store.dashboard(&Filter::default()).unwrap().totals;
    assert_eq!((totals.requests, totals.duration_ms), (7, 4032));
}

// 末尾的模型步骤还没有后续行时不估算；文件变长后从该行重读并补上估算，结果与整份读取相同，
// 回溯计数与重读起点一致，未变化的记录不重复写入。
#[test]
fn a_step_at_the_end_of_the_file_is_timed_once_its_follower_is_written() {
    let model = Some("gemini-3.1-pro");
    let usage = [100, 0, 10];
    let parts = [
        [
            planner_at(5, 1, model, usage),
            other(2, "USER_INPUT", 2),
            planner_at(3, 3, model, usage),
        ]
        .concat(),
        [other(4, "GENERIC", 6), planner_at(5, 7, model, usage)].concat(),
        other(6, "USER_INPUT", 9),
    ];
    let rows = |f: &Fixture| {
        [
            f.row("session", 5),
            f.rewound("session", 3, 1),
            f.rewound("session", 5, 1),
        ]
        .map(|row| row.attempts[0].estimated_duration_ms)
    };
    let mut whole = Fixture::new();
    whole.write("session", &parts.concat());
    assert_eq!(whole.sync().imported, 3);
    assert_eq!(rows(&whole), [None, Some(3000), None]);

    let mut f = Fixture::new();
    f.write("session", &parts[0]);
    assert_eq!(f.sync().imported, 2);
    assert_eq!(
        f.rewound("session", 3, 1).attempts[0].estimated_duration_ms,
        None
    );
    // Nothing new: the waiting line is left alone.
    assert_eq!(f.sync().imported, 0);
    // A follower still being written times nothing yet.
    let (head, rest) = parts[1].split_at(20);
    f.append("session", head);
    assert_eq!(f.sync().imported, 0);
    f.append("session", rest);
    let report = f.sync();
    assert_eq!((report.errors, report.imported), (0, 2));
    f.append("session", &parts[2]);
    assert_eq!(f.sync().imported, 0);
    assert_eq!(rows(&f), rows(&whole));
    assert_eq!(f.requests(), 3);
    assert_eq!(whole.requests(), 3);
}

// 进行中的步骤和非模型步骤不计入；缺少步骤序号的用量行报告为未同步，其后的步骤照常导入且不重复。
#[test]
fn a_line_that_cannot_be_counted_is_reported_and_the_rest_is_still_read() {
    let mut f = Fixture::new();
    let usage = [100, 0, 10];
    let running = planner(1, Some("gemini-3.1-pro"), usage).replace("\"DONE\"", "\"RUNNING\"");
    let tool = planner(2, Some("gemini-3.1-pro"), usage).replace("PLANNER_RESPONSE", "GENERIC");
    let unnumbered = json!({
        "source": "MODEL", "type": "PLANNER_RESPONSE", "status": "DONE",
        "created_at": "2026-01-01T00:00:03Z", "input_tokens": 5, "output_tokens": 1,
    });
    f.write(
        "session",
        &[
            running,
            tool,
            format!("{unnumbered}\n"),
            planner(4, Some("gemini-3.1-pro"), usage),
        ]
        .concat(),
    );
    for _ in 0..2 {
        let report = f.sync();
        assert_eq!(report.errors, 1);
        assert_eq!(
            report.last_error.as_deref(),
            Some("Antigravity CLI 用量缺少步骤序号或时间，未计入")
        );
        assert_eq!(f.requests(), 1);
        assert_eq!(f.row("session", 4).attempts[0].tokens.output, Some(10));
    }
}

// 模型依次取：该行的模型字段、此前的 Model Selection、会话库的生成器；都没有时留空且不计价。
#[test]
fn a_model_comes_from_the_line_then_the_selection_then_the_conversation_database() {
    let mut f = Fixture::new();
    let mut list = Learned::default();
    list.merge(
        &json!({"response":{"models":{"fixture-neon-tiered-high":{"model":"MODEL_PLACEHOLDER_M900","displayName":"Fixture Neon (High)"}}}}),
        1,
    );
    f.store
        .save_antigravity_models(&serde_json::to_string(&list).unwrap())
        .unwrap();
    let usage = [10, 0, 2];
    f.write(
        "session",
        &[
            selection(
                0,
                "The user changed setting `Model Selection` from None to Gemini 3.1 Pro (High). No need to comment on this change.",
            ),
            planner(1, Some("fixture-argon-xhigh"), usage),
            planner(2, None, usage),
            selection(3, "The user changed something else."),
            planner(4, None, usage),
            planner(5, None, usage),
            planner(6, None, usage),
            planner(7, None, usage),
        ]
        .concat(),
    );
    let db = f.database("session");
    save_generator(
        &db,
        0,
        "MODEL_PLACEHOLDER_M900",
        "fixture-neon-f",
        "Fixture Neon (High)",
    );
    save_generator(
        &db,
        1,
        "MODEL_PLACEHOLDER_M902",
        "fixture-carbon-max",
        "Fixture Carbon",
    );
    save_generator(&db, 2, "MODEL_PLACEHOLDER_M902", "", "Fixture Words");
    for (step, generator) in [(1, 0), (2, 0), (4, 0), (5, 1), (6, 2)] {
        save_step(&db, step, &format!("response-{step}"), 1, generator);
    }
    let report = f.sync();
    assert_eq!(
        (report.errors, report.imported),
        (0, 6),
        "{:?}",
        report.last_error
    );
    // The line's own model, less its effort suffix.
    assert_eq!(model(&f.row("session", 1)).0, Some("fixture-argon"));
    assert_eq!(model(&f.row("session", 1)).2, Some("xhigh"));
    // The selection before the step, through the seed's labels.
    assert_eq!(
        model(&f.row("session", 2)),
        (Some("gemini-3.1-pro"), Some("Gemini 3.1 Pro"), Some("high"))
    );
    // The generator's (enum, label) pair in the learned list.
    assert_eq!(
        model(&f.row("session", 4)),
        (Some("fixture-neon"), Some("Fixture Neon"), Some("high"))
    );
    // An enum nothing places: the served name without its effort suffix.
    assert_eq!(
        model(&f.row("session", 5)),
        (Some("fixture-carbon"), Some("Fixture Carbon"), Some("max"))
    );
    // A label alone names no model, and neither does a missing step.
    for step in [6, 7] {
        let row = f.row("session", step);
        assert_eq!(model(&row).0, None, "{step}");
        assert!(row.attempts[0].price.is_none(), "{step}");
        assert_eq!(row.attempts[0].tokens.input, Some(10), "{step}");
    }
}

// `-tiered` 没有独立价格时按基础模型计价；基础模型也没有价格时保持未计价。
#[test]
fn a_tiered_name_is_priced_as_its_base_model() {
    let mut f = Fixture::new();
    f.write(
        "session",
        &[
            planner(1, Some("gemini-4-argon-tiered-high"), [1000, 0, 100]),
            planner(2, Some("gemini-4-argon"), [1000, 0, 100]),
            planner(3, Some("gemini-9-unknown-tiered-high"), [1000, 0, 100]),
        ]
        .concat(),
    );
    assert_eq!(f.sync().errors, 0);
    let price = |step| f.row("session", step).attempts[0].price.clone();
    let tiered = price(1).unwrap();
    assert_eq!(tiered.model, "gemini-4-argon");
    assert_eq!(tiered.cost, price(2).unwrap().cost);
    assert!(price(3).is_none());
}

// 会话目录里的符号链接不会把读取引到根目录之外，会话库同理。
#[cfg(unix)]
#[test]
fn a_link_out_of_the_session_root_is_not_followed() {
    use std::os::unix::fs::symlink;
    let mut f = Fixture::new();
    let line = planner(1, Some("gemini-3.1-pro"), [100, 0, 10]);
    f.write("real", &line);
    let outside = f.dir.path().join("outside");
    let logs = outside.join("session/.system_generated/logs");
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::write(logs.join("transcript_full.jsonl"), &line).unwrap();
    let brain = f.home().join("antigravity-cli/brain");
    // A linked session, a linked log directory and a linked transcript.
    symlink(outside.join("session"), brain.join("linked-session")).unwrap();
    std::fs::create_dir_all(brain.join("linked-logs")).unwrap();
    symlink(
        outside.join("session/.system_generated"),
        brain.join("linked-logs/.system_generated"),
    )
    .unwrap();
    std::fs::create_dir_all(brain.join("linked-file").join(TRANSCRIPT).parent().unwrap()).unwrap();
    symlink(
        logs.join("transcript_full.jsonl"),
        brain.join("linked-file").join(TRANSCRIPT),
    )
    .unwrap();
    // A linked conversation database is not opened either.
    let conversations = f.home().join("antigravity-cli/conversations");
    std::fs::create_dir_all(&conversations).unwrap();
    let foreign = Connection::open(outside.join("real.db")).unwrap();
    foreign
        .execute_batch(
            "CREATE TABLE steps (idx INTEGER PRIMARY KEY, step_type INTEGER, metadata BLOB);",
        )
        .unwrap();
    save_step(&foreign, 1, "response-outside", 1, 0);
    drop(foreign);
    symlink(outside.join("real.db"), conversations.join("real.db")).unwrap();

    let report = f.sync();
    assert_eq!((report.errors, report.imported, report.files), (0, 1, 1));
    assert_eq!(f.requests(), 1);
    assert_eq!(f.row("real", 1).attempts[0].response_id, None);

    // Nor is a session root that is itself a link.
    let mut other = Fixture::new();
    std::fs::create_dir_all(other.home().join("antigravity-cli")).unwrap();
    symlink(&brain, other.home().join("antigravity-cli/brain")).unwrap();
    let report = other.sync();
    assert_eq!((report.errors, report.imported, report.files), (0, 0, 0));
}

// 会话库只读：不新建任何文件，不改动库、日志及其索引；尚未并入库的内容经由日志读取。
#[test]
fn the_conversation_database_is_only_read() {
    let mut f = Fixture::new();
    for session in ["closed", "open", "left"] {
        f.write(session, &planner(1, Some("gemini-3.1-pro"), [100, 0, 10]));
    }
    let closed = f.database("closed");
    closed.pragma_update(None, "journal_mode", "WAL").unwrap();
    save_step(&closed, 1, "response-closed", 1, 0);
    drop(closed);
    let open = f.database("open");
    open.pragma_update(None, "journal_mode", "WAL").unwrap();
    open.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    save_step(&open, 1, "response-open", 1, 0);
    let dir = f.home().join("antigravity-cli/conversations");
    // A writer that went away and left its step in the log.
    for kind in ["db", "db-wal", "db-shm"] {
        std::fs::copy(
            dir.join(format!("open.{kind}")),
            dir.join(format!("left.{kind}")),
        )
        .unwrap();
    }
    assert!(std::fs::metadata(dir.join("left.db-wal")).unwrap().len() > 0);
    // Every file and its bytes, but for the shared index of the database
    // still being written.
    let snapshot = || {
        let mut files: Vec<(String, Vec<u8>)> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|item| item.unwrap().file_name().into_string().unwrap())
            .map(|name| {
                let bytes = if name == "open.db-shm" {
                    Vec::new()
                } else {
                    std::fs::read(dir.join(&name)).unwrap()
                };
                (name, bytes)
            })
            .collect();
        files.sort();
        files
    };
    let before = snapshot();
    let names: Vec<&str> = before.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        [
            "closed.db",
            "left.db",
            "left.db-shm",
            "left.db-wal",
            "open.db",
            "open.db-shm",
            "open.db-wal"
        ]
    );
    assert_eq!(f.sync().errors, 0);
    for (session, response) in [
        ("closed", "response-closed"),
        ("open", "response-open"),
        ("left", "response-open"),
    ] {
        assert_eq!(
            f.row(session, 1).attempts[0].response_id,
            Some(safe_id(response)),
            "{session}"
        );
    }
    assert!(snapshot() == before, "a conversation file changed");
}

/// The row the gateway writes for a Gemini reply, metered by its own parser.
fn gateway(id: &str, response: &str, model: &str, at: i64) -> Record {
    let mut meter = Meter::default();
    meter.observe(
        &json!({"responseId": response, "usageMetadata": {
            "promptTokenCount": 1200, "cachedContentTokenCount": 200,
            "candidatesTokenCount": 50, "thoughtsTokenCount": 30,
        }}),
        0,
    );
    Record {
        id: id.into(),
        client: "antigravity".into(),
        source: "proxy".into(),
        started_at: at,
        completed: true,
        attempts: vec![Attempt {
            id: format!("{id}-attempt"),
            provider: Some("fixture-provider".into()),
            requested_model: Some(model.into()),
            response_model: Some(model.into()),
            response_id: meter.response_id,
            status: Some(200),
            outcome: "success".into(),
            started_at: at,
            transport: "http".into(),
            tokens: meter.tokens,
            ..Attempt::default()
        }],
        ..Record::default()
    }
}

// 经网关的 agy 请求同时留下网关记录与本机会话：按响应标识只计一次，以网关记录为准，会话记录归属的账号不影响配对；
// 没有响应标识时沿用既有的唯一指纹规则；会话库晚于转录写入时补上标识后同样合并。
#[test]
fn a_request_the_gateway_recorded_is_counted_once() {
    let mut f = Fixture::new();
    // The account signed in is not the gateway's provider, and still the two
    // rows of one request are paired.
    f.accounts(&[("account-a", 0)]);
    // The same counts the gateway derives from `usageMetadata`.
    let usage = [1000, 200, 80];
    f.write(
        "session",
        &[
            planner(1, Some("gemini-3.1-pro"), usage),
            planner(2, Some("gemini-3.1-pro"), usage),
        ]
        .concat(),
    );
    let db = f.database("session");
    save_step(&db, 1, "response-first", 30, 0);
    save_step(&db, 2, "response-second", 30, 0);
    // The gateway row exists before the session is read, under another model name.
    let first = gateway("gateway-first", "response-first", "fixture-upstream", at(1));
    f.store
        .write_batch(std::slice::from_ref(&first), None)
        .unwrap();
    assert_eq!(f.sync().errors, 0);
    let session = f.row("session", 1);
    assert_eq!(
        (
            session.duplicate_of.as_deref(),
            session.deduplication.as_str()
        ),
        (Some("gateway-first"), "response_id")
    );
    assert_eq!(session.attempts[0].tokens, first.attempts[0].tokens);
    assert_eq!(session.attempts[0].provider.as_deref(), Some("account-a"));
    assert_eq!(f.requests(), 2);
    // The gateway row arrives after the session was read.
    let second = gateway(
        "gateway-second",
        "response-second",
        "fixture-upstream",
        at(2),
    );
    f.store.write_batch(&[second], None).unwrap();
    assert_eq!(f.requests(), 2);
    assert_eq!(
        f.row("session", 2).duplicate_of.as_deref(),
        Some("gateway-second")
    );
    let totals = f.store.dashboard(&Filter::default()).unwrap().totals;
    assert_eq!(
        (totals.tokens.input, totals.tokens.output),
        (Some(2000), Some(160))
    );

    // Without a conversation database only an equal model, equal counts and a
    // nearby time pair the two.
    f.write("bare", &planner(3, Some("fixture-shared"), usage));
    let third = gateway("gateway-third", "response-third", "fixture-shared", at(3));
    f.store.write_batch(&[third], None).unwrap();
    assert_eq!(f.sync().errors, 0);
    assert_eq!(f.row("bare", 3).deduplication.as_str(), "strict_match");
    assert_eq!(f.requests(), 3);
    // A second candidate for that gateway row leaves both for review; neither is billed.
    f.write("twin", &planner(3, Some("fixture-shared"), usage));
    assert_eq!(f.sync().errors, 0);
    assert_eq!(f.row("twin", 3).deduplication.as_str(), "ambiguous");
    assert_eq!(f.requests(), 3);

    // A database written after its transcript supplies the identifier.
    f.write("late", &planner(4, Some("gemini-3.1-pro"), usage));
    let fourth = gateway(
        "gateway-fourth",
        "response-fourth",
        "fixture-upstream",
        at(4),
    );
    f.store.write_batch(&[fourth], None).unwrap();
    assert_eq!(f.sync().errors, 0);
    assert_eq!(f.requests(), 5);
    save_step(&f.database("late"), 4, "response-fourth", 30, 0);
    assert_eq!(f.sync().errors, 0);
    assert_eq!(
        f.row("late", 4).duplicate_of.as_deref(),
        Some("gateway-fourth")
    );
    assert_eq!(f.requests(), 4);
}

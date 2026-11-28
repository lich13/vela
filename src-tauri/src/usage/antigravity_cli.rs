//! Read-only Antigravity CLI (`agy`) session usage. A session keeps its steps in
//! `antigravity-cli/brain/<id>/.system_generated/logs/transcript_full.jsonl` and their
//! response metadata in `antigravity-cli/conversations/<id>.db`. Only counts, model
//! names, times and hashed identifiers are stored; no message text is.
use super::*;
use crate::antigravity::auth::ProviderTimeline;
use rusqlite::{Connection, OpenFlags, OptionalExtension};

#[cfg(test)]
#[path = "antigravity_cli_tests.rs"]
mod tests;

const TRANSCRIPT: &str = ".system_generated/logs/transcript_full.jsonl";
/// Marks a row as read from a CLI session file, next to the app's
/// `local_api` and `history_file`.
const TRANSPORT: &str = "cli_file";

/// The reading a saved position stands for. A position an older reading wrote
/// starts over, so the rows stored then take what this one adds.
const READING: u32 = 2;

/// How far one transcript was read. `database` and `unread` let a conversation
/// database written after its transcript complete the rows it could not serve.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Position {
    offset: u64,
    tail: String,
    identity: String,
    modified: String,
    complete: bool,
    database: String,
    unread: bool,
    reading: u32,
    /// What the lines before `offset` left.
    walk: Walk,
    /// The last usage line while the file ends before another step follows
    /// it. A file that grew is read from there, so that step can time it.
    #[serde(skip_serializing_if = "Option::is_none")]
    open: Option<Open>,
}

/// A usage line waiting for the step after it.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Open {
    /// Where the line starts.
    offset: u64,
    /// What the lines before it left.
    walk: Walk,
}

/// What the lines read so far mean for the next one.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Walk {
    model: Option<String>,
    /// Times the session went back to an earlier step. The steps run again
    /// reuse their indexes and are billed again.
    rewinds: u64,
    /// The step index of the last line that had one.
    step: Option<u64>,
    /// Each step with a usage line, by the rewind count of its first run.
    /// That run has the step's plain id, as every row stored before rewinds
    /// were told apart does; a later run adds its rewind count to it.
    first: BTreeMap<u64, u64>,
}

/// A usage line as its row, before the conversation database adds to it.
struct Draft {
    rewinds: u64,
    step: u64,
    /// Read again from a saved `Open`: its row is stored already.
    stored: bool,
    /// Milliseconds from the step's start to the tool result after it.
    estimate: Option<u64>,
    record: Record,
}

/// One pass over the lines of a transcript.
struct Pass<'a> {
    cascade: &'a str,
    timeline: &'a ProviderTimeline,
    learned: &'a Learned,
    walk: Walk,
    /// Keyed by rewind and step, so the last line written for a step is the one kept.
    drafts: BTreeMap<(u64, u64), Draft>,
    /// Each rewind met: its count and the step it went back to.
    rewound: Vec<(u64, u64)>,
    /// The last usage line, by its key, while no other step has followed it.
    open: Option<((u64, u64), Open)>,
    /// Where the saved `Open` this pass started from begins.
    resumed: Option<u64>,
}

/// Just enough of the protobuf wire format to pick fields out of a message.
mod wire {
    pub(super) enum Field<'a> {
        Number(u64),
        Bytes(&'a [u8]),
    }
    fn varint(bytes: &mut &[u8]) -> Option<u64> {
        let mut value = 0;
        for shift in (0..64).step_by(7) {
            let (byte, rest) = bytes.split_first()?;
            *bytes = rest;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    }
    /// The fields of one message, or `None` when it is malformed.
    pub(super) fn fields(mut bytes: &[u8]) -> Option<Vec<(u64, Field<'_>)>> {
        let mut found = Vec::new();
        while !bytes.is_empty() {
            let tag = varint(&mut bytes)?;
            let skip = match tag & 7 {
                0 => {
                    found.push((tag >> 3, Field::Number(varint(&mut bytes)?)));
                    0
                }
                1 => 8,
                2 => {
                    let length = usize::try_from(varint(&mut bytes)?).ok()?;
                    found.push((tag >> 3, Field::Bytes(bytes.get(..length)?)));
                    length
                }
                5 => 4,
                _ => return None,
            };
            bytes = bytes.get(skip..)?;
        }
        Some(found)
    }
    pub(super) fn number(fields: &[(u64, Field<'_>)], field: u64) -> Option<u64> {
        fields.iter().rev().find_map(|(number, value)| match value {
            Field::Number(value) if *number == field => Some(*value),
            _ => None,
        })
    }
    pub(super) fn bytes<'a>(fields: &[(u64, Field<'a>)], field: u64) -> Option<&'a [u8]> {
        fields.iter().rev().find_map(|(number, value)| match value {
            Field::Bytes(value) if *number == field => Some(*value),
            _ => None,
        })
    }
    pub(super) fn text<'a>(fields: &[(u64, Field<'a>)], field: u64) -> Option<&'a str> {
        std::str::from_utf8(bytes(fields, field)?)
            .ok()
            .filter(|text| !text.is_empty())
    }
}

/// What a step's metadata adds to its transcript line: usage message fields
/// 4, 9 and 11, the index of the generator that produced the step, and its
/// timing. Fields 1, 6 and 7 are timestamps: the request sent, its first
/// content shown and the model done writing.
struct Step {
    response_id: Option<String>,
    reasoning: Option<u64>,
    cache_write: Option<u64>,
    generator: u64,
    /// Milliseconds to the first content and to the end, when the three
    /// stamps are in order.
    timing: Option<(u64, u64)>,
}
/// A `google.protobuf.Timestamp` in milliseconds.
fn stamp(step: &[(u64, wire::Field<'_>)], field: u64) -> Option<u64> {
    let at = wire::fields(wire::bytes(step, field)?)?;
    let seconds = wire::number(&at, 1).filter(|s| *s < 100_000_000_000)?;
    let nanos = wire::number(&at, 2).unwrap_or(0).min(999_999_999);
    Some(seconds * 1000 + nanos / 1_000_000)
}
impl Step {
    fn read(metadata: &[u8]) -> Option<Self> {
        let step = wire::fields(metadata)?;
        let usage = wire::fields(wire::bytes(&step, 9).unwrap_or_default())?;
        let origin = wire::fields(wire::bytes(&step, 20).unwrap_or_default())?;
        let count = |field| wire::number(&usage, field).filter(|n| *n <= 1_000_000_000_000);
        Some(Self {
            response_id: wire::text(&usage, 11).map(str::to_owned),
            reasoning: count(9),
            cache_write: count(4),
            generator: wire::number(&origin, 3).unwrap_or(0),
            timing: stamp(&step, 1)
                .zip(stamp(&step, 6))
                .zip(stamp(&step, 7))
                .filter(|((sent, first), done)| sent <= first && first <= done && done > sent)
                .map(|((sent, first), done)| (first - sent, done - sent)),
        })
    }
}

/// The model a generator ran: its enum name, the name it was served under and
/// the label shown for it.
struct Generator {
    key: Option<String>,
    served: Option<String>,
    shown: Option<String>,
}
impl Generator {
    fn read(data: &[u8]) -> Option<Self> {
        let chat = wire::fields(wire::bytes(&wire::fields(data)?, 1)?)?;
        // Field 20 is a string map; the enum name sits under `model_enum`.
        let key = chat.iter().find_map(|(number, value)| match value {
            wire::Field::Bytes(entry) if *number == 20 => {
                let entry = wire::fields(entry)?;
                (wire::text(&entry, 1) == Some("model_enum"))
                    .then(|| wire::text(&entry, 2).map(str::to_owned))?
            }
            _ => None,
        });
        Some(Self {
            key,
            served: wire::text(&chat, 19).map(str::to_owned),
            shown: wire::text(&chat, 21).map(str::to_owned),
        })
    }
}

/// A session's conversation database, opened when a row first asks for it.
struct Conversation<'a> {
    path: Option<&'a Path>,
    opened: Option<Option<Connection>>,
    /// A step the database did not serve, which a later write may complete.
    unread: bool,
}
impl Conversation<'_> {
    fn blob(&mut self, sql: &str, index: u64) -> Option<Vec<u8>> {
        let path = self.path?;
        let db = self.opened.get_or_insert_with(|| open(path)).as_ref()?;
        db.query_row(sql, (i64::try_from(index).ok()?, LIMIT as i64), |row| {
            row.get::<_, Option<Vec<u8>>>(0)
        })
        .optional()
        .ok()??
    }
    fn step(&mut self, index: u64) -> Option<Step> {
        let step = self
            .blob(
                "SELECT metadata FROM steps WHERE idx=?1 AND step_type=15 AND length(metadata)<=?2",
                index,
            )
            .and_then(|metadata| Step::read(&metadata));
        self.unread |= step.is_none();
        step
    }
    fn generator(&mut self, index: u64) -> Option<Generator> {
        self.blob(
            "SELECT data FROM gen_metadata WHERE idx=?1 AND length(data)<=?2",
            index,
        )
        .and_then(|data| Generator::read(&data))
    }
}

/// Opens a conversation database without writing to it or beside it. agy keeps
/// these in WAL mode: a database with its log is read through that log, with
/// the log's shared index opened read-only too, and one without is complete, so
/// it is opened immutable and SQLite creates no side file.
fn open(path: &Path) -> Option<Connection> {
    let live = ["db-wal", "db-shm"]
        .iter()
        .all(|kind| plain_file(&path.with_extension(kind)));
    let mut uri = url::Url::from_file_path(path).ok()?;
    uri.set_query(Some(if live {
        "mode=ro&readonly_shm=1"
    } else {
        "mode=ro&immutable=1"
    }));
    let db = Connection::open_with_flags(
        uri.as_str(),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .ok()?;
    db.busy_timeout(std::time::Duration::ZERO).ok()?;
    Some(db)
}

/// True when `path` is reached without a symbolic link. `path` starts at an
/// already resolved directory, so any link below it changes the result.
fn direct(path: &Path) -> bool {
    path.canonicalize().is_ok_and(|real| real == path)
}
fn plain_file(path: &Path) -> bool {
    direct(path) && path.is_file()
}

/// The session transcripts under `brain`, by session id.
fn transcripts(brain: &Path) -> Result<Vec<(String, PathBuf)>> {
    if !direct(brain) {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    for item in std::fs::read_dir(brain).map_err(storage::io_error)? {
        let item = item.map_err(storage::io_error)?;
        let path = item.path().join(TRANSCRIPT);
        if let Some(id) = item.file_name().to_str().filter(|_| plain_file(&path)) {
            found.push((id.to_owned(), path));
        }
        if found.len() > 30_000 {
            return Err(failure("Antigravity CLI 会话目录超过读取限制"));
        }
    }
    found.sort();
    Ok(found)
}

/// Gives a row without a model the one its generator ran, resolved as the
/// app's own reading resolves it: the listed (enum, label) pair, the seed, the
/// enum alone, then the served name without its effort suffix.
fn place(attempt: &mut Attempt, generator: &Generator, learned: &Learned) {
    let shown = label(generator.shown.as_deref());
    let placed = generator
        .key
        .as_deref()
        .and_then(|key| learned.resolve(key, shown.as_deref()));
    let (backend, effort) = match non_enum_model(generator.served.as_deref()) {
        Some(served) if placed.is_none() => {
            let (base, effort) = split_effort_suffix(&served);
            (Some(base.to_owned()), effort)
        }
        _ => (None, None),
    };
    let Some(model) = placed.or(backend) else {
        return;
    };
    attempt.requested_model = Some(model.clone());
    attempt.response_model = Some(model);
    if let Some(shown) = shown {
        attempt.reasoning_effort =
            strip_display_effort_suffix(&shown).and_then(|(_, effort)| effort);
        attempt.model_display_name = Some(shown);
    }
    if attempt.reasoning_effort.is_none() {
        attempt.reasoning_effort = effort.map(str::to_owned);
    }
    normalize_attempt(attempt);
}

impl Pass<'_> {
    /// Takes the line starting at `offset`: its part in the walk, and its
    /// usage row from what the line itself says.
    fn line(&mut self, line: &[u8], offset: u64) -> Result<()> {
        let v: Value = serde_json::from_slice(line)
            .map_err(|_| failure("Antigravity CLI 会话文件存在无效完整行"))?;
        let index = number(&v["step_index"]);
        let before = (self.walk.rewinds, self.walk.step);
        if let Some(index) = index {
            if self.walk.step.is_some_and(|last| index < last) {
                self.walk.rewinds += 1;
                self.rewound.push((self.walk.rewinds, index));
            }
            self.walk.step = Some(index);
            let key = (self.walk.rewinds, index);
            // A tool's result is created about when the model's turn ends, so
            // the first line of another step times the turn when it is one.
            // The user's next message or a system note can come any time later.
            if let Some((waiting, _)) = self.open.take_if(|(waiting, _)| *waiting != key) {
                if let Some(draft) = self
                    .drafts
                    .get_mut(&waiting)
                    .filter(|_| v["type"].as_str() == Some("GENERIC"))
                {
                    draft.estimate = time(&v["created_at"])
                        .and_then(|next| u64::try_from(next - draft.record.started_at).ok())
                        .filter(|ms| *ms > 0);
                }
            }
        }
        if v["type"].as_str() == Some("USER_INPUT") {
            if let Some(content) = v["content"].as_str() {
                if let Some(model) = extract_settings_model(content) {
                    self.walk.model = Some(model);
                } else if content.contains("<USER_SETTINGS_CHANGE>") {
                    self.walk.model = None;
                }
            }
            return Ok(());
        }
        let reported = ["input_tokens", "output_tokens", "cache_read_tokens"]
            .iter()
            .any(|key| number(&v[*key]).is_some());
        // agy may write a step again when it finishes; only that line carries the
        // step's final counts.
        if v["type"].as_str() != Some("PLANNER_RESPONSE")
            || !reported
            || matches!(
                v["status"].as_str(),
                Some("RUNNING" | "IN_PROGRESS" | "PENDING")
            )
        {
            return Ok(());
        }
        let (Some(step), Some(started)) = (index, time(&v["created_at"])) else {
            return Err(failure("Antigravity CLI 用量缺少步骤序号或时间，未计入"));
        };
        let key = (self.walk.rewinds, step);
        let waiting = Open {
            offset,
            walk: Walk {
                model: self.walk.model.clone(),
                rewinds: before.0,
                step: before.1,
                first: self.walk.first.clone(),
            },
        };
        self.open = Some((key, waiting));
        self.walk.first.entry(step).or_insert(key.0);
        let draft = Draft {
            rewinds: key.0,
            step,
            stored: self.resumed == Some(offset),
            estimate: None,
            record: self.record(&v, step, started),
        };
        self.drafts.insert(key, draft);
        Ok(())
    }

    /// The row of a finished usage line.
    fn record(&self, v: &Value, step: u64, started: i64) -> Record {
        let (cascade, walk, learned) = (self.cascade, &self.walk, self.learned);
        let explicit = text(v, &["model"]);
        let listed = explicit.and_then(|id| learned.listed(id));
        let alias = resolve_model_alias(explicit);
        // As in the app's history file: a listed id resolves through its entry and
        // any other backend name only drops its effort suffix.
        let (named, suffix_effort) = match (&listed, explicit) {
            (Some((base, _)), _) => (Some(base.as_str()), None),
            (None, Some(name)) if alias.is_none() => {
                let (base, effort) = split_effort_suffix(name);
                (Some(base), effort)
            }
            (None, name) => (name, None),
        };
        let model = actual_model(named);
        // agy writes input without cached tokens and output with thinking tokens,
        // the split the app's local API reports (its usage message holds
        // output = thinking + response), so the counts are stored as written.
        let tokens = Tokens {
            input: number(&v["input_tokens"]),
            output: number(&v["output_tokens"]),
            cache_read: number(&v["cache_read_tokens"]),
            ..Tokens::default()
        };
        let id = safe_id(&match walk.rewinds {
            rewinds if walk.first.get(&step) == Some(&rewinds) => {
                format!("antigravity:cli:{cascade}:step:{step}")
            }
            rewinds => format!("antigravity:cli:{cascade}:step:{step}:rewind:{rewinds}"),
        });
        let outcome = line_outcome(v, &tokens);
        let attempt = Attempt {
            id: safe_id(&format!("{id}:final")),
            // agy runs on the login it shares with the app.
            provider: self.timeline.provider_at(started).map(str::to_owned),
            requested_model: model.clone(),
            response_model: model,
            model_display_name: listed
                .as_ref()
                .map(|(_, shown)| (*shown).to_owned())
                .or_else(|| alias.map(|a| a.display_name.to_owned()))
                .or_else(|| walk.model.clone()),
            reasoning_effort: alias
                .and_then(|a| a.effort)
                .or(suffix_effort)
                .map(str::to_owned),
            error_code: (outcome == "failed").then(|| error_code(v)).flatten(),
            started_at: started,
            outcome: outcome.into(),
            transport: TRANSPORT.into(),
            raw_input_tokens: tokens.input,
            input_basis: Some("exclusive".into()),
            tokens,
            usage_status: "reported".into(),
            parse_reasons: vec!["attempt_time_unreported".into()],
            ..Attempt::default()
        };
        Record {
            id,
            client: "antigravity".into(),
            source: "antigravity".into(),
            session_id: Some(safe_id(cascade)),
            started_at: started,
            finished_at: Some(started),
            // As with the app's history file, a usage line marked neither done
            // nor failed reports consumption and no outcome.
            completed: outcome == "success",
            attempts: vec![attempt],
            ..Record::default()
        }
    }
}

/// Finishes a row with its step's metadata and prices it. `replaced` marks a
/// step a rewind ran again: the database then holds the later run under the
/// same index, and its response id on both rows would merge them into one.
fn complete(
    draft: Draft,
    replaced: bool,
    conversation: &mut Conversation<'_>,
    pricing: &Pricing,
    settings: &Settings,
    learned: &Learned,
) -> Record {
    let mut record = draft.record;
    let recorded = if replaced {
        None
    } else {
        conversation.step(draft.step)
    };
    let attempt = &mut record.attempts[0];
    // Only a step the database does not time is timed by its follower.
    if !recorded.as_ref().is_some_and(|step| step.timing.is_some()) {
        attempt.estimated_duration_ms = draft.estimate;
    }
    if let Some(step) = &recorded {
        attempt.tokens.cache_write = step.cache_write;
        attempt.tokens.reasoning = step.reasoning;
        attempt.response_id = step.response_id.as_deref().map(safe_id);
        // The conversation database times the step; the transcript does not.
        if let Some((first, total)) = step.timing {
            attempt.stream = true;
            attempt.first_token_ms = Some(first);
            attempt.generation_ms = Some(total - first);
            attempt.duration_ms = total;
            attempt
                .availability
                .insert("duration".into(), "reported".into());
            attempt.parse_reasons.clear();
        }
    }
    normalize_attempt(attempt);
    if attempt.response_model.is_none() {
        if let Some(generator) = recorded.and_then(|step| conversation.generator(step.generator)) {
            place(attempt, &generator, learned);
        }
    }
    price(&mut record, pricing, settings);
    record
}

#[allow(clippy::too_many_arguments)]
fn scan_session(
    store: &mut impl Repository,
    cascade: &str,
    path: &Path,
    database: Option<&Path>,
    pricing: &Pricing,
    settings: &Settings,
    report: &mut Report,
    repair: bool,
    timeline: &ProviderTimeline,
    learned: &Learned,
) -> Result<()> {
    let key = safe_id(&format!("antigravity:cli:file:{}", path.to_string_lossy()));
    let saved = if repair { None } else { store.cursor(&key)? };
    let mut at: Position = saved
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    let stamp = database.map_or_else(String::new, |db| {
        let log = local_file_stamp(&db.with_extension("db-wal"));
        format!("{}+{log}", local_file_stamp(db))
    });
    if at.reading != READING || (at.unread && at.database != stamp) {
        at = Position::default();
    }
    let mut file = File::open(path).map_err(storage::io_error)?;
    let meta = file.metadata().map_err(storage::io_error)?;
    let identity = file_identity(&meta);
    let modified = file_modified(&meta).unwrap_or_default();
    if at.complete
        && !modified.is_empty()
        && at.identity == identity
        && at.offset == meta.len()
        && at.modified == modified
        && file_settled(&meta)
    {
        return Ok(());
    }
    let tail = |file: &mut File, offset: u64| -> Result<String> {
        use std::io::Read;
        file.seek(SeekFrom::Start(offset.saturating_sub(4096)))
            .map_err(storage::io_error)?;
        let mut b = vec![0; offset.min(4096) as usize];
        file.read_exact(&mut b).map_err(storage::io_error)?;
        Ok(storage::digest(&b))
    };
    if at.identity != identity || meta.len() < at.offset || tail(&mut file, at.offset)? != at.tail {
        at = Position::default();
    }
    at.reading = READING;
    at.identity = identity;
    // A file that grew is read from the usage line still waiting for the
    // step after it, with the walk as it stood before that line.
    let grown = meta.len() > at.offset;
    let waiting = at.open.clone().filter(|_| grown);
    let resumed = waiting.as_ref().map(|open| open.offset);
    let (mut next, walk) = match waiting {
        Some(open) => (open.offset, open.walk),
        None => (at.offset, at.walk.clone()),
    };
    file.seek(SeekFrom::Start(next))
        .map_err(storage::io_error)?;
    let mut reader = BufReader::new(file);
    let mut pass = Pass {
        cascade,
        timeline,
        learned,
        walk,
        drafts: BTreeMap::new(),
        rewound: Vec::new(),
        open: None,
        resumed,
    };
    let mut bytes = Vec::new();
    let mut gap = false;
    loop {
        bytes.clear();
        use std::io::Read;
        let size = (&mut reader)
            .take(LIMIT as u64 + 1)
            .read_until(b'\n', &mut bytes)
            .map_err(storage::io_error)?;
        if size > LIMIT {
            return Err(failure("Antigravity CLI 会话单行超过读取限制"));
        }
        // A line still being written is read again once it is complete.
        if bytes.last() != Some(&b'\n') {
            break;
        }
        let start = next;
        next += size as u64;
        if let Err(error) = pass.line(&bytes, start) {
            gap = true;
            report.failed(&error);
        }
        // Past a line that could not be counted the position stays put, and
        // with it what the lines before that one left.
        if !gap {
            at.offset = next;
            at.walk.clone_from(&pass.walk);
        }
    }
    if grown {
        at.open = pass
            .open
            .map(|(_, open)| open)
            .filter(|open| open.offset < at.offset);
    }
    at.tail = tail(reader.get_mut(), at.offset)?;
    at.complete = at.offset == meta.len();
    at.modified = if at.complete { modified } else { String::new() };
    let mut conversation = Conversation {
        path: database,
        opened: None,
        unread: false,
    };
    let rewound = pass.rewound;
    // Only now is every rewind of this pass known.
    let rows: Vec<Record> = pass
        .drafts
        .into_values()
        // The stored row of the line read again only gains an estimate.
        .filter(|draft| !draft.stored || draft.estimate.is_some())
        .map(|draft| {
            let replaced = rewound
                .iter()
                .any(|(count, to)| *count > draft.rewinds && *to <= draft.step);
            complete(
                draft,
                replaced,
                &mut conversation,
                pricing,
                settings,
                learned,
            )
        })
        .collect();
    at.unread |= conversation.unread;
    at.database = stamp;
    for batch in rows.chunks(128) {
        store.write_batch(batch, None)?;
    }
    report.imported += rows.len() as u64;
    let cursor = serde_json::to_string(&at).map_err(|_| failure("Antigravity CLI 同步位置无效"))?;
    if saved.as_deref() != Some(cursor.as_str()) {
        store.write_batch(&[], Some((&key, &cursor)))?;
    }
    Ok(())
}

/// Imports every CLI session under `home`. A session that cannot be read is
/// reported and does not stop the others.
pub(super) fn scan(
    store: &mut impl Repository,
    home: &Path,
    pricing: &Pricing,
    settings: &Settings,
    report: &mut Report,
    repair: bool,
) {
    // Resolved once: below it, nothing reached through a link is read.
    let Ok(root) = home.canonicalize() else {
        return;
    };
    let root = root.join("antigravity-cli");
    let sessions = match transcripts(&root.join("brain")) {
        Ok(sessions) => sessions,
        Err(error) => {
            report.failed(&error);
            return;
        }
    };
    let conversations = root.join("conversations");
    let timeline = store
        .data_dir()
        .map(|data| crate::antigravity::auth::provider_timeline(data, Some(home)))
        .unwrap_or_default();
    let learned = match Learned::load(store) {
        Ok(learned) => learned,
        Err(error) => {
            report.failed(&error);
            return;
        }
    };
    report.total_files += sessions.len() as u64;
    for (cascade, path) in &sessions {
        let database =
            Some(conversations.join(format!("{cascade}.db"))).filter(|db| plain_file(db));
        match scan_session(
            store,
            cascade,
            path,
            database.as_deref(),
            pricing,
            settings,
            report,
            repair,
            &timeline,
            &learned,
        ) {
            Ok(()) => report.files += 1,
            Err(error) => report.failed(&error),
        }
        store.progress(report);
    }
}

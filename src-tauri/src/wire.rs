//! What the desktop app and a Vela server say to each other: JSON messages,
//! each behind its length as four big-endian bytes. The same frames cross the
//! server's Unix socket and the SSH channel in front of it.
use crate::storage::AppError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The largest message either side accepts.
pub(crate) const FRAME_LIMIT: usize = 32 * 1024 * 1024;
/// Answered first on every connection, with [`Capabilities`].
pub(crate) const CAPABILITIES: &str = "system.capabilities";
/// Raised whenever a message changes shape in a way the other side must know.
pub(crate) const PROTOCOL: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Request {
    pub id: u64,
    pub command: String,
    #[serde(default)]
    pub args: Value,
    /// Set where the Rust of another Vela asks on its own account. A window's
    /// request never carries it, and the commands that hand credentials from
    /// one Vela to another are answered to nothing else. A Vela from before
    /// there was one neither sends it nor minds it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub internal: bool,
}

/// A message from the server: the answer to the request `id` names, or an
/// event nobody asked for.
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum Reply {
    Answer {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<AppError>,
    },
    Event {
        event: String,
        payload: Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Capabilities {
    pub protocol: u32,
    pub version: String,
    /// Every command the server answers.
    pub commands: Vec<String>,
}

/// The next message, or `None` once the other side has closed between two of
/// them.
pub(crate) async fn read<T: for<'de> Deserialize<'de>>(
    from: &mut (impl AsyncRead + Unpin),
) -> std::io::Result<Option<T>> {
    let mut length = [0; 4];
    match from.read_exact(&mut length).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let length = u32::from_be_bytes(length) as usize;
    if length > FRAME_LIMIT {
        return Err(invalid("message too large"));
    }
    let mut body = vec![0; length];
    from.read_exact(&mut body).await?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|_| invalid("message is not what was expected"))
}

pub(crate) async fn write(
    to: &mut (impl AsyncWrite + Unpin),
    message: &impl Serialize,
) -> std::io::Result<()> {
    let body = serde_json::to_vec(message).map_err(|_| invalid("message cannot be written"))?;
    if body.len() > FRAME_LIMIT {
        return Err(invalid("message too large"));
    }
    to.write_all(&(body.len() as u32).to_be_bytes()).await?;
    to.write_all(&body).await?;
    to.flush().await
}

fn invalid(reason: &'static str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn messages_cross_a_pipe_in_order_and_the_end_reads_as_none() {
        let (mut near, mut far) = tokio::io::duplex(64);
        let sent = tokio::spawn(async move {
            let ask = Request {
                id: 7,
                command: "get_gateway".into(),
                args: json!({"clientId": "codex"}),
                internal: false,
            };
            write(&mut near, &ask).await.unwrap();
            let event = Reply::Event {
                event: "gateway-state".into(),
                payload: json!({"running": true}),
            };
            write(&mut near, &event).await.unwrap();
        });
        let ask: Request = read(&mut far).await.unwrap().unwrap();
        assert_eq!((ask.id, ask.command.as_str()), (7, "get_gateway"));
        assert_eq!(ask.args["clientId"], "codex");
        let Some(Reply::Event { event, payload }) = read(&mut far).await.unwrap() else {
            panic!("expected an event");
        };
        assert_eq!(
            (event.as_str(), &payload["running"]),
            ("gateway-state", &json!(true))
        );
        sent.await.unwrap();
        assert!(read::<Reply>(&mut far).await.unwrap().is_none());
    }

    #[test]
    fn a_request_says_it_is_internal_only_where_it_is() {
        let request = |internal| Request {
            id: 1,
            command: "get_state".into(),
            args: json!({}),
            internal,
        };
        // A window's request is the message it always was.
        assert_eq!(
            serde_json::to_value(request(false)).unwrap(),
            json!({"id": 1, "command": "get_state", "args": {}})
        );
        assert_eq!(
            serde_json::to_value(request(true)).unwrap(),
            json!({"id": 1, "command": "get_state", "args": {}, "internal": true})
        );
        // One from a Vela that never heard of the word is a window's.
        let old: Request = serde_json::from_str(r#"{"id":1,"command":"get_state"}"#).unwrap();
        assert!(!old.internal);
        let marked: Request =
            serde_json::from_str(r#"{"id":1,"command":"get_state","internal":true}"#).unwrap();
        assert!(marked.internal);
    }

    #[tokio::test]
    async fn an_answer_carries_a_result_or_an_error_and_null_is_a_result() {
        for (text, expected) in [
            (r#"{"id":1,"result":null}"#, (true, false)),
            (r#"{"id":1,"result":{"a":1}}"#, (true, false)),
            (
                r#"{"id":1,"error":{"code":"UNSUPPORTED","message":"fixture"}}"#,
                (false, true),
            ),
        ] {
            let Reply::Answer { id, result, error } = serde_json::from_str(text).unwrap() else {
                panic!("expected an answer");
            };
            assert_eq!(id, 1);
            // `null` deserializes to no value at all, which is what a command
            // without a result answers.
            assert_eq!(
                (error.is_none(), error.is_some()),
                expected,
                "{text} {result:?}"
            );
        }
    }

    #[tokio::test]
    async fn an_oversized_or_cut_message_is_an_error() {
        let (mut near, mut far) = tokio::io::duplex(64);
        near.write_all(&(FRAME_LIMIT as u32 + 1).to_be_bytes())
            .await
            .unwrap();
        assert!(read::<Request>(&mut far).await.is_err());

        let (mut near, mut far) = tokio::io::duplex(64);
        near.write_all(&8u32.to_be_bytes()).await.unwrap();
        near.write_all(b"{\"id\"").await.unwrap();
        drop(near);
        assert!(read::<Request>(&mut far).await.is_err());
    }
}

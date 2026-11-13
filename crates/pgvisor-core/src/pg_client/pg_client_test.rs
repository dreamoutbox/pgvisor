use bytes::BytesMut;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::error::PgClientError;
use super::frame::extract_backend_frame;
use super::simple_query::{execute_simple_query_on_stream, SimpleQueryAccumulator};
use crate::protocol::message::{BackendMessage, FrontendMessage, TransactionStatus};

#[test]
fn test_extract_backend_frame_partial_and_complete() {
    let mut buf = BytesMut::new();

    buf.extend_from_slice(&[b'C', 0, 0]);
    assert_eq!(extract_backend_frame(&mut buf), None);
    assert_eq!(buf.len(), 3);

    buf.extend_from_slice(&[0, 9, b'D', b'R']);
    assert_eq!(extract_backend_frame(&mut buf), None);

    buf.extend_from_slice(&[b'O', b'P', 0]);
    let (tag, frame) = extract_backend_frame(&mut buf).unwrap();
    assert_eq!(tag, b'C');
    assert_eq!(frame.len(), 10);
    assert!(buf.is_empty());
}

#[test]
fn test_accumulator_select_with_nulls_and_truncation() {
    let mut buf = BytesMut::new();
    BackendMessage::RowDescription {
        columns: vec!["id".into(), "name".into()],
    }
    .encode(&mut buf);
    BackendMessage::DataRow {
        values: vec![Some("1".into()), Some("alice".into())],
    }
    .encode(&mut buf);
    BackendMessage::DataRow {
        values: vec![Some("2".into()), None],
    }
    .encode(&mut buf);
    BackendMessage::DataRow {
        values: vec![Some("3".into()), Some("bob".into())],
    }
    .encode(&mut buf);
    BackendMessage::CommandComplete {
        tag: "SELECT 3".into(),
    }
    .encode(&mut buf);
    BackendMessage::ReadyForQuery {
        status: TransactionStatus::Idle,
    }
    .encode(&mut buf);

    let mut acc = SimpleQueryAccumulator::new(2);
    assert!(acc.feed_buf(&mut buf));
    assert!(buf.is_empty());

    let res = acc.finish(5).unwrap();
    assert_eq!(res.columns, vec!["id", "name"]);
    assert_eq!(
        res.rows,
        vec![vec!["1", "alice"], vec!["2", "NULL"]]
    );
    assert_eq!(res.row_count, 2);
    assert!(res.truncated);
    assert_eq!(res.execution_time_ms, 5);
}

#[test]
fn test_accumulator_ddl_without_rows_synthesizes_status() {
    let mut buf = BytesMut::new();
    BackendMessage::CommandComplete {
        tag: "CREATE TABLE".into(),
    }
    .encode(&mut buf);
    BackendMessage::ReadyForQuery {
        status: TransactionStatus::Idle,
    }
    .encode(&mut buf);

    let mut acc = SimpleQueryAccumulator::new(10);
    assert!(acc.feed_buf(&mut buf));

    let res = acc.finish(1).unwrap();
    assert_eq!(res.columns, vec!["status"]);
    assert_eq!(res.rows, vec![vec!["CREATE TABLE"]]);
    assert_eq!(res.row_count, 1);
    assert!(!res.truncated);
}

#[test]
fn test_accumulator_error_response() {
    let mut buf = BytesMut::new();
    BackendMessage::ErrorResponse {
        message: "relation \"missing\" does not exist".into(),
    }
    .encode(&mut buf);
    BackendMessage::ReadyForQuery {
        status: TransactionStatus::Idle,
    }
    .encode(&mut buf);

    let mut acc = SimpleQueryAccumulator::new(10);
    assert!(acc.feed_buf(&mut buf));

    match acc.finish(1) {
        Err(PgClientError::SqlError(msg)) => {
            assert!(msg.contains("missing"));
        }
        other => panic!("Expected SqlError, got {:?}", other),
    }
}

#[tokio::test]
async fn test_execute_simple_query_on_stream_chunked() {
    let (mut client, mut server) = tokio::io::duplex(4096);

    let server_task = tokio::spawn(async move {
        let mut req_buf = BytesMut::with_capacity(256);
        let n = server.read_buf(&mut req_buf).await.unwrap();
        assert!(n > 0);
        match FrontendMessage::decode(&mut req_buf).unwrap().unwrap() {
            FrontendMessage::Query(q) => assert_eq!(q, "SELECT 42 AS num"),
            other => panic!("Expected Query, got {:?}", other),
        }

        let mut resp = BytesMut::new();
        BackendMessage::RowDescription {
            columns: vec!["num".into()],
        }
        .encode(&mut resp);
        BackendMessage::DataRow {
            values: vec![Some("42".into())],
        }
        .encode(&mut resp);
        BackendMessage::CommandComplete {
            tag: "SELECT 1".into(),
        }
        .encode(&mut resp);
        BackendMessage::ReadyForQuery {
            status: TransactionStatus::Idle,
        }
        .encode(&mut resp);

        // Send in two chunks to verify partial-frame reassembly
        let split_at = resp.len() / 2;
        server.write_all(&resp[..split_at]).await.unwrap();
        server.flush().await.unwrap();
        server.write_all(&resp[split_at..]).await.unwrap();
        server.flush().await.unwrap();
    });

    let res = execute_simple_query_on_stream(&mut client, "SELECT 42 AS num", 100)
        .await
        .unwrap();
    assert_eq!(res.columns, vec!["num"]);
    assert_eq!(res.rows, vec![vec!["42"]]);
    assert!(!res.truncated);
    server_task.await.unwrap();
}

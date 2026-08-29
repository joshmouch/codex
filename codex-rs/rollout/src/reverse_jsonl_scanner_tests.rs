use std::io::Cursor;
use std::io::Read;
use std::io::Seek;

use pretty_assertions::assert_eq;
use serde::Deserialize;
use serde::Serialize;

use super::ReverseJsonlScanner;
use super::ScanOutcome;

#[derive(Debug, Deserialize, Serialize, PartialEq)]
struct TestRecord {
    value: String,
}

fn record(value: &str) -> TestRecord {
    TestRecord {
        value: value.to_string(),
    }
}

fn parsed<T>(outcome: Option<ScanOutcome<T>>) -> T {
    let Some(ScanOutcome::Parsed(record)) = outcome else {
        panic!("expected parsed record");
    };
    record
}

fn assert_records<R>(scanner: &mut ReverseJsonlScanner<R>, expected: &[&str]) -> std::io::Result<()>
where
    R: Read + Seek,
{
    for value in expected {
        assert_eq!(parsed(scanner.scan_next::<TestRecord>()?), record(value));
    }
    assert!(scanner.scan_next::<TestRecord>()?.is_none());
    Ok(())
}

#[test]
fn scans_jsonl_records_from_end() -> std::io::Result<()> {
    let input = br#"{"value":"first"}
{"value":"second"}
{"value":"third"}
"#;

    assert_records(
        &mut ReverseJsonlScanner::new(Cursor::new(input))?,
        &["third", "second", "first"],
    )
}

#[test]
fn rejects_invalid_json_and_continues_scanning() -> std::io::Result<()> {
    let input = br#"{"value":"first"}
not-json
{"value":"third"}
"#;
    let mut scanner = ReverseJsonlScanner::new(Cursor::new(input))?;

    assert_eq!(parsed(scanner.scan_next::<TestRecord>()?), record("third"));
    let Some(ScanOutcome::Rejected(error)) = scanner.scan_next::<TestRecord>()? else {
        panic!("expected rejected record");
    };
    assert!(error.is_syntax());
    assert_eq!(parsed(scanner.scan_next::<TestRecord>()?), record("first"));
    Ok(())
}

#[test]
fn skips_records_over_the_configured_limit() -> std::io::Result<()> {
    let oversized = record(&"x".repeat(128));
    let input = format!(
        "{}\n{}\n{}\n",
        serde_json::to_string(&record("first"))?,
        serde_json::to_string(&oversized)?,
        serde_json::to_string(&record("third"))?
    );
    let mut scanner = ReverseJsonlScanner::new(Cursor::new(input.into_bytes()))?
        .with_max_record_bytes(/*max_record_bytes*/ 32);

    assert_records(&mut scanner, &["third", "first"])
}

#[test]
fn accepts_valid_json_at_eof() -> std::io::Result<()> {
    let input = b"{\"value\":\"first\"}\n{\"value\":\"second\"}";

    assert_records(
        &mut ReverseJsonlScanner::new(Cursor::new(input))?,
        &["second", "first"],
    )
}

#[test]
fn scans_from_a_frozen_prefix_end() -> std::io::Result<()> {
    let prefix = b"{\"value\":\"first\"}\n{\"value\":\"second\"}\n";
    let mut input = prefix.to_vec();
    input.extend_from_slice(b"{\"value\":\"later\"}\n");

    assert_records(
        &mut ReverseJsonlScanner::new_at(Cursor::new(input), prefix.len() as u64)?,
        &["second", "first"],
    )
}

#[test]
fn rejects_invalid_json_at_eof_and_continues_scanning() -> std::io::Result<()> {
    let input = b"{\"value\":\"first\"}\n{\"value\":";
    let mut scanner = ReverseJsonlScanner::new(Cursor::new(input))?;

    let Some(ScanOutcome::Rejected(error)) = scanner.scan_next::<TestRecord>()? else {
        panic!("expected rejected record");
    };
    assert!(error.is_eof());
    assert_eq!(parsed(scanner.scan_next::<TestRecord>()?), record("first"));
    Ok(())
}

#[test]
fn skips_blank_lines_with_or_without_termination() -> std::io::Result<()> {
    let input = b"{\"value\":\"first\"}\r\n\n \t\r";

    assert_records(
        &mut ReverseJsonlScanner::new(Cursor::new(input))?,
        &["first"],
    )
}

#[test]
fn scans_across_read_chunk_boundaries() -> std::io::Result<()> {
    let empty_record_len = serde_json::to_string(&record(""))?.len();
    for distance_from_eof in [
        super::READ_CHUNK_SIZE - 1,
        super::READ_CHUNK_SIZE,
        super::READ_CHUNK_SIZE + 1,
    ] {
        let large_value = "x".repeat(distance_from_eof - empty_record_len - 2);
        let input = format!(
            "{}\n{}\n",
            serde_json::to_string(&record("first"))?,
            serde_json::to_string(&record(&large_value))?
        );
        let mut scanner = ReverseJsonlScanner::new(Cursor::new(input.into_bytes()))?;

        assert_eq!(
            parsed(scanner.scan_next::<TestRecord>()?),
            record(&large_value)
        );
        assert_eq!(parsed(scanner.scan_next::<TestRecord>()?), record("first"));
    }
    Ok(())
}

#[test]
fn scans_record_spanning_three_read_chunks() -> std::io::Result<()> {
    let large_value = "x".repeat(super::READ_CHUNK_SIZE * 2);
    let input = format!(
        "{}\n{}\n{}\n",
        serde_json::to_string(&record("first"))?,
        serde_json::to_string(&record(&large_value))?,
        serde_json::to_string(&record("third"))?
    );
    let mut scanner = ReverseJsonlScanner::new(Cursor::new(input.into_bytes()))?;

    assert_records(&mut scanner, &["third", &large_value, "first"])
}

#[test]
fn scans_rollout_lines_through_canonical_decoder() -> std::io::Result<()> {
    let input = br#"{"timestamp":"2025-01-01T00:00:00Z","ordinal":7,"type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":{"limit_id":null,"limit_name":null,"primary":{"used_percent":12.5,"window_minutes":60,"resets_at":1800000000},"secondary":null,"credits":null,"individual_limit":null,"spend_control_reached":null,"plan_type":null,"rate_limit_reached_type":null}}}
"#;
    let mut scanner = ReverseJsonlScanner::new(Cursor::new(input.as_slice()))?;

    let line = parsed(scanner.scan_next_rollout_line()?);
    let value = serde_json::to_value(line)?;
    assert_eq!(value["ordinal"], serde_json::json!(7));
    assert_eq!(
        value["payload"]["rate_limits"]["primary"]["used_percent"],
        serde_json::json!(12.5)
    );
    assert!(scanner.scan_next_rollout_line()?.is_none());
    Ok(())
}

#[test]
fn canonical_rollout_scan_preserves_unknown_record_recovery() -> std::io::Result<()> {
    let input = b"{\"timestamp\":\"2025-01-01T00:00:00Z\",\"ordinal\":1,\"type\":\"event_msg\",\"payload\":{\"type\":\"warning\",\"message\":\"older\"}}\n{\"timestamp\":\"2025-01-01T00:00:01Z\",\"ordinal\":2,\"type\":\"future_item\",\"payload\":{}}\n";
    let mut scanner = ReverseJsonlScanner::new(Cursor::new(input.as_slice()))?;

    assert!(matches!(
        scanner.scan_next_rollout_line()?,
        Some(ScanOutcome::Rejected(_))
    ));
    let older = parsed(scanner.scan_next_rollout_line()?);
    assert_eq!(older.ordinal, Some(1));
    assert!(scanner.scan_next_rollout_line()?.is_none());
    Ok(())
}

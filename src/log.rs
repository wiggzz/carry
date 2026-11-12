use std::{
    fs::{File, OpenOptions},
    io::{BufWriter, Write},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::broadcast;

pub struct RunLogger {
    run_id: String,
    seq: u64,
    jsonl: BufWriter<File>,
    text: BufWriter<File>,
    events: Option<broadcast::Sender<Value>>,
}

impl RunLogger {
    pub fn create_with_events(
        run_dir: &Path,
        events: Option<broadcast::Sender<Value>>,
    ) -> Result<Self> {
        std::fs::create_dir_all(run_dir)
            .with_context(|| format!("failed to create run directory: {}", run_dir.display()))?;
        let jsonl = open(run_dir.join("trace.jsonl"))?;
        let text = open(run_dir.join("trace.log"))?;
        let run_id = run_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("run")
            .to_owned();
        Ok(Self {
            run_id,
            seq: 0,
            jsonl: BufWriter::new(jsonl),
            text: BufWriter::new(text),
            events,
        })
    }

    pub fn resume_with_events(
        run_dir: &Path,
        events: Option<broadcast::Sender<Value>>,
    ) -> Result<Self> {
        let trace_path = run_dir.join("trace.jsonl");
        let trace = std::fs::read_to_string(&trace_path)
            .with_context(|| format!("failed to read session trace: {}", trace_path.display()))?;
        let last = trace
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .context("session trace is empty")?;
        let last: Value = serde_json::from_str(last).context("session trace has invalid JSON")?;
        let seq = last["seq"]
            .as_u64()
            .context("session trace event has no sequence")?;
        let run_id = last["run_id"]
            .as_str()
            .context("session trace event has no run ID")?
            .to_owned();
        let jsonl = append(run_dir.join("trace.jsonl"))?;
        let text = append(run_dir.join("trace.log"))?;
        Ok(Self {
            run_id,
            seq,
            jsonl: BufWriter::new(jsonl),
            text: BufWriter::new(text),
            events,
        })
    }

    pub fn recovery_segment_with_events(
        run_dir: &Path,
        events: Option<broadcast::Sender<Value>>,
    ) -> Result<Self> {
        std::fs::create_dir_all(run_dir)
            .with_context(|| format!("failed to create run directory: {}", run_dir.display()))?;
        let run_id = format!(
            "{}-recovered-{}",
            run_dir
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("run"),
            now_ms()
        );
        Ok(Self {
            run_id,
            seq: 0,
            jsonl: BufWriter::new(append(run_dir.join("trace.jsonl"))?),
            text: BufWriter::new(append(run_dir.join("trace.log"))?),
            events,
        })
    }

    pub fn event<T: Serialize>(&mut self, event: &str, data: T, human: &str) -> Result<()> {
        self.write_event(event, &data, Some(human))
    }
    pub fn event_silent<T: Serialize>(&mut self, event: &str, data: T) -> Result<()> {
        self.write_event(event, &data, None)
    }

    fn write_event<T: Serialize>(
        &mut self,
        event: &str,
        data: &T,
        human: Option<&str>,
    ) -> Result<()> {
        self.seq += 1;
        let value = json!({"schema_version": 1, "seq": self.seq, "timestamp_ms": now_ms(), "run_id": self.run_id, "event": event, "data": serde_json::to_value(data)?});
        serde_json::to_writer(&mut self.jsonl, &value)?;
        self.jsonl.write_all(b"\n")?;
        self.jsonl.flush()?;
        if let Some(events) = &self.events {
            let _ = events.send(value.clone());
        }
        if let Some(human) = human {
            writeln!(self.text, "{human}")?;
            self.text.flush()?;
            if self.events.is_none() {
                crate::terminal::output(human);
            }
        }
        Ok(())
    }
}
fn open(path: impl AsRef<Path>) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path.as_ref())
        .with_context(|| format!("failed to open log: {}", path.as_ref().display()))
}
fn append(path: impl AsRef<Path>) -> Result<File> {
    OpenOptions::new()
        .append(true)
        .open(path.as_ref())
        .with_context(|| format!("failed to append log: {}", path.as_ref().display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_sinks_preserve_payloads_sequence_and_resume_identity() {
        let directory = tempfile::tempdir().unwrap();
        let (events, mut receiver) = broadcast::channel(8);
        let payload = json!({"nested":{"answer":42},"text":"é 🦀"});
        let mut logger =
            RunLogger::create_with_events(directory.path(), Some(events.clone())).unwrap();
        logger
            .event("visible", payload.clone(), "human output")
            .unwrap();
        logger.event_silent("silent", payload.clone()).unwrap();
        drop(logger);
        let mut logger = RunLogger::resume_with_events(directory.path(), Some(events)).unwrap();
        logger.event_silent("resumed", payload.clone()).unwrap();
        drop(logger);

        let trace = std::fs::read_to_string(directory.path().join("trace.jsonl")).unwrap();
        let records = trace
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(records.len(), 3);
        for (index, record) in records.iter().enumerate() {
            assert_eq!(record["seq"], index + 1);
            assert_eq!(record["run_id"], records[0]["run_id"]);
            assert_eq!(record["schema_version"], 1);
            assert_eq!(record["data"], payload);
            assert_eq!(receiver.try_recv().unwrap(), *record);
        }
        assert_eq!(
            records
                .iter()
                .map(|r| r["event"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["visible", "silent", "resumed"]
        );
        assert_eq!(
            std::fs::read_to_string(directory.path().join("trace.log")).unwrap(),
            "human output\n"
        );
    }

    #[test]
    fn recovery_segment_appends_without_rewriting_an_invalid_trace() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("trace.jsonl"), "broken\n").unwrap();
        std::fs::write(directory.path().join("trace.log"), "old output\n").unwrap();
        assert!(RunLogger::resume_with_events(directory.path(), None).is_err());
        let (events, mut receiver) = broadcast::channel(1);
        let mut logger =
            RunLogger::recovery_segment_with_events(directory.path(), Some(events)).unwrap();
        logger
            .event("recovered", json!({"reason":"invalid trace"}), "new output")
            .unwrap();
        drop(logger);
        let event = receiver.try_recv().unwrap();
        assert_eq!(event["seq"], 1);
        assert!(event["run_id"].as_str().unwrap().contains("-recovered-"));
        assert_eq!(
            std::fs::read_to_string(directory.path().join("trace.jsonl")).unwrap(),
            format!("broken\n{event}\n")
        );
        assert_eq!(
            std::fs::read_to_string(directory.path().join("trace.log")).unwrap(),
            "old output\nnew output\n"
        );
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

use std::fs::{File, OpenOptions};
use std::io::Write;

use chrono::Utc;
use serde_json::{json, Value};
use tracing::warn;

use crate::diff::diff_json;

pub enum MessageLogMode {
    Full,
    Diffed,
}

pub(crate) struct MessageLogger {
    mode: MessageLogMode,
    file: File,
    previous_state: Option<Value>,
}

impl MessageLogger {
    pub fn new(mode: MessageLogMode, path: &str) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            mode,
            file,
            previous_state: None,
        })
    }

    pub fn log_request(&mut self, method: &str, path: &str, body: Option<&Value>) {
        let entry = json!({
            "ts": Utc::now().to_rfc3339(),
            "dir": "req",
            "method": method,
            "path": path,
            "body": body,
        });
        self.write_line(&entry);
    }

    pub fn log_command(&mut self, action: &str, zone: Option<u8>, body: &Value) {
        let entry = json!({
            "ts": Utc::now().to_rfc3339(),
            "dir": "cmd",
            "action": action,
            "zone": zone,
            "body": body,
        });
        self.write_line(&entry);
    }

    pub fn log_poll(&mut self, status: u16, body: &Value) {
        if status == 204 {
            let entry = json!({
                "ts": Utc::now().to_rfc3339(),
                "dir": "poll",
                "status": 204,
            });
            self.write_line(&entry);
            return;
        }

        match self.mode {
            MessageLogMode::Full => {
                let entry = json!({
                    "ts": Utc::now().to_rfc3339(),
                    "dir": "poll",
                    "status": status,
                    "body": body,
                });
                self.write_line(&entry);
            }
            MessageLogMode::Diffed => {
                if self.previous_state.is_none() {
                    let entry = json!({
                        "ts": Utc::now().to_rfc3339(),
                        "dir": "poll",
                        "status": status,
                        "full": true,
                        "body": body,
                    });
                    self.write_line(&entry);
                    self.previous_state = Some(body.clone());
                } else {
                    let prev = self.previous_state.as_ref().unwrap();
                    let mut changes = Vec::new();
                    diff_json(prev, body, "", &mut changes);

                    let change_entries: Vec<Value> = changes
                        .iter()
                        .map(|(path, old, new)| {
                            json!({ "path": path, "old": old, "new": new })
                        })
                        .collect();

                    let entry = json!({
                        "ts": Utc::now().to_rfc3339(),
                        "dir": "poll",
                        "status": status,
                        "changes": change_entries,
                    });
                    self.write_line(&entry);
                    self.previous_state = Some(body.clone());
                }
            }
        }
    }

    fn write_line(&mut self, entry: &Value) {
        if let Ok(line) = serde_json::to_string(entry)
            && let Err(e) = writeln!(self.file, "{line}")
        {
            warn!("failed to write log entry: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use tempfile::NamedTempFile;

    #[test]
    fn log_request_writes_ndjson() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap();
        let mut logger = MessageLogger::new(MessageLogMode::Full, path).unwrap();
        logger.log_request("POST", "/Endpoints/app/Connect", None);

        let mut contents = String::new();
        std::fs::File::open(path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        let line: Value = serde_json::from_str(contents.trim()).unwrap();
        assert_eq!(line["dir"], "req");
        assert_eq!(line["method"], "POST");
        assert!(line["ts"].as_str().is_some());
    }

    #[test]
    fn diffed_mode_logs_full_first_then_changes() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap();
        let mut logger = MessageLogger::new(MessageLogMode::Diffed, path).unwrap();

        let body1 = json!({"system": {"status": {"outdoorTemperature": 72}}});
        logger.log_poll(200, &body1);

        let body2 = json!({"system": {"status": {"outdoorTemperature": 74}}});
        logger.log_poll(200, &body2);

        let mut contents = String::new();
        std::fs::File::open(path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        let lines: Vec<Value> = contents
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();

        assert_eq!(lines[0]["full"], true);
        assert!(lines[0]["body"].is_object());
        assert!(lines[1].get("changes").is_some());
        assert!(!lines[1]["changes"].as_array().unwrap().is_empty());
    }

    #[test]
    fn log_poll_204() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap();
        let mut logger = MessageLogger::new(MessageLogMode::Full, path).unwrap();
        logger.log_poll(204, &json!(null));

        let mut contents = String::new();
        std::fs::File::open(path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        let line: Value = serde_json::from_str(contents.trim()).unwrap();
        assert_eq!(line["dir"], "poll");
        assert_eq!(line["status"], 204);
    }

    #[test]
    fn log_command_captures_zone() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap();
        let mut logger = MessageLogger::new(MessageLogMode::Full, path).unwrap();
        logger.log_command("set_mode", Some(0), &json!({"systemMode": "heat"}));

        let mut contents = String::new();
        std::fs::File::open(path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        let line: Value = serde_json::from_str(contents.trim()).unwrap();
        assert_eq!(line["dir"], "cmd");
        assert_eq!(line["action"], "set_mode");
        assert_eq!(line["zone"], 0);
    }

    #[test]
    fn diffed_mode_no_changes_logs_empty_array() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap();
        let mut logger = MessageLogger::new(MessageLogMode::Diffed, path).unwrap();

        let body = json!({"system": {"status": {"outdoorTemperature": 72}}});
        logger.log_poll(200, &body);
        logger.log_poll(200, &body);

        let mut contents = String::new();
        std::fs::File::open(path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        let lines: Vec<Value> = contents
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();

        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1]["changes"].as_array().unwrap().len(), 0);
    }
}

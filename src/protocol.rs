use serde_json::{json, Value};
use uuid::Uuid;

pub const DEFAULT_APP_ID: &str = "lennox_s30";

const LAN_SUBSCRIBE_PATHS: &str = "1;\
    /zones;/occupancy;/schedules;/system;/equipments;\
    /devices;/systemController;/reminderSensors;/reminders;\
    /alerts/active;/alerts/meta;/indoorAirQuality;\
    /fwm;/rgw;/ble;/bleProvisionDB";

pub const TARGET_LCC: &str = "LCC";

pub fn subscribe_message(app_id: &str) -> Value {
    json!({
        "MessageType": "RequestData",
        "SenderID": app_id,
        "MessageID": Uuid::new_v4().to_string(),
        "TargetID": TARGET_LCC,
        "AdditionalParameters": {
            "JSONPath": LAN_SUBSCRIBE_PATHS
        }
    })
}

pub fn command_message(app_id: &str, data: Value) -> Value {
    json!({
        "MessageType": "Command",
        "SenderID": app_id,
        "MessageID": Uuid::new_v4().to_string(),
        "TargetID": TARGET_LCC,
        "Data": data
    })
}

pub fn manual_schedule_id(zone_id: u8) -> u32 {
    16 + zone_id as u32
}

#[allow(dead_code)]
pub fn away_schedule_id(zone_id: u8) -> u32 {
    24 + zone_id as u32
}

#[allow(dead_code)]
pub fn override_schedule_id(zone_id: u8) -> u32 {
    32 + zone_id as u32
}

pub fn set_hvac_mode_data(schedule_id: u32, mode: &str) -> Value {
    json!({
        "schedules": [{
            "schedule": {
                "periods": [{
                    "id": 0,
                    "period": {
                        "systemMode": mode
                    }
                }]
            },
            "id": schedule_id
        }]
    })
}

pub fn set_manual_mode_data(zone_id: u8) -> Value {
    json!({
        "zones": [{
            "config": { "scheduleId": manual_schedule_id(zone_id) },
            "id": zone_id
        }]
    })
}

pub fn set_setpoint_data(schedule_id: u32, hsp_f: i32, hsp_c: f64, csp_f: i32, csp_c: f64) -> Value {
    json!({
        "schedules": [{
            "schedule": {
                "periods": [{
                    "id": 0,
                    "period": {
                        "hsp": hsp_f,
                        "hspC": hsp_c,
                        "csp": csp_f,
                        "cspC": csp_c
                    }
                }]
            },
            "id": schedule_id
        }]
    })
}

pub fn set_fan_mode_data(schedule_id: u32, mode: &str) -> Value {
    json!({
        "schedules": [{
            "schedule": {
                "periods": [{
                    "id": 0,
                    "period": {
                        "fanMode": mode
                    }
                }]
            },
            "id": schedule_id
        }]
    })
}

pub fn parse_retrieve_response(body: &str) -> Vec<Value> {
    let parsed: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    let messages = match parsed.get("messages") {
        Some(Value::Array(msgs)) => msgs,
        _ => return vec![],
    };
    messages
        .iter()
        .filter_map(|msg| {
            let sender = msg.get("SenderID").or_else(|| msg.get("SenderId"));
            match sender.and_then(|v| v.as_str()) {
                Some(TARGET_LCC) => msg.get("Data").cloned(),
                _ => None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscribe_message_structure() {
        let msg = subscribe_message("test_app");
        assert_eq!(msg["MessageType"], "RequestData");
        assert_eq!(msg["SenderID"], "test_app");
        assert_eq!(msg["TargetID"], "LCC");
        assert!(msg["AdditionalParameters"]["JSONPath"].as_str().unwrap().contains("/zones"));
    }

    #[test]
    fn schedule_ids() {
        assert_eq!(manual_schedule_id(0), 16);
        assert_eq!(manual_schedule_id(1), 17);
        assert_eq!(away_schedule_id(0), 24);
        assert_eq!(override_schedule_id(0), 32);
    }

    #[test]
    fn parse_retrieve_with_messages() {
        let body = r#"{"messages": [{"SenderID": "LCC", "Data": {"system": {"status": {"outdoorTemperature": 72}}}}]}"#;
        let data = parse_retrieve_response(body);
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["system"]["status"]["outdoorTemperature"], 72);
    }

    #[test]
    fn parse_retrieve_empty() {
        let data = parse_retrieve_response("");
        assert!(data.is_empty());
    }

    #[test]
    fn parse_retrieve_filters_non_lcc() {
        let body = r#"{"messages": [
            {"SenderID": "LCC", "Data": {"system": {}}},
            {"SenderID": "mapp012345678901234567890", "Data": {"echo": true}},
            {"SenderID": "other", "Data": {"ignored": true}}
        ]}"#;
        let data = parse_retrieve_response(body);
        assert_eq!(data.len(), 1);
        assert!(data[0].get("system").is_some());
    }

    #[test]
    fn command_message_structure() {
        let msg = command_message("test_app", serde_json::json!({"zones": []}));
        assert_eq!(msg["MessageType"], "Command");
        assert_eq!(msg["SenderID"], "test_app");
        assert_eq!(msg["TargetID"], "LCC");
        assert!(msg["Data"]["zones"].is_array());
        assert!(!msg["MessageID"].as_str().unwrap().is_empty());
    }
}

use serde_json::Value;

use crate::types::*;

const TEMPERATURE_PAIRS: &[(&str, &str)] = &[
    ("temperature", "temperatureC"),
    ("hsp", "hspC"),
    ("csp", "cspC"),
    ("sp", "spC"),
    ("outdoorTemperature", "outdoorTemperatureC"),
    ("maxHsp", "maxHspC"),
    ("minHsp", "minHspC"),
    ("maxCsp", "maxCspC"),
    ("minCsp", "minCspC"),
];

fn is_celsius_companion(field: &str) -> bool {
    TEMPERATURE_PAIRS.iter().any(|(_, c)| *c == field)
}

fn celsius_companion(f_field: &str) -> Option<&'static str> {
    TEMPERATURE_PAIRS
        .iter()
        .find(|(f, _)| *f == f_field)
        .map(|(_, c)| *c)
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Scope {
    System,
    Zone(u8),
    Equipment(u16),
}

pub(crate) fn diff_json(
    previous: &Value,
    current: &Value,
    path_prefix: &str,
    changes: &mut Vec<(String, Value, Value)>,
) {
    match (previous, current) {
        (Value::Object(prev_map), Value::Object(curr_map)) => {
            for (key, curr_val) in curr_map {
                let path = if path_prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{path_prefix}.{key}")
                };
                match prev_map.get(key) {
                    Some(prev_val) => diff_json(prev_val, curr_val, &path, changes),
                    None => {
                        if curr_val.is_object() {
                            diff_json(&Value::Object(serde_json::Map::new()), curr_val, &path, changes);
                        } else {
                            changes.push((path, Value::Null, curr_val.clone()));
                        }
                    }
                }
            }
        }
        (prev, curr) if prev != curr => {
            changes.push((path_prefix.to_string(), prev.clone(), curr.clone()));
        }
        _ => {}
    }
}

fn try_build_temperature(f_field: &str, parent: &Value) -> Option<Temperature> {
    let c_field = celsius_companion(f_field)?;
    let f_val = parent.get(f_field)?.as_f64()?;
    let c_val = parent.get(c_field)?.as_f64()?;
    Some(Temperature::from_pair(f_val, c_val))
}

pub(crate) fn map_typed_event(
    scope: Scope,
    path: &str,
    new_value: &Value,
    zone_name: &str,
    parent_obj: &Value,
) -> Option<Event> {
    match (scope, path) {
        (Scope::System, "status.outdoorTemperature") => {
            let temp = try_build_temperature(
                "outdoorTemperature",
                parent_obj.pointer("/status").unwrap_or(&Value::Null),
            )?;
            Some(Event::OutdoorTempChanged { temp })
        }
        (Scope::Zone(id), "status.temperature") => {
            let temp = try_build_temperature(
                "temperature",
                parent_obj.pointer("/status").unwrap_or(&Value::Null),
            )?;
            Some(Event::ZoneTemperatureChanged {
                zone_id: id,
                name: zone_name.to_string(),
                temp,
            })
        }
        (Scope::Zone(id), "status.humidity") => {
            let humidity = new_value.as_f64()?;
            Some(Event::ZoneHumidityChanged {
                zone_id: id,
                name: zone_name.to_string(),
                humidity,
            })
        }
        (Scope::Zone(id), "status.period.systemMode") => {
            let mode = HvacMode::from_lennox_str(new_value.as_str()?)?;
            Some(Event::ZoneModeChanged {
                zone_id: id,
                name: zone_name.to_string(),
                mode,
            })
        }
        (Scope::Zone(id), "status.tempOperation") => {
            let state = new_value
                .as_str()
                .and_then(OperatingState::from_lennox_str)
                .unwrap_or(OperatingState::Idle);
            let aux = parent_obj
                .pointer("/status/aux")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            Some(Event::ZoneOperatingChanged {
                zone_id: id,
                name: zone_name.to_string(),
                state,
                aux,
            })
        }
        (Scope::Zone(id), "status.period.hsp" | "status.period.csp") => {
            let status = parent_obj
                .pointer("/status/period")
                .unwrap_or(&Value::Null);
            let heat = try_build_temperature("hsp", status);
            let cool = try_build_temperature("csp", status);
            Some(Event::ZoneSetpointsChanged {
                zone_id: id,
                name: zone_name.to_string(),
                heat,
                cool,
            })
        }
        (Scope::Zone(id), "status.period.fanMode" | "status.fan") => {
            let fan_mode_str = parent_obj
                .pointer("/status/period/fanMode")
                .and_then(|v| v.as_str());
            let fan_running = parent_obj
                .pointer("/status/fan")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let mode = fan_mode_str
                .and_then(FanMode::from_lennox_str)
                .unwrap_or(FanMode::Auto);
            Some(Event::ZoneFanChanged {
                zone_id: id,
                name: zone_name.to_string(),
                mode,
                running: fan_running,
            })
        }
        _ => None,
    }
}

pub(crate) fn generic_event(scope: Scope, path: &str, value: &Value) -> Option<Event> {
    let leaf = path.rsplit('.').next().unwrap_or(path);
    if is_celsius_companion(leaf) {
        return None;
    }

    match scope {
        Scope::System => match value {
            Value::Number(n) => Some(Event::SystemNumeric {
                path: path.to_string(),
                value: n.as_f64()?,
            }),
            Value::String(s) => Some(Event::SystemString {
                path: path.to_string(),
                value: s.clone(),
            }),
            Value::Bool(b) => Some(Event::SystemBool {
                path: path.to_string(),
                value: *b,
            }),
            _ => None,
        },
        Scope::Zone(id) => match value {
            Value::Number(n) => Some(Event::ZoneNumeric {
                zone_id: id,
                path: path.to_string(),
                value: n.as_f64()?,
            }),
            Value::String(s) => Some(Event::ZoneString {
                zone_id: id,
                path: path.to_string(),
                value: s.clone(),
            }),
            Value::Bool(b) => Some(Event::ZoneBool {
                zone_id: id,
                path: path.to_string(),
                value: *b,
            }),
            _ => None,
        },
        Scope::Equipment(id) => match value {
            Value::Number(n) => Some(Event::EquipmentNumeric {
                equipment_id: id,
                path: path.to_string(),
                value: n.as_f64()?,
            }),
            Value::String(s) => Some(Event::EquipmentString {
                equipment_id: id,
                path: path.to_string(),
                value: s.clone(),
            }),
            Value::Bool(b) => Some(Event::EquipmentBool {
                equipment_id: id,
                path: path.to_string(),
                value: *b,
            }),
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn diff_detects_leaf_change() {
        let prev = json!({"status": {"temperature": 71.0}});
        let curr = json!({"status": {"temperature": 72.0}});
        let mut changes = vec![];
        diff_json(&prev, &curr, "", &mut changes);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].0, "status.temperature");
        assert_eq!(changes[0].1, json!(71.0));
        assert_eq!(changes[0].2, json!(72.0));
    }

    #[test]
    fn diff_ignores_unchanged() {
        let val = json!({"status": {"temperature": 71.0, "humidity": 45.0}});
        let mut changes = vec![];
        diff_json(&val, &val, "", &mut changes);
        assert!(changes.is_empty());
    }

    #[test]
    fn diff_detects_new_key() {
        let prev = json!({"status": {}});
        let curr = json!({"status": {"temperature": 72.0}});
        let mut changes = vec![];
        diff_json(&prev, &curr, "", &mut changes);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].0, "status.temperature");
    }

    #[test]
    fn temperature_pair_folding() {
        let parent =
            json!({"status": {"outdoorTemperature": 72, "outdoorTemperatureC": 22.0}});
        let event = map_typed_event(
            Scope::System,
            "status.outdoorTemperature",
            &json!(72),
            "",
            &parent,
        );
        assert!(event.is_some());
        match event.unwrap() {
            Event::OutdoorTempChanged { temp } => {
                assert_eq!(temp.celsius(), 22.0);
            }
            other => panic!("expected OutdoorTempChanged, got {other:?}"),
        }
    }

    #[test]
    fn celsius_companion_suppressed() {
        let event = generic_event(Scope::System, "status.outdoorTemperatureC", &json!(22.0));
        assert!(event.is_none());
    }

    #[test]
    fn unknown_field_emits_generic() {
        let event = generic_event(Scope::System, "status.someUnknownField", &json!(42.5));
        match event {
            Some(Event::SystemNumeric { path, value }) => {
                assert_eq!(path, "status.someUnknownField");
                assert_eq!(value, 42.5);
            }
            other => panic!("expected SystemNumeric, got {other:?}"),
        }

        let event = generic_event(Scope::Zone(0), "config.enabled", &json!(true));
        match event {
            Some(Event::ZoneBool {
                zone_id,
                path,
                value,
            }) => {
                assert_eq!(zone_id, 0);
                assert_eq!(path, "config.enabled");
                assert!(value);
            }
            other => panic!("expected ZoneBool, got {other:?}"),
        }
    }

    #[test]
    fn zone_mode_change_emits_typed() {
        let parent = json!({"status": {"period": {"systemMode": "heat"}}});
        let event = map_typed_event(
            Scope::Zone(1),
            "status.period.systemMode",
            &json!("heat"),
            "Upstairs",
            &parent,
        );
        match event {
            Some(Event::ZoneModeChanged {
                zone_id,
                name,
                mode,
            }) => {
                assert_eq!(zone_id, 1);
                assert_eq!(name, "Upstairs");
                assert_eq!(mode, HvacMode::Heat);
            }
            other => panic!("expected ZoneModeChanged, got {other:?}"),
        }
    }

    #[test]
    fn zone_setpoints_from_period() {
        let parent = json!({
            "status": {
                "period": {
                    "hsp": 70, "hspC": 21.0,
                    "csp": 76, "cspC": 24.5
                }
            }
        });
        let event = map_typed_event(
            Scope::Zone(0),
            "status.period.hsp",
            &json!(70),
            "Main",
            &parent,
        );
        match event {
            Some(Event::ZoneSetpointsChanged { heat, cool, .. }) => {
                assert_eq!(heat.unwrap().celsius(), 21.0);
                assert_eq!(cool.unwrap().celsius(), 24.5);
            }
            other => panic!("expected ZoneSetpointsChanged, got {other:?}"),
        }
    }

    #[test]
    fn equipment_generic_events() {
        let event = generic_event(
            Scope::Equipment(1),
            "status.compressorSpeed",
            &json!(85.0),
        );
        match event {
            Some(Event::EquipmentNumeric {
                equipment_id,
                path,
                value,
            }) => {
                assert_eq!(equipment_id, 1);
                assert_eq!(path, "status.compressorSpeed");
                assert_eq!(value, 85.0);
            }
            other => panic!("expected EquipmentNumeric, got {other:?}"),
        }
    }
}

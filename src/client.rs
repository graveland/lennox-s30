use serde_json::{Map, Value};
use tracing::{debug, trace};

use crate::diff::{diff_json, generic_event, map_typed_event, Scope};
use crate::logger::{MessageLogMode, MessageLogger};
use crate::protocol::{
    generate_app_id, manual_schedule_id, parse_retrieve_response, subscribe_message,
};
use crate::types::*;
use crate::{Error, Result};

const DEADBAND_C: f64 = 1.5;

type EventCallback = Box<dyn Fn(&Event) + Send + Sync>;
type SnapshotCallback = Box<dyn Fn(&System) + Send + Sync>;

pub struct S30ClientBuilder {
    ip: String,
    protocol: String,
    event_callbacks: Vec<EventCallback>,
    snapshot_callbacks: Vec<SnapshotCallback>,
    log_mode: Option<MessageLogMode>,
    log_path: Option<String>,
}

impl S30ClientBuilder {
    pub fn new(ip: impl Into<String>) -> Self {
        Self {
            ip: ip.into(),
            protocol: "https".to_string(),
            event_callbacks: Vec::new(),
            snapshot_callbacks: Vec::new(),
            log_mode: None,
            log_path: None,
        }
    }

    pub fn protocol(mut self, proto: &str) -> Self {
        self.protocol = proto.to_string();
        self
    }

    pub fn on_event(mut self, f: impl Fn(&Event) + Send + Sync + 'static) -> Self {
        self.event_callbacks.push(Box::new(f));
        self
    }

    pub fn on_snapshot(mut self, f: impl Fn(&System) + Send + Sync + 'static) -> Self {
        self.snapshot_callbacks.push(Box::new(f));
        self
    }

    pub fn message_log(mut self, mode: MessageLogMode, path: impl Into<String>) -> Self {
        self.log_mode = Some(mode);
        self.log_path = Some(path.into());
        self
    }

    pub fn build(self) -> S30Client {
        let http = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .expect("failed to build HTTP client");

        let logger = match (self.log_mode, self.log_path) {
            (Some(mode), Some(path)) => {
                Some(MessageLogger::new(mode, &path).expect("failed to open log file"))
            }
            _ => None,
        };

        S30Client {
            http,
            base_url: format!("{}://{}", self.protocol, self.ip),
            app_id: generate_app_id(),
            connected: false,
            systems: Vec::new(),
            previous_json: Value::Object(Map::new()),
            event_callbacks: self.event_callbacks,
            snapshot_callbacks: self.snapshot_callbacks,
            logger,
        }
    }
}

pub struct S30Client {
    http: reqwest::Client,
    base_url: String,
    app_id: String,
    connected: bool,
    systems: Vec<System>,
    previous_json: Value,
    event_callbacks: Vec<EventCallback>,
    snapshot_callbacks: Vec<SnapshotCallback>,
    logger: Option<MessageLogger>,
}

impl S30Client {
    pub fn builder(ip: impl Into<String>) -> S30ClientBuilder {
        S30ClientBuilder::new(ip)
    }

    pub async fn connect(&mut self) -> Result<()> {
        let connect_url = format!("{}/Endpoints/{}/Connect", self.base_url, self.app_id);
        debug!(url = %connect_url, "connecting to S30");

        let connect_path = format!("/Endpoints/{}/Connect", self.app_id);
        if let Some(ref mut logger) = self.logger {
            logger.log_request("POST", &connect_path, None);
        }

        self.http
            .post(&connect_url)
            .send()
            .await?
            .error_for_status()?;

        let subscribe_url = format!("{}/Messages/RequestData", self.base_url);
        let msg = subscribe_message(&self.app_id);
        debug!(url = %subscribe_url, "subscribing to data");

        if let Some(ref mut logger) = self.logger {
            logger.log_request("POST", "/Messages/RequestData", Some(&msg));
        }

        self.http
            .post(&subscribe_url)
            .json(&msg)
            .send()
            .await?
            .error_for_status()?;

        self.connected = true;
        Ok(())
    }

    pub async fn poll(&mut self) -> Result<()> {
        if !self.connected {
            return Err(Error::NotConnected);
        }

        let url = format!(
            "{}/Messages/{}/Retrieve?LongPollingTimeout=15",
            self.base_url, self.app_id
        );
        let resp = self.http.get(&url).send().await?;
        let status = resp.status().as_u16();

        match status {
            204 => {
                trace!("poll: no changes");
                if let Some(ref mut logger) = self.logger {
                    logger.log_poll(204, &Value::Null);
                }
                return Ok(());
            }
            502 => {
                debug!("poll: transient 502");
                return Ok(());
            }
            s if (400..600).contains(&s) => {
                resp.error_for_status()?;
                unreachable!();
            }
            _ => {}
        }

        let body = resp.text().await?;

        if let Some(ref mut logger) = self.logger {
            let body_json = serde_json::from_str(&body).unwrap_or(Value::Null);
            logger.log_poll(status, &body_json);
        }

        let data_payloads = parse_retrieve_response(&body);
        if data_payloads.is_empty() {
            return Ok(());
        }

        for data in &data_payloads {
            self.process_data(data);
        }

        Ok(())
    }

    pub async fn disconnect(&mut self) -> Result<()> {
        let url = format!("{}/Endpoints/{}/Disconnect", self.base_url, self.app_id);
        debug!(url = %url, "disconnecting from S30");
        self.http.post(&url).send().await?.error_for_status()?;
        self.connected = false;
        Ok(())
    }

    pub fn systems(&self) -> &[System] {
        &self.systems
    }

    pub fn zone(&self, system: usize, zone: u8) -> Option<&Zone> {
        self.systems
            .get(system)
            .and_then(|s| s.zones.iter().find(|z| z.id == zone))
    }

    fn process_data(&mut self, data: &Value) {
        let mut all_events = Vec::new();
        let mut snapshot_system_indices = std::collections::HashSet::new();

        if let Some(system_data) = data.get("system") {
            let sys_idx = self.ensure_system("0");

            let prev_system = self
                .previous_json
                .pointer("/system")
                .cloned()
                .unwrap_or(Value::Object(Map::new()));

            let mut changes = Vec::new();
            diff_json(&prev_system, system_data, "", &mut changes);

            for (path, _old, new_val) in &changes {
                if let Some(evt) =
                    map_typed_event(Scope::System, path, new_val, "", system_data)
                {
                    all_events.push(evt);
                } else if let Some(evt) = generic_event(Scope::System, path, new_val) {
                    all_events.push(evt);
                }
            }

            self.update_system_from_json(sys_idx, system_data);
            snapshot_system_indices.insert(sys_idx);

            merge_json(
                self.previous_json
                    .as_object_mut()
                    .expect("previous_json is always an object"),
                "system",
                system_data,
            );
        }

        if let Some(Value::Array(zones_arr)) = data.get("zones") {
            for zone_data in zones_arr {
                let zone_id = match zone_data.get("id").and_then(|v| v.as_u64()) {
                    Some(id) => id as u8,
                    None => continue,
                };

                let sys_idx = self.ensure_system("0");
                let prev_zone = self
                    .previous_json
                    .pointer(&format!("/zones/{zone_id}"))
                    .cloned()
                    .unwrap_or(Value::Object(Map::new()));

                let mut changes = Vec::new();
                diff_json(&prev_zone, zone_data, "", &mut changes);

                let zone_name = zone_data
                    .pointer("/name")
                    .or_else(|| zone_data.pointer("/config/name"))
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        self.systems
                            .get(sys_idx)
                            .and_then(|s| s.zones.iter().find(|z| z.id == zone_id))
                            .map(|z| z.name.as_str())
                    })
                    .unwrap_or("")
                    .to_string();

                for (path, _old, new_val) in &changes {
                    if let Some(evt) = map_typed_event(
                        Scope::Zone(zone_id),
                        path,
                        new_val,
                        &zone_name,
                        zone_data,
                    ) {
                        all_events.push(evt);
                    } else if let Some(evt) = generic_event(Scope::Zone(zone_id), path, new_val) {
                        all_events.push(evt);
                    }
                }

                self.update_zone_from_json(sys_idx, zone_id, zone_data);
                snapshot_system_indices.insert(sys_idx);

                let zones_map = self
                    .previous_json
                    .as_object_mut()
                    .expect("previous_json is always an object")
                    .entry("zones")
                    .or_insert_with(|| Value::Object(Map::new()));
                if let Value::Object(m) = zones_map {
                    m.insert(zone_id.to_string(), zone_data.clone());
                }
            }
        }

        if let Some(Value::Array(equip_arr)) = data.get("equipments") {
            for equip_data in equip_arr {
                let equip_id = match equip_data.get("id").and_then(|v| v.as_u64()) {
                    Some(id) => id as u16,
                    None => continue,
                };

                let prev_equip = self
                    .previous_json
                    .pointer(&format!("/equipments/{equip_id}"))
                    .cloned()
                    .unwrap_or(Value::Object(Map::new()));

                let mut changes = Vec::new();
                diff_json(&prev_equip, equip_data, "", &mut changes);

                for (path, _old, new_val) in &changes {
                    if let Some(evt) = generic_event(Scope::Equipment(equip_id), path, new_val) {
                        all_events.push(evt);
                    }
                }

                let equip_map = self
                    .previous_json
                    .as_object_mut()
                    .expect("previous_json is always an object")
                    .entry("equipments")
                    .or_insert_with(|| Value::Object(Map::new()));
                if let Value::Object(m) = equip_map {
                    m.insert(equip_id.to_string(), equip_data.clone());
                }
            }
        }

        for event in &all_events {
            for cb in &self.event_callbacks {
                cb(event);
            }
        }

        for sys_idx in snapshot_system_indices {
            if let Some(system) = self.systems.get(sys_idx) {
                for cb in &self.snapshot_callbacks {
                    cb(system);
                }
            }
        }

        if !all_events.is_empty() {
            debug!(count = all_events.len(), "processed events from poll");
        }
    }

    fn ensure_system(&mut self, id: &str) -> usize {
        if let Some(idx) = self.systems.iter().position(|s| s.id == id) {
            return idx;
        }
        self.systems.push(System {
            id: id.to_string(),
            ..Default::default()
        });
        self.systems.len() - 1
    }

    fn update_system_from_json(&mut self, sys_idx: usize, data: &Value) {
        let system = &mut self.systems[sys_idx];

        if let Some(name) = data.pointer("/config/name").and_then(|v| v.as_str()) {
            system.name = name.to_string();
        }
        if let Some(pt) = data
            .pointer("/config/options/productType")
            .and_then(|v| v.as_str())
        {
            system.product_type = pt.to_string();
        }
        if let Some(tu) = data
            .pointer("/config/options/temperatureUnit")
            .and_then(|v| v.as_str())
        {
            system.temperature_unit = tu.to_string();
        }
        if let Some(iu) = data
            .pointer("/config/options/indoorUnitType")
            .and_then(|v| v.as_str())
        {
            system.indoor_unit_type = iu.to_string();
        }
        if let Some(ou) = data
            .pointer("/config/options/outdoorUnitType")
            .and_then(|v| v.as_str())
        {
            system.outdoor_unit_type = ou.to_string();
        }

        let status = data.pointer("/status").unwrap_or(&Value::Null);
        if let (Some(f), Some(c)) = (
            status.get("outdoorTemperature").and_then(|v| v.as_f64()),
            status
                .get("outdoorTemperatureC")
                .and_then(|v| v.as_f64()),
        ) {
            system.outdoor_temperature = Some(Temperature::from_pair(f, c));
        } else if let Some(c) = status
            .get("outdoorTemperatureC")
            .and_then(|v| v.as_f64())
        {
            system.outdoor_temperature = Some(Temperature::from_celsius(c));
        } else if let Some(f) = status
            .get("outdoorTemperature")
            .and_then(|v| v.as_f64())
        {
            system.outdoor_temperature = Some(Temperature::from_fahrenheit(f));
        }
    }

    fn update_zone_from_json(&mut self, sys_idx: usize, zone_id: u8, data: &Value) {
        let system = &mut self.systems[sys_idx];
        let zone = match system.zones.iter_mut().find(|z| z.id == zone_id) {
            Some(z) => z,
            None => {
                system.zones.push(Zone {
                    id: zone_id,
                    ..Default::default()
                });
                system.zones.last_mut().unwrap()
            }
        };

        if let Some(name) = data.get("name").and_then(|v| v.as_str()) {
            zone.name = name.to_string();
        } else if let Some(name) = data.pointer("/config/name").and_then(|v| v.as_str()) {
            zone.name = name.to_string();
        }

        let status = data.pointer("/status").unwrap_or(&Value::Null);

        if let (Some(f), Some(c)) = (
            status.get("temperature").and_then(|v| v.as_f64()),
            status.get("temperatureC").and_then(|v| v.as_f64()),
        ) {
            zone.temperature = Some(Temperature::from_pair(f, c));
        } else if let Some(c) = status.get("temperatureC").and_then(|v| v.as_f64()) {
            zone.temperature = Some(Temperature::from_celsius(c));
        } else if let Some(f) = status.get("temperature").and_then(|v| v.as_f64()) {
            zone.temperature = Some(Temperature::from_fahrenheit(f));
        }

        if let Some(h) = status.get("humidity").and_then(|v| v.as_f64()) {
            zone.humidity = Some(h);
        }

        let period = status.pointer("/period").unwrap_or(&Value::Null);

        if let Some(mode_str) = period.get("systemMode").and_then(|v| v.as_str()) {
            zone.mode = HvacMode::from_lennox_str(mode_str);
        }

        if let (Some(f), Some(c)) = (
            period.get("hsp").and_then(|v| v.as_f64()),
            period.get("hspC").and_then(|v| v.as_f64()),
        ) {
            zone.heat_setpoint = Some(Temperature::from_pair(f, c));
        }

        if let (Some(f), Some(c)) = (
            period.get("csp").and_then(|v| v.as_f64()),
            period.get("cspC").and_then(|v| v.as_f64()),
        ) {
            zone.cool_setpoint = Some(Temperature::from_pair(f, c));
        }

        if let Some(fan_mode_str) = period.get("fanMode").and_then(|v| v.as_str()) {
            zone.fan_mode = FanMode::from_lennox_str(fan_mode_str);
        }

        if let Some(fan) = status.get("fan").and_then(|v| v.as_bool()) {
            zone.fan_running = fan;
        }

        if let Some(op_str) = status.get("tempOperation").and_then(|v| v.as_str()) {
            zone.operating = OperatingState::from_lennox_str(op_str).unwrap_or_default();
        }

        if let Some(aux) = status.get("aux").and_then(|v| v.as_bool()) {
            zone.aux_heat = aux;
        }

        if let Some(sched_id) = data.pointer("/config/scheduleId").and_then(|v| v.as_u64()) {
            zone.schedule_id = Some(sched_id as u32);
        }
    }

    // -- Command methods --

    /// Set HVAC mode for a zone. Switches to manual schedule if needed.
    pub async fn set_hvac_mode(&mut self, zone_id: u8, mode: HvacMode) -> Result<()> {
        self.ensure_manual_schedule(zone_id).await?;
        let manual_id = manual_schedule_id(zone_id);
        let data = crate::protocol::set_hvac_mode_data(manual_id, mode.as_lennox_str());
        self.publish_command_logged("set_hvac_mode", Some(zone_id), data)
            .await
    }

    /// Set heat setpoint for a zone. Enforces deadband against cool setpoint.
    pub async fn set_heat_setpoint(&mut self, zone_id: u8, temp: Temperature) -> Result<()> {
        let zone = self.find_zone(zone_id)?;
        let manual_id = manual_schedule_id(zone_id);

        let hsp_c = temp.to_lennox_celsius();
        let hsp_f = temp.to_lennox_fahrenheit();

        let (csp_c, csp_f) = if let Some(ref c) = zone.cool_setpoint {
            let min_cool = hsp_c + DEADBAND_C;
            if c.to_lennox_celsius() < min_cool {
                let adjusted = Temperature::from_celsius(min_cool);
                (adjusted.to_lennox_celsius(), adjusted.to_lennox_fahrenheit())
            } else {
                (c.to_lennox_celsius(), c.to_lennox_fahrenheit())
            }
        } else {
            let default_cool = Temperature::from_celsius(hsp_c + DEADBAND_C);
            (
                default_cool.to_lennox_celsius(),
                default_cool.to_lennox_fahrenheit(),
            )
        };

        self.ensure_manual_schedule(zone_id).await?;
        let data = crate::protocol::set_setpoint_data(manual_id, hsp_f, hsp_c, csp_f, csp_c);
        self.publish_command_logged("set_heat_setpoint", Some(zone_id), data)
            .await
    }

    /// Set cool setpoint for a zone. Enforces deadband against heat setpoint.
    pub async fn set_cool_setpoint(&mut self, zone_id: u8, temp: Temperature) -> Result<()> {
        let zone = self.find_zone(zone_id)?;
        let manual_id = manual_schedule_id(zone_id);

        let csp_c = temp.to_lennox_celsius();
        let csp_f = temp.to_lennox_fahrenheit();

        let (hsp_c, hsp_f) = if let Some(ref h) = zone.heat_setpoint {
            let max_heat = csp_c - DEADBAND_C;
            if h.to_lennox_celsius() > max_heat {
                let adjusted = Temperature::from_celsius(max_heat);
                (adjusted.to_lennox_celsius(), adjusted.to_lennox_fahrenheit())
            } else {
                (h.to_lennox_celsius(), h.to_lennox_fahrenheit())
            }
        } else {
            let default_heat = Temperature::from_celsius(csp_c - DEADBAND_C);
            (
                default_heat.to_lennox_celsius(),
                default_heat.to_lennox_fahrenheit(),
            )
        };

        self.ensure_manual_schedule(zone_id).await?;
        let data = crate::protocol::set_setpoint_data(manual_id, hsp_f, hsp_c, csp_f, csp_c);
        self.publish_command_logged("set_cool_setpoint", Some(zone_id), data)
            .await
    }

    /// Set fan mode for a zone. Switches to manual schedule if needed.
    pub async fn set_fan_mode(&mut self, zone_id: u8, mode: FanMode) -> Result<()> {
        self.ensure_manual_schedule(zone_id).await?;
        let manual_id = manual_schedule_id(zone_id);
        let data = crate::protocol::set_fan_mode_data(manual_id, mode.as_lennox_str());
        self.publish_command_logged("set_fan_mode", Some(zone_id), data)
            .await
    }

    // -- Helpers --

    fn find_zone(&self, zone_id: u8) -> Result<&Zone> {
        for system in &self.systems {
            for zone in &system.zones {
                if zone.id == zone_id {
                    return Ok(zone);
                }
            }
        }
        Err(Error::InvalidZone(zone_id))
    }

    async fn ensure_manual_schedule(&mut self, zone_id: u8) -> Result<()> {
        let schedule_id = self.find_zone(zone_id)?.schedule_id;
        let manual_id = manual_schedule_id(zone_id);
        if schedule_id != Some(manual_id) {
            let data = crate::protocol::set_manual_mode_data(zone_id);
            self.publish_command_logged("set_manual_schedule", Some(zone_id), data)
                .await?;
        }
        Ok(())
    }

    async fn publish_command_logged(
        &mut self,
        action: &str,
        zone: Option<u8>,
        data: serde_json::Value,
    ) -> Result<()> {
        if !self.connected {
            return Err(Error::NotConnected);
        }

        if let Some(ref mut logger) = self.logger {
            logger.log_command(action, zone, &data);
        }

        let msg = crate::protocol::command_message(&self.app_id, data);
        let url = format!("{}/Messages/Publish", self.base_url);
        self.http
            .post(&url)
            .json(&msg)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }
}

fn deep_merge(target: &mut Value, source: &Value) {
    match (target, source) {
        (Value::Object(t), Value::Object(s)) => {
            for (k, v) in s {
                deep_merge(t.entry(k.clone()).or_insert(Value::Null), v);
            }
        }
        (t, s) => {
            *t = s.clone();
        }
    }
}

fn merge_json(target: &mut Map<String, Value>, key: &str, new_data: &Value) {
    let entry = target
        .entry(key.to_string())
        .or_insert(Value::Null);
    deep_merge(entry, new_data);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadband_enforced_on_heat_setpoint() {
        let heat = Temperature::from_fahrenheit(74.0);
        let cool = Temperature::from_fahrenheit(75.0); // only 1F gap
        let min_cool_c = heat.to_lennox_celsius() + DEADBAND_C;
        assert!(cool.to_lennox_celsius() < min_cool_c);
        let adjusted = Temperature::from_celsius(min_cool_c);
        assert!(adjusted.to_lennox_celsius() >= heat.to_lennox_celsius() + DEADBAND_C);
    }

    #[test]
    fn deadband_not_needed() {
        let heat = Temperature::from_fahrenheit(70.0);
        let cool = Temperature::from_fahrenheit(76.0); // 6F gap
        let min_cool_c = heat.to_lennox_celsius() + DEADBAND_C;
        assert!(cool.to_lennox_celsius() >= min_cool_c);
    }

    #[test]
    fn deadband_enforced_on_cool_setpoint() {
        let cool = Temperature::from_fahrenheit(71.0);
        let heat = Temperature::from_fahrenheit(70.0); // only 1F gap
        let max_heat_c = cool.to_lennox_celsius() - DEADBAND_C;
        assert!(heat.to_lennox_celsius() > max_heat_c);
    }
}

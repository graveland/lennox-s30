use std::time::Instant;

use serde_json::{Map, Value};
use tracing::{debug, trace};

use crate::diff::{diff_json, generic_event, map_typed_event, Scope};
use crate::logger::{MessageLogMode, MessageLogger};
use crate::protocol::{
    manual_schedule_id, override_schedule_id, parse_retrieve_response, subscribe_message,
    DEFAULT_APP_ID,
};
use crate::types::*;
use crate::{Error, Result};

const DEADBAND_C: f64 = 1.5;

type EventCallback = Box<dyn Fn(&Event) + Send + Sync>;
type SnapshotCallback = Box<dyn Fn(&System) + Send + Sync>;

const DIAG_COOLDOWN_SECS: u64 = 300;
const DIAG_MAX_ATTEMPTS_PER_HOUR: u8 = 3;

struct DiagEnforcer {
    target_level: u8,
    last_sent: Option<Instant>,
    attempts_this_hour: u8,
    hour_start: Instant,
}

impl DiagEnforcer {
    fn new(level: u8) -> Self {
        Self {
            target_level: level,
            last_sent: None,
            attempts_this_hour: 0,
            hour_start: Instant::now(),
        }
    }

    fn should_send(&mut self) -> bool {
        let now = Instant::now();

        if now.duration_since(self.hour_start).as_secs() >= 3600 {
            self.attempts_this_hour = 0;
            self.hour_start = now;
        }

        if self.attempts_this_hour >= DIAG_MAX_ATTEMPTS_PER_HOUR {
            return false;
        }

        if let Some(last) = self.last_sent
            && now.duration_since(last).as_secs() < DIAG_COOLDOWN_SECS
        {
            return false;
        }

        true
    }

    fn record_sent(&mut self) {
        self.last_sent = Some(Instant::now());
        self.attempts_this_hour += 1;
    }

    fn reset(&mut self) {
        self.last_sent = None;
        self.attempts_this_hour = 0;
        self.hour_start = Instant::now();
    }
}

pub struct S30ClientBuilder {
    ip: String,
    protocol: String,
    app_id: Option<String>,
    event_callbacks: Vec<EventCallback>,
    snapshot_callbacks: Vec<SnapshotCallback>,
    log_mode: Option<MessageLogMode>,
    log_path: Option<String>,
    diag_level: Option<u8>,
}

impl S30ClientBuilder {
    pub fn new(ip: impl Into<String>) -> Self {
        Self {
            ip: ip.into(),
            protocol: "https".to_string(),
            app_id: None,
            event_callbacks: Vec::new(),
            snapshot_callbacks: Vec::new(),
            log_mode: None,
            log_path: None,
            diag_level: None,
        }
    }

    pub fn protocol(mut self, proto: &str) -> Self {
        self.protocol = proto.to_string();
        self
    }

    pub fn app_id(mut self, id: impl Into<String>) -> Self {
        self.app_id = Some(id.into());
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

    pub fn diag_level(mut self, level: u8) -> Self {
        self.diag_level = Some(level);
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
            app_id: self.app_id.unwrap_or_else(|| DEFAULT_APP_ID.to_string()),
            connected: false,
            systems: Vec::new(),
            previous_json: Value::Object(Map::new()),
            event_callbacks: self.event_callbacks,
            snapshot_callbacks: self.snapshot_callbacks,
            logger,
            diag_enforcer: self.diag_level.map(DiagEnforcer::new),
            diag_reassert_needed: false,
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
    diag_enforcer: Option<DiagEnforcer>,
    diag_reassert_needed: bool,
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

        if let Some(ref mut enforcer) = self.diag_enforcer {
            let data = crate::protocol::set_diag_level_data(enforcer.target_level);
            let msg = crate::protocol::command_message(&self.app_id, data.clone());
            let url = format!("{}/Messages/Publish", self.base_url);
            if let Some(ref mut logger) = self.logger {
                logger.log_command("set_diag_level", None, &data);
            }
            self.http.post(&url).json(&msg).send().await?.error_for_status()?;
            enforcer.reset();
            enforcer.record_sent();
        }

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

        if self.diag_reassert_needed {
            self.diag_reassert_needed = false;
            let target = self.diag_enforcer.as_ref().map(|e| e.target_level);
            if let Some(level) = target {
                let data = crate::protocol::set_diag_level_data(level);
                self.publish_command_logged("reassert_diag_level", None, data).await?;
                if let Some(ref mut enforcer) = self.diag_enforcer {
                    enforcer.record_sent();
                    if enforcer.attempts_this_hour >= DIAG_MAX_ATTEMPTS_PER_HOUR {
                        debug!("diagLevel circuit breaker tripped, stopping reassertions for this hour");
                    }
                }
            }
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

        if let Some(occ) = data.get("occupancy") {
            let sys_idx = self.ensure_system("0");
            let system = &mut self.systems[sys_idx];
            let prev_away = system.is_away();

            if let Some(away) = occ.get("manualAway").and_then(|v| v.as_bool()) {
                system.manual_away = away;
            }
            if let Some(sa) = occ.pointer("/smartAway") {
                if let Some(enabled) = sa.get("enabled").and_then(|v| v.as_bool()) {
                    system.smart_away_enabled = enabled;
                }
                if let Some(state) = sa.get("setpointState").and_then(|v| v.as_str()) {
                    system.smart_away_setpoint_state = state.to_string();
                }
            }

            let new_away = system.is_away();
            if new_away != prev_away {
                all_events.push(Event::AwayModeChanged { away: new_away });
            }
            snapshot_system_indices.insert(sys_idx);

            merge_json(
                self.previous_json
                    .as_object_mut()
                    .expect("previous_json is always an object"),
                "occupancy",
                occ,
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

                let prev_override = self.systems[sys_idx]
                    .zones
                    .iter()
                    .find(|z| z.id == zone_id)
                    .map(|z| z.override_active);

                self.update_zone_from_json(sys_idx, zone_id, zone_data);

                let zone_ref = self.systems[sys_idx]
                    .zones
                    .iter()
                    .find(|z| z.id == zone_id)
                    .unwrap();
                if Some(zone_ref.override_active) != prev_override {
                    all_events.push(Event::ZoneHoldChanged {
                        zone_id,
                        name: zone_name.clone(),
                        active: zone_ref.override_active,
                    });
                }

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

                let sys_idx = self.ensure_system("0");
                self.update_equipment_from_json(sys_idx, equip_id, equip_data, &mut all_events);
                snapshot_system_indices.insert(sys_idx);

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

        if let Some(alerts_data) = data.get("alerts")
            && let Some(Value::Array(active)) = alerts_data.get("active")
        {
            let sys_idx = self.ensure_system("0");
            let system = &mut self.systems[sys_idx];

            let prev_hp_lockout = system.hp_low_ambient_lockout;
            let prev_aux_lockout = system.aux_heat_high_ambient_lockout;

            let mut hp_lockout = false;
            let mut aux_lockout = false;

            for alert_entry in active {
                let alert = match alert_entry.get("alert") {
                    Some(a) => a,
                    None => continue,
                };
                let code = match alert.get("code").and_then(|v| v.as_u64()) {
                    Some(c) => c as u16,
                    None => continue,
                };
                let is_active = alert.get("isStillActive").and_then(|v| v.as_bool()).unwrap_or(false);

                match code {
                    18 => hp_lockout = is_active,
                    19 => aux_lockout = is_active,
                    _ => {}
                }

                all_events.push(Event::AlertChanged { code, active: is_active });
            }

            system.hp_low_ambient_lockout = hp_lockout;
            system.aux_heat_high_ambient_lockout = aux_lockout;

            if hp_lockout != prev_hp_lockout {
                all_events.push(Event::HpLockoutChanged { locked_out: hp_lockout });
            }
            if aux_lockout != prev_aux_lockout {
                all_events.push(Event::AuxLockoutChanged { locked_out: aux_lockout });
            }

            snapshot_system_indices.insert(sys_idx);
        }

        if let Some(ref mut enforcer) = self.diag_enforcer {
            let current_level = self.systems.first().and_then(|s| s.diag_level);
            if let Some(level) = current_level
                && level < enforcer.target_level
                && enforcer.should_send()
            {
                debug!(
                    current = level,
                    target = enforcer.target_level,
                    "diagLevel dropped, reasserting"
                );
                self.diag_reassert_needed = true;
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

        if let Some(ssp) = status.get("singleSetpointMode").and_then(|v| v.as_bool()) {
            system.single_setpoint_mode = ssp;
        }
        if let Some(dl) = status.get("diagLevel").and_then(|v| v.as_u64()) {
            system.diag_level = Some(dl as u8);
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

        if let Some(hold) = data.pointer("/config/scheduleHold") {
            let hold_sched = hold.get("scheduleId").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let enabled = hold.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
            zone.override_active = hold_sched == override_schedule_id(zone_id) && enabled;
        }
    }

    fn update_equipment_from_json(
        &mut self,
        sys_idx: usize,
        equip_id: u16,
        data: &Value,
        events: &mut Vec<Event>,
    ) {
        let system = &mut self.systems[sys_idx];
        let equipment = match system.equipments.iter_mut().find(|e| e.id == equip_id) {
            Some(e) => e,
            None => {
                system.equipments.push(Equipment {
                    id: equip_id,
                    ..Default::default()
                });
                system.equipments.last_mut().unwrap()
            }
        };

        if let Some(et) = data.pointer("/equipment/equipType").and_then(|v| v.as_u64()) {
            equipment.equip_type = et as u16;
        }

        if let Some(Value::Array(params)) = data.pointer("/equipment/parameters") {
            for param_entry in params {
                let param_data = match param_entry.get("parameter") {
                    Some(p) => p,
                    None => continue,
                };
                let pid = match param_data.get("pid").and_then(|v| v.as_u64()) {
                    Some(p) => p as u16,
                    None => continue,
                };
                let name = param_data.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let value = param_data.get("value").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let enabled = param_data.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
                let descriptor = parse_descriptor(param_data);

                let prev_value = equipment.parameters.get(&pid).map(|p| p.value.clone());

                equipment.parameters.insert(pid, Parameter {
                    pid,
                    name: name.clone(),
                    value: value.clone(),
                    enabled,
                    descriptor,
                });

                if prev_value.as_deref() != Some(&value) && prev_value.is_some() {
                    events.push(Event::ParameterChanged {
                        equipment_id: equip_id,
                        pid,
                        name,
                        value,
                    });
                }
            }
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

    /// Set system-wide away mode (occupancy override).
    pub async fn set_away(&mut self, away: bool) -> Result<()> {
        let data = crate::protocol::set_manual_away_data(away);
        self.publish_command_logged("set_away", None, data).await
    }

    /// Set schedule hold for a zone (temporary override of current schedule period).
    pub async fn set_schedule_hold(&mut self, zone_id: u8, hold: bool) -> Result<()> {
        self.find_zone(zone_id)?;
        let data = crate::protocol::set_schedule_hold_data(zone_id, hold);
        self.publish_command_logged("set_schedule_hold", Some(zone_id), data)
            .await
    }

    /// Set both heat and cool setpoints atomically. Rejects deadband violations.
    pub async fn set_setpoints(
        &mut self,
        zone_id: u8,
        heat: Temperature,
        cool: Temperature,
    ) -> Result<()> {
        let hsp_c = heat.to_lennox_celsius();
        let csp_c = cool.to_lennox_celsius();
        if csp_c < hsp_c + DEADBAND_C {
            return Err(Error::InvalidSetpoints {
                heat_c: hsp_c,
                cool_c: csp_c,
                deadband_c: DEADBAND_C,
            });
        }
        self.find_zone(zone_id)?;
        self.ensure_manual_schedule(zone_id).await?;
        let manual_id = manual_schedule_id(zone_id);
        let data = crate::protocol::set_setpoint_data(
            manual_id,
            heat.to_lennox_fahrenheit(),
            hsp_c,
            cool.to_lennox_fahrenheit(),
            csp_c,
        );
        self.publish_command_logged("set_setpoints", Some(zone_id), data)
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

    /// Set an equipment parameter value. Validates against descriptor before sending.
    pub async fn set_equipment_parameter(
        &mut self,
        equipment_id: u16,
        pid: u16,
        value: &str,
    ) -> Result<()> {
        let equipment = self.systems.iter()
            .flat_map(|s| &s.equipments)
            .find(|e| e.id == equipment_id)
            .ok_or_else(|| Error::InvalidParameter {
                equipment_id,
                pid,
                reason: "equipment not found".to_string(),
            })?;

        let equip_type = equipment.equip_type;

        let param = equipment.parameters.get(&pid)
            .ok_or_else(|| Error::InvalidParameter {
                equipment_id,
                pid,
                reason: "parameter not found".to_string(),
            })?;

        if !param.enabled {
            return Err(Error::InvalidParameter {
                equipment_id,
                pid,
                reason: "parameter is read-only (enabled=false)".to_string(),
            });
        }

        let validated = validate_parameter(param, value).map_err(|reason| {
            Error::InvalidParameter { equipment_id, pid, reason }
        })?;

        let data = crate::protocol::set_parameter_data(equip_type, pid, &validated);
        self.publish_command_logged("set_parameter", None, data).await
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

fn validate_parameter(param: &Parameter, value: &str) -> std::result::Result<String, String> {
    match &param.descriptor {
        Descriptor::Range { min, max, inc, .. } => {
            let v: f64 = value.parse().map_err(|_| format!("not a number: {value}"))?;
            if v < *min || v > *max {
                return Err(format!("out of range: {v} not in {min}..{max}"));
            }
            if *inc > 0.0 {
                let steps = ((v - min) / inc).round();
                let reconstructed = min + steps * inc;
                if (reconstructed - v).abs() > 1e-9 {
                    return Err(format!("{v} not a multiple of {inc} (from {min})"));
                }
            }
            Ok(value.to_string())
        }
        Descriptor::Radio { options } => {
            if options.contains_key(value) {
                return Ok(value.to_string());
            }
            for (id, label) in options {
                if label == value {
                    return Ok(id.clone());
                }
            }
            let valid: Vec<_> = options.values().collect();
            Err(format!("unknown option: {value} (valid: {valid:?})"))
        }
        Descriptor::String { max_len } => {
            if let Some(max) = max_len
                && value.len() > *max as usize
            {
                return Err(format!("too long: {} > {max}", value.len()));
            }
            Ok(value.to_string())
        }
    }
}

fn parse_descriptor(param_data: &Value) -> Descriptor {
    match param_data.get("descriptor").and_then(|v| v.as_str()) {
        Some("range") => {
            let range = param_data.pointer("/range").unwrap_or(&Value::Null);
            Descriptor::Range {
                min: range.get("min").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(0.0),
                max: range.get("max").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(0.0),
                inc: range.get("inc").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(1.0),
                unit: param_data.get("unit").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            }
        }
        Some("radio") => {
            let mut options = std::collections::BTreeMap::new();
            if let Some(Value::Object(map)) = param_data.get("radio") {
                for (id, label) in map {
                    if let Some(text) = label.as_str() {
                        options.insert(id.clone(), text.to_string());
                    }
                }
            }
            Descriptor::Radio { options }
        }
        _ => {
            let max_len = param_data.get("string_max").and_then(|v| v.as_u64()).map(|v| v as u32);
            Descriptor::String { max_len }
        }
    }
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

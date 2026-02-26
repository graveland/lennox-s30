use std::fmt;

/// Temperature stored as Celsius internally.
/// Handles Lennox rounding: F to whole degrees, C to 0.5 increments.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Temperature(f64);

impl Temperature {
    pub fn from_celsius(c: f64) -> Self {
        Self(c)
    }

    pub fn from_fahrenheit(f: f64) -> Self {
        Self((f - 32.0) * (5.0 / 9.0))
    }

    /// Construct from paired F+C values as sent by the thermostat.
    /// Prefers the C value (avoids rounding loss).
    pub fn from_pair(_f: f64, c: f64) -> Self {
        Self(c)
    }

    pub fn celsius(&self) -> f64 {
        self.0
    }

    pub fn fahrenheit(&self) -> f64 {
        self.0 * (9.0 / 5.0) + 32.0
    }

    /// Round to Lennox C precision (0.5 increments).
    pub fn to_lennox_celsius(&self) -> f64 {
        (self.0 * 2.0).round() / 2.0
    }

    /// Round to Lennox F precision (whole degrees).
    pub fn to_lennox_fahrenheit(&self) -> i32 {
        self.fahrenheit().round() as i32
    }
}

impl fmt::Display for Temperature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.1}\u{00b0}C", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvacMode {
    Off,
    Heat,
    Cool,
    HeatCool,
    EmergencyHeat,
}

impl HvacMode {
    pub fn as_lennox_str(&self) -> &'static str {
        match self {
            HvacMode::Off => "off",
            HvacMode::Heat => "heat",
            HvacMode::Cool => "cool",
            HvacMode::HeatCool => "heat and cool",
            HvacMode::EmergencyHeat => "emergency heat",
        }
    }

    pub fn from_lennox_str(s: &str) -> Option<Self> {
        match s {
            "off" => Some(HvacMode::Off),
            "heat" => Some(HvacMode::Heat),
            "cool" => Some(HvacMode::Cool),
            "heat and cool" => Some(HvacMode::HeatCool),
            "emergency heat" => Some(HvacMode::EmergencyHeat),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FanMode {
    On,
    Auto,
    Circulate,
}

impl FanMode {
    pub fn as_lennox_str(&self) -> &'static str {
        match self {
            FanMode::On => "on",
            FanMode::Auto => "auto",
            FanMode::Circulate => "circulate",
        }
    }

    pub fn from_lennox_str(s: &str) -> Option<Self> {
        match s {
            "on" => Some(FanMode::On),
            "auto" => Some(FanMode::Auto),
            "circulate" => Some(FanMode::Circulate),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OperatingState {
    #[default]
    Idle,
    Heating,
    Cooling,
}

impl OperatingState {
    pub fn from_lennox_str(s: &str) -> Option<Self> {
        match s {
            "idle" | "off" => Some(OperatingState::Idle),
            "heating" => Some(OperatingState::Heating),
            "cooling" => Some(OperatingState::Cooling),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Zone {
    pub id: u8,
    pub name: String,
    pub temperature: Option<Temperature>,
    pub humidity: Option<f64>,
    pub heat_setpoint: Option<Temperature>,
    pub cool_setpoint: Option<Temperature>,
    pub mode: Option<HvacMode>,
    pub fan_mode: Option<FanMode>,
    pub fan_running: bool,
    pub operating: OperatingState,
    pub aux_heat: bool,
    pub schedule_id: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct System {
    pub id: String,
    pub name: String,
    pub zones: Vec<Zone>,
    pub outdoor_temperature: Option<Temperature>,
    pub product_type: String,
    pub temperature_unit: String,
    pub indoor_unit_type: String,
    pub outdoor_unit_type: String,
}

/// Events emitted by the diff engine when state changes.
#[derive(Debug, Clone)]
pub enum Event {
    ZoneTemperatureChanged { zone_id: u8, name: String, temp: Temperature },
    ZoneHumidityChanged { zone_id: u8, name: String, humidity: f64 },
    ZoneModeChanged { zone_id: u8, name: String, mode: HvacMode },
    ZoneOperatingChanged { zone_id: u8, name: String, state: OperatingState, aux: bool },
    ZoneSetpointsChanged { zone_id: u8, name: String, heat: Option<Temperature>, cool: Option<Temperature> },
    ZoneFanChanged { zone_id: u8, name: String, mode: FanMode, running: bool },
    OutdoorTempChanged { temp: Temperature },

    SystemTemperature { path: String, temp: Temperature },
    SystemNumeric { path: String, value: f64 },
    SystemString { path: String, value: String },
    SystemBool { path: String, value: bool },

    ZoneTemperature { zone_id: u8, path: String, temp: Temperature },
    ZoneNumeric { zone_id: u8, path: String, value: f64 },
    ZoneString { zone_id: u8, path: String, value: String },
    ZoneBool { zone_id: u8, path: String, value: bool },

    EquipmentNumeric { equipment_id: u16, path: String, value: f64 },
    EquipmentString { equipment_id: u16, path: String, value: String },
    EquipmentBool { equipment_id: u16, path: String, value: bool },
}

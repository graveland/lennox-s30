use std::fmt;

#[derive(Debug)]
pub enum Error {
    Http(reqwest::Error),
    NotConnected,
    InvalidZone(u8),
    InvalidMode(String),
    Protocol(String),
    InvalidSetpoints { heat_c: f64, cool_c: f64, deadband_c: f64 },
    Timeout,
    Io(std::io::Error),
    InvalidParameter { equipment_id: u16, pid: u16, reason: String },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Http(e) => write!(f, "HTTP error: {e}"),
            Error::NotConnected => write!(f, "not connected"),
            Error::InvalidZone(id) => write!(f, "invalid zone: {id}"),
            Error::InvalidMode(mode) => write!(f, "invalid mode: {mode}"),
            Error::Protocol(msg) => write!(f, "protocol error: {msg}"),
            Error::InvalidSetpoints { heat_c, cool_c, deadband_c } => write!(
                f,
                "invalid setpoints: heat {heat_c}°C, cool {cool_c}°C (need {deadband_c}°C deadband)"
            ),
            Error::Timeout => write!(f, "poll timeout (no data)"),
            Error::Io(e) => write!(f, "IO error: {e}"),
            Error::InvalidParameter { equipment_id, pid, reason } => write!(
                f,
                "invalid parameter: equipment {equipment_id} pid {pid}: {reason}"
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Http(e) => Some(e),
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Http(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

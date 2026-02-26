# lennox-s30

Rust client for Lennox S30/E30/M30 thermostats over the local LAN API.

Connects directly to the thermostat's HTTPS endpoint (self-signed cert, no authentication). No cloud dependency, no Home Assistant required.

## Usage

```rust
use lennox_s30::S30Client;
use std::time::Duration;

let mut client = S30Client::builder("192.168.1.175")
    .app_id("my_app")
    .on_event(|event| println!("{event:?}"))
    .on_snapshot(|system| {
        for zone in &system.zones {
            if let Some(temp) = zone.temperature {
                println!("[{}] {:.1}°F {:?}", zone.name, temp.fahrenheit(), zone.mode);
            }
        }
    })
    .build();

client.connect().await?;
loop {
    if let Err(e) = client.poll().await {
        eprintln!("poll error: {e}");
        tokio::time::sleep(Duration::from_secs(5)).await;
        client.connect().await?;
    }
}
```

### Builder Options

| Method | Default | Description |
|---|---|---|
| `app_id(id)` | `"lennox_s30"` | Stable identifier for the thermostat's message queue |
| `protocol(proto)` | `"https"` | `"http"` for simulators |
| `on_event(callback)` | none | Granular typed events (temperature, mode, setpoints, etc.) |
| `on_snapshot(callback)` | none | Full system state after each poll cycle |
| `message_log(mode, path)` | none | NDJSON message log (`Full` or `Diffed`) |

### Commands

```rust
use lennox_s30::{HvacMode, FanMode, Temperature};

client.set_hvac_mode(0, HvacMode::Heat).await?;
client.set_heat_setpoint(0, Temperature::from_fahrenheit(68.0)).await?;
client.set_cool_setpoint(0, Temperature::from_fahrenheit(76.0)).await?;
client.set_fan_mode(0, FanMode::Auto).await?;
```

### Multiple LAN Clients

Each `app_id` gets its own message queue on the thermostat. Multiple clients (e.g., this crate + Home Assistant) can coexist safely as long as they use different app IDs.

## Monitor Example

Live-stream thermostat state to the terminal:

```sh
# Basic monitoring
cargo run --example monitor -- 192.168.1.175

# With a custom app ID
cargo run --example monitor -- 192.168.1.175 --app-id my_monitor

# Log all messages (full — ~50MB/day, good for test fixtures)
cargo run --example monitor -- 192.168.1.175 --log /tmp/lennox-full.ndjson

# Log only changes (diffed — ~500KB/day, good for ongoing capture)
cargo run --example monitor -- 192.168.1.175 --log-diff /tmp/lennox.ndjson

# Against HTTP simulator
cargo run --example monitor -- 127.0.0.1:8080 --http
```

## Testing

```sh
# Unit + wiremock tests
cargo test

# Integration tests (requires Python simulator)
cd ~/home/lennoxs30api
.venv/bin/python -m aiohttp.web simulator.main:init_func \
    -c simulator/conf/config_heat_cool.json --port 8080

# In another terminal
cargo test --test integration -- --ignored
```

## Protocol

The thermostat exposes five HTTP endpoints:

| Operation | Method | Path |
|---|---|---|
| Connect | POST | `/Endpoints/{app_id}/Connect` |
| Subscribe | POST | `/Messages/RequestData` |
| Poll | GET | `/Messages/{app_id}/Retrieve?LongPollingTimeout=15` |
| Command | POST | `/Messages/Publish` |
| Disconnect | POST | `/Endpoints/{app_id}/Disconnect` |

Poll returns HTTP 200 (data), 204 (no changes), or 502 (transient error, retry).

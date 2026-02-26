use lennox_s30::{Event, MessageLogMode, S30Client, Temperature};
use std::env;
use std::future::Future;
use std::io::{self, BufRead, Write as _};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[tokio::main]
async fn main() -> lennox_s30::Result<()> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = env::args().collect();
    let ip = args
        .get(1)
        .expect("usage: validate_commands <ip> [--http] [--zone <id>] [--no-log]");
    let use_http = args.iter().any(|a| a == "--http");
    let no_log = args.iter().any(|a| a == "--no-log");
    let zone_id: u8 = args
        .iter()
        .position(|a| a == "--zone")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let events: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(vec![]));
    let events_clone = events.clone();

    let mut builder = S30Client::builder(ip).on_event(move |event| {
        events_clone.lock().unwrap().push(event.clone());
    });

    if use_http {
        builder = builder.protocol("http");
    }

    let log_path = if !no_log {
        let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let path = format!("logs/validate_{ts}.ndjson");
        std::fs::create_dir_all("logs").ok();
        println!("Logging all requests/responses to {path}");
        builder = builder.message_log(MessageLogMode::Full, &path);
        Some(path)
    } else {
        None
    };

    let mut client = builder.build();

    println!("Connecting to {ip}...");
    client.connect().await?;
    println!("Connected. Draining initial state...");

    for i in 0..15 {
        client.poll().await?;
        if client.zone(0, zone_id).is_some_and(|z| z.has_data()) {
            println!("Got zone data after {} polls", i + 1);
            break;
        }
    }

    let zone = client
        .zone(0, zone_id)
        .unwrap_or_else(|| panic!("zone {zone_id} not found"));
    println!("\n=== Current State (zone {zone_id}: {}) ===", zone.name);
    print_zone(zone);

    let system = &client.systems()[0];
    println!(
        "Away: {} (manual_away={})",
        system.is_away(),
        system.manual_away
    );
    println!();

    let orig_heat = zone.heat_setpoint;
    let orig_cool = zone.cool_setpoint;
    let orig_away = system.manual_away;

    type TestCase = (&'static str, String, Box<dyn AsyncTestFn>, Box<dyn AsyncTestFn>);
    let mut all_cases: Vec<TestCase> = vec![
        (
            "Away Mode",
            format!("set_away({}) [currently {}]", !orig_away, orig_away),
            Box::new(SetAway(!orig_away)),
            Box::new(SetAway(orig_away)),
        ),
        (
            "Schedule Hold",
            format!("set_schedule_hold(zone {zone_id}, true)"),
            Box::new(SetScheduleHold(zone_id, true)),
            Box::new(SetScheduleHold(zone_id, false)),
        ),
    ];

    if let (Some(heat), Some(cool)) = (orig_heat, orig_cool) {
        let test_heat = Temperature::from_celsius(heat.celsius() + 1.0);
        let test_cool = Temperature::from_celsius(cool.celsius() + 1.0);
        all_cases.push((
            "Atomic Setpoints",
            format!(
                "set_setpoints({:.1}°C, {:.1}°C) [currently {:.1}°C, {:.1}°C]",
                test_heat.celsius(),
                test_cool.celsius(),
                heat.celsius(),
                cool.celsius()
            ),
            Box::new(SetSetpoints(zone_id, test_heat, test_cool)),
            Box::new(SetSetpoints(zone_id, heat, cool)),
        ));
    } else {
        println!("⚠ Skipping setpoints test (no current heat+cool setpoints)");
    }

    let total = all_cases.len();
    for (i, (name, desc, apply, revert)) in all_cases.into_iter().enumerate() {
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!("Test {}/{total}: {name}", i + 1);
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");

        println!("\n  → Will execute: {desc}");
        wait_for_enter("Press Enter to apply (Ctrl-C to abort)...");

        events.lock().unwrap().clear();
        apply.call(&mut client).await?;
        println!("  ✓ Command sent");

        println!("  Waiting for thermostat response...");
        wait_for_events(&mut client, &events, 30).await;

        print_state(&client, zone_id);
        wait_for_enter("Verify at thermostat, then press Enter to revert...");

        events.lock().unwrap().clear();
        revert.call(&mut client).await?;
        println!("  ✓ Revert sent");

        println!("  Waiting for revert confirmation...");
        wait_for_events(&mut client, &events, 30).await;

        print_state(&client, zone_id);
        println!("  ✓ Reverted\n");
    }

    println!("All tests complete.");
    client.disconnect().await?;
    if let Some(path) = log_path {
        println!("Full request/response log: {path}");
    }
    Ok(())
}

fn print_zone(zone: &lennox_s30::Zone) {
    println!(
        "  Zone {}: {} | temp: {} | heat_sp: {} | cool_sp: {} | mode: {:?} | hold: {}",
        zone.id,
        zone.name,
        fmt_temp(zone.temperature),
        fmt_temp(zone.heat_setpoint),
        fmt_temp(zone.cool_setpoint),
        zone.mode,
        zone.override_active,
    );
}

fn print_state(client: &S30Client, zone_id: u8) {
    if let Some(z) = client.zone(0, zone_id) {
        print_zone(z);
    }
    let system = &client.systems()[0];
    println!(
        "  Away: {} (manual_away={})",
        system.is_away(),
        system.manual_away
    );
}

fn fmt_temp(t: Option<Temperature>) -> String {
    t.map(|t| format!("{:.1}°C/{:.0}°F", t.celsius(), t.fahrenheit()))
        .unwrap_or_else(|| "-".into())
}

fn wait_for_enter(prompt: &str) {
    print!("  {prompt} ");
    io::stdout().flush().unwrap();
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line).unwrap();
}

async fn wait_for_events(
    client: &mut S30Client,
    events: &Arc<Mutex<Vec<Event>>>,
    timeout_s: u64,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_s);
    while tokio::time::Instant::now() < deadline {
        client.poll().await.ok();
        let captured = events.lock().unwrap();
        if !captured.is_empty() {
            for e in captured.iter() {
                println!("  ← {e:?}");
            }
            return;
        }
    }
    println!("  ⚠ Timed out waiting for events ({timeout_s}s)");
}

trait AsyncTestFn: Send {
    fn call(
        self: Box<Self>,
        client: &mut S30Client,
    ) -> Pin<Box<dyn Future<Output = lennox_s30::Result<()>> + '_>>;
}

struct SetAway(bool);
impl AsyncTestFn for SetAway {
    fn call(
        self: Box<Self>,
        client: &mut S30Client,
    ) -> Pin<Box<dyn Future<Output = lennox_s30::Result<()>> + '_>> {
        Box::pin(client.set_away(self.0))
    }
}

struct SetScheduleHold(u8, bool);
impl AsyncTestFn for SetScheduleHold {
    fn call(
        self: Box<Self>,
        client: &mut S30Client,
    ) -> Pin<Box<dyn Future<Output = lennox_s30::Result<()>> + '_>> {
        Box::pin(client.set_schedule_hold(self.0, self.1))
    }
}

struct SetSetpoints(u8, Temperature, Temperature);
impl AsyncTestFn for SetSetpoints {
    fn call(
        self: Box<Self>,
        client: &mut S30Client,
    ) -> Pin<Box<dyn Future<Output = lennox_s30::Result<()>> + '_>> {
        Box::pin(client.set_setpoints(self.0, self.1, self.2))
    }
}

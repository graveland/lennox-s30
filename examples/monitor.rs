use lennox_s30::S30Client;
use std::env;
use std::time::Duration;

#[tokio::main]
async fn main() -> lennox_s30::Result<()> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = env::args().collect();
    let ip = args.get(1).expect("usage: monitor <ip> [--http]");
    let use_http = args.iter().any(|a| a == "--http");

    let mut builder = S30Client::builder(ip)
        .on_event(|event| {
            println!("{event:?}");
        })
        .on_snapshot(|system| {
            for zone in &system.zones {
                if let Some(temp) = zone.temperature {
                    println!(
                        "[{}] {:.1}\u{00b0}C / {:.1}\u{00b0}F | mode: {:?} | fan: {:?}{}",
                        zone.name,
                        temp.celsius(),
                        temp.fahrenheit(),
                        zone.mode,
                        zone.fan_mode,
                        if zone.aux_heat { " | AUX" } else { "" },
                    );
                }
            }
            if let Some(outdoor) = system.outdoor_temperature {
                println!(
                    "Outdoor: {:.1}\u{00b0}C / {:.1}\u{00b0}F",
                    outdoor.celsius(),
                    outdoor.fahrenheit(),
                );
            }
        });

    if use_http {
        builder = builder.protocol("http");
    }

    let mut client = builder.build();

    println!("Connecting to {ip}...");
    client.connect().await?;
    println!("Connected. Polling for updates...");

    loop {
        if let Err(e) = client.poll().await {
            eprintln!("Poll error: {e}");
            tokio::time::sleep(Duration::from_secs(5)).await;
            println!("Reconnecting...");
            client.connect().await?;
        }
    }
}

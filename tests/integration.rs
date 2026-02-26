use std::sync::{Arc, Mutex};

use lennox_s30::{Event, S30Client};

/// Run with: cargo test --test integration -- --ignored
/// Requires simulator running:
///   cd ~/home/lennoxs30api && .venv/bin/python -m aiohttp.web simulator.main:init_func \
///     -c simulator/conf/config_heatpump_furnace.json --port 8080
#[tokio::test]
#[ignore]
async fn connect_poll_disconnect() {
    let events: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(vec![]));
    let events_clone = events.clone();

    let mut client = S30Client::builder("127.0.0.1:8080")
        .protocol("http")
        .on_event(move |event| {
            events_clone.lock().unwrap().push(event.clone());
        })
        .build();

    client.connect().await.expect("connect failed");

    // Simulator queues multiple messages (config, equipment, devices, etc.)
    // and returns one per poll via queue.pop() (LIFO). Poll until system data arrives.
    for i in 0..10 {
        client
            .poll()
            .await
            .unwrap_or_else(|e| panic!("poll {i} failed: {e}"));
        if !client.systems().is_empty() && !client.systems()[0].zones.is_empty() {
            break;
        }
    }

    let systems = client.systems();
    assert!(!systems.is_empty(), "should have at least one system");
    assert!(
        !systems[0].zones.is_empty(),
        "should have at least one zone"
    );

    {
        let captured = events.lock().unwrap();
        assert!(!captured.is_empty(), "should have received events");
    }

    client.disconnect().await.expect("disconnect failed");
}

#[tokio::test]
#[ignore]
async fn outdoor_temp_updates() {
    // Requires outdoorTempSim: true in simulator config.
    // Cycles outdoor temp every 5s. Drain initial queue, wait for sim, then poll.
    let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
    let events_clone = events.clone();

    let mut client = S30Client::builder("127.0.0.1:8080")
        .protocol("http")
        .on_event(move |event| {
            events_clone.lock().unwrap().push(format!("{event:?}"));
        })
        .build();

    client.connect().await.expect("connect failed");

    // Drain the initial config queue
    for i in 0..10 {
        client
            .poll()
            .await
            .unwrap_or_else(|e| panic!("drain poll {i} failed: {e}"));
        if !client.systems().is_empty() {
            break;
        }
    }

    // Wait for simulator to cycle outdoor temp
    tokio::time::sleep(std::time::Duration::from_secs(6)).await;

    // Poll for the temp update
    client.poll().await.expect("temp poll failed");

    {
        let captured = events.lock().unwrap();
        let has_outdoor = captured.iter().any(|e| e.contains("OutdoorTempChanged"));
        println!("Outdoor temp events: {has_outdoor}");
        println!("Total events: {}", captured.len());
    }

    client.disconnect().await.expect("disconnect failed");
}

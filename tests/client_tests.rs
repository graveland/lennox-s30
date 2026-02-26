use std::sync::{Arc, Mutex};

use lennox_s30::S30Client;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn setup_connect_mocks() -> Vec<Mock> {
    vec![
        Mock::given(method("POST"))
            .and(path_regex(r"/Endpoints/.+/Connect"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}")),
        Mock::given(method("POST"))
            .and(path_regex(r"/Messages/RequestData"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}")),
    ]
}

async fn connected_client(server: &MockServer) -> S30Client {
    for mock in setup_connect_mocks() {
        mock.mount(server).await;
    }
    let addr = server.address();
    let mut client = S30Client::builder(format!("{}:{}", addr.ip(), addr.port()))
        .protocol("http")
        .build();
    client.connect().await.expect("connect should succeed");
    client
}

#[tokio::test]
async fn connect_sends_to_correct_endpoints() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_regex(r"/Endpoints/.+/Connect"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path_regex(r"/Messages/RequestData"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(1)
        .mount(&server)
        .await;

    let addr = server.address();
    let mut client = S30Client::builder(format!("{}:{}", addr.ip(), addr.port()))
        .protocol("http")
        .build();
    client.connect().await.expect("connect should succeed");
}

#[tokio::test]
async fn poll_204_returns_ok() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"/Messages/.+/Retrieve"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let mut client = connected_client(&server).await;
    client.poll().await.expect("204 poll should succeed");
}

#[tokio::test]
async fn poll_fires_events_on_state_change() {
    let server = MockServer::start().await;
    let poll_body = serde_json::json!({
        "messages": [{
            "SenderID": "LCC",
            "Data": {
                "system": {
                    "status": {
                        "outdoorTemperature": 72,
                        "outdoorTemperatureC": 22.0
                    }
                }
            }
        }]
    });
    Mock::given(method("GET"))
        .and(path_regex(r"/Messages/.+/Retrieve"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&poll_body))
        .mount(&server)
        .await;

    let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
    let events_clone = events.clone();

    let addr = server.address();
    for mock in setup_connect_mocks() {
        mock.mount(&server).await;
    }
    let mut client = S30Client::builder(format!("{}:{}", addr.ip(), addr.port()))
        .protocol("http")
        .on_event(move |event| {
            events_clone.lock().unwrap().push(format!("{event:?}"));
        })
        .build();

    client.connect().await.unwrap();
    client.poll().await.unwrap();
    let captured = events.lock().unwrap();
    assert!(!captured.is_empty(), "should have fired events");
}

#[tokio::test]
async fn poll_not_connected_returns_error() {
    let client_builder = S30Client::builder("127.0.0.1:9999").protocol("http");
    let mut client = client_builder.build();
    let err = client.poll().await.unwrap_err();
    assert!(
        matches!(err, lennox_s30::Error::NotConnected),
        "expected NotConnected, got {err:?}"
    );
}

#[tokio::test]
async fn poll_updates_system_state() {
    let server = MockServer::start().await;
    let poll_body = serde_json::json!({
        "messages": [{
            "SenderID": "LCC",
            "Data": {
                "system": {
                    "config": { "name": "Test System" },
                    "status": {
                        "outdoorTemperature": 72,
                        "outdoorTemperatureC": 22.0
                    }
                }
            }
        }]
    });
    Mock::given(method("GET"))
        .and(path_regex(r"/Messages/.+/Retrieve"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&poll_body))
        .mount(&server)
        .await;

    let mut client = connected_client(&server).await;
    client.poll().await.unwrap();

    assert_eq!(client.systems().len(), 1);
    let system = &client.systems()[0];
    assert_eq!(system.name, "Test System");
    let temp = system.outdoor_temperature.unwrap();
    assert!((temp.celsius() - 22.0).abs() < 0.01);
}

#[tokio::test]
async fn poll_updates_zone_state() {
    let server = MockServer::start().await;
    let poll_body = serde_json::json!({
        "messages": [{
            "SenderID": "LCC",
            "Data": {
                "zones": [{
                    "id": 0,
                    "name": "Upstairs",
                    "status": {
                        "temperature": 71,
                        "temperatureC": 21.5,
                        "humidity": 42.0,
                        "fan": false,
                        "tempOperation": "heating",
                        "aux": true,
                        "period": {
                            "systemMode": "heat",
                            "hsp": 70,
                            "hspC": 21.0,
                            "csp": 76,
                            "cspC": 24.5,
                            "fanMode": "auto"
                        }
                    },
                    "config": {
                        "scheduleId": 16
                    }
                }]
            }
        }]
    });
    Mock::given(method("GET"))
        .and(path_regex(r"/Messages/.+/Retrieve"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&poll_body))
        .mount(&server)
        .await;

    let mut client = connected_client(&server).await;
    client.poll().await.unwrap();

    let zone = client.zone(0, 0).expect("zone 0 should exist");
    assert_eq!(zone.name, "Upstairs");
    assert!((zone.temperature.unwrap().celsius() - 21.5).abs() < 0.01);
    assert!((zone.humidity.unwrap() - 42.0).abs() < 0.01);
    assert_eq!(zone.mode, Some(lennox_s30::HvacMode::Heat));
    assert!((zone.heat_setpoint.unwrap().celsius() - 21.0).abs() < 0.01);
    assert!((zone.cool_setpoint.unwrap().celsius() - 24.5).abs() < 0.01);
    assert_eq!(zone.fan_mode, Some(lennox_s30::FanMode::Auto));
    assert!(!zone.fan_running);
    assert_eq!(zone.operating, lennox_s30::OperatingState::Heating);
    assert!(zone.aux_heat);
    assert_eq!(zone.schedule_id, Some(16));
}

#[tokio::test]
async fn snapshot_callback_fires_after_events() {
    let server = MockServer::start().await;
    let poll_body = serde_json::json!({
        "messages": [{
            "SenderID": "LCC",
            "Data": {
                "system": {
                    "status": {
                        "outdoorTemperature": 50,
                        "outdoorTemperatureC": 10.0
                    }
                }
            }
        }]
    });
    Mock::given(method("GET"))
        .and(path_regex(r"/Messages/.+/Retrieve"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&poll_body))
        .mount(&server)
        .await;

    let snapshot_temps: Arc<Mutex<Vec<f64>>> = Arc::new(Mutex::new(vec![]));
    let temps_clone = snapshot_temps.clone();

    let addr = server.address();
    for mock in setup_connect_mocks() {
        mock.mount(&server).await;
    }
    let mut client = S30Client::builder(format!("{}:{}", addr.ip(), addr.port()))
        .protocol("http")
        .on_snapshot(move |system| {
            if let Some(temp) = system.outdoor_temperature {
                temps_clone.lock().unwrap().push(temp.celsius());
            }
        })
        .build();

    client.connect().await.unwrap();
    client.poll().await.unwrap();

    let temps = snapshot_temps.lock().unwrap();
    assert_eq!(temps.len(), 1);
    assert!((temps[0] - 10.0).abs() < 0.01);
}

#[tokio::test]
async fn second_poll_only_fires_events_for_changes() {
    let server = MockServer::start().await;

    let poll1 = serde_json::json!({
        "messages": [{
            "SenderID": "LCC",
            "Data": {
                "system": {
                    "status": {
                        "outdoorTemperature": 72,
                        "outdoorTemperatureC": 22.0
                    }
                }
            }
        }]
    });
    let poll2 = serde_json::json!({
        "messages": [{
            "SenderID": "LCC",
            "Data": {
                "system": {
                    "status": {
                        "outdoorTemperature": 72,
                        "outdoorTemperatureC": 22.0
                    }
                }
            }
        }]
    });

    Mock::given(method("GET"))
        .and(path_regex(r"/Messages/.+/Retrieve"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&poll1))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
    let events_clone = events.clone();

    let addr = server.address();
    for mock in setup_connect_mocks() {
        mock.mount(&server).await;
    }
    let mut client = S30Client::builder(format!("{}:{}", addr.ip(), addr.port()))
        .protocol("http")
        .on_event(move |event| {
            events_clone.lock().unwrap().push(format!("{event:?}"));
        })
        .build();

    client.connect().await.unwrap();
    client.poll().await.unwrap();

    let first_count = events.lock().unwrap().len();
    assert!(first_count > 0, "first poll should fire events");

    Mock::given(method("GET"))
        .and(path_regex(r"/Messages/.+/Retrieve"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&poll2))
        .mount(&server)
        .await;

    client.poll().await.unwrap();
    let second_count = events.lock().unwrap().len();
    assert_eq!(
        first_count, second_count,
        "second poll with same data should fire no new events"
    );
}

#[tokio::test]
async fn disconnect_sets_not_connected() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_regex(r"/Endpoints/.+/Disconnect"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = connected_client(&server).await;
    client.disconnect().await.expect("disconnect should succeed");

    let err = client.poll().await.unwrap_err();
    assert!(matches!(err, lennox_s30::Error::NotConnected));
}

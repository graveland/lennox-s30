use lennox_s30::Temperature;

#[test]
fn from_celsius() {
    let t = Temperature::from_celsius(22.0);
    assert_eq!(t.celsius(), 22.0);
    assert!((t.fahrenheit() - 71.6).abs() < 0.01);
}

#[test]
fn from_fahrenheit() {
    let t = Temperature::from_fahrenheit(72.0);
    assert!((t.celsius() - 22.222).abs() < 0.01);
    assert!((t.fahrenheit() - 72.0).abs() < 0.01);
}

#[test]
fn from_pair_prefers_celsius() {
    let t = Temperature::from_pair(72.0, 22.0);
    assert_eq!(t.celsius(), 22.0);
}

#[test]
fn lennox_rounding_celsius() {
    let t = Temperature::from_celsius(22.3);
    assert_eq!(t.to_lennox_celsius(), 22.5);
    let t = Temperature::from_celsius(22.1);
    assert_eq!(t.to_lennox_celsius(), 22.0);
    let t = Temperature::from_celsius(22.25);
    assert_eq!(t.to_lennox_celsius(), 22.5);
}

#[test]
fn lennox_rounding_fahrenheit() {
    let t = Temperature::from_fahrenheit(72.4);
    assert_eq!(t.to_lennox_fahrenheit(), 72);
    let t = Temperature::from_fahrenheit(72.6);
    assert_eq!(t.to_lennox_fahrenheit(), 73);
}

#[test]
fn display() {
    let t = Temperature::from_celsius(22.5);
    assert_eq!(format!("{t}"), "22.5\u{00b0}C");
}

#[test]
fn hvac_mode_roundtrip() {
    use lennox_s30::HvacMode;
    for mode in [
        HvacMode::Off,
        HvacMode::Heat,
        HvacMode::Cool,
        HvacMode::HeatCool,
        HvacMode::EmergencyHeat,
    ] {
        let s = mode.as_lennox_str();
        assert_eq!(HvacMode::from_lennox_str(s), Some(mode));
    }
}

#[test]
fn fan_mode_roundtrip() {
    use lennox_s30::FanMode;
    for mode in [FanMode::On, FanMode::Auto, FanMode::Circulate] {
        let s = mode.as_lennox_str();
        assert_eq!(FanMode::from_lennox_str(s), Some(mode));
    }
}

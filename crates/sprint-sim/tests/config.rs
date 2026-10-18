//! SimConfig serialization (it is stored in printer profiles).

use sprint_sim::SimConfig;

#[test]
fn round_trips_through_json() {
    let config = SimConfig {
        extruders: 2,
        mesh_size: (7, 3),
        sd_files: vec![("a.gcode".into(), 10)],
        ..SimConfig::default()
    };
    let json = serde_json::to_string(&config).unwrap();
    let back: SimConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(back, config);
}

#[test]
fn missing_fields_take_defaults() {
    let config: SimConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(config, SimConfig::default());
    let config: SimConfig =
        serde_json::from_str(r#"{"extruders": 3, "time_scale": 10.0}"#).unwrap();
    assert_eq!(config.extruders, 3);
    assert_eq!(config.time_scale, 10.0);
    assert_eq!(config.planner_buffer, 16);
}

#[test]
fn out_of_range_values_do_not_break_spawn() {
    let config: SimConfig = serde_json::from_str(
        r#"{"extruders": 0, "planner_buffer": 0, "rx_buffer": 0, "time_scale": -1.0,
            "mesh_size": [0, 99], "checksum_error_rate": 9.0, "boot_delay_ms": 0}"#,
    )
    .unwrap();
    let link = sprint_sim::spawn(config);
    assert!(link.control.is_running());
    link.control.shutdown();
}

//! Helpers shared by the integration tests.

use sprint_gcode::{MoveKind, Toolpath};

/// Relative comparison with an absolute floor.
pub fn close(a: f32, b: f32, rel: f32) -> bool {
    (a - b).abs() <= rel * a.abs().max(b.abs()).max(1.0)
}

/// Invariants every toolpath must satisfy, whatever the input.
pub fn check_toolpath(tp: &Toolpath) {
    let n = tp.segments.len();
    // Layers: contiguous, cover every segment, consistent indices.
    if n > 0 {
        assert!(!tp.layers.is_empty(), "segments without layers");
    }
    let mut next_seg = 0;
    let mut next_line = 0;
    for (i, layer) in tp.layers.iter().enumerate() {
        assert_eq!(layer.segments.start, next_seg, "layer {i} segment gap");
        assert!(layer.segments.end >= layer.segments.start);
        assert_eq!(layer.lines.start, next_line, "layer {i} line gap");
        assert!(layer.lines.end >= layer.lines.start);
        next_seg = layer.segments.end;
        next_line = layer.lines.end;
        for s in &tp.segments[layer.segments.clone()] {
            assert_eq!(s.layer as usize, i);
            // A single command (a long helix) can span several layers, so a
            // segment may sit on the line where the next layer starts.
            assert!(s.line >= layer.lines.start && s.line <= layer.lines.end);
        }
        assert!(layer.z.is_finite() && layer.height.is_finite() && layer.height >= 0.0);
        assert!(layer.time_start.is_finite() && layer.duration >= 0.0);
    }
    assert_eq!(next_seg, n);
    if !tp.layers.is_empty() {
        assert_eq!(next_line, tp.line_count);
    }
    // Segments: file order, finite, consistent timing.
    let mut prev_end = 0.0f32;
    let mut prev_line = 0;
    for s in &tp.segments {
        assert!(s.line >= prev_line);
        prev_line = s.line;
        for v in s.start.iter().chain(&s.end) {
            assert!(v.is_finite());
        }
        assert!(s.time.is_finite() && s.duration.is_finite() && s.duration >= 0.0);
        assert!(
            s.time + 1e-3 * s.time.abs().max(1.0) >= prev_end,
            "segment at line {} starts at {} before {}",
            s.line,
            s.time,
            prev_end
        );
        prev_end = s.time + s.duration;
        match s.kind {
            MoveKind::Extrude => {
                assert!(s.extruded > 0.0 && s.width >= 0.0 && s.height > 0.0);
            }
            MoveKind::Travel => assert_eq!((s.extruded, s.width, s.height), (0.0, 0.0, 0.0)),
        }
        assert!((s.layer as usize) < tp.layers.len());
    }
    for r in &tp.retractions {
        assert!(r.amount != 0.0 && r.amount.is_finite());
        assert!(tp.layers.is_empty() || (r.layer as usize) < tp.layers.len());
    }
    // Stats add up.
    let st = &tp.stats;
    assert!(st.estimated_time.is_finite() && st.estimated_time >= 0.0);
    for v in [
        st.filament_length,
        st.filament_volume,
        st.filament_weight,
        st.travel_distance,
        st.extrude_distance,
        st.travel_time,
        st.other_time,
        st.avg_print_speed,
    ] {
        assert!(v.is_finite(), "{st:?}");
    }
    let feature_time: f32 = st.features.iter().map(|f| f.time).sum();
    assert!(
        close(
            feature_time + st.travel_time + st.other_time,
            st.estimated_time,
            1e-3
        ),
        "{feature_time} + {} + {} != {}",
        st.travel_time,
        st.other_time,
        st.estimated_time
    );
    let feature_e: f32 = st.features.iter().map(|f| f.extruded).sum();
    assert!(close(feature_e, st.filament_length, 1e-4));
    let tool_e: f32 = st.filament_per_tool.iter().sum();
    assert!(close(tool_e, st.filament_length, 1e-4));
    let layer_e: f32 = tp.layers.iter().map(|l| l.extruded).sum();
    assert!(close(layer_e, st.filament_length, 1e-4));
    let feature_d: f32 = st.features.iter().map(|f| f.distance).sum();
    assert!(close(feature_d, st.extrude_distance, 1e-4));
    let feature_n: usize = st.features.iter().map(|f| f.segments).sum();
    assert_eq!(feature_n, st.extrude_moves);
    assert_eq!(st.extrude_moves + st.travel_moves, n);
    assert_eq!(st.layer_count, tp.layers.len());
    assert_eq!(
        st.retractions,
        tp.retractions.iter().filter(|r| r.amount < 0.0).count()
    );
    if !tp.layers.is_empty() {
        let layer_time: f32 = tp.layers.iter().map(|l| l.duration).sum();
        assert!(close(layer_time, st.estimated_time, 1e-3));
    }
    // Objects: valid indices, totals match their segments, ranges sorted.
    assert!(tp.objects.len() < usize::from(u16::MAX));
    let mut per_object = vec![0usize; tp.objects.len()];
    for s in &tp.segments {
        if let Some(i) = s.object_index() {
            assert!(i < tp.objects.len(), "segment object {i} out of range");
            per_object[i] += 1;
            assert!(tp.objects[i].contains_line(s.line), "line {}", s.line);
        }
    }
    for (object, count) in tp.objects.iter().zip(per_object) {
        assert_eq!(object.segments, count, "{}", object.name);
        assert!(!object.name.is_empty());
        assert!(object.time.is_finite() && object.extruded.is_finite());
        assert!(object.lines.windows(2).all(|w| w[0].end < w[1].start));
        assert!(object.lines.iter().all(|r| r.start <= r.end));
    }
}

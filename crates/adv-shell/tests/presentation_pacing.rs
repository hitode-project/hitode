//! Pure presentation decisions; no host window or display is opened.
#[path = "../src/frame_pacer.rs"]
mod frame_pacer;
use adv_machine::frame_spin::FramePacer;
use frame_pacer::{Cadence, HostPacer, submission_at};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

// spec: HOST-SES-10
#[test]
fn publication_is_submitted_before_its_next_refresh_without_an_extra_tick() {
    let origin = Instant::now();
    let period = Duration::from_nanos(16_666_667);
    let publication = origin + Duration::from_millis(17);
    let submit = submission_at(publication, origin + period);
    assert_eq!(submit, publication);
    let display = adv_sync::vsync::Vsync::default();
    display.publish(origin, period);
    let (since, refresh) = display.phase(submit).unwrap();
    assert_eq!(submit + refresh - since, origin + period * 2);
    // An early second publication respects the cap, with no display-link gate.
    assert_eq!(
        submission_at(origin + Duration::from_millis(10), origin + period),
        origin + period
    );
    let pacer = HostPacer {
        vsync: Some(Arc::new(display)),
        keys: Arc::default(),
        origin,
    };
    assert!(pacer.until_vsync().is_some());
}

// spec: HOST-SES-11
#[test]
fn requested_rate_follows_publication_cadence_including_dropped_frames() {
    let origin = Instant::now();
    for rate in [30, 60] {
        let mut cadence = Cadence::default();
        assert_eq!(cadence.rate(), 60);
        let period = Duration::from_secs_f64(1.0 / f64::from(rate));
        for serial in [1, 2, 4, 7, 8, 9] {
            cadence.published(u64::from(serial), origin + period * serial);
        }
        assert_eq!(cadence.rate(), rate);
        cadence.published(9, origin + period * 9); // Repaint is not a new publication.
        assert_eq!(cadence.rate(), rate);
        cadence.published(10, origin + Duration::from_secs(2));
        assert_eq!(
            cadence.rate(),
            60,
            "a still screen does not request a low rate"
        );
    }
}

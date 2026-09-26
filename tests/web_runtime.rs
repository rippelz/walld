//! Requires a working, sandboxed Chromium installation.
use std::{
    thread,
    time::{Duration, Instant},
};
use walld::video::VideoDecoder;

#[test]
#[ignore = "launches Chromium; run explicitly on a desktop host"]
fn properties_nested_entrypoint_and_persistent_animation() {
    let project = tempfile::tempdir().unwrap();
    let nested = project.path().join("web # assets");
    std::fs::create_dir(&nested).unwrap();
    std::fs::write(
        project.path().join("project.json"),
        r#"{"general":{"properties":{"enabled":{"type":"bool","value":true}}}}"#,
    )
    .unwrap();
    let entry = nested.join("index.html");
    std::fs::write(
        &entry,
        r#"<!doctype html><html><body style="margin:0;background:red"><script>
    let ready = false, ticks = 0;
    window.wallpaperPropertyListener = {
      applyUserProperties(p) { ready = p.enabled.value; }
    };
    setInterval(() => {
      if (ready) document.body.style.background = `rgb(0,128,${++ticks % 256})`;
    }, 100);
    </script></body></html>"#,
    )
    .unwrap();
    // Two live instances catch accidental reuse of Chromium's profile directory.
    let first = VideoDecoder::start_web(&entry, 10, 640).unwrap();
    let second = VideoDecoder::start_web(&entry, 10, 640).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut colors = [
        std::collections::HashSet::new(),
        std::collections::HashSet::new(),
    ];
    while Instant::now() < deadline {
        for (decoder, seen) in [&first, &second].into_iter().zip(&mut colors) {
            if let Some(frame) = decoder.try_frame() {
                let pixel = &frame.rgba[..4];
                if pixel[0] == 0 && pixel[1] == 128 {
                    seen.insert(pixel[2]);
                }
            }
        }
        if colors.iter().all(|seen| seen.len() >= 4) {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        colors.iter().all(|seen| seen.len() >= 4),
        "properties/animation failed: {colors:?}"
    );
    drop((first, second));
}

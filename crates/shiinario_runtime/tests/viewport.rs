use shiinario_runtime::viewport::ViewportTransform;
use shiinario_scenario::{BinaryVm, Event, PlatformRequest};

#[test]
fn resized_button_hits_use_canvas_coordinates_exactly_once() {
    // The title's Start button is near (600, 296) on its 800 x 600 canvas.
    // Exercise both direct 0456 queries and the 0456 -> 0492 script pattern.
    for (size, pointer) in [
        ([800, 600], [600, 296]),
        ([1600, 1200], [1200, 592]),
        ([1920, 1080], [1320, 532]),
        ([1000, 1200], [750, 595]),
        ([400, 300], [300, 148]),
    ] {
        let transform = ViewportTransform::fit([800, 600], size).unwrap();
        let mut vm = BinaryVm::new(
            "cursor.scn",
            vec![
                0x56, 0x04, 12, 0, 0, 12, 1, 0, 0x92, 0x04, 12, 0, 0, 12, 1, 0,
            ],
        )
        .unwrap();
        for _ in 0..2 {
            let Event::Platform { request, .. } = vm.step().unwrap() else {
                panic!("expected cursor query")
            };
            let point = transform.cursor_reply(&request, pointer).unwrap();
            assert!(
                (599..=600).contains(&point[0]),
                "window {size:?}: {point:?}"
            );
            assert!(
                (295..=296).contains(&point[1]),
                "window {size:?}: {point:?}"
            );
            vm.respond_point(point).unwrap();
        }
    }
}

#[test]
fn resizing_recomputes_a_stationary_pointer_and_keeps_bars_outside_canvas() {
    let query = PlatformRequest::CursorPosition;
    let original = ViewportTransform::fit([800, 600], [800, 600]).unwrap();
    let enlarged = ViewportTransform::fit([800, 600], [1600, 1200]).unwrap();
    assert_eq!(
        original.cursor_reply(&query, [600, 296]).unwrap(),
        [600, 296]
    );
    assert_eq!(
        enlarged.cursor_reply(&query, [600, 296]).unwrap(),
        [300, 148]
    );
    let wide = ViewportTransform::fit([800, 600], [1920, 1080]).unwrap();
    assert!(wide.cursor_reply(&query, [100, 540]).unwrap()[0] < 0);
    assert!(wide.cursor_reply(&query, [1800, 540]).unwrap()[0] > 800);
}

//! `HTTPRedirectHandler`'s loop check, which `Request` carries across the
//! attempts that reuse one urllib `Request`.
use tax_radar_au::fadden::http::Request;

#[test]
fn a_url_is_followed_4_times_then_refused_across_attempts() {
    let mut request = Request::default();
    // urllib's first redirect is unchecked, then each URL may repeat 4 times.
    assert!((0..4).all(|_| request.follow("http://h/self")));
    assert!(!request.follow("http://h/self"));
    // The next attempt with the same request is refused at once.
    assert!(!request.follow("http://h/self"));
    assert!(request.follow("http://h/other"));
}

#[test]
fn ten_urls_are_followed_then_refused() {
    let mut request = Request::default();
    assert!((0..10).all(|n| request.follow(&format!("http://h/{n}"))));
    assert!(!request.follow("http://h/10"));
    assert!(!request.follow("http://h/0"));
}

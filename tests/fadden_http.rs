//! `HTTPRedirectHandler`'s loop check, which `Redirects` carries across the
//! attempts that reuse one urllib `Request`.
use tax_radar_au::fadden::http::Redirects;

#[test]
fn a_url_is_followed_4_times_then_refused_across_attempts() {
    let mut redirects = Redirects::default();
    // urllib's first redirect is unchecked, then each URL may repeat 4 times.
    assert!((0..4).all(|_| redirects.follow("http://h/self")));
    assert!(!redirects.follow("http://h/self"));
    // The next attempt with the same request is refused at once.
    assert!(!redirects.follow("http://h/self"));
    assert!(redirects.follow("http://h/other"));
}

#[test]
fn ten_urls_are_followed_then_refused() {
    let mut redirects = Redirects::default();
    assert!((0..10).all(|n| redirects.follow(&format!("http://h/{n}"))));
    assert!(!redirects.follow("http://h/10"));
    assert!(!redirects.follow("http://h/0"));
}

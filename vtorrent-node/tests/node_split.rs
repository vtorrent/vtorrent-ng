#[allow(clippy::assertions_on_constants)]
#[test]
fn node_modules_importable() {
    use vtorrent_node::node::chain::handle_block;
    use vtorrent_node::node::p2p::handle_peer_event;
    let _ = handle_block;
    let _ = handle_peer_event;
    assert!(true);
}

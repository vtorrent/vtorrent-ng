use bitcoin::{
    block::{Header, Version},
    consensus::serialize,
    hashes::Hash,
    BlockHash, Network, TxMerkleNode,
};
use vtorrent_btc::headers::HeaderChain;

#[test]
fn audit_retained_floor_is_scannable() {
    let mut chain = HeaderChain::anchored(Network::Regtest);
    for height in 1..=6 {
        let parent = chain.get(&chain.best_hash().unwrap()).unwrap();
        let mut header = Header {
            version: Version::ONE,
            prev_blockhash: BlockHash::from_byte_array(chain.best_hash().unwrap()),
            merkle_root: TxMerkleNode::all_zeros(),
            time: parent.header.time + 1,
            bits: parent.header.bits,
            nonce: 0,
        };
        while header.validate_pow(header.target()).is_err() {
            header.nonce += 1;
        }
        chain.add_header(&serialize(&header), height).unwrap();
    }
    chain.set_retention(2);
    assert_eq!(
        chain.hashes_from(4).len(),
        3,
        "retained heights 4, 5, 6 must remain scannable"
    );
}

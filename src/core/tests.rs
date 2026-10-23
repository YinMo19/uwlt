// Public test vectors, never use this key for real funds.
// Sources (Apache-2.0, Copyright Trust Wallet):
// https://github.com/trustwallet/wallet-core/blob/4.8.4/tests/chains/Tron/AddressTests.cpp
// https://github.com/trustwallet/wallet-core/blob/4.8.4/tests/chains/Tron/SignerTests.cpp
// AddressTests.FromPublicKey and SignerTests.SignRawJsonTransferContract.
use super::*;
use serde_json::json;

const ADDRESS: &str = "TJRyWwFs9wTFGZg3JbrVriFbNfCug5tDeC";
const TXID: &str = "dc6f6d9325ee44ab3c00528472be16e1572ab076aa161ccd12515029869d0451";
const SIGNATURE: &str = "ede769f6df28aefe6a846be169958c155e23e7e5c9621d2e8dce1719b4d952b63e8a8bf9f00e41204ac1bf69b1a663dacdf764367e48e4a5afcd6b055a747fb200";

fn public_test_key() -> Zeroizing<[u8; 32]> {
    let mut key = Zeroizing::new([0; 32]);
    hex::decode_to_slice(
        "2d8f68944bdbfbc0769542fba8fc2d2a3de67393334471624364c7006da2aa54",
        &mut *key,
    )
    .unwrap();
    key
}

fn unsigned() -> Value {
    json!({
        "raw_data": {
            "contract": [{
                "parameter": {
                    "type_url": "type.googleapis.com/protocol.TransferContract",
                    "value": {
                        "amount": 2000000,
                        "owner_address": "415cd0fb0ab3ce40f3051414c604b27756e69e43db",
                        "to_address": "41521ea197907927725ef36d70f25f850d1659c7c7"
                    }
                },
                "type": "TransferContract"
            }],
            "expiration": 1539331479000_u64,
            "ref_block_bytes": "7b3b",
            "ref_block_hash": "b21ace8d6ac20e7e",
            "timestamp": 1539295479000_u64
        },
        "raw_data_hex": "0a027b3b2208b21ace8d6ac20e7e40d8abb9bae62c5a67080112630a2d747970652e676f6f676c65617069732e636f6d2f70726f746f636f6c2e5472616e73666572436f6e747261637412320a15415cd0fb0ab3ce40f3051414c604b27756e69e43db121541521ea197907927725ef36d70f25f850d1659c7c71880897a70d889a4a9e62c",
        "txID": TXID
    })
}

#[test]
fn official_private_key_derives_expected_address() {
    assert_eq!(address(&public_test_key()).unwrap(), ADDRESS);
    assert!(address(&[0; 32]).is_err());
    assert!(address(&[255; 32]).is_err());
}

#[test]
fn official_transaction_matches_expected_id_and_signature() {
    let signed = sign(&public_test_key(), &unsigned()).unwrap();
    assert_eq!(signed["txID"], TXID);
    assert_eq!(signed["signature"], json!([SIGNATURE]));
    assert_eq!(signed["raw_data"], unsigned()["raw_data"]);
}

#[test]
fn native_signer_rejects_inconsistent_raw_bytes_json_and_id() {
    let key = public_test_key();
    let mut raw_bytes = unsigned();
    raw_bytes["raw_data_hex"] = json!("00");
    assert!(sign(&key, &raw_bytes).is_err());

    let mut raw_json = unsigned();
    raw_json["raw_data"]["contract"][0]["parameter"]["value"]["amount"] = json!(3000000);
    assert!(sign(&key, &raw_json).is_err());

    let mut wrong_id = unsigned();
    wrong_id["txID"] = json!("00".repeat(32));
    assert!(sign(&key, &wrong_id).is_err());
}

#[test]
fn solana_uses_rfc8032_ed25519_not_secp256k1_or_prehashing() {
    // RFC 8032 section 7.1, TEST 1 (public test seed; never fund it).
    let key: [u8; 32] =
        hex::decode("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60")
            .unwrap()
            .try_into()
            .unwrap();
    let public = bs58::decode(solana_address(&key).unwrap())
        .into_vec()
        .unwrap();
    assert_eq!(
        hex::encode(public),
        "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
    );
    assert_eq!(
        hex::encode(solana_sign(&key, b"").unwrap()),
        concat!(
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e06522490155",
            "5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"
        )
    );
}

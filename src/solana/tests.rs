use super::*;
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    thread,
};

fn rpc_server(steps: Vec<(&'static str, Value)>) -> (String, thread::JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let worker = thread::spawn(move || {
        let mut requests = Vec::new();
        for (method, result) in steps {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut length = None;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        length = Some(value.trim().parse::<usize>().unwrap());
                    }
                }
            }
            let mut body = vec![0; length.unwrap()];
            reader.read_exact(&mut body).unwrap();
            let request: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(request["method"], method);
            requests.push(request);
            let response = json!({"jsonrpc":"2.0","id":1,"result":result}).to_string();
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",response.len(),response).unwrap();
        }
        requests
    });
    (endpoint, worker)
}
fn account_json(owner: Pubkey, data: &[u8], lamports: u64) -> Value {
    json!({"owner":owner.to_string(),"data":[STANDARD.encode(data),"base64"],"lamports":lamports,"executable":false})
}
fn token_data(owner: Pubkey, mint: Pubkey, amount: u64) -> Vec<u8> {
    let token = TokenAccount {
        owner,
        mint,
        amount,
        delegate: COption::None,
        state: AccountState::Initialized,
        is_native: COption::None,
        delegated_amount: 0,
        close_authority: COption::None,
    };
    let mut bytes = vec![0; TokenAccount::LEN];
    TokenAccount::pack(token, &mut bytes).unwrap();
    bytes
}
fn latest() -> Value {
    json!({"value":{"blockhash":Hash::new_from_array([8;32]).to_string(),"lastValidBlockHeight":100}})
}

#[test]
fn wrong_cluster_is_rejected_before_queries() {
    let (endpoint, worker) = rpc_server(vec![(
        "getGenesisHash",
        json!(Hash::new_from_array([9; 32]).to_string()),
    )]);
    assert!(
        Client::new(Some(&endpoint))
            .err()
            .unwrap()
            .to_string()
            .contains("genesis hash does not match mainnet")
    );
    assert_eq!(worker.join().unwrap().len(), 1);
}

#[test]
fn split_token_balance_is_spent_with_checked_transfers_and_rent() {
    let sender = address(&crate::core::solana_address(&[7; 32]).unwrap()).unwrap();
    let recipient = address(&crate::core::solana_address(&[8; 32]).unwrap()).unwrap();
    let mint = address(USDT).unwrap();
    let mut mint_data = vec![0; Mint::LEN];
    Mint::pack(
        Mint {
            mint_authority: COption::None,
            supply: 10,
            decimals: 6,
            is_initialized: true,
            freeze_authority: COption::None,
        },
        &mut mint_data,
    )
    .unwrap();
    let accounts = [(Pubkey::new_from_array([1;32]),3u64),(Pubkey::new_from_array([2;32]),4u64)].map(|(key,amount)| json!({"pubkey":key.to_string(),"account":account_json(spl_token::id(),&token_data(sender,mint,amount),2_000_000)}));
    let (endpoint, worker) = rpc_server(vec![
        ("getGenesisHash", json!(MAINNET_GENESIS)),
        (
            "getAccountInfo",
            json!({"value":account_json(system_program::id(),&[],10_000_000)}),
        ),
        (
            "getAccountInfo",
            json!({"value":account_json(spl_token::id(),&mint_data,1_000_000)}),
        ),
        ("getAccountInfo", json!({"value":null})),
        ("getMinimumBalanceForRentExemption", json!(2_039_280)),
        ("getTokenAccountsByOwner", json!({"value":accounts})),
        ("getLatestBlockhash", latest()),
        ("getFeeForMessage", json!({"value":5000})),
        ("getBalance", json!({"value":10_000_000})),
        ("simulateTransaction", json!({"value":{"err":null}})),
    ]);
    let client = Client::new(Some(&endpoint)).unwrap();
    let prepared = client
        .prepare(
            &sender.to_string(),
            &recipient.to_string(),
            "0.000005",
            Asset::Usdt,
        )
        .unwrap();
    assert_eq!(prepared.rent, "0.00203928");
    assert_eq!(prepared.fee, "0.000005");
    let requests = worker.join().unwrap();
    let simulated =
        decode_transaction(requests.last().unwrap()["params"][0].as_str().unwrap()).unwrap();
    let instructions = simulated.message.instructions();
    assert_eq!(instructions.len(), 3);
    assert_eq!(instructions[0].data, [1]);
    assert_eq!(instructions[1].data[0], 12);
    assert_eq!(
        u64::from_le_bytes(instructions[1].data[1..9].try_into().unwrap()),
        3
    );
    assert_eq!(
        u64::from_le_bytes(instructions[2].data[1..9].try_into().unwrap()),
        2
    );
    assert_eq!(
        requests.last().unwrap()["params"][1]["replaceRecentBlockhash"],
        false
    );
}

fn signed_fixture() -> Signed {
    let key = [7; 32];
    let sender = address(&crate::core::solana_address(&key).unwrap()).unwrap();
    let message = VersionedMessage::Legacy(Message::new_with_blockhash(
        &[system_instruction::transfer(
            &sender,
            &Pubkey::new_from_array([3; 32]),
            42,
        )],
        Some(&sender),
        &Hash::new_from_array([8; 32]),
    ));
    let prepared = Prepared {
        message,
        sender,
        last_valid_block_height: Some(100),
        expires_at: now().unwrap() + 90,
        from: sender.to_string(),
        to: String::new(),
        amount: String::new(),
        symbol: "SOL".to_owned(),
        fee: String::new(),
        rent: String::new(),
    };
    sign(&prepared, &key).unwrap()
}
#[test]
fn expiry_after_authorization_never_broadcasts() {
    let signed = signed_fixture();
    let (endpoint, worker) = rpc_server(vec![
        ("getGenesisHash", json!(MAINNET_GENESIS)),
        ("getGenesisHash", json!(MAINNET_GENESIS)),
        ("getBlockHeight", json!(101)),
    ]);
    let client = Client::new(Some(&endpoint)).unwrap();
    assert!(
        client
            .broadcast(&signed)
            .unwrap_err()
            .to_string()
            .contains("expired")
    );
    assert_eq!(worker.join().unwrap().len(), 3);
}
#[test]
fn broadcast_mismatch_is_single_attempt_with_recovery_signature() {
    let signed = signed_fixture();
    let (endpoint, worker) = rpc_server(vec![
        ("getGenesisHash", json!(MAINNET_GENESIS)),
        ("getGenesisHash", json!(MAINNET_GENESIS)),
        ("getBlockHeight", json!(99)),
        ("isBlockhashValid", json!({"value":true})),
        ("simulateTransaction", json!({"value":{"err":null}})),
        ("sendTransaction", json!(Signature::default().to_string())),
    ]);
    let client = Client::new(Some(&endpoint)).unwrap();
    let error = client.broadcast(&signed).unwrap_err().to_string();
    assert!(error.contains(&signed.txid) && error.contains("uncertain"));
    let requests = worker.join().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "sendTransaction")
            .count(),
        1
    );
    assert_eq!(requests.last().unwrap()["params"][1]["maxRetries"], 0);
}

#[test]
fn balance_uses_official_mainnet_usdt() {
    let owner = address(&crate::core::solana_address(&[7; 32]).unwrap()).unwrap();
    let mint = address(USDT).unwrap();
    let mut mint_data = vec![0; Mint::LEN];
    Mint::pack(
        Mint {
            mint_authority: COption::None,
            supply: 10_000_000,
            decimals: 6,
            is_initialized: true,
            freeze_authority: COption::None,
        },
        &mut mint_data,
    )
    .unwrap();
    let accounts = [(1, 1_250_000), (2, 2_000_000)].map(|(key, amount)| {
        json!({
            "pubkey": Pubkey::new_from_array([key; 32]).to_string(),
            "account": account_json(spl_token::id(), &token_data(owner, mint, amount), 2_000_000),
        })
    });
    let (endpoint, worker) = rpc_server(vec![
        ("getGenesisHash", json!(MAINNET_GENESIS)),
        ("getBalance", json!({"value":1_500_000_000})),
        (
            "getAccountInfo",
            json!({"value":account_json(spl_token::id(),&mint_data,1_000_000)}),
        ),
        ("getTokenAccountsByOwner", json!({"value":accounts})),
    ]);
    let client = Client::new(Some(&endpoint)).unwrap();
    let balance = client.balance(&owner.to_string()).unwrap();
    assert_eq!(balance.native, "1.5");
    assert_eq!(balance.usdt, "3.25");
    let requests = worker.join().unwrap();
    assert_eq!(requests[2]["params"][0], USDT);
    assert_eq!(requests[3]["params"][1]["mint"], USDT);
}

#[test]
fn changed_genesis_never_broadcasts() {
    let signed = signed_fixture();
    let (endpoint, worker) = rpc_server(vec![
        ("getGenesisHash", json!(MAINNET_GENESIS)),
        (
            "getGenesisHash",
            json!(Hash::new_from_array([9; 32]).to_string()),
        ),
    ]);
    let client = Client::new(Some(&endpoint)).unwrap();
    assert!(
        client
            .broadcast(&signed)
            .unwrap_err()
            .to_string()
            .contains("genesis hash does not match mainnet")
    );
    assert_eq!(worker.join().unwrap().len(), 2);
}

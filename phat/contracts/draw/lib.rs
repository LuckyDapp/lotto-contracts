#![cfg_attr(not(feature = "std"), no_std, no_main)]

extern crate alloc;
extern crate core;

#[ink::contract(env = pink_extension::PinkEnvironment)]
mod lotto_draw {
    use crate::lotto_draw::Request::{CheckWinners, DrawNumbers};
    use alloc::vec::Vec;
    use ink::prelude::{format, string::String};
    use phat_offchain_rollup::clients::ink::{Action, ContractId, InkRollupClient};
    use pink_extension::chain_extension::signing;
    use pink_extension::{error, http_post, info, vrf, ResultExt};
    use scale::{Decode, Encode};
    use serde::Deserialize;
    use serde_json_core;
    use sp_core::crypto::{AccountId32, Ss58AddressFormatRegistry, Ss58Codec};

    const REQUEST_TYPE_DRAW_NUMBERS: u8 = 10;
    const REQUEST_TYPE_CHECK_WINNERS: u8 = 11;

    /// Type of response when the offchain rollup communicates with this contract
    const RESPONSE_TYPE_ERROR: u8 = 0;
    const RESPONSE_TYPE_DRAW_NUMBERS: u8 = 10;
    const RESPONSE_TYPE_CHECK_WINNERS: u8 = 10;

    pub type NUMBER = u16;

    /// Message to request the lotto draw
    /// message pushed in the queue by the Ink! smart contract and read by the offchain rollup
    #[derive(Eq, PartialEq, Clone, scale::Encode, scale::Decode)]
    #[cfg_attr(
        feature = "std",
        derive(scale_info::TypeInfo, ink::storage::traits::StorageLayout)
    )]
    pub struct LottoDrawRequestMessage {
        /// Type of request
        request_type: u8,
        /// id of the requestor
        requestor_id: AccountId,
        /// draw number
        draw_num: u32,
        /// request
        request: Request,
    }

    #[derive(Eq, PartialEq, Clone, scale::Encode, scale::Decode)]
    #[cfg_attr(
        feature = "std",
        derive(scale_info::TypeInfo, ink::storage::traits::StorageLayout)
    )]
    pub enum Request {
        /// request to draw the n number between min and max values
        /// arg1: number of numbers for the draw
        /// arg2:  smallest number for the draw
        /// arg2:  biggest number for the draw
        DrawNumbers(u8, NUMBER, NUMBER),
        /// request to check if there is a winner for the given numbers
        CheckWinners(Vec<NUMBER>),
    }

    /// Message sent to provide a random value
    /// response pushed in the queue by the offchain rollup and read by the Ink! smart contract
    #[derive(Encode, Decode)]
    struct LottoDrawResponseMessage {
        /// Type of response
        resp_type: u8,
        /// initial request
        request: LottoDrawRequestMessage,
        /// numbers of the draw
        numbers: Option<Vec<NUMBER>>,
        /// winners
        winners: Option<Vec<AccountId>>,
        /// when an error occurs
        error: Option<Vec<u8>>,
    }

    /// DTO use for serializing and deserializing the json
    #[derive(Deserialize, Encode, Clone, Debug, PartialEq)]
    pub struct IndexerResponse<'a> {
        #[serde(borrow)]
        data: IndexerResponseData<'a>,
    }

    #[derive(Deserialize, Encode, Clone, Debug, PartialEq)]
    #[allow(non_snake_case)]
    struct IndexerResponseData<'a> {
        #[serde(borrow)]
        participations: Participations<'a>,
    }

    #[derive(Deserialize, Encode, Clone, Debug, PartialEq)]
    struct Participations<'a> {
        #[serde(borrow)]
        nodes: Vec<ParticipationNode<'a>>,
    }

    #[derive(Deserialize, Encode, Clone, Debug, PartialEq)]
    struct ParticipationNode<'a> {
        accountId: &'a str,
    }

    #[ink(storage)]
    pub struct Lotto {
        owner: AccountId,
        /// config to send the data to the ink! smart contract
        consumer_config: Option<Config>,
        /// indexer endpoint
        indexer_url: Option<String>,
        /// Key for signing the rollup tx.
        attest_key: [u8; 32],
    }

    #[derive(Encode, Decode, Debug)]
    #[cfg_attr(
        feature = "std",
        derive(scale_info::TypeInfo, ink::storage::traits::StorageLayout)
    )]
    struct Config {
        /// The RPC endpoint of the target blockchain
        rpc: String,
        pallet_id: u8,
        call_id: u8,
        /// The rollup anchor address on the target blockchain
        contract_id: ContractId,
        /// Key for sending out the rollup meta-tx. None to fallback to the wallet based auth.
        sender_key: Option<[u8; 32]>,
    }

    #[derive(Encode, Decode, Debug)]
    #[repr(u8)]
    #[cfg_attr(feature = "std", derive(scale_info::TypeInfo))]
    pub enum ContractError {
        BadOrigin,
        ClientNotConfigured,
        InvalidKeyLength,
        InvalidAddressLength,
        NoRequestInQueue,
        FailedToCreateClient,
        FailedToCommitTx,
        HttpRequestFailed,
        IndexerNotConfigured,
        InvalidResponseBody,

        FailedToGetStorage,
        FailedToCreateTransaction,
        FailedToSendTransaction,
        FailedToGetBlockHash,
        FailedToDecode,
        InvalidRequest,
        FailedToCallRollup,

        MinGreaterThanMax,
        DivByZero,
        MulOverFlow,
        AddOverFlow,
        SubOverFlow,
    }

    type Result<T> = core::result::Result<T, ContractError>;

    impl From<phat_offchain_rollup::Error> for ContractError {
        fn from(error: phat_offchain_rollup::Error) -> Self {
            error!("error in the rollup: {:?}", error);
            ContractError::FailedToCallRollup
        }
    }

    impl Lotto {
        #[ink(constructor)]
        pub fn default() -> Self {
            const NONCE: &[u8] = b"lotto";
            let private_key = signing::derive_sr25519_key(NONCE);

            Self {
                owner: Self::env().caller(),
                attest_key: private_key[..32].try_into().expect("Invalid Key Length"),
                consumer_config: None,
                indexer_url: None,
            }
        }

        /// Gets the owner of the contract
        #[ink(message)]
        pub fn owner(&self) -> AccountId {
            self.owner
        }

        /// Gets the attestor address used by this rollup
        #[ink(message)]
        pub fn get_attest_address(&self) -> Vec<u8> {
            signing::get_public_key(&self.attest_key, signing::SigType::Sr25519)
        }

        /// Gets the ecdsa address used by this rollup in the meta transaction
        #[ink(message)]
        pub fn get_attest_ecdsa_address(&self) -> Vec<u8> {
            use ink::env::hash;
            let input = signing::get_public_key(&self.attest_key, signing::SigType::Ecdsa);
            let mut output = <hash::Blake2x256 as hash::HashOutput>::Type::default();
            ink::env::hash_bytes::<hash::Blake2x256>(&input, &mut output);
            output.to_vec()
        }

        /// Set attestor key.
        ///
        /// For dev purpose. (admin only)
        #[ink(message)]
        pub fn set_attest_key(&mut self, attest_key: Option<Vec<u8>>) -> Result<()> {
            self.ensure_owner()?;
            self.attest_key = match attest_key {
                Some(key) => key.try_into().or(Err(ContractError::InvalidKeyLength))?,
                None => {
                    const NONCE: &[u8] = b"lotto";
                    let private_key = signing::derive_sr25519_key(NONCE);
                    private_key[..32]
                        .try_into()
                        .or(Err(ContractError::InvalidKeyLength))?
                }
            };
            Ok(())
        }

        /// Gets the sender address used by this rollup (in case of meta-transaction)
        #[ink(message)]
        pub fn get_sender_address(&self) -> Option<Vec<u8>> {
            if let Some(Some(sender_key)) =
                self.consumer_config.as_ref().map(|c| c.sender_key.as_ref())
            {
                let sender_key = signing::get_public_key(sender_key, signing::SigType::Sr25519);
                Some(sender_key)
            } else {
                None
            }
        }

        /// Gets the config of the target consumer contract
        #[ink(message)]
        pub fn get_target_contract(&self) -> Option<(String, u8, u8, ContractId)> {
            self.consumer_config
                .as_ref()
                .map(|c| (c.rpc.clone(), c.pallet_id, c.call_id, c.contract_id))
        }

        /// Configures the target consumer contract (admin only)
        #[ink(message)]
        pub fn config_target_contract(
            &mut self,
            rpc: String,
            pallet_id: u8,
            call_id: u8,
            contract_id: Vec<u8>,
            sender_key: Option<Vec<u8>>,
        ) -> Result<()> {
            self.ensure_owner()?;
            self.consumer_config = Some(Config {
                rpc,
                pallet_id,
                call_id,
                contract_id: contract_id
                    .try_into()
                    .or(Err(ContractError::InvalidAddressLength))?,
                sender_key: match sender_key {
                    Some(key) => Some(key.try_into().or(Err(ContractError::InvalidKeyLength))?),
                    None => None,
                },
            });
            Ok(())
        }

        /// Gets the config to target the indexer
        #[ink(message)]
        pub fn get_indexer_url(&self) -> Option<String> {
            self.indexer_url.clone()
        }

        /// Configures the indexer (admin only)
        #[ink(message)]
        pub fn config_indexer(&mut self, indexer_url: String) -> Result<()> {
            self.ensure_owner()?;
            self.indexer_url = Some(indexer_url);
            Ok(())
        }

        /// Transfers the ownership of the contract (admin only)
        #[ink(message)]
        pub fn transfer_ownership(&mut self, new_owner: AccountId) -> Result<()> {
            self.ensure_owner()?;
            self.owner = new_owner;
            Ok(())
        }

        /// Processes a request by a rollup transaction
        #[ink(message)]
        pub fn answer_request(&self) -> Result<Option<Vec<u8>>> {
            let config = self.ensure_client_configured()?;
            let mut client = connect(config)?;

            // Get a request if presents
            let request: LottoDrawRequestMessage = client
                .pop()
                .log_err("answer_request: failed to read queue")?
                .ok_or(ContractError::NoRequestInQueue)?;

            let response = self.handle_request(request)?;
            // Attach an action to the tx by:
            client.action(Action::Reply(response.encode()));

            maybe_submit_tx(client, &self.attest_key, config.sender_key.as_ref())
        }

        fn handle_request(
            &self,
            message: LottoDrawRequestMessage,
        ) -> Result<LottoDrawResponseMessage> {
            let response = match message.request {
                DrawNumbers(nb_numbers, smallest_number, biggest_number) => {
                    let result = self.inner_get_numbers(
                        message.requestor_id,
                        message.draw_num,
                        nb_numbers,
                        smallest_number,
                        biggest_number,
                    );
                    match result {
                        Ok(numbers) => LottoDrawResponseMessage {
                            resp_type: RESPONSE_TYPE_DRAW_NUMBERS,
                            request: message,
                            numbers: Some(numbers),
                            winners: None,
                            error: None,
                        },
                        Err(e) => LottoDrawResponseMessage {
                            resp_type: RESPONSE_TYPE_ERROR,
                            request: message,
                            numbers: None,
                            winners: None,
                            error: Some(e.encode()),
                        },
                    }
                }
                CheckWinners(ref numbers) => {
                    let result = self.inner_get_winners(message.draw_num, numbers);
                    match result {
                        Ok(winners) => LottoDrawResponseMessage {
                            resp_type: RESPONSE_TYPE_CHECK_WINNERS,
                            request: message,
                            numbers: None,
                            winners: Some(winners),
                            error: None,
                        },
                        Err(e) => LottoDrawResponseMessage {
                            resp_type: RESPONSE_TYPE_ERROR,
                            request: message,
                            numbers: None,
                            winners: None,
                            error: Some(e.encode()),
                        },
                    }
                }
            };

            Ok(response)
        }

        /// Simulate and return numbers for the draw (admin only - for dev purpose)
        #[ink(message)]
        pub fn get_numbers(
            &self,
            requestor_id: AccountId,
            draw_num: u32,
            nb_numbers: u8,
            smallest_number: NUMBER,
            biggest_number: NUMBER,
        ) -> Result<Vec<NUMBER>> {
            self.ensure_owner()?;
            self.inner_get_numbers(
                requestor_id,
                draw_num,
                nb_numbers,
                smallest_number,
                biggest_number,
            )
        }

        fn inner_get_numbers(
            &self,
            requestor_id: AccountId,
            draw_num: u32,
            nb_numbers: u8,
            smallest_number: NUMBER,
            biggest_number: NUMBER,
        ) -> Result<Vec<NUMBER>> {
            info!(
                "Request received from requestor {requestor_id:?} / {draw_num} - draw {nb_numbers} numbers between {smallest_number} and {biggest_number}"
            );

            if smallest_number > biggest_number {
                return Err(ContractError::MinGreaterThanMax);
            }

            // build a common salt for this draw
            let mut salt_requestor: Vec<u8> = Vec::new();
            salt_requestor.extend_from_slice(&draw_num.to_be_bytes());
            salt_requestor.extend_from_slice(requestor_id.as_ref());

            let mut numbers = Vec::new();
            let mut i: u8 = 0;

            while numbers.len() < nb_numbers as usize {
                // build a salt for this draw number
                let mut salt: Vec<u8> = Vec::new();
                salt.extend_from_slice(&i.to_be_bytes());
                salt.extend_from_slice(salt_requestor.as_ref());

                // draw the number
                let number = self.inner_get_number(salt, smallest_number, biggest_number)?;
                // check if the number has already been drawn
                if !numbers.iter().any(|&n| n == number) {
                    // the number has not been drawn yet => we added it
                    numbers.push(number);
                }
                i += 1;
            }

            Ok(numbers)
        }

        fn inner_get_number(&self, salt: Vec<u8>, min: NUMBER, max: NUMBER) -> Result<NUMBER> {
            let output = vrf(&salt);
            // keep only 8 bytes to compute the random u64
            let mut arr = [0x00; 8];
            arr.copy_from_slice(&output[0..8]);
            let rand_u64 = u64::from_le_bytes(arr);

            // r = rand_u64() % (max - min + 1) + min
            // use u128 because (max - min + 1) can be equal to (U64::MAX - 0 + 1)
            let a = (max as u128)
                .checked_sub(min as u128)
                .ok_or(ContractError::SubOverFlow)?
                .checked_add(1u128)
                .ok_or(ContractError::AddOverFlow)?;
            let b = (rand_u64 as u128) % a;
            let r = b
                .checked_add(min as u128)
                .ok_or(ContractError::AddOverFlow)?;

            Ok(r as NUMBER)
        }

        /// Simulate and return the winners (admin only - for dev purpose)
        #[ink(message)]
        pub fn get_winners(&self, draw_num: u32, numbers: Vec<NUMBER>) -> Result<Vec<AccountId>> {
            self.ensure_owner()?;
            self.inner_get_winners(draw_num, &numbers)
        }

        fn inner_get_winners(
            &self,
            draw_num: u32,
            numbers: &Vec<NUMBER>,
        ) -> Result<Vec<AccountId>> {
            info!(
                "Request received to get the winners for draw {draw_num} and numbers {numbers:?} "
            );

            // check if the endpoint is configured
            let indexer_endpoint = self.ensure_indexer_configured()?;

            // build the headers
            let headers = alloc::vec![
                ("Content-Type".into(), "application/json".into()),
                ("Accept".into(), "application/json".into())
            ];
            // build the filter
            let mut filter = format!(
                r#"filter:{{and:[{{numRaffle:{{equalTo:\"{}\"}}}}"#,
                draw_num
            );
            for n in numbers {
                let f = format!(r#",{{numbers:{{contains:\"{}\"}}}}"#, n);
                filter.push_str(&f);
            }
            filter.push_str("]}");

            // build the body
            let body = format!(
                r#"{{"query" : "{{participations({}){{ nodes {{ accountId }} }} }}"}}"#,
                filter
            );
            ink::env::debug_println!("body: {}", body);

            // query the indexer
            let resp = http_post!(indexer_endpoint, body, headers);

            // check the result
            if resp.status_code != 200 {
                ink::env::debug_println!("status code {}", resp.status_code);
                return Err(ContractError::HttpRequestFailed);
            }

            // parse the result
            let result: IndexerResponse = serde_json_core::from_slice(resp.body.as_slice())
                .or(Err(ContractError::InvalidResponseBody))?
                .0;

            // add the winners
            let mut winners = Vec::new();
            for w in result.data.participations.nodes.iter() {
                // build the accountId from the string address
                let account_id =
                    AccountId32::from_ss58check(w.accountId).expect("incorrect address");
                let address_hex: [u8; 32] = scale::Encode::encode(&account_id)
                    .try_into()
                    .expect("incorrect length");
                winners.push(AccountId::from(address_hex));
            }

            Ok(winners)
        }

        /// Returns BadOrigin error if the caller is not the owner
        fn ensure_owner(&self) -> Result<()> {
            if self.env().caller() == self.owner {
                Ok(())
            } else {
                Err(ContractError::BadOrigin)
            }
        }

        /// Returns the config reference or raise the error `ClientNotConfigured`
        fn ensure_client_configured(&self) -> Result<&Config> {
            self.consumer_config
                .as_ref()
                .ok_or(ContractError::ClientNotConfigured)
        }

        fn ensure_indexer_configured(&self) -> Result<&String> {
            self.indexer_url
                .as_ref()
                .ok_or(ContractError::IndexerNotConfigured)
        }
    }

    fn connect(config: &Config) -> Result<InkRollupClient> {
        let result = InkRollupClient::new(
            &config.rpc,
            config.pallet_id,
            config.call_id,
            &config.contract_id,
        )
        .log_err("failed to create rollup client");

        match result {
            Ok(client) => Ok(client),
            Err(e) => {
                error!("Error : {:?}", e);
                Err(ContractError::FailedToCreateClient)
            }
        }
    }

    fn maybe_submit_tx(
        client: InkRollupClient,
        attest_key: &[u8; 32],
        sender_key: Option<&[u8; 32]>,
    ) -> Result<Option<Vec<u8>>> {
        let maybe_submittable = client
            .commit()
            .log_err("failed to commit")
            .map_err(|_| ContractError::FailedToCommitTx)?;

        if let Some(submittable) = maybe_submittable {
            let tx_id = if let Some(sender_key) = sender_key {
                // Prefer to meta-tx
                submittable
                    .submit_meta_tx(attest_key, sender_key)
                    .log_err("failed to submit rollup meta-tx")?
            } else {
                // Fallback to account-based authentication
                submittable
                    .submit(attest_key)
                    .log_err("failed to submit rollup tx")?
            };
            return Ok(Some(tx_id));
        }
        Ok(None)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use ink::env::debug_println;
        use ink::primitives::AccountId;

        struct EnvVars {
            /// The RPC endpoint of the target blockchain
            rpc: String,
            pallet_id: u8,
            call_id: u8,
            /// The rollup anchor address on the target blockchain
            contract_id: ContractId,
            /// When we want to manually set the attestor key for signing the message (only dev purpose)
            attest_key: Vec<u8>,
            /// When we want to use meta tx
            sender_key: Option<Vec<u8>>,
        }

        fn get_env(key: &str) -> String {
            std::env::var(key).expect("env not found")
        }

        fn config() -> EnvVars {
            dotenvy::dotenv().ok();
            let rpc = get_env("RPC");
            let pallet_id: u8 = get_env("PALLET_ID").parse().expect("u8 expected");
            let call_id: u8 = get_env("CALL_ID").parse().expect("u8 expected");
            let contract_id: ContractId = hex::decode(get_env("CONTRACT_ID"))
                .expect("hex decode failed")
                .try_into()
                .expect("incorrect length");
            let attest_key = hex::decode(get_env("ATTEST_KEY")).expect("hex decode failed");
            let sender_key = std::env::var("SENDER_KEY")
                .map(|s| hex::decode(s).expect("hex decode failed"))
                .ok();

            EnvVars {
                rpc: rpc.to_string(),
                pallet_id,
                call_id,
                contract_id: contract_id.into(),
                attest_key,
                sender_key,
            }
        }

        #[ink::test]
        fn test_update_attestor_key() {
            let _ = env_logger::try_init();
            pink_extension_runtime::mock_ext::mock_all_ext();

            let mut lotto = Lotto::default();

            // Secret key and address of Alice in localhost
            let sk_alice: [u8; 32] = [0x01; 32];
            let address_alice = hex_literal::hex!(
                "189dac29296d31814dc8c56cf3d36a0543372bba7538fa322a4aebfebc39e056"
            );

            let initial_attestor_address = lotto.get_attest_address();
            assert_ne!(address_alice, initial_attestor_address.as_slice());

            lotto.set_attest_key(Some(sk_alice.into())).unwrap();

            let attestor_address = lotto.get_attest_address();
            assert_eq!(address_alice, attestor_address.as_slice());

            lotto.set_attest_key(None).unwrap();

            let attestor_address = lotto.get_attest_address();
            assert_eq!(initial_attestor_address, attestor_address);
        }

        fn init_contract() -> Lotto {
            let EnvVars {
                rpc,
                pallet_id,
                call_id,
                contract_id,
                attest_key,
                sender_key,
            } = config();

            let mut lotto = Lotto::default();
            lotto
                .config_target_contract(rpc, pallet_id, call_id, contract_id.into(), sender_key)
                .unwrap();

            lotto
                .config_indexer("https://query.substrate.fi/lotto-subquery-shibuya".to_string())
                .unwrap();
            lotto.set_attest_key(Some(attest_key)).unwrap();

            lotto
        }

        #[ink::test]
        fn test_get_numbers() {
            let _ = env_logger::try_init();
            pink_extension_runtime::mock_ext::mock_all_ext();

            let lotto = init_contract();

            let requestor_id = AccountId::try_from(*&[1u8; 32]).unwrap();
            let draw_num = 1;
            let nb_numbers = 5;
            let smallest_number = 1;
            let biggest_number = 50;

            let result = lotto
                .get_numbers(
                    requestor_id,
                    draw_num,
                    nb_numbers,
                    smallest_number,
                    biggest_number,
                )
                .unwrap();
            assert_eq!(nb_numbers as usize, result.len());
            for &n in result.iter() {
                assert!(n >= smallest_number);
                assert!(n <= biggest_number);
            }

            debug_println!("random numbers: {result:?}");
        }

        #[ink::test]
        fn test_get_numbers_from_1_to_5() {
            let _ = env_logger::try_init();
            pink_extension_runtime::mock_ext::mock_all_ext();

            let lotto = init_contract();

            let requestor_id = AccountId::try_from(*&[1u8; 32]).unwrap();
            let draw_num = 1;
            let nb_numbers = 5;
            let smallest_number = 1;
            let biggest_number = 5;

            let result = lotto
                .get_numbers(
                    requestor_id,
                    draw_num,
                    nb_numbers,
                    smallest_number,
                    biggest_number,
                )
                .unwrap();
            assert_eq!(nb_numbers as usize, result.len());
            for &n in result.iter() {
                assert!(n >= smallest_number);
                assert!(n <= biggest_number);
            }

            debug_println!("random numbers: {result:?}");
        }

        #[ink::test]
        fn test_with_different_draw_num() {
            let _ = env_logger::try_init();
            pink_extension_runtime::mock_ext::mock_all_ext();

            let lotto = init_contract();

            let requestor_id = AccountId::try_from(*&[1u8; 32]).unwrap();

            let nb_numbers = 5;
            let smallest_number = 1;
            let biggest_number = 50;

            let mut results = Vec::new();

            for i in 0..100 {
                let result = lotto
                    .get_numbers(requestor_id, i, nb_numbers, smallest_number, biggest_number)
                    .unwrap();
                // this result must be different from the previous ones
                results.iter().for_each(|r| assert_ne!(result, *r));

                // same request message means same result
                let result_2 = lotto
                    .get_numbers(requestor_id, i, nb_numbers, smallest_number, biggest_number)
                    .unwrap();
                assert_eq!(result, result_2);

                results.push(result);
            }
        }

        #[ink::test]
        fn test_get_winners() {
            let _ = env_logger::try_init();
            pink_extension_runtime::mock_ext::mock_all_ext();

            let lotto = init_contract();

            let draw_num = 2;
            let numbers = vec![15, 1, 44, 28];

            let winners = lotto.get_winners(draw_num, numbers).unwrap();
            debug_println!("winners: {winners:?}");
        }

        #[ink::test]
        #[ignore = "The target contract must be deployed on the Substrate node and a random number request must be submitted"]
        fn answer_request() {
            let _ = env_logger::try_init();
            pink_extension_runtime::mock_ext::mock_all_ext();

            let lotto = init_contract();

            let r = lotto.answer_request().expect("failed to answer request");
            debug_println!("answer request: {r:?}");
        }
    }
}

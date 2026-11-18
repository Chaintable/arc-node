//! DeBank protocol types and projections used by the RPC endpoints.

pub use pipeline_rs_arc::execution::debank::*;

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256 as H256;

    #[test]
    fn arc_mainnet_genesis_ids_and_validation_are_stable() {
        let genesis: alloy_genesis::Genesis =
            serde_json::from_str(include_str!("../../../assets/mainnet/genesis.json")).unwrap();
        let (transactions, traces) = build_genesis_txs_and_traces(&genesis);

        assert_eq!(transactions.len(), 273);
        assert_eq!(traces.len(), 273);
        assert!(transactions
            .iter()
            .all(|transaction| transaction.id.parse::<H256>().is_ok()));
        assert!(traces
            .iter()
            .all(|trace| trace.tx_id.parse::<H256>().is_ok()));
        assert_eq!(
            transactions[12..17]
                .iter()
                .map(|transaction| transaction.id.as_str())
                .collect::<Vec<_>>(),
            [
                "0x020000000000000000000000180000000000000000000000000000000000000d",
                "0x020000000000000000000000180000000000000000000000000000000000000a",
                "0x020000000000000000000000180000000000000000000000000000000000000b",
                "0x020000000000000000000000180000000000000000000000000000000000000c",
                "0x020000000000000000000000180000000000000000000000000000000000000e",
            ]
        );
        assert!(transactions.iter().any(|transaction| {
            transaction.id == "0x01000000000000000000000050a2b0b577ec24d7ce1aed372a8a6fd14ce1be57"
        }));

        let block_file = BlockFile {
            block: DebankBlock {
                id: "0x09944e07412986bb417fd0006c89ffb71ee523d68ce2017ec2dabc944c42edad"
                    .parse()
                    .unwrap(),
                ..Default::default()
            },
            transactions,
            traces,
            ..Default::default()
        };
        assert_eq!(block_file.validation().validation_hash, 356_679);
    }
}

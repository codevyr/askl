// clang indexer tests are disabled — the sqlx-based Index has been removed.
// These tests relied on Index::new_in_memory and query methods that no longer exist.

#[ignore]
#[tokio::test]
async fn create_state() {
    unimplemented!("sqlx-based Index has been removed")
}

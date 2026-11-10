mod common;

use common::broker;
use lr_store::SqliteBrokerStore;

lr_abstractions::broker_store_contract!(
    make: broker(),
    reopen: |s: SqliteBrokerStore| s.reopen().unwrap(),
);

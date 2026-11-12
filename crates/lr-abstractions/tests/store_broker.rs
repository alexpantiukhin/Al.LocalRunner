use lr_abstractions::fakes::MemoryBrokerStore;

lr_abstractions::broker_store_contract!(
    make: MemoryBrokerStore::new(),
    reopen: |s: MemoryBrokerStore| s.reopen(),
);

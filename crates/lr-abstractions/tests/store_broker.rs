use lr_abstractions::fakes::MemoryBrokerStore;

lr_abstractions::broker_store_contract!(MemoryBrokerStore::new());

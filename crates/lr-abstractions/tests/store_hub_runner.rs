use lr_abstractions::fakes::{MemoryHubStore, MemoryRunnerStore};
use lr_abstractions::HubStore;
use std::path::Path;

mod hub {
    use super::MemoryHubStore;
    lr_abstractions::hub_store_contract!(MemoryHubStore::new());
}

mod runner {
    use super::MemoryRunnerStore;
    lr_abstractions::runner_store_contract!(MemoryRunnerStore::new());
}

#[tokio::test]
async fn hub_log_copy_appends_to_the_file_in_submitter_directory() {
    let store = MemoryHubStore::new();
    let path = Path::new("/work/repo/temp/runner/j1.log");
    store.append_log_copy(path, b"one\n").await.unwrap();
    store.append_log_copy(path, b"two\n").await.unwrap();
    assert_eq!(store.file(path), Some(b"one\ntwo\n".to_vec()));
}

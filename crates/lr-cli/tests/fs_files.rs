mod common;

use common::TempDir;
use lr_abstractions::Files;
use lr_cli::fs_files::StdFiles;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_writes_of_one_path_leave_one_whole_file() {
    let dir = TempDir::new("files");
    let path = Arc::new(dir.path().join("sub").join("handle.json"));
    let mut tasks = Vec::new();
    for i in 0..32u8 {
        let path = path.clone();
        tasks.push(tokio::spawn(async move {
            StdFiles
                .write(&path, &vec![b'a' + (i % 26); 64 * 1024])
                .await
        }));
    }
    for t in tasks {
        t.await.unwrap().expect("write succeeds");
    }
    let data = StdFiles.read(&path).await.unwrap().expect("file exists");
    assert_eq!(data.len(), 64 * 1024);
    assert!(data.iter().all(|b| *b == data[0]), "mixed content");
    let left: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(left.len(), 1, "temporary files left: {left:?}");
}

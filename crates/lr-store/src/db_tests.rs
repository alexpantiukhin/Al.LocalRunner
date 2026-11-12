use crate::db::read;
use rusqlite::Connection;

fn count(c: &Connection) -> i64 {
    c.query_row("SELECT count(*) FROM t", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn queries_inside_read_see_one_snapshot_even_when_another_connection_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let reader = Connection::open(&path).unwrap();
    reader
        .execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE t (a INTEGER);")
        .unwrap();
    let writer = Connection::open(&path).unwrap();
    let (first, second) = read(&reader, |c| {
        let first = count(c);
        writer.execute("INSERT INTO t VALUES (1)", []).unwrap();
        Ok((first, count(c)))
    })
    .unwrap_or_else(|_| panic!("read failed"));
    assert_eq!((first, second), (0, 0));
    assert_eq!(count(&reader), 1);
}

#[test]
fn without_read_a_second_query_sees_the_concurrent_write() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let reader = Connection::open(&path).unwrap();
    reader
        .execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE t (a INTEGER);")
        .unwrap();
    let writer = Connection::open(&path).unwrap();
    let first = count(&reader);
    writer.execute("INSERT INTO t VALUES (1)", []).unwrap();
    assert_eq!((first, count(&reader)), (0, 1));
}

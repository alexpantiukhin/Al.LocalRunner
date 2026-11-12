mod common;

use common::summary;
use lr_core::{JobId, MAX_FRAME_BYTES};
use lr_proto::*;

fn order(i: usize, log_bytes: usize) -> HubOrder {
    let mut s = summary();
    s.job = JobId(format!("j{i:05}"));
    HubOrder {
        broker: Some("local".into()),
        outbox: i.is_multiple_of(2),
        summary: s,
        log: Some(format!("/w/{}/j{i:05}.log", "d".repeat(log_bytes))),
    }
}

fn parts(reply: CliReply) -> (Vec<HubOrder>, bool) {
    match reply {
        CliReply::Orders { orders, truncated } => (orders, truncated),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_short_list_of_orders_is_answered_whole() {
    let all: Vec<HubOrder> = (0..3).map(|i| order(i, 10)).collect();
    let (orders, truncated) = parts(CliReply::orders_within_frame(all.clone()));
    assert_eq!(orders, all);
    assert!(!truncated);
}

#[test]
fn a_list_too_big_for_one_frame_keeps_the_oldest_and_says_it_was_cut() {
    let all: Vec<HubOrder> = (0..3000).map(|i| order(i, 4000)).collect();
    let whole = serde_json::to_vec(&CliReply::Orders {
        orders: all.clone(),
        truncated: false,
    })
    .unwrap();
    assert!(whole.len() > MAX_FRAME_BYTES);
    let reply = CliReply::orders_within_frame(all.clone());
    let frame = encode(&reply).expect("fits one frame");
    assert!(frame.len() <= MAX_FRAME_BYTES);
    let (orders, truncated) = parts(reply);
    assert!(truncated);
    assert!(!orders.is_empty() && orders.len() < all.len());
    assert_eq!(orders[..], all[..orders.len()]);
}

#[test]
fn an_answer_without_the_cut_mark_reads_as_whole() {
    let old = serde_json::json!({ "type": "orders", "orders": [] });
    let reply: CliReply = serde_json::from_value(old).unwrap();
    assert_eq!(
        reply,
        CliReply::Orders {
            orders: vec![],
            truncated: false
        }
    );
}

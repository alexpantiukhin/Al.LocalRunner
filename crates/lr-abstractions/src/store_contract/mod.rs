//! Contract tests every store implementation must pass (feature `test-support`). The in-memory
//! fakes and the SQLite stores run the same cases through the macros
//! [`broker_store_contract!`](crate::broker_store_contract),
//! [`hub_store_contract!`](crate::hub_store_contract) and
//! [`runner_store_contract!`](crate::runner_store_contract), which expand to one
//! `#[tokio::test]` per case; the calling crate needs `tokio` with `rt` and `macros`.
//!
//! ```ignore
//! lr_abstractions::broker_store_contract!(
//!     make: SqliteBrokerStore::open_temp().unwrap(),
//!     reopen: |s: SqliteBrokerStore| s.reopen().unwrap(),
//! );
//! ```
//!
//! `make` is evaluated anew for every case and must yield an empty store whose type coerces
//! to `&dyn Store` (a concrete store type; an `Arc<S>` factory is not supported, wrap it in a
//! newtype that implements the trait). `reopen` takes the store and returns a new handle over
//! the same data, as after a process restart: the SQLite store closes its connection and opens
//! the same file; the in-memory fakes share their state.

pub mod broker;
pub mod broker_history;
pub mod broker_registry;
pub mod fixtures;
pub mod hub;
pub mod reopen;
pub mod runner;
pub mod seq;

#[doc(hidden)]
#[macro_export]
macro_rules! __store_contract_cases {
    ($make:expr; $($path:ident :: $case:ident),* $(,)?) => {
        $(
            #[::tokio::test]
            async fn $case() {
                let store = $make;
                $crate::store_contract::$path::$case(&store).await;
            }
        )*
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __store_reopen_cases {
    ($make:expr, $reopen:expr; $($case:ident),* $(,)?) => {
        $(
            #[::tokio::test]
            async fn $case() {
                let store = $make;
                $crate::store_contract::reopen::$case(store, $reopen).await;
            }
        )*
    };
}

/// Expands to one test per [`crate::BrokerStore`] contract case on stores made by `make`, reopened by `reopen`.
#[macro_export]
macro_rules! broker_store_contract {
    (make: $make:expr, reopen: $reopen:expr $(,)?) => {
        $crate::__store_contract_cases!($make;
            broker::accept_creates_queued_run_and_order_row,
            broker::duplicate_order_same_client_key_and_job_id_is_noop,
            broker::same_job_id_from_another_client_is_a_separate_run,
            broker::order_queue_is_oldest_first,
            broker::order_row_removed_after_grant,
            broker::start_requires_grant_and_records_broker_time,
            broker::final_state_written_once,
            broker::disallowed_transition_is_refused,
            broker::cancel_of_waiting_order_removes_it_at_once,
            broker::accepted_cancel_keeps_canceller_until_runner_reports,
            broker::cancelling_run_of_vanished_runner_ends_lost,
            broker::unstarted_granted_run_can_be_requeued,
            broker::started_run_cannot_be_requeued,
            broker::requeue_starts_a_new_stream_for_the_next_runner,
            broker::step_updates_stop_at_step_end_and_run_end,
            seq::broker_seq_counted_per_run_and_direction,
            seq::broker_seq_gap_is_refused,
            seq::broker_to_runner_seq_counts_per_run,
            broker::log_is_appended_and_read_back,
            broker::log_cut_at_limit_with_mark_then_dropped,
            broker_history::history_is_newest_first_filtered_and_limited,
            broker_history::stats_count_final_runs_with_median_and_p95,
            broker_history::prune_removes_rows_and_logs_by_age,
            broker_history::prune_by_total_log_size_removes_oldest_first,
            broker_history::prune_never_touches_active_runs,
            broker_registry::join_records_key_and_spends_single_use,
            broker_registry::multi_use_code_counts_down_per_join,
            broker_registry::named_code_admits_only_its_name,
            broker_registry::code_label_applied_to_joining_machine,
            broker_registry::join_code_found_by_hash,
            broker_registry::invalid_codes_refused_without_spending_a_use,
            broker_registry::taken_name_refused_until_revoked,
            broker_registry::machine_found_by_key_and_last_seen_updated,
            broker_registry::revoke_removes_runners_and_outbox_of_the_key,
            broker_registry::runner_needs_machine_and_unique_name,
            broker_registry::runner_state_and_last_seen_stored,
            broker_registry::result_outbox_seq_per_client_and_job_and_ack_removes,
            broker_registry::chunks_over_limit_dropped_results_kept,
            broker_registry::expired_results_removed,
            broker_registry::result_seq_never_reused_after_expire_or_revoke,
        );
        $crate::__store_reopen_cases!($make, $reopen;
            broker_seq_counters_survive_reopen,
            broker_outbox_survives_reopen_and_ack_stays_acked,
            broker_final_state_survives_reopen,
        );
    };
}

/// Expands to one test per [`crate::HubStore`] contract case on stores made by `make`, reopened by `reopen`.
#[macro_export]
macro_rules! hub_store_contract {
    (make: $make:expr, reopen: $reopen:expr $(,)?) => {
        $crate::__store_contract_cases!($make;
            hub::order_waits_in_outbox_until_accepted,
            hub::enqueue_of_active_job_refused,
            hub::outbox_seq_per_job_survives_ack_and_ack_is_cumulative,
            hub::cancel_before_accepted_discards_order_entry,
            hub::outbox_push_needs_active_order,
            hub::received_seq_repeat_dropped_and_last_seq_kept,
            hub::finish_removes_active_order_and_its_outbox,
            hub::broker_of_active_order_set_and_cleared,
            hub::broker_list_upsert_zone_and_remove,
            seq::hub_seq_gap_is_refused,
        );
        $crate::__store_reopen_cases!($make, $reopen; hub_outbox_and_seq_survive_reopen);
    };
}

/// Expands to one test per [`crate::RunnerStore`] contract case on stores made by `make`, reopened by `reopen`.
#[macro_export]
macro_rules! runner_store_contract {
    (make: $make:expr, reopen: $reopen:expr $(,)?) => {
        $crate::__store_contract_cases!($make;
            runner::runner_outbox_keeps_order_until_acked,
            runner::runner_outbox_discard_and_clear,
            runner::finished_jobs_are_forgotten_after_the_keep_period,
            runner::service_registry_upsert_list_remove,
            runner::cache_index_keyed_by_client_project_and_name,
            seq::runner_received_seq_per_job,
            seq::runner_outbox_counters_independent_across_jobs,
        );
        $crate::__store_reopen_cases!($make, $reopen; runner_outbox_and_seq_survive_reopen);
    };
}

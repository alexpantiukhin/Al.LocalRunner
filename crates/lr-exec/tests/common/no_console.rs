use super::helper::{helper_command, say, Helper};
use super::{base_env, step, workdir, Recorder};
use lr_abstractions::{stop_signal, JobSpec, ProcessRunner, Stop};
use lr_core::JobId;
use lr_exec::{ExecRunner, SystemClock, SOFT_STOP_NOT_DELIVERED};
use std::sync::Arc;
use std::time::Duration;

pub fn run() -> ! {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let cmd = helper_command(&Helper::new("ignore"));
        let mut spec = JobSpec::new(JobId("nc".to_string()), 600, vec![step("s", &cmd, 300)]);
        spec.cwd = workdir("job");
        spec.env = base_env();
        spec.cancel_grace = Duration::from_millis(300);
        let runner = ExecRunner::new(Arc::new(SystemClock));
        let out = Recorder::new();
        let (handle, stop) = stop_signal();
        let o = out.clone();
        let task = tokio::spawn(async move { runner.run_job(spec, o.as_ref(), stop).await });
        out.pid_of("child").await;
        handle.stop(Stop::Cancel);
        let outcome = tokio::time::timeout(Duration::from_secs(20), task)
            .await
            .unwrap()
            .unwrap();
        let reported = out.text().contains(SOFT_STOP_NOT_DELIVERED);
        say(&format!(
            "outcome={:?} soft-not-delivered={reported} report-end\n",
            outcome.state
        ));
    });
    std::process::exit(0);
}

//! Intake of an order that needs runner services (spec 3.6, 3.11): it is refused when the job
//! together with the memory of its services can never fit the pool, its quota or a runner.

use super::orders::{overridden, reject_reason};
use super::service_budget::{cheapest_service_mb, service_cost_mb, with_extra};
use super::Engine;
use lr_core::{check_runners, QueueEntry, Reject};
use lr_proto::{OrderSpec, RejectReason};

impl Engine {
    /// Policy and runner checks of an order at intake, with the memory of its services: the
    /// pool, reserves and quotas see the job plus the cheapest cold start of its services, and
    /// each runner sees the job plus its own services.
    pub(crate) fn admit_fit(
        &self,
        entry: &QueueEntry,
        spec: &OrderSpec,
    ) -> Result<(), (RejectReason, String)> {
        let req = &entry.order.requirements;
        let extra = cheapest_service_mb(self.runners.values(), req, false);
        let job_mb = req.mem_mb;
        let with_services = |detail: String| {
            if extra == 0 {
                detail
            } else {
                format!("{detail} (джоба {job_mb} МБ + сервисы {extra} МБ)")
            }
        };
        let policy = self.policy.policy();
        let raised = with_extra(entry.clone(), extra);
        policy.admit(&raised.order, &raised.client).map_err(|r| {
            overridden(&r, &spec.overrides, req, policy.pool())
                .unwrap_or_else(|| (reject_reason(&r), with_services(r.to_string())))
        })?;
        self.fits_a_runner(entry).map_err(|r| {
            let detail = r.to_string();
            let detail = match &r {
                Reject::NoMatchingRunner { .. } if !req.services.is_empty() => {
                    format!("{detail} (джоба {job_mb} МБ + сервисы раннера)")
                }
                _ => detail,
            };
            (reject_reason(&r), detail)
        })
    }

    fn fits_a_runner(&self, entry: &QueueEntry) -> Result<(), Reject> {
        let views = self.runner_views();
        let req = &entry.order.requirements;
        if req.services.is_empty() || views.is_empty() {
            return check_runners(entry, &views, &self.site);
        }
        let mut mismatches = Vec::new();
        let mut elsewhere = false;
        for view in &views {
            let cost = self
                .runners
                .get(&view.name)
                .map_or(0, |s| service_cost_mb(s, req, false));
            let raised = with_extra(entry.clone(), cost);
            match check_runners(&raised, std::slice::from_ref(view), &self.site) {
                Ok(()) => return Ok(()),
                Err(Reject::NoMatchingRunner { runners }) => mismatches.extend(runners),
                Err(_) => elsewhere = true,
            }
        }
        if mismatches.is_empty() && elsewhere {
            Err(Reject::NoRunnerOnMachine)
        } else {
            Err(Reject::NoMatchingRunner {
                runners: mismatches,
            })
        }
    }
}

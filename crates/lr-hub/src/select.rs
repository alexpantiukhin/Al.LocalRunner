//! Broker choice (spec 3.1): the order's zone boundary first, then the zones in
//! `broker_zone_order`, the brokers of a zone queried in parallel, the choice among suitable
//! ones by `[brokers] strategy`.

use crate::engine::Shared;
use crate::policy::{BrokerStrategy, BrokersPolicy, ZoneQuery};
use crate::LOCAL_BROKER;
use lr_abstractions::Frame;
use lr_core::{satisfies, Requirements, Zone};
use lr_proto::{
    BrokerToHub, CapabilitiesQuery, HubToBroker, OrderSpec, ReasonCode, RejectReason, RunnerCaps,
};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// Whether the order may run only in the zones `local` and `shared`, which only the broker of
/// this machine serves: every other broker lies in `lan` or `remote` from the hub.
pub fn local_only(req: &Requirements) -> bool {
    req.zones
        .as_ref()
        .is_some_and(|z| z.iter().all(|z| *z <= Zone::Shared))
}

/// Zones an order may go to, nearest first: up to the farthest of its zones. The boundary is a hard invariant: no policy widens it.
pub fn allowed_zones(req: &Requirements) -> Vec<Zone> {
    let farthest = req
        .zones
        .as_ref()
        .and_then(|z| z.iter().max().copied())
        .unwrap_or(Zone::Remote);
    [Zone::Local, Zone::Shared, Zone::Lan, Zone::Remote]
        .into_iter()
        .filter(|z| *z <= farthest)
        .collect()
}

/// The zone a broker counts in: [`LOCAL_BROKER`] is local; any other broker is in the zone it
/// last reported, and remote while it never did, so an unknown zone never crosses a boundary.
pub fn broker_zone(name: &str, reported: Option<Zone>) -> Zone {
    if name == LOCAL_BROKER {
        return Zone::Local;
    }
    reported.unwrap_or(Zone::Remote)
}

/// Brokers ranked by `[brokers] order`: the named ones in that order, then the rest by name.
pub fn ranked(mut names: Vec<String>, order: &[String]) -> Vec<String> {
    names.sort_by_key(|n| {
        let pos = order.iter().position(|o| o == n).unwrap_or(usize::MAX);
        (pos, n.clone())
    });
    names
}

/// Online runners of a reply that satisfy the order, or the figures of why none does.
pub fn suitable(runners: &[RunnerCaps], req: &Requirements) -> Result<usize, String> {
    let mut count = 0;
    let mut why = Vec::new();
    for r in runners {
        if !r.online {
            why.push(format!("{}: not connected", r.name));
            continue;
        }
        match satisfies(&r.clone().into_capabilities(), req) {
            Ok(()) => count += 1,
            Err(m) => why.push(format!("{}: {m}", r.name)),
        }
    }
    if count > 0 {
        return Ok(count);
    }
    if why.is_empty() {
        why.push("no runners".into());
    }
    Err(why.join("; "))
}

/// What became of one broker in a query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// No reply within `query_timeout_s`, or not connected.
    Unavailable,
    /// The broker's runners, and whether it would start the probed order now.
    Runners(Vec<RunnerCaps>, Option<bool>),
}

/// Outcome of a broker choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Send the order to this broker.
    Chosen(String),
    /// No allowed broker answered: the order waits in the outbox with this reason.
    Wait(String),
    /// The order is refused with this code and text.
    Reject(ReasonCode, String),
}

#[derive(Clone)]
pub(crate) struct Candidate {
    pub(crate) name: String,
    pub(crate) zone: Zone,
    pub(crate) out: Option<mpsc::Sender<Frame>>,
}

pub(crate) struct Choice {
    pub(crate) policy: BrokersPolicy,
    pub(crate) req: Requirements,
    pub(crate) probe: Option<Box<OrderSpec>>,
    pub(crate) candidates: Vec<Candidate>,
    pub(crate) shared: Arc<Shared>,
}

enum Pass {
    Chosen(String),
    NoneSuitable(String),
    NoneAnswered,
    SomeUnavailable(Vec<String>),
}

impl Choice {
    fn groups(&self) -> Vec<Vec<Candidate>> {
        let allowed = allowed_zones(&self.req);
        self.policy
            .broker_zone_order
            .iter()
            .filter(|z| allowed.contains(z))
            .map(|z| {
                let names = self
                    .candidates
                    .iter()
                    .filter(|c| c.zone == *z)
                    .map(|c| c.name.clone())
                    .collect();
                ranked(names, &self.policy.order)
                    .into_iter()
                    .filter_map(|n| self.candidates.iter().find(|c| c.name == n).cloned())
                    .collect::<Vec<_>>()
            })
            .filter(|g: &Vec<Candidate>| !g.is_empty())
            .collect()
    }

    async fn query(&self, brokers: &[Candidate]) -> BTreeMap<String, Answer> {
        let s = &self.shared;
        let mut waits = Vec::new();
        let mut out = BTreeMap::new();
        for b in brokers {
            let Some(tx) = &b.out else {
                out.insert(b.name.clone(), Answer::Unavailable);
                continue;
            };
            let req = s.next_req();
            let rx = s.pending.register(req, &b.name);
            let sent = Frame::json(&HubToBroker::CapabilitiesQuery(CapabilitiesQuery {
                req,
                probe: self.probe.clone(),
            }))
            .ok()
            .is_some_and(|f| tx.try_send(f).is_ok());
            if sent {
                waits.push((b.name.clone(), req, rx));
            } else {
                s.pending.forget(req, &b.name);
                out.insert(b.name.clone(), Answer::Unavailable);
            }
        }
        let deadline = s.clock.now().plus_secs(self.policy.query_timeout_s);
        for (name, req, rx) in waits {
            let answer = tokio::select! {
                biased;
                r = rx => match r {
                    Ok(BrokerToHub::CapabilitiesReply(reply)) => {
                        Answer::Runners(reply.runners, reply.start_now)
                    }
                    _ => Answer::Unavailable,
                },
                _ = s.clock.sleep_until(deadline) => Answer::Unavailable,
            };
            s.pending.forget(req, &name);
            out.insert(name, answer);
        }
        out
    }

    /// The broker of `group` to place the order at; with `only_now`, only one that said it
    /// would start the order at once.
    fn pick(
        &self,
        group: &[Candidate],
        answers: &BTreeMap<String, Answer>,
        only_now: bool,
    ) -> Option<String> {
        let fits: Vec<(&str, usize)> = group
            .iter()
            .filter_map(|c| match answers.get(&c.name) {
                Some(Answer::Runners(r, now)) if !only_now || *now == Some(true) => {
                    suitable(r, &self.req).ok().map(|n| (c.name.as_str(), n))
                }
                _ => None,
            })
            .collect();
        let best = match self.policy.strategy {
            BrokerStrategy::First => fits.first().copied(),
            BrokerStrategy::LeastLoaded => {
                fits.iter()
                    .copied()
                    .fold(None, |acc: Option<(&str, usize)>, x| match acc {
                        Some(a) if a.1 >= x.1 => Some(a),
                        _ => Some(x),
                    })
            }
        };
        best.map(|(n, _)| n.to_string())
    }

    async fn pass(&self) -> Pass {
        let groups = self.groups();
        let mut answers = BTreeMap::new();
        if self.policy.zone_query == ZoneQuery::All {
            let all: Vec<Candidate> = groups.iter().flatten().cloned().collect();
            answers = self.query(&all).await;
        }
        let mut nearest = None;
        for (i, group) in groups.iter().enumerate() {
            if self.policy.zone_query == ZoneQuery::Sequential {
                answers.extend(self.query(group).await);
            }
            if let Some(name) = self.pick(group, &answers, true) {
                return Pass::Chosen(name);
            }
            if nearest.is_none() {
                nearest = self.pick(group, &answers, false);
            }
            if nearest.is_none() {
                continue;
            }
            let rest = &groups[i + 1..];
            let busy = group.iter().any(|c| {
                matches!(
                    answers.get(&c.name),
                    Some(Answer::Runners(r, Some(false))) if suitable(r, &self.req).is_ok()
                )
            });
            if busy && self.policy.zone_query == ZoneQuery::Sequential && !rest.is_empty() {
                let farther: Vec<Candidate> = rest.iter().flatten().cloned().collect();
                answers.extend(self.query(&farther).await);
            }
            if let Some(name) = rest.iter().find_map(|g| self.pick(g, &answers, true)) {
                return Pass::Chosen(name);
            }
            break;
        }
        if let Some(name) = nearest {
            return Pass::Chosen(name);
        }
        let unavailable: Vec<String> = answers
            .iter()
            .filter(|(_, a)| **a == Answer::Unavailable)
            .map(|(n, _)| n.clone())
            .collect();
        if unavailable.len() == answers.len() {
            return Pass::NoneAnswered;
        }
        if !unavailable.is_empty() {
            return Pass::SomeUnavailable(unavailable);
        }
        let figures: Vec<String> = answers
            .iter()
            .filter_map(|(n, a)| match a {
                Answer::Runners(r, _) => suitable(r, &self.req).err().map(|w| format!("{n}: {w}")),
                Answer::Unavailable => None,
            })
            .collect();
        Pass::NoneSuitable(figures.join("; "))
    }

    /// The broker, other than `from`, that would start the order now, nearest zone first. Only
    /// when `from` itself answers `start_now` (it knows `Withdraw`, as it knows the probe).
    pub(crate) async fn move_target(&self, from: &str) -> Option<String> {
        let groups = self.groups();
        let all: Vec<Candidate> = groups.iter().flatten().cloned().collect();
        let answers = self.query(&all).await;
        if !matches!(answers.get(from), Some(Answer::Runners(_, Some(_)))) {
            return None;
        }
        groups.iter().find_map(|group| {
            let others: Vec<Candidate> = group.iter().filter(|c| c.name != from).cloned().collect();
            self.pick(&others, &answers, true)
        })
    }

    pub(crate) async fn run(self) -> Decision {
        if self.groups().is_empty() {
            return Decision::Reject(
                ReasonCode::Rejected(RejectReason::ZoneNotServed),
                "no brokers in the zones the order allows".into(),
            );
        }
        let attempts = self.policy.query_attempts.max(1);
        for attempt in 1..=attempts {
            match self.pass().await {
                Pass::Chosen(name) => return Decision::Chosen(name),
                Pass::NoneSuitable(figures) => {
                    return Decision::Reject(
                        ReasonCode::Rejected(RejectReason::NoMatchingRunner),
                        format!("no matching runner: {figures}"),
                    )
                }
                Pass::NoneAnswered => return Decision::Wait("brokers unavailable".into()),
                Pass::SomeUnavailable(names) if attempt == attempts => {
                    return Decision::Reject(
                        ReasonCode::Rejected(RejectReason::BrokersUnreachable),
                        format!(
                            "the brokers did not answer in {attempts} attempts: {}",
                            names.join(", ")
                        ),
                    )
                }
                Pass::SomeUnavailable(_) => {
                    self.shared
                        .clock
                        .sleep(Duration::from_secs(self.policy.query_retry_s))
                        .await;
                }
            }
        }
        Decision::Wait("brokers unavailable".into())
    }
}

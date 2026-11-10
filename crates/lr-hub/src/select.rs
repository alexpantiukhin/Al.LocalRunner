//! Broker choice (spec 3.1): the order's zone boundary first, then the zones in
//! `broker_zone_order`, the brokers of a zone queried in parallel, the choice among suitable
//! ones by `[brokers] strategy`.

use crate::engine::Shared;
use crate::policy::{BrokerStrategy, BrokersPolicy, ZoneQuery};
use crate::LOCAL_BROKER;
use lr_abstractions::Frame;
use lr_core::{satisfies, Requirements, Zone};
use lr_proto::{BrokerToHub, CapabilitiesQuery, HubToBroker, ReasonCode, RejectReason, RunnerCaps};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// Zones an order may go to, nearest first: only local for a same-machine order, up to its
/// `zone` otherwise. The boundary is a hard invariant: no policy widens it.
pub fn allowed_zones(req: &Requirements) -> Vec<Zone> {
    let farthest = if req.same_machine.is_some() {
        Zone::Local
    } else {
        req.max_zone.unwrap_or(Zone::Remote)
    };
    [Zone::Local, Zone::Lan, Zone::Remote]
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
            why.push(format!("{}: не на связи", r.name));
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
        why.push("нет раннеров".into());
    }
    Err(why.join("; "))
}

/// What became of one broker in a query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// No reply within `query_timeout_s`, or not connected.
    Unavailable,
    /// The broker's runners.
    Runners(Vec<RunnerCaps>),
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

/// A broker that can be asked.
#[derive(Clone)]
pub(crate) struct Candidate {
    pub(crate) name: String,
    pub(crate) zone: Zone,
    pub(crate) out: Option<mpsc::Sender<Frame>>,
}

pub(crate) struct Choice {
    pub(crate) policy: BrokersPolicy,
    pub(crate) req: Requirements,
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
            let rx = s.pending.register(req);
            let sent = Frame::json(&HubToBroker::CapabilitiesQuery(CapabilitiesQuery { req }))
                .ok()
                .is_some_and(|f| tx.try_send(f).is_ok());
            if sent {
                waits.push((b.name.clone(), req, rx));
            } else {
                s.pending.forget(req);
                out.insert(b.name.clone(), Answer::Unavailable);
            }
        }
        let deadline = s.clock.now().plus_secs(self.policy.query_timeout_s);
        for (name, req, rx) in waits {
            let answer = tokio::select! {
                biased;
                r = rx => match r {
                    Ok(BrokerToHub::CapabilitiesReply(reply)) => Answer::Runners(reply.runners),
                    _ => Answer::Unavailable,
                },
                _ = s.clock.sleep_until(deadline) => Answer::Unavailable,
            };
            s.pending.forget(req);
            out.insert(name, answer);
        }
        out
    }

    fn pick(&self, group: &[Candidate], answers: &BTreeMap<String, Answer>) -> Option<String> {
        let fits: Vec<(&str, usize)> = group
            .iter()
            .filter_map(|c| match answers.get(&c.name) {
                Some(Answer::Runners(r)) => {
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
        for group in &groups {
            if self.policy.zone_query == ZoneQuery::Sequential {
                answers.extend(self.query(group).await);
            }
            if let Some(name) = self.pick(group, &answers) {
                return Pass::Chosen(name);
            }
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
                Answer::Runners(r) => suitable(r, &self.req).err().map(|w| format!("{n}: {w}")),
                Answer::Unavailable => None,
            })
            .collect();
        Pass::NoneSuitable(figures.join("; "))
    }

    /// Runs the choice with up to `query_attempts` passes.
    pub(crate) async fn run(self) -> Decision {
        if self.groups().is_empty() {
            return Decision::Reject(
                ReasonCode::Rejected(RejectReason::ZoneNotServed),
                "нет брокеров в допустимых зонах заказа".into(),
            );
        }
        let attempts = self.policy.query_attempts.max(1);
        for attempt in 1..=attempts {
            match self.pass().await {
                Pass::Chosen(name) => return Decision::Chosen(name),
                Pass::NoneSuitable(figures) => {
                    return Decision::Reject(
                        ReasonCode::Rejected(RejectReason::NoMatchingRunner),
                        format!("нет подходящего раннера: {figures}"),
                    )
                }
                Pass::NoneAnswered => return Decision::Wait("брокеры недоступны".into()),
                Pass::SomeUnavailable(names) if attempt == attempts => {
                    return Decision::Reject(
                        ReasonCode::Rejected(RejectReason::BrokersUnreachable),
                        format!(
                            "брокеры не ответили за {attempts} попыток: {}",
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
        Decision::Wait("брокеры недоступны".into())
    }
}

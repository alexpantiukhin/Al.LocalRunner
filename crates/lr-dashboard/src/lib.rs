//! The dashboard role (spec 9): one self-contained page served from loopback over a
//! [`lr_abstractions::DataSource`], its JSON and SSE feed, the cancel action, and the same page
//! rendered as a single HTML snapshot file.
//!
//! Security (spec 9.4): the socket is bound to `127.0.0.1` (and `::1`) only, with no option to
//! widen it; a request whose `Host` or `Origin` is not the loopback address with the dashboard
//! port is refused; the random launch token in the URL is exchanged for an `HttpOnly`,
//! `SameSite=Strict` session cookie; state-changing requests are `POST` with the
//! [`CSRF_HEADER`] token; data is rendered as text, never as markup, under a strict CSP.

mod actions;
mod guard;
mod server;
mod snapshot;
mod sse;

pub use guard::CSRF_HEADER;
pub use server::{
    ensure_loopback, Dashboard, DashboardError, DashboardOptions, RunningDashboard, LOG_TAIL_BYTES,
};
pub use snapshot::{escape_html, render_snapshot};

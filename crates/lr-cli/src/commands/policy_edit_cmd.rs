//! `policy allow-secret|remove-secret|allow-service|remove-service|add-local-user|
//! remove-local-user|add-shared|remove-shared`: one record of a list of tables in the broker
//! policy, through the same change flow as `config set`.

use crate::commands::config_cmd::elevated_dirs;
use crate::config_edit::{
    add_local_user, add_name, add_shared, plan, remove_local_user, remove_name, remove_shared,
    resolve, run_change_with, ApplyOpts, Change, ConfigDoc, Elevator, LiveRoles, OsElevator,
    RoleControl, Target,
};
use crate::paths::Layout;
use std::io::Write;

/// A record of `policy.toml` that a command adds or removes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    /// Allow secret `name` for `label` in `[[secrets.allow]]`.
    AllowSecret {
        /// The secret.
        name: String,
        /// The client label.
        label: String,
    },
    /// Take secret `name` from `label`.
    RemoveSecret {
        /// The secret.
        name: String,
        /// The client label.
        label: String,
    },
    /// Allow service `name` for `label` in `[[services.allow]]`.
    AllowService {
        /// The service.
        name: String,
        /// The client label.
        label: String,
    },
    /// Take service `name` from `label`.
    RemoveService {
        /// The service.
        name: String,
        /// The client label.
        label: String,
    },
    /// Add a `[[local_user]]` entry.
    AddLocalUser {
        /// The OS user.
        name: String,
        /// Its client label; none: the empty default.
        label: Option<String>,
    },
    /// Remove the `[[local_user]]` entry of an OS user.
    RemoveLocalUser {
        /// The OS user.
        name: String,
    },
    /// Add a `[[shared]]` block.
    AddShared {
        /// Its members.
        members: Vec<String>,
    },
    /// Remove the `[[shared]]` block with this set of members.
    RemoveShared {
        /// Its members.
        members: Vec<String>,
    },
}

const SECRETS: &str = "secrets.allow";
const SERVICES: &str = "services.allow";

impl Record {
    /// The dotted key of the list the record belongs to.
    pub fn key(&self) -> &'static str {
        match self {
            Record::AllowSecret { .. } | Record::RemoveSecret { .. } => SECRETS,
            Record::AllowService { .. } | Record::RemoveService { .. } => SERVICES,
            Record::AddLocalUser { .. } | Record::RemoveLocalUser { .. } => "local_user",
            Record::AddShared { .. } | Record::RemoveShared { .. } => "shared",
        }
    }

    fn is_add(&self) -> bool {
        matches!(
            self,
            Record::AllowSecret { .. }
                | Record::AllowService { .. }
                | Record::AddLocalUser { .. }
                | Record::AddShared { .. }
        )
    }

    /// The command-line words of the record: the verb and its arguments.
    pub fn words(&self) -> Vec<String> {
        let named = |verb: &str, name: &str, label: &str| {
            vec![verb.into(), name.into(), "--label".into(), label.into()]
        };
        match self {
            Record::AllowSecret { name, label } => named("allow-secret", name, label),
            Record::RemoveSecret { name, label } => named("remove-secret", name, label),
            Record::AllowService { name, label } => named("allow-service", name, label),
            Record::RemoveService { name, label } => named("remove-service", name, label),
            Record::AddLocalUser { name, label } => {
                let mut out = vec!["add-local-user".to_string(), name.clone()];
                if let Some(label) = label {
                    out.extend(["--label".to_string(), label.clone()]);
                }
                out
            }
            Record::RemoveLocalUser { name } => vec!["remove-local-user".into(), name.clone()],
            Record::AddShared { members } => std::iter::once("add-shared".to_string())
                .chain(members.iter().cloned())
                .collect(),
            Record::RemoveShared { members } => std::iter::once("remove-shared".to_string())
                .chain(members.iter().cloned())
                .collect(),
        }
    }

    fn describe(&self) -> String {
        match self {
            Record::AllowSecret { name, label } | Record::RemoveSecret { name, label } => {
                format!("secret `{name}` for label `{label}`")
            }
            Record::AllowService { name, label } | Record::RemoveService { name, label } => {
                format!("service `{name}` for label `{label}`")
            }
            Record::AddLocalUser { name, .. } | Record::RemoveLocalUser { name } => {
                format!("local user `{name}`")
            }
            Record::AddShared { members } | Record::RemoveShared { members } => {
                format!("shared block [{}]", members.join(", "))
            }
        }
    }

    fn apply(&self, doc: &mut ConfigDoc) -> Result<usize, String> {
        let one = |done: bool| usize::from(done);
        match self {
            Record::AllowSecret { name, label } | Record::AllowService { name, label } => {
                add_name(doc, self.key(), label, name).map(one)
            }
            Record::RemoveSecret { name, label } | Record::RemoveService { name, label } => {
                remove_name(doc, self.key(), label, name)
            }
            Record::AddLocalUser { name, label } => {
                add_local_user(doc, name, label.as_deref()).map(one)
            }
            Record::RemoveLocalUser { name } => remove_local_user(doc, name).map(one),
            Record::AddShared { members } => add_shared(doc, members).map(one),
            Record::RemoveShared { members } => remove_shared(doc, members).map(one),
        }
    }
}

/// One record command on the broker policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordArgs {
    /// The record.
    pub record: Record,
    /// Print the change and write nothing.
    pub dry_run: bool,
    /// Restart the broker when the list applies only at its next start.
    pub restart: bool,
}

/// The arguments of the elevated re-run of `args`: the directories of [`elevated_dirs`], then
/// the same command without `--dry-run`.
pub fn child_command(layout: &Layout, args: &RecordArgs, change: &Change) -> Vec<String> {
    let mut out = elevated_dirs(layout, change);
    out.push("policy".to_string());
    out.extend(args.record.words());
    if args.restart {
        out.push("--restart".to_string());
    }
    out
}

/// Reads the broker policy, applies the record and validates the result; nothing is written.
/// The count is how many records or names the command added or removed: 0 when an add found
/// the record already there. A remove of a record the file does not have is an error that names
/// the file.
pub fn plan_record(layout: &Layout, args: &RecordArgs) -> Result<(Change, usize), String> {
    let path = resolve(layout, &Target::Broker)?;
    let mut count = 0;
    let change = plan(layout, &Target::Broker, |doc| {
        count = args.record.apply(doc)?;
        match count > 0 || args.record.is_add() {
            true => Ok(()),
            false => Err(format!(
                "{} is not in {}",
                args.record.describe(),
                path.display()
            )),
        }
    })?;
    Ok((change, count))
}

/// A record command on this machine, with the elevation of this OS.
pub async fn edit(layout: &Layout, args: RecordArgs, elevated_child: bool) -> Result<i32, String> {
    let (mut out, mut err) = (std::io::stdout(), std::io::stderr());
    edit_with(
        layout,
        args,
        elevated_child,
        &OsElevator,
        &LiveRoles,
        &mut out,
        &mut err,
    )
    .await
}

/// A record command with the given elevation, roles and output. An add of a record that is
/// already there says so and goes on like any command that changes nothing: the broker is
/// still reloaded.
pub async fn edit_with(
    layout: &Layout,
    args: RecordArgs,
    elevated_child: bool,
    elevator: &dyn Elevator,
    roles: &dyn RoleControl,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32, String> {
    let (change, count) = plan_record(layout, &args)?;
    let what = args.record.describe();
    let removal = (!args.record.is_add() && !elevated_child).then(|| {
        let times = if count == 1 {
            "occurrence"
        } else {
            "occurrences"
        };
        (
            format!("{count} {times}"),
            change.path.clone(),
            change.after.clone(),
        )
    });
    if args.record.is_add() && count == 0 && !elevated_child {
        let _ = writeln!(out, "{what} is already present");
    }
    let opts = ApplyOpts {
        dry_run: args.dry_run,
        restart: args.restart,
        elevated_child,
        args: child_command(layout, &args, &change),
        keys: vec![args.record.key().to_string()],
    };
    let result = run_change_with(
        layout,
        &Target::Broker,
        change,
        opts,
        elevator,
        roles,
        out,
        err,
    )
    .await;
    if let (Ok(_), Some((times, path, after))) = (&result, removal) {
        if args.dry_run {
            let _ = writeln!(out, "{what}: would remove {times}");
        } else if std::fs::read_to_string(&path).is_ok_and(|text| text == after) {
            let _ = writeln!(out, "{what}: removed {times}");
        }
    }
    result
}

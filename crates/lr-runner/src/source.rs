use lr_proto::WireCode;
use std::path::PathBuf;

const NAME_PART: usize = 48;

fn fnv(parts: &[&str]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for b in part.bytes().chain(std::iter::once(0)) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

pub(crate) fn dir_name(label: &str, key: &[&str]) -> String {
    let clean: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .take(NAME_PART)
        .collect();
    format!("{clean}-{:016x}", fnv(key))
}

/// Where the jobs of a runner that run outside the `local` zone work: a work folder of the
/// job `<job root>/workspace/own/<project>-<hash>`, removed when the job ends. The name part is
/// reduced to `[A-Za-z0-9._-]` and 48 characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceLayout {
    root: PathBuf,
}

impl WorkspaceLayout {
    /// The layout inside the runner's job root.
    pub fn new(job_root: PathBuf) -> WorkspaceLayout {
        WorkspaceLayout {
            root: job_root.join("workspace"),
        }
    }

    /// The work folder of one job: the steps of a job that runs outside the `local` zone start
    /// here and bring the code themselves; two jobs of a project never share it.
    pub fn own_dir(&self, project: &str, job: &str) -> PathBuf {
        self.root
            .join("own")
            .join(dir_name(project, &[project, job]))
    }
}

/// Where the steps of a job run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePlan {
    /// Directories the runner creates before the steps.
    pub make_dirs: Vec<PathBuf>,
    /// Working directory of the steps.
    pub job_cwd: PathBuf,
}

/// Plans the working directory of a job: the submitter's directory for the `local` zone
/// (`WorkTree`), a work folder of the job otherwise (`Own`).
pub fn plan_source(
    layout: &WorkspaceLayout,
    code: &WireCode,
    project: &str,
    job: &str,
) -> SourcePlan {
    match code {
        WireCode::WorkTree { cwd } => SourcePlan {
            make_dirs: Vec::new(),
            job_cwd: PathBuf::from(cwd),
        },
        WireCode::Own => {
            let dir = layout.own_dir(project, job);
            SourcePlan {
                make_dirs: vec![dir.clone()],
                job_cwd: dir,
            }
        }
    }
}

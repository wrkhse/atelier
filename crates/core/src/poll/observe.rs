use std::collections::BTreeMap;

use super::{Listed, Plugins, Project, Record, State};
use crate::{Filter, Issue, IssueKey, Listing, Phase, PluginError, Seen};

/// What one poll saw of a project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observation {
    pub issues: BTreeMap<IssueKey, Watched>,
    /// Requests the project's worker read for this poll.
    pub requests: BTreeMap<IssueKey, Vec<super::Request>>,
    /// Listing tags to keep once every issue is acted on.
    pub etags: BTreeMap<(String, Listed), String>,
}

/// One issue as the poll saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watched {
    pub issue: Issue,
    pub seen: Seen,
}

/// Asks the project's trackers what changed, reads every issue atelier is
/// working on, and asks the forge for the pull request of each run whose
/// pipeline has a step of the forge's.
///
/// Each tracker is listed once per pipeline, for its trigger, and once for
/// the working mark, so an issue an earlier atelier never claimed is found
/// too.
///
/// # Errors
///
/// The first plugin failure; the poll backs off and tries again.
pub fn observe(
    project: &Project,
    plugins: &Plugins<'_>,
    state: &State,
) -> Result<Observation, PluginError> {
    let mut observation = Observation::default();

    for source in &plugins.trackers {
        for pipeline in &project.pipelines {
            let listed = Listed::Trigger(pipeline.name().to_owned());

            list(project, source, listed, state, &mut observation)?;
        }

        list(project, source, Listed::Working, state, &mut observation)?;
    }

    for (key, record) in &state.runs {
        if !record.phase.is_working() || observation.issues.contains_key(key) {
            continue;
        }

        let Some(source) = plugins.source(&key.tracker) else {
            continue;
        };

        let issue = source.tracker.issue(&project.repo, &key.id)?;
        observation.issues.insert(key.clone(), observed(issue));
    }

    for (key, observed) in &mut observation.issues {
        let Some(record) = state.runs.get(key) else {
            continue;
        };
        let forged = project.pipeline(&record.pipeline).is_some_and(|pipeline| {
            pipeline
                .steps()
                .iter()
                .any(|step| step.plugin == project.forge)
        });

        if record.phase == Phase::Running && forged {
            observed.seen.pull_request =
                plugins.forge.pull_request(&project.repo, &record.branch)?;
        }
    }

    Ok(observation)
}

fn list(
    project: &Project,
    source: &super::Source<'_>,
    listed: Listed,
    state: &State,
    observation: &mut Observation,
) -> Result<(), PluginError> {
    let filter = match listed {
        Listed::Trigger(_) => source.filter.clone(),
        Listed::Working => Filter::default(),
    };
    let key = (source.name.to_owned(), listed);
    let tag = key.1.tag();
    let etag = state.etags.get(&key).map(String::as_str);

    let Listing::Changed { issues, etag } =
        source.tracker.issues(&project.repo, tag, &filter, etag)?
    else {
        return Ok(());
    };

    for issue in issues {
        observation
            .issues
            .entry(issue.key.clone())
            .or_insert_with(|| observed(issue));
    }

    if let Some(etag) = etag {
        observation.etags.insert(key, etag);
    }

    Ok(())
}

fn observed(issue: Issue) -> Watched {
    let seen = Seen {
        open: issue.open,
        marks: issue.marks.clone(),
        pull_request: None,
    };

    Watched { issue, seen }
}

impl State {
    /// The item's run, made when atelier first takes it.
    pub(crate) fn record(&mut self, issue: &Issue, pipeline: &str) -> &mut Record {
        self.runs
            .entry(issue.key.clone())
            .or_insert_with(|| Record::new(&issue.key, &issue.title, pipeline))
    }
}

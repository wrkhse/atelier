//! GitHub issues as atelier's tracker: labels are the marks.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use atelier_core::{
    Filter, Issue, IssueKey, Listing, Marks, PluginError, Repo, Settings, SettingsError, Tag,
    Tracker,
};
use serde::Deserialize;

use crate::GitHub;
use crate::api::{api, encode, owner_and_name};
use crate::labels::{Label, Labels};

/// The most pages one listing reads, 100 issues each.
const MAX_PAGES: usize = 10;

/// The longest `gh label create` may take.
const LABEL_DEADLINE: Duration = Duration::from_secs(30);

/// GitHub as one project's tracker, with that project's labels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubTracker {
    program: PathBuf,
    labels: Labels,
}

impl GitHub {
    /// This project's tracker, from its `[[tracker]]` settings and its
    /// pipelines' names, in the project's order.
    ///
    /// # Errors
    ///
    /// A setting it cannot use; see [`Labels::from_settings`].
    pub fn tracker(
        &self,
        settings: &Settings,
        pipelines: &[&str],
    ) -> Result<GitHubTracker, SettingsError> {
        let labels = Labels::from_settings(settings, pipelines)?;

        Ok(GitHubTracker {
            program: self.program.clone(),
            labels,
        })
    }
}

#[derive(Deserialize)]
struct Raw {
    number: u64,
    title: String,
    state: String,
    labels: Vec<RawLabel>,
    pull_request: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct RawLabel {
    name: String,
}

#[derive(Deserialize)]
struct Search {
    items: Vec<Raw>,
}

impl Raw {
    fn issue(self, labels: &Labels) -> Option<Issue> {
        if self.pull_request.is_some() {
            return None;
        }

        let marks = self
            .labels
            .iter()
            .filter_map(|found| labels.tag_of(&found.name))
            .collect::<Marks>();

        Some(Issue {
            key: IssueKey {
                tracker: "github".into(),
                id: self.number.to_string(),
            },
            title: self.title,
            open: self.state == "open",
            marks,
        })
    }
}

fn json<T: for<'de> Deserialize<'de>>(body: &str, what: &str) -> Result<T, PluginError> {
    serde_json::from_str(body)
        .map_err(|error| PluginError::permanent(format!("{what}: GitHub's reply: {error}")))
}

impl GitHubTracker {
    /// This project's labels.
    #[must_use]
    pub const fn labels(&self) -> &Labels {
        &self.labels
    }

    /// The label `tag` names, or a failure naming the tag when this
    /// project has no such label.
    fn label(&self, tag: Tag<'_>) -> Result<&Label, PluginError> {
        self.labels.tag(tag).ok_or_else(|| {
            let missing = match tag {
                Tag::Trigger(pipeline) => format!("the {pipeline} pipeline's trigger"),
                Tag::Working => "working".to_owned(),
                Tag::Blocked => "blocked".to_owned(),
            };

            PluginError::permanent(format!("this project has no label for {missing}"))
        })
    }

    fn api(&self, args: &[&str]) -> Result<crate::api::Reply, PluginError> {
        api(&self.program, args)
    }

    /// Makes the project's labels in `repo`, or updates them if they exist.
    ///
    /// # Errors
    ///
    /// `gh` fails to make one.
    pub fn create_labels(&self, repo: &Repo) -> Result<(), PluginError> {
        for label in self.labels.all() {
            let name = &label.name;
            let mut command = Command::new(&self.program);
            command.args(["label", "create", name, "--force", "--repo", &repo.0]);
            command.args(["--color", &label.color, "--description", &label.description]);

            let output = atelier_command::run(command, LABEL_DEADLINE)?;

            if !output.success {
                return Err(PluginError::permanent(format!(
                    "gh label create {name}: {}",
                    output.stderr.trim()
                )));
            }
        }

        Ok(())
    }

    fn listing(&self, path: &str, etag: Option<&str>) -> Result<Listing, PluginError> {
        let header = etag.map(|etag| format!("If-None-Match: {etag}"));
        let mut args = vec![path];

        if let Some(header) = &header {
            args.extend(["--header", header]);
        }

        let first = self.api(&args)?.ok("listing issues")?;

        if first.status == 304 {
            return Ok(Listing::Unchanged);
        }

        let mut issues: Vec<Raw> = json(&first.body, "listing issues")?;
        let mut next = first.next().map(str::to_owned);
        let single = next.is_none();

        for _ in 1..MAX_PAGES {
            let Some(url) = next.take() else { break };
            let page = self.api(&[&url])?.ok("listing issues")?;
            issues.extend(json::<Vec<Raw>>(&page.body, "listing issues")?);
            next = page.next().map(str::to_owned);
        }

        let etag = if single {
            first.header("etag").map(str::to_owned)
        } else {
            None
        };
        let issues = issues
            .into_iter()
            .filter_map(|raw| raw.issue(&self.labels))
            .collect();

        Ok(Listing::Changed { issues, etag })
    }

    fn search(&self, repo: &Repo, label: &Label, filter: &Filter) -> Result<Listing, PluginError> {
        let query = filter.query.as_deref().unwrap_or_default();
        let mut terms = vec![format!("repo:{}", repo.0), "is:issue is:open".into()];
        terms.push(format!("label:\"{}\"", label.name));
        terms.extend(
            filter
                .labels
                .iter()
                .map(|extra| format!("label:\"{extra}\"")),
        );
        terms.extend(filter.assignee.iter().map(|who| format!("assignee:{who}")));
        terms.extend(filter.author.iter().map(|who| format!("author:{who}")));
        terms.push(query.to_owned());

        let path = format!(
            "search/issues?per_page=100&q={}",
            encode(terms.join(" ").trim())
        );
        let reply = self.api(&[&path])?.ok("searching issues")?;
        let search: Search = json(&reply.body, "searching issues")?;
        let issues = search
            .items
            .into_iter()
            .filter_map(|raw| raw.issue(&self.labels))
            .collect();

        Ok(Listing::Changed { issues, etag: None })
    }
}

impl Tracker for GitHubTracker {
    /// Lists with `If-None-Match`, so an unchanged repository answers 304
    /// and costs no rate limit. A filter's `query` goes through GitHub
    /// search instead, with no tag. At most ten pages of 100 are read, and a
    /// listing of more than one page keeps no tag.
    fn issues(
        &self,
        repo: &Repo,
        tag: Tag<'_>,
        filter: &Filter,
        etag: Option<&str>,
    ) -> Result<Listing, PluginError> {
        owner_and_name(repo)?;

        let label = self.label(tag)?;

        if filter.query.is_some() {
            return self.search(repo, label, filter);
        }

        let mut labels = vec![label.name.clone()];
        labels.extend(filter.labels.iter().cloned());

        let mut query = vec![
            "state=open".to_owned(),
            "per_page=100".to_owned(),
            format!("labels={}", encode(&labels.join(","))),
        ];
        query.extend(
            filter
                .assignee
                .iter()
                .map(|who| format!("assignee={}", encode(who))),
        );
        query.extend(
            filter
                .author
                .iter()
                .map(|who| format!("creator={}", encode(who))),
        );

        let path = format!("repos/{}/issues?{}", repo.0, query.join("&"));

        self.listing(&path, etag)
    }

    fn issue(&self, repo: &Repo, id: &str) -> Result<Issue, PluginError> {
        let what = format!("reading #{id}");
        let reply = self
            .api(&[&format!("repos/{}/issues/{}", repo.0, encode(id))])?
            .ok(&what)?;
        let raw: Raw = json(&reply.body, &what)?;

        raw.issue(&self.labels)
            .ok_or_else(|| PluginError::permanent(format!("#{id} is a pull request")))
    }

    fn mark(&self, repo: &Repo, id: &str, tag: Tag<'_>) -> Result<(), PluginError> {
        let label = self.label(tag)?;
        let path = format!("repos/{}/issues/{}/labels", repo.0, encode(id));
        let field = format!("labels[]={}", label.name);

        self.api(&["--method", "POST", &path, "--raw-field", &field])?
            .ok(&format!("labelling #{id}"))?;

        Ok(())
    }

    /// A label already gone answers 404, which is success here, and so is a
    /// tag this project has no label for: that is a pipeline it no longer
    /// has. Its trigger is ignored, so the unmark settles.
    fn unmark(&self, repo: &Repo, id: &str, tag: Tag<'_>) -> Result<(), PluginError> {
        let Some(label) = self.labels.tag(tag) else {
            return Ok(());
        };

        let path = format!(
            "repos/{}/issues/{}/labels/{}",
            repo.0,
            encode(id),
            encode(&label.name)
        );
        let reply = self.api(&["--method", "DELETE", &path])?;

        if reply.status == 404 {
            return Ok(());
        }

        reply.ok(&format!("unlabelling #{id}"))?;
        Ok(())
    }

    fn comment(&self, repo: &Repo, id: &str, text: &str) -> Result<(), PluginError> {
        let path = format!("repos/{}/issues/{}/comments", repo.0, encode(id));
        let field = format!("body={text}");

        self.api(&["--method", "POST", &path, "--raw-field", &field])?
            .ok(&format!("commenting on #{id}"))?;

        Ok(())
    }

    fn close(&self, repo: &Repo, id: &str) -> Result<(), PluginError> {
        let path = format!("repos/{}/issues/{}", repo.0, encode(id));
        let fields = [
            "--raw-field",
            "state=closed",
            "--raw-field",
            "state_reason=completed",
        ];
        let mut args = vec!["--method", "PATCH", path.as_str()];
        args.extend(fields);

        self.api(&args)?.ok(&format!("closing #{id}"))?;

        Ok(())
    }

    fn reference(&self, issue: &Issue) -> String {
        format!("#{}", issue.key.id)
    }

    fn brief(&self, repo: &Repo, issue: &Issue) -> String {
        let id = &issue.key.id;
        let repo = &repo.0;
        let blocked = &self.labels.blocked.name;

        format!(
            "The issue is #{id} in {repo}. Read it with its comments: \
             `gh issue view {id} --repo {repo} --comments`. When only a person can \
             unblock you, mark it blocked: \
             `gh issue edit {id} --repo {repo} --add-label {blocked}`."
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_trigger_label_is_its_pipeline_and_other_labels_are_ignored() {
        let raw: Raw = serde_json::from_str(
            r#"{"number": 12, "title": "Audit", "state": "closed", "pull_request": null,
                "labels": [{"name": "atelier:audit"}, {"name": "atelier:gone"},
                           {"name": "atelier:working"}, {"name": "atelier:blocked"}]}"#,
        )
        .unwrap();

        let issue = raw.issue(&Labels::new(&["build", "audit"])).unwrap();

        assert_eq!(
            issue.marks,
            Marks::default()
                .with(Tag::Trigger("audit"))
                .with(Tag::Working)
                .with(Tag::Blocked)
        );
        assert!(!issue.open);
    }
}

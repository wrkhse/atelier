//! The labels that are atelier's marks on GitHub, set per tracker in the
//! project's config, with atelier's own as defaults: a trigger for each
//! pipeline, then working, blocked and improvement.

use atelier_core::{Settings, SettingsError, Tag};

/// One label: its name, and the color and description `atelier init`
/// gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub name: String,
    pub color: String,
    pub description: String,
}

/// atelier's labels: a trigger for each of the project's pipelines, in the
/// project's order, `working` and `blocked` for a run, and `improvement`
/// for a critic's findings that do not block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Labels {
    triggers: Vec<(String, Label)>,
    pub working: Label,
    pub blocked: Label,
    pub improvement: Label,
}

/// The keys that are no pipeline's, after each pipeline's.
const FIXED: [&str; 3] = ["working", "blocked", "improvement"];

const FIELDS: [&str; 3] = ["name", "color", "description"];

/// The longest label name GitHub accepts, in characters.
const NAME_LIMIT: usize = 50;

fn label(name: &str, color: &str, description: &str) -> Label {
    Label {
        name: name.into(),
        color: color.into(),
        description: description.into(),
    }
}

fn trigger(pipeline: &str) -> Label {
    let description = if pipeline == "build" {
        "Start an atelier builder on this issue".to_owned()
    } else {
        format!("Start atelier's {pipeline} pipeline on this issue")
    };

    label(&format!("atelier:{pipeline}"), "0e8a16", &description)
}

impl Default for Labels {
    fn default() -> Self {
        Self::new(&["build"])
    }
}

impl Labels {
    /// The defaults for a project with `pipelines`, in its order.
    #[must_use]
    pub fn new(pipelines: &[&str]) -> Self {
        Self {
            triggers: pipelines
                .iter()
                .map(|pipeline| ((*pipeline).to_owned(), trigger(pipeline)))
                .collect(),
            working: label(
                "atelier:working",
                "fbca04",
                "An atelier builder holds this issue",
            ),
            blocked: label(
                "atelier:blocked",
                "d93f0b",
                "The atelier builder stopped; the pull request says why",
            ),
            improvement: label(
                "improvement",
                "c5def5",
                "A critic's finding that does not block, kept for later",
            ),
        }
    }

    /// The defaults, with what a tracker's `labels` table sets in their
    /// place:
    ///
    /// ```toml
    /// [[tracker]]
    /// plugin = "github"
    /// labels.audit = { name = "queue:audit", color = "1d76db" }
    /// labels.working = { name = "bot:working", color = "5319e7" }
    /// ```
    ///
    /// # Errors
    ///
    /// An unknown key, a value that is not text, an empty name, a color
    /// that is not six hex digits, two labels whose names are equal ignoring
    /// case, or a trigger label longer than GitHub's 50 characters.
    pub fn from_settings(settings: &Settings, pipelines: &[&str]) -> Result<Self, SettingsError> {
        settings.only("", &["labels"])?;

        let mut labels = Self::new(pipelines);
        let keys: Vec<&str> = pipelines.iter().copied().chain(FIXED).collect();

        if let Some(table) = settings.table("", "labels")? {
            table.only("labels", &keys)?;

            for (key, label) in keys.iter().copied().zip(labels.all_mut()) {
                if let Some(fields) = table.table("labels", key)? {
                    set(label, fields, &format!("labels.{key}"))?;
                }
            }
        }

        labels.check()?;

        Ok(labels)
    }

    /// The label `tag` names, if this project has one: a pipeline's
    /// trigger, working or blocked. `improvement` is no tag.
    #[must_use]
    pub fn tag(&self, tag: Tag<'_>) -> Option<&Label> {
        match tag {
            Tag::Trigger(pipeline) => self
                .triggers
                .iter()
                .find(|(name, _)| name.as_str() == pipeline)
                .map(|(_, label)| label),
            Tag::Working => Some(&self.working),
            Tag::Blocked => Some(&self.blocked),
        }
    }

    /// Every label, in the order `atelier init` makes them: each pipeline's
    /// trigger in the project's order, then working, blocked and
    /// improvement.
    #[must_use]
    pub fn all(&self) -> Vec<&Label> {
        self.triggers
            .iter()
            .map(|(_, label)| label)
            .chain([&self.working, &self.blocked, &self.improvement])
            .collect()
    }

    /// The tag the label named `name` is, if any.
    pub(crate) fn tag_of(&self, name: &str) -> Option<Tag<'_>> {
        if name == self.working.name {
            return Some(Tag::Working);
        }

        if name == self.blocked.name {
            return Some(Tag::Blocked);
        }

        self.triggers
            .iter()
            .find(|(_, label)| label.name == name)
            .map(|(pipeline, _)| Tag::Trigger(pipeline.as_str()))
    }

    fn all_mut(&mut self) -> Vec<&mut Label> {
        self.triggers
            .iter_mut()
            .map(|(_, label)| label)
            .chain([&mut self.working, &mut self.blocked, &mut self.improvement])
            .collect()
    }

    /// Refuses two labels with one name, and a trigger's name GitHub would
    /// refuse.
    fn check(&self) -> Result<(), SettingsError> {
        for (pipeline, label) in &self.triggers {
            if label.name.chars().count() > NAME_LIMIT {
                return Err(SettingsError::new(
                    &format!("labels.{pipeline}"),
                    "name",
                    &format!("must be at most {NAME_LIMIT} characters, GitHub's limit"),
                ));
            }
        }

        let mut seen: Vec<(&str, &Label)> = Vec::new();

        for (key, label) in self.keys().into_iter().zip(self.all()) {
            let same = seen
                .iter()
                .find(|(_, other)| other.name.eq_ignore_ascii_case(&label.name));

            if let Some((earlier, _)) = same {
                return Err(SettingsError::new(
                    &format!("labels.{key}"),
                    "name",
                    &format!("{} is also labels.{earlier}'s name", label.name),
                ));
            }

            seen.push((key, label));
        }

        Ok(())
    }

    /// Each label's key under `labels`, in [`Self::all`]'s order.
    fn keys(&self) -> Vec<&str> {
        self.triggers
            .iter()
            .map(|(pipeline, _)| pipeline.as_str())
            .chain(FIXED)
            .collect()
    }
}

fn set(label: &mut Label, fields: &Settings, path: &str) -> Result<(), SettingsError> {
    fields.only(path, &FIELDS)?;

    if let Some(name) = fields.text(path, "name")? {
        if name.trim().is_empty() {
            return Err(SettingsError::new(path, "name", "must not be empty"));
        }

        name.clone_into(&mut label.name);
    }

    if let Some(color) = fields.text(path, "color")? {
        if color.len() != 6 || !color.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(SettingsError::new(
                path,
                "color",
                "must be six hex digits, such as 0e8a16",
            ));
        }

        color.clone_into(&mut label.color);
    }

    if let Some(description) = fields.text(path, "description")? {
        description.clone_into(&mut label.description);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use atelier_core::Setting;

    use super::*;

    fn text(value: &str) -> Setting {
        Setting::Text(value.into())
    }

    fn labels(key: &str, fields: Settings) -> Settings {
        let table = Settings::default().with(key, Setting::Table(fields));

        Settings::default().with("labels", Setting::Table(table))
    }

    fn build() -> Labels {
        Labels::new(&["build"])
    }

    #[test]
    fn a_project_renames_or_recolors_a_label_and_keeps_the_rest() {
        let fields = Settings::default()
            .with("name", text("bot:working"))
            .with("color", text("5319E7"));

        let labels = Labels::from_settings(&labels("working", fields), &["build"]).unwrap();

        assert_eq!(labels.tag(Tag::Working).unwrap().name, "bot:working");
        assert_eq!(labels.working.color, "5319E7");
        assert_eq!(labels.working.description, build().working.description);
        assert_eq!(
            labels.tag(Tag::Trigger("build")),
            build().tag(Tag::Trigger("build"))
        );
        assert_eq!(labels.tag(Tag::Blocked).unwrap().name, "atelier:blocked");
        assert_eq!(
            Labels::from_settings(&Settings::default(), &["build"]).unwrap(),
            build()
        );
    }

    #[test]
    fn each_pipeline_gets_its_own_trigger_label_in_the_project_s_order() {
        let labels = Labels::new(&["build", "audit"]);

        assert_eq!(
            labels.tag(Tag::Trigger("audit")).unwrap().name,
            "atelier:audit"
        );
        assert_eq!(
            labels.tag(Tag::Trigger("audit")).unwrap().description,
            "Start atelier's audit pipeline on this issue"
        );
        assert_eq!(
            labels.tag(Tag::Trigger("build")).unwrap().description,
            "Start an atelier builder on this issue"
        );
        assert_eq!(labels.tag(Tag::Trigger("gone")), None);
        assert_eq!(
            labels
                .all()
                .iter()
                .map(|label| label.name.as_str())
                .collect::<Vec<_>>(),
            [
                "atelier:build",
                "atelier:audit",
                "atelier:working",
                "atelier:blocked",
                "improvement"
            ]
        );
    }

    #[test]
    fn a_pipeline_s_trigger_label_is_set_by_its_own_key() {
        let fields = Settings::default()
            .with("name", text("queue:audit"))
            .with("color", text("1d76db"));
        let labels = Labels::from_settings(&labels("audit", fields), &["build", "audit"]).unwrap();

        assert_eq!(
            labels.tag(Tag::Trigger("audit")).unwrap().name,
            "queue:audit"
        );
        assert_eq!(labels.tag(Tag::Trigger("audit")).unwrap().color, "1d76db");
        assert_eq!(
            labels.tag(Tag::Trigger("build")).unwrap().name,
            "atelier:build"
        );
    }

    #[test]
    fn a_setting_it_cannot_use_is_named() {
        let cases = [
            (
                Settings::default().with("lables", text("x")),
                "lables: unknown; it takes labels",
            ),
            (
                labels("wroking", Settings::default()),
                "labels.wroking: unknown; it takes build, working, blocked, improvement",
            ),
            (
                labels("build", Settings::default().with("colour", text("x"))),
                "labels.build.colour: unknown; it takes name, color, description",
            ),
            (
                labels("build", Settings::default().with("name", text(" "))),
                "labels.build.name: must not be empty",
            ),
            (
                labels("blocked", Settings::default().with("color", text("red"))),
                "labels.blocked.color: must be six hex digits, such as 0e8a16",
            ),
            (
                labels("blocked", Settings::default().with("color", text("12345g"))),
                "labels.blocked.color: must be six hex digits, such as 0e8a16",
            ),
            (
                labels(
                    "improvement",
                    Settings::default().with("description", Setting::Flag(true)),
                ),
                "labels.improvement.description: must be text",
            ),
        ];

        for (settings, message) in cases {
            assert_eq!(
                Labels::from_settings(&settings, &["build"])
                    .unwrap_err()
                    .to_string(),
                message
            );
        }
    }

    #[test]
    fn the_unknown_key_message_lists_each_pipeline_in_the_project_s_order() {
        let error =
            Labels::from_settings(&labels("wroking", Settings::default()), &["build", "audit"])
                .unwrap_err();

        assert_eq!(
            error.to_string(),
            "labels.wroking: unknown; it takes build, audit, working, blocked, improvement"
        );
    }

    #[test]
    fn two_labels_with_one_name_are_refused_however_they_differ_in_case() {
        let pipelines = ["build", "audit"];
        let cases = [
            (
                labels(
                    "working",
                    Settings::default().with("name", text("atelier:audit")),
                ),
                "labels.working.name: atelier:audit is also labels.audit's name",
            ),
            (
                labels(
                    "audit",
                    Settings::default().with("name", text("atelier:blocked")),
                ),
                "labels.blocked.name: atelier:blocked is also labels.audit's name",
            ),
            (
                labels(
                    "audit",
                    Settings::default().with("name", text("ATELIER:BUILD")),
                ),
                "labels.audit.name: ATELIER:BUILD is also labels.build's name",
            ),
        ];

        for (settings, message) in cases {
            assert_eq!(
                Labels::from_settings(&settings, &pipelines)
                    .unwrap_err()
                    .to_string(),
                message
            );
        }
    }

    #[test]
    fn pipelines_whose_labels_differ_only_in_case_are_refused() {
        let error = Labels::from_settings(&Settings::default(), &["Build", "build"]).unwrap_err();

        assert_eq!(
            error.to_string(),
            "labels.build.name: atelier:build is also labels.Build's name"
        );
    }

    #[test]
    fn a_trigger_label_longer_than_github_allows_is_refused() {
        let long = "a".repeat(51);
        let fields = Settings::default().with("name", text(&long));

        let error =
            Labels::from_settings(&labels("audit", fields), &["build", "audit"]).unwrap_err();

        assert_eq!(
            error.to_string(),
            "labels.audit.name: must be at most 50 characters, GitHub's limit"
        );

        let fits = "a".repeat(50);
        let fields = Settings::default().with("name", text(&fits));

        assert!(Labels::from_settings(&labels("audit", fields), &["build", "audit"]).is_ok());
    }
}

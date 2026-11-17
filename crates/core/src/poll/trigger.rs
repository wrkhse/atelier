//! Which pipelines an item's trigger marks ask for.

use crate::{Marks, Pipeline, Project, Tag};

/// The project's pipelines whose trigger the item carries, in the project's
/// order; a trigger the project has no pipeline for is ignored.
pub(super) fn triggered<'p>(marks: &Marks, project: &'p Project) -> Vec<&'p Pipeline> {
    project
        .pipelines
        .iter()
        .filter(|pipeline| marks.has(Tag::Trigger(pipeline.name())))
        .collect()
}

//! What a poll sees of one issue: whether it is open, its marks, and the
//! pull request on its branch.

use std::collections::BTreeSet;

use crate::Mark;

/// A mark as a tracker carries it: a pipeline's trigger, or one of
/// atelier's own marks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tag<'a> {
    /// The mark that asks for the pipeline called this.
    Trigger(&'a str),
    /// Atelier is working on the item.
    Working,
    /// Atelier stopped for good, and says why.
    Blocked,
}

impl Mark {
    /// The tag this mark carries on an item: a request is the trigger of the
    /// pipeline asked for, and the other marks are atelier's own.
    #[must_use]
    pub const fn on(self, pipeline: &str) -> Tag<'_> {
        match self {
            Self::Requested => Tag::Trigger(pipeline),
            Self::Working => Tag::Working,
            Self::Blocked => Tag::Blocked,
        }
    }
}

/// The marks an item carries: atelier's working and blocked marks, and one
/// trigger for each pipeline asked for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Marks {
    working: bool,
    blocked: bool,
    triggers: BTreeSet<String>,
}

impl Marks {
    /// Whether the item carries this tag.
    #[must_use]
    pub fn has(&self, tag: Tag<'_>) -> bool {
        match tag {
            Tag::Trigger(pipeline) => self.triggers.contains(pipeline),
            Tag::Working => self.working,
            Tag::Blocked => self.blocked,
        }
    }

    /// The marks with this tag added.
    #[must_use]
    pub fn with(mut self, tag: Tag<'_>) -> Self {
        match tag {
            Tag::Trigger(pipeline) => self.triggers.extend([pipeline.to_owned()]),
            Tag::Working => self.working = true,
            Tag::Blocked => self.blocked = true,
        }

        self
    }

    /// The marks with this tag removed.
    #[must_use]
    pub fn without(mut self, tag: Tag<'_>) -> Self {
        match tag {
            Tag::Trigger(pipeline) => self.triggers.retain(|kept| kept != pipeline),
            Tag::Working => self.working = false,
            Tag::Blocked => self.blocked = false,
        }

        self
    }

    /// Each pipeline whose trigger the item carries, in name order.
    pub fn triggers(&self) -> impl Iterator<Item = &str> {
        self.triggers.iter().map(String::as_str)
    }
}

impl<'a> FromIterator<Tag<'a>> for Marks {
    fn from_iter<I: IntoIterator<Item = Tag<'a>>>(tags: I) -> Self {
        tags.into_iter().fold(Self::default(), Self::with)
    }
}

/// The combined state of the checks on a pull request's head commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rollup {
    Pending,
    Passed,
    Failed,
}

/// A pull request on an issue's branch, as its forge reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullRequest {
    /// `failed_rounds` counts head commits that had a failed check.
    Open {
        number: u64,
        draft: bool,
        failed_rounds: u8,
        checks: Rollup,
    },
    Merged {
        number: u64,
    },
    Closed {
        number: u64,
    },
}

impl PullRequest {
    /// The pull request's number on its forge.
    #[must_use]
    pub const fn number(self) -> u64 {
        match self {
            Self::Open { number, .. } | Self::Merged { number } | Self::Closed { number } => number,
        }
    }
}

/// One issue as a poll saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    pub open: bool,
    pub marks: Marks,
    pub pull_request: Option<PullRequest>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_are_added_and_removed_one_tag_at_a_time() {
        let marks: Marks = [Tag::Trigger("audit"), Tag::Blocked].into_iter().collect();

        assert!(marks.has(Tag::Trigger("audit")) && marks.has(Tag::Blocked));
        assert!(!marks.has(Tag::Working));
        assert!(marks.clone().with(Tag::Working).has(Tag::Working));
        assert!(
            !marks
                .clone()
                .without(Tag::Trigger("audit"))
                .has(Tag::Trigger("audit"))
        );
        assert_eq!(marks.triggers().collect::<Vec<_>>(), ["audit"]);
        assert!(!marks.without(Tag::Blocked).has(Tag::Blocked));
        assert_eq!(PullRequest::Merged { number: 3 }.number(), 3);
        assert_eq!(PullRequest::Closed { number: 4 }.number(), 4);
    }
}

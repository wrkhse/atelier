# Roles

Each role is a trait a plugin implements, with the values that cross it. A
plugin's `register` adds the roles it fills to the `Registry`. A tracker is
registered as a constructor, so each project builds its own from its
`[[tracker]]` settings and its pipelines' names; a forge and an agent are one
instance each.

| File        | Holds                                                                 |
| ----------- | --------------------------------------------------------------------- |
| `traits.rs` | `Tracker`, `Forge`, `Agent` and `Worktrees`                           |
| `step.rs`   | `Step`: kinds, starting and observing a step, with its `RunContext`   |
| `issue.rs`  | `Repo`, `IssueKey`, `Issue`, `Filter`, `Listing` and `Session`        |
| `error.rs`  | `PluginError` and its `Failure`: transient, rate-limited or permanent |

```rust
impl Plugin for GitHub {
    fn name(&self) -> &'static str {
        "github"
    }

    fn register<'a>(&'a self, registry: &mut Registry<'a>) {
        registry.tracker(|settings, pipelines| {
            Ok(Box::new(self.tracker(settings, pipelines)?))
        });
        registry.forge(self);
    }
}

let registry = Registry::new(&[&GitHub, &OpenCode]);

let tracker = registry.find_tracker("jira");
// → Err: this atelier has no tracker plugin "jira"; it has github
```

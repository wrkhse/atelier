#!/bin/bash
# Prints atelier when the workflow runs from the repository it checks
# (atelier's own CI), so atelier's floor applies: both core proofs and the
# mutants canary. Prints nothing for any other caller.
set -euo pipefail
if [[ -n "${WORKFLOW_REPOSITORY:-}" && "$WORKFLOW_REPOSITORY" == "${GITHUB_REPOSITORY:-}" ]]; then
  echo atelier
fi

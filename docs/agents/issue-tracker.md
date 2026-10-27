# Issue tracker: GitHub

Tasks live in GitHub Issues for Lqm1/simeji-windows. Agreed product requirements live in `docs/specification.md`; link to that document from issues instead of maintaining a second product specification.
Use the gh CLI from this repository for issue operations.

## Conventions

- Publish tickets and specs as GitHub issues.
- Read the issue body, labels, and comments before working on a ticket.
- Use the labels defined in `docs/agents/triage-labels.md`.
- Link child issues using native sub-issues when available.
  Otherwise, add "Part of #<parent>" to the child and link it from the parent.
- Record blockers using native issue dependencies when available.
  Otherwise, add "Blocked by: #<number>" to the issue body.
- For multiline bodies and comments, write the text to a temporary file
  and pass it using --body-file.

## Pull requests as a triage surface

PRs as a request surface: no.

## Skill instructions

"Publish to the issue tracker" means create a GitHub issue.
"Fetch the relevant ticket" means read its body, labels, and comments.

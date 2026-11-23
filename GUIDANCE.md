# Running a project with parallel coding agents

A reusable setup for long-running projects driven by Claude Code. It assumes one coordinator session, a few focused worker agents, and durable state on disk. Distilled from a multi-day upgrade project where the user ended up doing most of the scheduling by hand.

## Principles

**The agent owns scheduling, verification, and finding the next task.** The user should set goals and make decisions, not nudge.

| Failure mode | Prevention |
|---|---|
| All work concentrated in one agent for days | Coordinator plus focused workers, with state on disk |
| Background jobs mistaken for parallel work | Watchers are fine, but delegate independent reasoning and implementation explicitly |
| Status only arrives when the user asks | Schedule reporting at kickoff |
| User has to say "work on X while CI runs" | Dispatch the next ready task whenever a worker frees up |
| Issues parked as "see if it recurs" | Every open issue has an owner, a next action, and a resume condition |
| Fix confirmed on the target check only, breaks something nearby | Name what could regress before fixing; check both |
| Serious bug found late by an outside review | Independent review of risky changes before integration |
| Parallel jobs corrupt timing measurements | Separate agent concurrency from build/benchmark concurrency |
| Notes-only pushes trigger expensive CI | Keep live state local; batch documentation pushes |

Practices worth keeping: minimal reproductions, baseline comparisons, correcting earlier assumptions openly, grouping failures by cause, and a readable issues document.

When the user imposes ordering ("wait for all results first", "do C, then B"), record it as a constraint with a scope and an expiry. Go back to parallel work when it no longer applies.

## Operating model

Start with four agent slots and one lightweight watcher. Add agents only when there are independent tasks and enough machine resources.

| Role | Responsibility |
|---|---|
| Coordinator | Own the queue, assign work, integrate, manage CI, report |
| Investigator A | Diagnose and fix the top-priority independent issue |
| Investigator B | Work a different issue, or test a competing explanation |
| Reviewer | Review fixes, challenge assumptions, check affected behavior |
| Watcher script | Poll CI/job status and notify the coordinator of changes |

The reviewer investigates when nothing needs review. The coordinator should not disappear into a long investigation while workers sit idle.

**Maximize independent reasoning; limit competing measurements.** Worktrees isolate source. They do not isolate CPU, memory, ports, caches, build directories, databases, or package stores. Assign those explicitly. Timing-sensitive runs get exclusive use of the host.

## Setup

Nothing below runs automatically. Run each step deliberately.

### 0. Create the repository

Skip if the repo already exists. Worktrees need at least one commit.

```bash
#!/usr/bin/env bash
# Usage: edit repo_dir, then run once.
set -e

repo_dir="$HOME/src/my-project"

mkdir -p "$repo_dir"
cd "$repo_dir"
git init -b main
printf '.work/\n' > .gitignore
git add .gitignore
git commit -m "Initial commit"
```

Add `CLAUDE.md`, a `flake.nix` or `shell.nix` if the project uses Nix, and the basic build/test commands before spawning workers. Workers copy whatever conventions exist at the starting commit.

### 1. Create isolated worktrees

Edit the values at the top. `start_ref` is what workers branch from. `baseline_ref` is what results are compared against. For a brand-new repo both can be `main`.

```bash
#!/usr/bin/env bash
# Usage: edit the four variables, then run once.
set -e

repo_dir="$HOME/src/my-project"
project_slug="my-project"
start_ref="main"
baseline_ref="main"

cd "$repo_dir"
git status --short
git remote | grep -q . && git fetch --all

start_sha=$(git rev-parse "${start_ref}^{commit}")
baseline_sha=$(git rev-parse "${baseline_ref}^{commit}")
work_root="$(dirname "$repo_dir")/rv-${project_slug}"
mkdir -p "$work_root"

for role in lead fix-a fix-b review; do
  git worktree add -b "rv-${project_slug}-${role}" "$work_root/$role" "$start_sha"
done

cd "$work_root/lead"
mkdir -p .work/experiments .claude/agents
printf '%s\n' "$start_sha" > .work/starting-commit
printf '%s\n' "$baseline_sha" > .work/baseline-commit
printf '%s\n' "$project_slug" > .work/session-name
for role in lead fix-a fix-b review; do
  printf '%s\t%s\n' "$role" "$work_root/$role"
done > .work/worktrees.tsv

git worktree list
```

Only committed source reaches the worktrees. If the script stops partway, check `git worktree list` and finish the missing steps rather than rerunning blindly. If a fetch fails with `Permission denied (publickey)`, insert the YubiKey and retry; don't switch auth methods.

Before each new task, the coordinator moves an idle, clean worker onto the current integration commit. A running task stays on its recorded commit.

### 2. Define worker roles

Run from the `lead` worktree before starting Claude.

```bash
#!/usr/bin/env bash
# Usage: run from the lead worktree. Fails if the agent files already exist.
set -e

mkdir -p .claude/agents
test ! -e .claude/agents/parallel-investigator.md
test ! -e .claude/agents/parallel-reviewer.md

cat > .claude/agents/parallel-investigator.md <<'EOF'
---
name: parallel-investigator
description: Diagnose and fix one independent issue in an assigned worktree.
model: inherit
---
Work only on the task and absolute worktree path the coordinator assigned.
Check the branch and starting commit before editing. Use that worktree as
the explicit working directory for every command; shell cd does not persist
between tool calls. Read the repository instructions.

Reproduce the issue, separate facts from hypotheses, and test the smallest
useful change. Follow the repository's toolchain rules (e.g. Nix). Get a
host resource assignment before expensive jobs. Keep mutable test data,
logs, and build state separate from other workers.

Do not push, integrate other workers' changes, or edit the coordinator's
queue. Commit a ready patch on your assigned rv- branch. Report the commit,
measured result, commands and log paths, affected behavior checked, and
remaining uncertainty. A partial result is not a verified fix.
EOF

cat > .claude/agents/parallel-reviewer.md <<'EOF'
---
name: parallel-reviewer
description: Independently review a proposed fix and its validation evidence.
model: inherit
tools: Read, Glob, Grep, Bash
---
Review the explicit base and candidate commits in the assigned review
worktree. Read the repository instructions. Do not modify tracked source or
push. Put reproduction scripts and logs in the assigned scratch directory.
Follow the repository's toolchain rules. Coordinate expensive jobs with
the lead.

Check correctness, invariants, and behavior outside the target check. Try
to falsify the claimed fix. For caches, check equality, invalidation,
lifetime, and concurrency. Separate measured failures from suspected risks.
Return concrete findings, reproductions, and missing checks. A passing test
is not proof that every invariant holds.
EOF
```

Spawn two `parallel-investigator` instances (in `fix-a` and `fix-b`) and one `parallel-reviewer` (in `review`).

Custom agents also support `isolation: worktree`. If you use that instead, verify the starting commit: automatic worktrees can branch from the default branch, not the parent's current commit. [Subagent docs](https://code.claude.com/docs/en/sub-agents)

### 3. Write the kickoff prompt

Run from `lead`, then edit `.work/kickoff.md`. Fill in the objective and acceptance criteria. The default push policy is local commits only.

```bash
#!/usr/bin/env bash
# Usage: run from the lead worktree, then edit the REPLACE fields.
set -e

test ! -e .work/kickoff.md
cat > .work/kickoff.md <<'EOF'
Own this project through implementation, verification, and review.

Objective: REPLACE WITH THE CONCRETE OUTCOME.
Acceptance criteria: REPLACE WITH REQUIRED TESTS, COMPATIBILITY CHECKS,
PERFORMANCE BUDGETS, AND REVIEW REQUIREMENTS.
Starting commit: read .work/starting-commit.
Baseline: read .work/baseline-commit.
Work period: 4 hours, then checkpoint if unfinished.
Push policy: commit locally; push and trigger CI only when authorized.
Do not merge, deploy, or contact others without authorization.

Act as coordinator. Explicitly use parallel subagents.
Start two parallel-investigator instances and one parallel-reviewer.
Assign the worktree paths from .work/worktrees.tsv.
Give each worker its task, worktree, exact commit, validation criteria,
resource assignment, and the relevant instructions.
You own integration, the task queue, CI, and progress reporting.

First:
- Read repository instructions, existing docs, toolchains, and test commands.
- Establish working baseline and candidate environments.
- Create .work/state.json, runs.json, handoff.md, and experiments/.
- Record acceptance criteria and authorization boundaries.
- Dispatch independent work as soon as prerequisites are known.

Scheduling:
- Keep workers on the highest-priority ready tasks.
- When a worker finishes, review its result and assign the next task.
- While CI or builds run, continue independent diagnosis, fixes, or review.
- A blocked task blocks only its dependents.
- If a remote host or credential is unavailable, commit locally and continue
  with local work or code/log analysis. Do not retry failed authentication.
- Before waiting, check whether independent work is ready.
- After two inconclusive experiments, get an independent second opinion
  or record exactly what evidence is missing.
- Record explicit sequencing constraints and when they expire.

Parallelism:
- Verify each worker's branch and commit before it starts a task.
- Give every task a scope, owner, expected result, and validation.
- Keep mutable build/test state separate.
- Limit expensive jobs by host capacity.
- No competing workloads during timing-sensitive measurements.
- Only you integrate and push. Batch notes-only changes.

Evidence and verification:
- Reproduce failures before changing behavior where practical.
- Separate measured facts, hypotheses, and untested claims.
- Record commands, commit, versions, host, flags, and log paths.
- Validate the intended improvement and the behavior the fix could hurt.
- Independently review risky changes (concurrency, caching, persistence,
  security, public APIs) before integration.
- Recheck interacting fixes together on the integrated commit.
- Do not call a fix verified until its required checks pass.
- Do not weaken tests or accept regressions without an explicit decision.
- Confirm a metric's meaning and direction before optimizing it.

Continuous work:
- After each failure or completed run, update priorities and dispatch.
- Label partial results; list missing jobs.
- Turn confirmed recurring failures into targeted checks or short reusable
  instructions. Keep CLAUDE.md short.
- After compaction or restart, recover .work/ state, verify live jobs and
  workers, and continue. Don't assume a listed process is still alive.

Communication:
- Report meaningful findings immediately.
- While active, post a status update about every 10 minutes:
  task | owner/host | state | latest result | next action/blocker.
- Status questions do not cancel the objective.
- Ask only for decisions or authorization that block work.
  Continue everything that doesn't depend on the answer.

Finish only when the acceptance criteria are demonstrated.
At the work-period limit, or when external blockers stop progress,
write a full handoff and state clearly that the project is unfinished.
EOF
```

Acceptance criteria should cover more than green CI: required test suites, compatibility checks, performance or resource budgets, and which changes need review. If a regression is acceptable, record the decision and its scope.

### 4. Start the coordinator

From `lead`:

```bash
claude --permission-mode auto \
  --name "$(cat .work/session-name)" \
  --add-dir ../fix-a ../fix-b ../review \
  'Read .work/kickoff.md and execute it. Begin by showing the acceptance criteria, worker assignments, and first independent tasks.'
```

`--name` makes the session easy to resume. `--add-dir` gives it access to the worker directories. Auto mode still applies its approval policy; it is not blanket authorization. [CLI reference](https://code.claude.com/docs/en/cli-reference)

### 5. Turn on continuation and reporting

Once the queue and criteria exist, enter these inside Claude:

```text
/goal Complete the acceptance criteria in .work/state.json, show validation evidence for the integrated commit, and resolve review findings. Keep dispatching ready tasks per .work/kickoff.md. After 4 hours, checkpoint unfinished work with owners, blockers, live jobs, and next actions; report the project as unfinished if criteria remain unmet.
```

```text
/loop 10m Inspect the queue and live jobs. Report changes, failures, blocked work, and next actions in a compact table. Dispatch ready work to free workers without duplicating assignments. Stop at the checkpoint or when the project is finished.
```

`/goal` keeps starting work turns toward a completion condition. It can stop when the condition is met, judged impossible, or on errors. The time clause is read from the conversation; it is not a hard kill. [Goal docs](https://code.claude.com/docs/en/goal)

`/loop` fires only while the session is running and idle, so reports can be late. For work that must survive closing the terminal, use a scheduled runner. [Scheduling docs](https://code.claude.com/docs/en/scheduled-tasks)

### 6. Check status without becoming the scheduler

Inside Claude:

```text
Show the current task and run tables, including each commit and host.
Identify idle workers, ready tasks, pending reviews, and stale results.
Continue the project after answering; assign ready work immediately.
```

From another terminal (read-only):

```bash
git worktree list
for d in ../fix-a ../fix-b ../review; do git -C "$d" status --short; done
cat .work/state.json .work/runs.json
```

For GitHub Actions on the current commit:

```bash
candidate_sha=$(git rev-parse HEAD)
gh run list --commit "$candidate_sha" \
  --json databaseId,workflowName,status,conclusion,headSha,url
gh run view RUN_ID --log-failed > ".work/experiments/gha-RUN_ID-failed.log"
```

The watcher should dedupe notifications by commit, run, job, and state. An old completed run must not overwrite the current commit's status. Polling is cheap; having the model reread unchanged logs is not.

### 7. Resume after an interruption

```bash
claude --resume "$(cat .work/session-name)" --permission-mode auto \
  --add-dir ../fix-a ../fix-b ../review
```

Then paste:

```text
Read .work/kickoff.md, state.json, runs.json, and handoff.md.
Verify live processes, remote jobs, worker availability, and exact commits.
Recover finished results before rerunning anything. Replace workers that
no longer exist. Reassign ready tasks, keep current authorization, and
continue. Show whether the goal and reporting loop are still active.
```

Recreate only a missing `/goal` or `/loop`. Set a new work period explicitly.

To end a work period:

```text
Checkpoint now. Stop dispatching new work. Record each live job, its owner,
commit, log path, and next action. State which jobs keep running.
Cancel the reporting loop, clear the goal, and report unresolved work.
```

`/goal clear` stops continuation only. It does not kill shell jobs, remote builds, or CI.

## Durable state

| Artifact | Contents |
|---|---|
| `.work/state.json` | Acceptance criteria; tasks with priority, owner, dependencies, status, blockers, next action; authorization; resource assignments |
| `.work/runs.json` | Commit, versions, host, flags, process/CI ID, log paths, start/end, result |
| `.work/handoff.md` | Current state, decisions, live jobs, next actions |
| `.work/experiments/` | Reproductions, exact commands, results, rejected explanations |
| `docs/issues.md` (or similar, committed) | Readable summary of validated findings and open decisions |

Only the coordinator writes the queue. Workers report back; they don't edit shared JSON. Keep credentials out of state and logs. `.work/` stays out of commits.

Task states:

```text
ready → investigating → patch-ready → reviewing → validating → verified
                          ↘ blocked
```

"Diagnosed", "passes locally", "passes CI", and "accepted regression" are different outcomes. Record the evidence for each. A blocked task names what unblocks it.

Minimal worker assignment:

```text
Task ID and priority:
Question or failure to resolve:
Worktree and starting commit:
Files/systems in scope:
Known evidence and rejected explanations:
Host/resource allocation:
Required checks and acceptance criteria:
Expected handback: findings, patch commit, commands/logs, remaining uncertainty.
```

## Validation that prevents fix-and-break cycles

Before a fix, name what could regress. The check should follow from the change, not repeat its implementation.

| Change | Check the intended effect | Also check |
|---|---|---|
| Performance optimization | The slow case | Small inputs, memory use, cold vs warm, other callers |
| Cache or index | Hit rate / speedup | Equality and hashing consistency, invalidation, lifetime, concurrency, memory release |
| Concurrency change | Throughput under load | Single-thread behavior, different thread counts, races, shutdown |
| Dependency or toolchain upgrade | Build and tests pass | Behavior changes in release notes, performance baseline, transitive pins |
| Schema or data migration | New shape works | Existing data, rollback path, mixed-version readers |
| Public API change | New usage works | Existing callers, error paths, docs |
| Revert | Original regression gone | Whatever the reverted change was improving |

Confirm metric meaning first: which direction is better, and what it actually measures (client vs server time, total vs per-request). Label partial results. Repeat suspicious timings on a quiet host. After integrating separately validated patches, recheck them together. An old green run is evidence about the old configuration only.

## Improving across projects

After each confirmed recurring failure, add one prevention: a focused regression test, an environment preflight, or a short instruction. Keep history in `.work/experiments/`, not `CLAUDE.md`.

| Measure | Record | Better looks like |
|---|---|---|
| User nudges | Requests to continue, use idle time, assign work, report status | Fewer |
| Dispatch delay | Time from a result arriving to the next useful action | Shorter |
| Introduced regressions | Fixes later found to break something | Fewer |
| Invalid experiments | Runs redone due to wrong version, config, or interference | Fewer |
| CI churn | Runs triggered only by notes or superseded changes | Fewer |
| Useful parallelism | Independent issues resolved per work period | More done, not more processes |
| Cost | Agent usage and expensive runs per verified fix | Extra agents pay for themselves |

At each checkpoint:

```text
Review this work period. Which user nudges, repeated experiments, introduced
regressions, or idle gaps could have been prevented? Propose at most three
specific workflow changes, each tied to evidence. Apply authorized local
improvements and record how we'll measure whether they help.
```

## Agent teams

Start with coordinator-managed subagents. Agent teams help when workers need sustained discussion or direct coordination, but they're experimental and have resume limits. [Agent team docs](https://code.claude.com/docs/en/agent-teams)

```bash
CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1 claude --permission-mode auto
```

Teams don't supply resource scheduling, a durable queue, review rules, or acceptance criteria. You still need everything above.

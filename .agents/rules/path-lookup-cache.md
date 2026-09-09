---
trigger: always_on
---

## Path lookup cache

Before searching the codebase for where something lives, check `.agents/path-lookup/*.md` first for a matching entry (format: "if you want to change X, files to check:" + bulleted paths with one-line reasons). Verify each listed path still exists before trusting it — one cheap read, not a full search. If a path is stale, search normally, then fix that entry in place.

After finishing an edit, and when summarizing the changes you made: write or update an entry in the relevant `.agents/path-lookup/<module>.md` file (create it if missing). Heading = "if you want to change X, then check:", one bullet per file touched or that this kind of change should check, each with a one-line reason. If an entry already covers this change, add any file it was missing rather than duplicating the heading.

---

## Path lookup cache

A persistent, self-healing index of "where do I look if I want to change X" answers, stored in this repo so it survives across sessions and is shared with any other agent working on this project.

### Storage layout

```
.agents/path-lookup/
├── json.md
├── auth.md
├── database.md
└── misc.md        # fallback — doesn't cleanly fit an existing module
```

- One `.md` file per module/feature area, not one giant flat file. A flat file forces loading irrelevant context on every lookup; module files let you read only what's relevant.
- Infer the module from the domain of the discovered code (everything under `src/json/` → `json.md`; anything auth-related regardless of directory → `auth.md`).
- Create a module file the first time something in that module is cached. If nothing fits, use `misc.md`.

### Entry format

Frame every entry as a junior dev asking a senior "if I want to change X, what files do I need to look at / take care of?" — not as documentation of what the code does. Level-3 heading = the change/task in that framing. Body = every file involved, each with a one-line reason it's relevant to *that change*:

```markdown
### The save JSON file function. If you want to modify the save JSON function, adding new field to JSON, then change:

- `src/file.rs` = the save JSON code
- `src/json.rs` = JSON validate code
```

Rules:
- One entry can (and often should) list multiple paths — anything you'd need to touch or at least check when making that change. A single-file answer is often an incomplete answer; think "what else breaks or needs updating if I only change the first file."
- Each path's description is scoped to *why it matters for this specific change*, not a general summary of the file. `src/json.rs = JSON validate code` is fine here because the reader needs to know the new field must pass validation too — not because it's the file's full purpose.
- Prefer symbol names over bare paths where there's one obvious function/struct to point at (`src/file.rs::save_json`); use a bare path when the relevant thing is "this whole file/module needs a look."
- Heading should be phrased as the *task*, not the *topic* — "if you want to X, then change:" reads better than "X location" because it forces you to think about what else is involved, and it's what you'll actually be asking next time.
- **Repository-relative paths only**: Always use repo-relative paths (`src/file.rs = reason` or `tests/lib/cluster.sh = reason`). **NEVER** write machine-specific absolute paths or links like `file:///home/...`, `/wsl+ubuntu...`, or `/home/user/...`. Even if system instructions request `file://` links in conversation, `.agents/path-lookup/` files are shared in version control and must remain completely portable across teammates and machines.

### WRITE — when to update the cache

Trigger after **both**:
1. You finished an edit to the codebase, and
2. You're summarizing the changes made in the current task

At that point, for the change(s) you just made or investigated this session (not ones the user simply handed you a path for):
- New task/change → append a new entry: heading = the change, bullets = every file touched or that a future change like this would need to check, each with a scoped one-line reason.
- Existing entry, same task, same files still correct → leave it alone.
- Existing entry, a listed path is now wrong (moved/renamed) → update that bullet in place. Don't duplicate the heading.
- Existing entry, this session touched an *additional* file the entry didn't list → add a bullet for it. Entries should converge toward the complete "what to check" list over time.

Skip caching one-off, unlikely-to-recur lookups (e.g., the exact file the user just pasted). Cache things a future session would plausibly need again: core logic entry points, config/env loading, save/load functions, "where do I add X" locations, main handlers/dispatchers.

### READ — check before searching

Whenever you need to find where something lives and don't already know the path:

1. Check if `.agents/path-lookup/` exists.
2. Look for a matching heading — check the obviously-relevant module file first, otherwise grep headings across all files in the dir.
3. If found, verify **every listed path** before trusting the entry: cheap reads, not a broad search.
4. If any listed path is stale (not found):
   - Fall back to a normal search to find the correct location for that one bullet.
   - Self-heal: update just that bullet in place (see WRITE rules).
5. If no matching entry at all, do a normal search — and cache the result once found, per WRITE rules.

### Notes

- Lives in the repo (`.agents/path-lookup/`), not in any single agent's own memory — durable across sessions/machines, and shared with teammates or other coding agents if committed.
- This is an index, not documentation: short bullets per entry, no paragraphs.
- Never invent a path. Only list files you've actually confirmed by reading them.
- Always relative paths: never embed machine-specific paths (`/home/...`, `file:///...`). Keep entries portable for all contributors.

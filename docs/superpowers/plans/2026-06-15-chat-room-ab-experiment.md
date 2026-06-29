# Chat Room A/B Experiment — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the measurement harness + fixtures to run a 3-arm A/B experiment (solo / team-no-chat / team+chat) that produces objective data on whether foreman's chat room improves result quality and token efficiency, then run it and rank improvements from the data.

**Architecture:** Foreman gets minimal chat-log-to-JSONL persistence (one pure serializer + two append call-sites). A standalone Python toolkit (`scripts/ab/`) harvests per-agent token usage from Claude Code session transcripts, grades produced apps against a hidden acceptance suite, and aggregates per-arm distributions. Agents build a small "taskapi" REST service + CLI client from a behavior-complete but wire-contract-ambiguous spec, each arm in a fresh empty working directory.

**Tech Stack:** Rust (foreman, `serde_json`), Python 3.11+ stdlib (`json`, `pathlib`, `statistics`, `subprocess`) + `pytest` for the acceptance suite. No third-party Python deps in the agent worktrees (the built app is stdlib-only).

**Spec:** `docs/superpowers/specs/2026-06-15-chat-room-ab-experiment-design.md`

---

## File structure

| Path | Responsibility |
|------|----------------|
| `src/chat.rs` | (modify) add `ChatMsg::to_jsonl()` — pure serializer |
| `src/wm.rs` | (modify) `persist_chat_msg()` helper + two append call-sites |
| `scripts/ab/harvest_tokens.py` | locate session JSONL for a run, sum usage, split chat vs work tokens |
| `scripts/ab/test_harvest_tokens.py` | unit tests for the harvester core |
| `scripts/ab/grade.py` | run acceptance suite against a produced repo → quality record |
| `scripts/ab/test_grade.py` | unit tests for the grading core |
| `scripts/ab/aggregate.py` | per-run records → per-arm medians + 3 comparisons → results doc |
| `scripts/ab/test_aggregate.py` | unit tests for aggregation math |
| `scripts/ab/fixtures/spec.md` | the app spec agents receive |
| `scripts/ab/fixtures/requirements.json` | requirement id → description + acceptance-test node id |
| `scripts/ab/fixtures/acceptance/conftest.py` | starts a fresh server per session |
| `scripts/ab/fixtures/acceptance/test_acceptance.py` | the hidden graded suite (black-box, drives the CLI client) |
| `scripts/ab/fixtures/reference/taskapi/` | a correct reference impl — validates the acceptance suite; NEVER shown to agents |
| `scripts/ab/fixtures/prompts/*.md` | the 7 arm prompts (solo, B×3, C×3) |
| `scripts/ab/fixtures/reviewer_rubric.md` | blind reviewer agent instructions |
| `scripts/ab/run_arm.md` | the reproducible run procedure |
| `docs/chat-ab-results.md` | (output) per-arm tables + comparisons + data-ranked backlog |

**Worktree isolation:** the foreman repo holds the harness. Each experiment run happens in a **separate fresh empty directory** seeded only with a copy of `spec.md` (plus `git init` for change attribution). The acceptance suite, requirements, reference impl, prompts, and rubric never enter an agent's working directory.

**Git:** current branch is `main`. Per the repo's rules, do NOT commit on `main`. Before Task 1, create a feature branch:
```bash
git switch -c ab-chat-experiment --no-track
git branch -vv   # confirm upstream is NOT origin/main
```

---

## Phase 1 — Foreman chat-log persistence

### Task 1: `ChatMsg::to_jsonl()` pure serializer

**Files:**
- Modify: `src/chat.rs` (add a method on `impl ChatMsg`, ~after `frame()` near line 99)
- Test: `src/chat.rs` (in the existing `#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test** — add to `mod tests` in `src/chat.rs`:

```rust
#[test]
fn to_jsonl_emits_one_line_with_all_fields() {
    let mut log = ChatLog::new();
    let m = log.post_re(
        "t7", "mech", "status is an enum",
        vec!["t6".into()], Some(1),
    );
    let line = m.to_jsonl();
    assert!(!line.contains('\n'), "one object, no embedded newline");
    let v: serde_json::Value = serde_json::from_str(&line).expect("valid JSON");
    assert_eq!(v["seq"], 1);
    assert_eq!(v["from"], "t7");
    assert_eq!(v["name"], "mech");
    assert_eq!(v["text"], "status is an enum");
    assert_eq!(v["to"][0], "t6");
    assert_eq!(v["re"], 1);
    assert_eq!(v["kind"], "post");
    assert!(v["at_ms"].as_u64().is_some(), "epoch millis present");
}
```

- [ ] **Step 2: Run it, verify it fails**

Run: `cargo test --lib chat::tests::to_jsonl_emits_one_line_with_all_fields 2>&1 | Select-Object -Last 20`
Expected: FAIL — `no method named to_jsonl`.

- [ ] **Step 3: Implement `to_jsonl()`** — add inside `impl ChatMsg` in `src/chat.rs`:

```rust
    /// One-line JSON record for the on-disk transcript (append-only JSONL).
    /// Pure — no IO. `at` is rendered as Unix epoch milliseconds; `kind` as a
    /// lowercase tag. The file writer (wm.rs) adds the trailing newline.
    pub fn to_jsonl(&self) -> String {
        let at_ms = self
            .at
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let kind = match self.kind {
            ChatKind::Post => "post",
            ChatKind::Joined => "joined",
            ChatKind::Exited => "exited",
        };
        serde_json::json!({
            "seq": self.seq,
            "from": self.from,
            "name": self.name,
            "text": self.text,
            "to": self.to,
            "re": self.re,
            "kind": kind,
            "at_ms": at_ms,
        })
        .to_string()
    }
```

- [ ] **Step 4: Run it, verify it passes**

Run: `cargo test --lib chat::tests::to_jsonl_emits_one_line_with_all_fields 2>&1 | Select-Object -Last 20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/chat.rs
git commit -m "feat(chat): pure ChatMsg::to_jsonl serializer for transcript persistence"
```

---

### Task 2: Persist posts to `%APPDATA%\foreman\chat-<project>.jsonl`

**Files:**
- Modify: `src/wm.rs` — add a free fn `persist_chat_msg`; call it in `chat_post_re` (after line 1408) and `chat_post_human` (after line 1440)
- Test: `src/wm.rs` (`#[cfg(test)] mod tests`)

The writer is **best-effort**: a failed write must never break a post (chat works even if disk is read-only). The directory is overridable by `FOREMAN_CHAT_LOG_DIR` so tests can target a temp dir.

- [ ] **Step 1: Write the failing test** — add to `mod tests` in `src/wm.rs`:

```rust
#[test]
fn persist_chat_msg_appends_jsonl_line() {
    let dir = std::env::temp_dir().join(format!("foreman-chat-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::env::set_var("FOREMAN_CHAT_LOG_DIR", &dir);

    let mut log = crate::chat::ChatLog::new();
    let m1 = log.post("t1", "a", "first");
    persist_chat_msg("p1", m1);
    let m2 = log.post("t2", "b", "second");
    persist_chat_msg("p1", m2);

    let path = dir.join("chat-p1.jsonl");
    let body = std::fs::read_to_string(&path).expect("log file written");
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 2, "one line per post");
    let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(v["text"], "first");

    std::env::remove_var("FOREMAN_CHAT_LOG_DIR");
    let _ = std::fs::remove_dir_all(&dir);
}
```

- [ ] **Step 2: Run it, verify it fails**

Run: `cargo test --lib wm::tests::persist_chat_msg_appends_jsonl_line 2>&1 | Select-Object -Last 20`
Expected: FAIL — `cannot find function persist_chat_msg`.

- [ ] **Step 3: Implement the helper** — add near the other free fns in `src/wm.rs` (e.g. beside `term_tag` / `display_name`):

```rust
/// Append one chat entry to `<dir>/chat-<project>.jsonl`, where <dir> is
/// `FOREMAN_CHAT_LOG_DIR` if set, else `%APPDATA%\foreman`. Best-effort:
/// any failure is logged to stderr and swallowed — a post must never fail
/// because the transcript could not be written.
fn persist_chat_msg(project: &str, msg: &crate::chat::ChatMsg) {
    use std::io::Write;
    let dir = match std::env::var("FOREMAN_CHAT_LOG_DIR") {
        Ok(d) => std::path::PathBuf::from(d),
        Err(_) => match std::env::var("APPDATA") {
            Ok(a) => std::path::Path::new(&a).join("foreman"),
            Err(_) => return, // no place to write; skip silently
        },
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("chat persist: mkdir failed: {e}");
        return;
    }
    let path = dir.join(format!("chat-{project}.jsonl"));
    let line = msg.to_jsonl();
    match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut f) => {
            if let Err(e) = writeln!(f, "{line}") {
                eprintln!("chat persist: write failed: {e}");
            }
        }
        Err(e) => eprintln!("chat persist: open failed: {e}"),
    }
}
```

- [ ] **Step 4: Wire the agent-post call-site** — in `chat_post_re`, change the tail (currently line 1408–1409):

```rust
        let msg = log.post_re(&from_tag, &name, text, targets, re);
        persist_chat_msg(project, msg);
        Ok((msg.frame(project), resolved, msg.seq))
```

- [ ] **Step 5: Wire the human-post call-site** — in `chat_post_human`, change the tail (currently line 1440–1441):

```rust
        let msg = log.post_to(Self::HUMAN_ID, Self::HUMAN_ID, text, to);
        persist_chat_msg(&project, msg);
        Some((msg.frame(&project), resolved))
```

- [ ] **Step 6: Run the test + the full chat/wm suite**

Run: `cargo test --lib wm::tests::persist_chat_msg_appends_jsonl_line 2>&1 | Select-Object -Last 20`
Expected: PASS.
Run: `cargo test 2>&1 | Select-Object -Last 20`
Expected: all existing tests still PASS (no protocol-freeze regressions).

- [ ] **Step 7: Commit**

```bash
git add src/wm.rs
git commit -m "feat(chat): best-effort JSONL persistence of posts for A/B analysis"
```

---

## Phase 2 — Measurement tooling (Python)

> Run the Python tests with `python -m pytest scripts/ab/ -v` from the repo root. Install once: `python -m pip install pytest`.

### Task 3: Token harvester core — `summarize_transcript`

The core is a **pure** function over already-parsed JSONL line dicts, so it's testable without real files. Claude Code writes one JSON object per line; assistant turns carry `message.usage`; user turns carry `message.content`. A turn counts as **coordination** if the most recent preceding user message text contains the chat injection framing prefix `"[chat "`.

**Files:**
- Create: `scripts/ab/harvest_tokens.py`
- Test: `scripts/ab/test_harvest_tokens.py`

- [ ] **Step 1: Write the failing test** — `scripts/ab/test_harvest_tokens.py`:

```python
from harvest_tokens import summarize_transcript

def _assistant(inp, out, cc=0, cr=0):
    return {"type": "assistant", "message": {"role": "assistant",
            "usage": {"input_tokens": inp, "output_tokens": out,
                      "cache_creation_input_tokens": cc,
                      "cache_read_input_tokens": cr}}}

def _user(text):
    return {"type": "user", "message": {"role": "user",
            "content": [{"type": "text", "text": text}]}}

def test_sums_all_token_fields():
    lines = [_user("build the app"), _assistant(100, 50, cc=10, cr=5)]
    s = summarize_transcript(lines)
    assert s["input_tokens"] == 100
    assert s["output_tokens"] == 50
    assert s["cache_creation_input_tokens"] == 10
    assert s["cache_read_input_tokens"] == 5
    assert s["total_tokens"] == 165

def test_splits_coordination_from_work():
    lines = [
        _user("build the app"),          # work context
        _assistant(100, 50),             # -> work
        _user("[chat p1 #4] t3: use cents not dollars"),  # chat injection
        _assistant(30, 20),              # -> coordination
    ]
    s = summarize_transcript(lines)
    assert s["work_tokens"] == 150
    assert s["coordination_tokens"] == 50

def test_handles_string_content_and_missing_usage():
    lines = [
        {"type": "user", "message": {"role": "user", "content": "plain string"}},
        {"type": "assistant", "message": {"role": "assistant"}},  # no usage
        _assistant(10, 10),
    ]
    s = summarize_transcript(lines)
    assert s["total_tokens"] == 20
```

- [ ] **Step 2: Run it, verify it fails**

Run: `python -m pytest scripts/ab/test_harvest_tokens.py -v`
Expected: FAIL — `ModuleNotFoundError: No module named 'harvest_tokens'` (or ImportError).

- [ ] **Step 3: Implement the core** — `scripts/ab/harvest_tokens.py`:

```python
"""Harvest per-agent token usage from Claude Code session transcripts.

A transcript is JSONL: one JSON object per line. Assistant turns carry
`message.usage`; user turns carry `message.content`. A turn is "coordination"
if the most recent user message before it contains the chat injection framing
prefix "[chat " — this is best-effort attribution (the raw totals are exact).
"""
from __future__ import annotations
import json
import sys
from pathlib import Path

CHAT_FRAME_PREFIX = "[chat "
_TOKEN_FIELDS = (
    "input_tokens",
    "output_tokens",
    "cache_creation_input_tokens",
    "cache_read_input_tokens",
)


def _user_text(msg: dict) -> str:
    content = msg.get("content", "")
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return " ".join(
            part.get("text", "")
            for part in content
            if isinstance(part, dict) and part.get("type") == "text"
        )
    return ""


def summarize_transcript(lines: list[dict]) -> dict:
    """Pure: fold parsed JSONL objects into a token summary."""
    totals = {f: 0 for f in _TOKEN_FIELDS}
    coordination = 0
    work = 0
    last_user_was_chat = False
    for obj in lines:
        msg = obj.get("message") or {}
        if obj.get("type") == "user" or msg.get("role") == "user":
            last_user_was_chat = CHAT_FRAME_PREFIX in _user_text(msg)
            continue
        usage = msg.get("usage")
        if not isinstance(usage, dict):
            continue
        turn = 0
        for f in _TOKEN_FIELDS:
            v = int(usage.get(f, 0) or 0)
            totals[f] += v
            turn += v
        if last_user_was_chat:
            coordination += turn
        else:
            work += turn
    out = dict(totals)
    out["total_tokens"] = sum(totals.values())
    out["coordination_tokens"] = coordination
    out["work_tokens"] = work
    return out


def parse_jsonl(path: Path) -> list[dict]:
    out = []
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            out.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    return out
```

- [ ] **Step 4: Run it, verify it passes**

Run: `python -m pytest scripts/ab/test_harvest_tokens.py -v`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add scripts/ab/harvest_tokens.py scripts/ab/test_harvest_tokens.py
git commit -m "feat(ab): token-harvester core with coordination/work split"
```

---

### Task 4: Harvester session discovery + CLI

Claude Code stores transcripts under `~/.claude/projects/<slug>/<uuid>.jsonl`. Rather than reverse-engineer the slug, **match on the `cwd` field inside each transcript and the file mtime window** — robust to slug-format changes.

**Files:**
- Modify: `scripts/ab/harvest_tokens.py` (append discovery + `main`)
- Test: `scripts/ab/test_harvest_tokens.py` (append a discovery test using a temp dir)

- [ ] **Step 1: Write the failing test** — append to `scripts/ab/test_harvest_tokens.py`:

```python
import json as _json
from pathlib import Path
from harvest_tokens import find_sessions

def test_find_sessions_matches_cwd_and_window(tmp_path):
    root = tmp_path / "projects" / "slug"
    root.mkdir(parents=True)
    target = root / "s1.jsonl"
    target.write_text(_json.dumps({"cwd": "C:/run/armA-1", "type": "user",
                                   "message": {"role": "user", "content": "hi"}}) + "\n",
                      encoding="utf-8")
    other = root / "s2.jsonl"
    other.write_text(_json.dumps({"cwd": "C:/somewhere/else"}) + "\n", encoding="utf-8")
    found = find_sessions(projects_root=tmp_path / "projects",
                          cwd="C:/run/armA-1", start_ts=0, stop_ts=2**40)
    assert [p.name for p in found] == ["s1.jsonl"]
```

- [ ] **Step 2: Run it, verify it fails**

Run: `python -m pytest scripts/ab/test_harvest_tokens.py::test_find_sessions_matches_cwd_and_window -v`
Expected: FAIL — `cannot import name 'find_sessions'`.

- [ ] **Step 3: Implement discovery + CLI** — append to `scripts/ab/harvest_tokens.py`:

```python
def _norm(p: str) -> str:
    return str(p).replace("\\", "/").rstrip("/").lower()


def find_sessions(projects_root: Path, cwd: str, start_ts: float, stop_ts: float) -> list[Path]:
    """Transcripts whose first-line `cwd` matches and whose mtime is in window."""
    projects_root = Path(projects_root)
    if not projects_root.exists():
        return []
    want = _norm(cwd)
    hits = []
    for f in projects_root.rglob("*.jsonl"):
        try:
            mtime = f.stat().st_mtime
        except OSError:
            continue
        if not (start_ts <= mtime <= stop_ts):
            # mtime is the LAST write; allow a session that started before the
            # window but finished inside it by also accepting files touched in-window.
            if mtime < start_ts or mtime > stop_ts:
                continue
        first = f.read_text(encoding="utf-8", errors="replace").splitlines()[:1]
        if not first:
            continue
        try:
            cwd_field = json.loads(first[0]).get("cwd", "")
        except json.JSONDecodeError:
            continue
        if _norm(cwd_field) == want:
            hits.append(f)
    return sorted(hits)


def default_projects_root() -> Path:
    return Path.home() / ".claude" / "projects"


def main(argv: list[str]) -> int:
    import argparse
    ap = argparse.ArgumentParser(description="Harvest token usage for one run.")
    ap.add_argument("--cwd", required=True, help="the run's working directory")
    ap.add_argument("--start", type=float, required=True, help="epoch seconds")
    ap.add_argument("--stop", type=float, required=True, help="epoch seconds")
    ap.add_argument("--projects-root", default=str(default_projects_root()))
    args = ap.parse_args(argv)
    sessions = find_sessions(Path(args.projects_root), args.cwd, args.start, args.stop)
    agg = {"sessions": len(sessions), "input_tokens": 0, "output_tokens": 0,
           "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0,
           "total_tokens": 0, "coordination_tokens": 0, "work_tokens": 0}
    for s in sessions:
        summ = summarize_transcript(parse_jsonl(s))
        for k in summ:
            agg[k] = agg.get(k, 0) + summ[k]
    json.dump(agg, sys.stdout, indent=2)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
```

- [ ] **Step 4: Run the suite, verify pass**

Run: `python -m pytest scripts/ab/test_harvest_tokens.py -v`
Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add scripts/ab/harvest_tokens.py scripts/ab/test_harvest_tokens.py
git commit -m "feat(ab): session discovery by cwd+mtime and harvester CLI"
```

---

### Task 5: Aggregation core — per-arm medians + comparisons

**Files:**
- Create: `scripts/ab/aggregate.py`
- Test: `scripts/ab/test_aggregate.py`

- [ ] **Step 1: Write the failing test** — `scripts/ab/test_aggregate.py`:

```python
from aggregate import aggregate

def rec(arm, total, reqs, bugs, secs):
    return {"arm": arm, "total_tokens": total, "requirements_pct": reqs,
            "bugs": bugs, "wall_secs": secs}

def test_medians_and_comparisons():
    records = [
        rec("A", 1000, 80, 2, 600), rec("A", 1200, 70, 3, 650), rec("A", 1100, 75, 2, 620),
        rec("C", 3000, 95, 0, 400), rec("C", 3200, 90, 1, 420), rec("C", 3100, 92, 1, 410),
    ]
    out = aggregate(records)
    assert out["arms"]["A"]["total_tokens"]["median"] == 1100
    assert out["arms"]["C"]["requirements_pct"]["median"] == 92
    # headline derived metric present
    assert "tokens_per_requirement" in out["arms"]["A"]
    # A vs C comparison computed on medians
    cmp = out["comparisons"]["A_vs_C"]
    assert cmp["total_tokens"]["A"] == 1100 and cmp["total_tokens"]["C"] == 3100
```

- [ ] **Step 2: Run it, verify it fails**

Run: `python -m pytest scripts/ab/test_aggregate.py -v`
Expected: FAIL — no module `aggregate`.

- [ ] **Step 3: Implement** — `scripts/ab/aggregate.py`:

```python
"""Aggregate per-run metric records into per-arm distributions + comparisons."""
from __future__ import annotations
import json
import statistics
import sys
from pathlib import Path

METRICS = ("total_tokens", "coordination_tokens", "work_tokens",
           "requirements_pct", "bugs", "wall_secs")
COMPARISONS = (("A", "C"), ("B", "C"), ("A", "B"))


def _dist(values: list[float]) -> dict:
    return {"median": statistics.median(values),
            "min": min(values), "max": max(values), "n": len(values)}


def aggregate(records: list[dict]) -> dict:
    arms: dict[str, list[dict]] = {}
    for r in records:
        arms.setdefault(r["arm"], []).append(r)

    arm_out = {}
    for arm, recs in arms.items():
        d = {}
        for m in METRICS:
            vals = [r[m] for r in recs if m in r and r[m] is not None]
            if vals:
                d[m] = _dist(vals)
        # derived headline: tokens per requirement-point, bugs per 1k tokens
        if "total_tokens" in d and "requirements_pct" in d:
            reqs = d["requirements_pct"]["median"] or 1
            d["tokens_per_requirement"] = round(d["total_tokens"]["median"] / reqs, 1)
        if "bugs" in d and "total_tokens" in d:
            tok = d["total_tokens"]["median"] or 1
            d["bugs_per_1k_tokens"] = round(d["bugs"]["median"] / (tok / 1000), 3)
        arm_out[arm] = d

    comparisons = {}
    for x, y in COMPARISONS:
        if x in arm_out and y in arm_out:
            key = f"{x}_vs_{y}"
            comparisons[key] = {
                m: {x: arm_out[x][m]["median"], y: arm_out[y][m]["median"]}
                for m in METRICS if m in arm_out[x] and m in arm_out[y]
            }
    return {"arms": arm_out, "comparisons": comparisons}


def _load_records(runs_dir: Path) -> list[dict]:
    out = []
    for f in sorted(runs_dir.glob("*/metrics.json")):
        out.append(json.loads(f.read_text(encoding="utf-8")))
    return out


def main(argv: list[str]) -> int:
    runs_dir = Path(argv[0]) if argv else Path("runs")
    out = aggregate(_load_records(runs_dir))
    json.dump(out, sys.stdout, indent=2)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
```

- [ ] **Step 4: Run it, verify it passes**

Run: `python -m pytest scripts/ab/test_aggregate.py -v`
Expected: 1 passed.

- [ ] **Step 5: Commit**

```bash
git add scripts/ab/aggregate.py scripts/ab/test_aggregate.py
git commit -m "feat(ab): per-arm aggregation with medians and arm comparisons"
```

---

### Task 6: Grading core — score a pytest run against requirements

`grade.py` runs the hidden acceptance suite against a produced repo with pytest's machine-readable output (`--tb=no -q` plus `--junit-xml`), maps passing test node ids to requirement ids, and computes `requirements_pct` + a test-failure `bugs` count. The XML→score function is pure and unit-tested.

**Files:**
- Create: `scripts/ab/grade.py`
- Test: `scripts/ab/test_grade.py`

- [ ] **Step 1: Write the failing test** — `scripts/ab/test_grade.py`:

```python
from grade import score_from_junit

JUNIT = """<?xml version="1.0"?>
<testsuites><testsuite tests="3" failures="1" errors="0">
  <testcase classname="test_acceptance" name="test_add_prints_id"/>
  <testcase classname="test_acceptance" name="test_list_filters_open"/>
  <testcase classname="test_acceptance" name="test_get_missing_fails">
     <failure message="assert 0 != 0">boom</failure>
  </testcase>
</testsuite></testsuites>"""

REQS = {
    "R1": {"test": "test_add_prints_id"},
    "R4": {"test": "test_list_filters_open"},
    "R8": {"test": "test_get_missing_fails"},
}

def test_scores_requirements_and_counts_failures():
    s = score_from_junit(JUNIT, REQS)
    assert s["passed"] == ["R1", "R4"]
    assert s["failed"] == ["R8"]
    assert s["requirements_pct"] == round(2 / 3 * 100, 1)
    assert s["bugs"] == 1
```

- [ ] **Step 2: Run it, verify it fails**

Run: `python -m pytest scripts/ab/test_grade.py -v`
Expected: FAIL — no module `grade`.

- [ ] **Step 3: Implement** — `scripts/ab/grade.py`:

```python
"""Grade a produced repo against the hidden acceptance suite."""
from __future__ import annotations
import json
import subprocess
import sys
import xml.etree.ElementTree as ET
from pathlib import Path


def score_from_junit(junit_xml: str, requirements: dict) -> dict:
    """Pure: map junit results to requirement pass/fail + bug count."""
    root = ET.fromstring(junit_xml)
    failed_tests = set()
    total_cases = 0
    for case in root.iter("testcase"):
        total_cases += 1
        if case.find("failure") is not None or case.find("error") is not None:
            failed_tests.add(case.get("name"))
    passed, failed = [], []
    for rid, meta in sorted(requirements.items()):
        tname = meta["test"]
        (failed if tname in failed_tests else passed).append(rid)
    total = len(requirements) or 1
    return {
        "passed": passed,
        "failed": failed,
        "requirements_pct": round(len(passed) / total * 100, 1),
        "bugs": len(failed_tests),
    }


def run_acceptance(repo: Path, acceptance_dir: Path, junit_out: Path) -> str:
    """Run the suite against `repo` (PYTHONPATH=repo) and return junit XML text."""
    env = {"PYTHONPATH": str(repo)}
    import os
    full_env = {**os.environ, **env}
    subprocess.run(
        [sys.executable, "-m", "pytest", str(acceptance_dir),
         "--junit-xml", str(junit_out), "-q", "--tb=no"],
        env=full_env, cwd=str(repo), capture_output=True, text=True,
    )
    return junit_out.read_text(encoding="utf-8")


def main(argv: list[str]) -> int:
    import argparse
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", required=True)
    ap.add_argument("--acceptance", default="scripts/ab/fixtures/acceptance")
    ap.add_argument("--requirements", default="scripts/ab/fixtures/requirements.json")
    args = ap.parse_args(argv)
    reqs = json.loads(Path(args.requirements).read_text(encoding="utf-8"))
    junit = Path(args.repo) / "_ab_junit.xml"
    xml = run_acceptance(Path(args.repo), Path(args.acceptance), junit)
    score = score_from_junit(xml, reqs)
    json.dump(score, sys.stdout, indent=2)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
```

- [ ] **Step 4: Run it, verify it passes**

Run: `python -m pytest scripts/ab/test_grade.py -v`
Expected: 1 passed.

- [ ] **Step 5: Commit**

```bash
git add scripts/ab/grade.py scripts/ab/test_grade.py
git commit -m "feat(ab): grading core mapping acceptance results to requirements"
```

---

## Phase 3 — Experiment fixtures

### Task 7: The app spec (agent-facing)

This is behavior-complete but **deliberately silent on the wire contract** (JSON field names, status codes, error bodies, the server↔client protocol). The CLI invocation interface IS fixed so grading is uniform.

**Files:**
- Create: `scripts/ab/fixtures/spec.md`

- [ ] **Step 1: Write the spec** — `scripts/ab/fixtures/spec.md`:

````markdown
# Build: `taskapi` — a task-list REST service + CLI client

Build a small task-list application in **Python 3.11+, standard library only**
(no pip installs). It has two parts that talk over HTTP:

- a **server** (the REST API + storage), and
- a **client** (a command-line tool that calls the server).

## Fixed invocation interface (you MUST honor these exactly — they are how the app is run and graded)

**Server** — `python -m taskapi.server`
- Reads `TASKAPI_PORT` from the environment (default `8080`); listens on `127.0.0.1:<port>`.
- On startup, prints exactly one line to stdout: `listening on <port>` then keeps running.
- Storage may be in-memory (fresh on each start). Ids are integers starting at 1.

**Client** — `python -m taskapi.client <command> [args]`
- Reads `TASKAPI_URL` from the environment (e.g. `http://127.0.0.1:8080`).
- Commands and required stdout / exit-code behavior:
  - `add "<title>" [--status open|done]` → create a task (default status `open`); print the new id as a bare integer on its own line; exit 0.
  - `list [--status open|done]` → print one line per task as `<id>\t<status>\t<title>`, in ascending id order; with `--status`, only matching tasks; exit 0.
  - `get <id>` → print `<id>\t<status>\t<title>`; exit 0. If the id does not exist: print an error to **stderr**, exit non-zero.
  - `done <id>` → set the task's status to `done`; exit 0. Missing id → stderr error, exit non-zero.
  - `rm <id>` → delete the task; exit 0. Missing id → stderr error, exit non-zero.

## What is intentionally NOT specified (you decide — this is the contract)

The HTTP wire contract between client and server is yours to design: URL paths,
request/response JSON shapes and field names, HTTP status codes, error body
format, and how filters are passed. The client and server must simply agree.

## Done when

All five client commands work end-to-end against your running server, matching
the stdout/exit-code behavior above.
````

- [ ] **Step 2: Verify it renders** — open the file, confirm the invocation table and the "NOT specified" section are intact (the ambiguity is the experiment's coordination surface).

- [ ] **Step 3: Commit**

```bash
git add scripts/ab/fixtures/spec.md
git commit -m "test(ab): agent-facing taskapi spec (behavior-complete, wire-ambiguous)"
```

---

### Task 8: Requirements checklist

**Files:**
- Create: `scripts/ab/fixtures/requirements.json`

- [ ] **Step 1: Write the file** — `scripts/ab/fixtures/requirements.json`:

```json
{
  "R1":  {"desc": "add prints a new integer id",                 "test": "test_add_prints_id"},
  "R2":  {"desc": "add then get returns the same open task",      "test": "test_add_then_get_roundtrip"},
  "R3":  {"desc": "list shows added tasks",                       "test": "test_list_shows_tasks"},
  "R4":  {"desc": "list --status open returns only open",         "test": "test_list_filters_open"},
  "R5":  {"desc": "list --status done returns only done",         "test": "test_list_filters_done"},
  "R6":  {"desc": "done marks a task done (get reflects it)",      "test": "test_done_marks_task"},
  "R7":  {"desc": "rm deletes a task (get then fails)",           "test": "test_rm_deletes_task"},
  "R8":  {"desc": "get missing id exits non-zero with stderr",     "test": "test_get_missing_fails"},
  "R9":  {"desc": "done missing id exits non-zero",               "test": "test_done_missing_fails"},
  "R10": {"desc": "rm missing id exits non-zero",                "test": "test_rm_missing_fails"},
  "R11": {"desc": "add --status done creates a done task",        "test": "test_add_status_done"},
  "R12": {"desc": "list returns tasks in ascending id order",      "test": "test_list_id_order"}
}
```

- [ ] **Step 2: Validate JSON**

Run: `python -c "import json,pathlib; json.loads(pathlib.Path('scripts/ab/fixtures/requirements.json').read_text())"`
Expected: no output, exit 0.

- [ ] **Step 3: Commit**

```bash
git add scripts/ab/fixtures/requirements.json
git commit -m "test(ab): 12-requirement checklist mapped to acceptance tests"
```

---

### Task 9: The hidden acceptance suite

Black-box: it starts the produced server, sets `TASKAPI_URL`, drives the client CLI as a subprocess, and asserts on stdout/exit codes. It NEVER imports the app's internals, so it works regardless of the wire contract the agents chose.

**Files:**
- Create: `scripts/ab/fixtures/acceptance/conftest.py`
- Create: `scripts/ab/fixtures/acceptance/test_acceptance.py`

- [ ] **Step 1: Write `conftest.py`** — starts a fresh server per session on a free port:

```python
import os, socket, subprocess, sys, time
import pytest


def _free_port() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


@pytest.fixture(scope="session")
def server():
    port = _free_port()
    env = {**os.environ, "TASKAPI_PORT": str(port)}
    proc = subprocess.Popen([sys.executable, "-m", "taskapi.server"],
                            env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                            text=True)
    # wait for "listening on <port>" or process death, up to 10s
    deadline = time.time() + 10
    while time.time() < deadline:
        if proc.poll() is not None:
            raise RuntimeError("server exited during startup")
        line = proc.stdout.readline()
        if line and "listening" in line:
            break
    else:
        proc.kill()
        raise RuntimeError("server did not announce readiness")
    url = f"http://127.0.0.1:{port}"
    yield url
    proc.kill()


@pytest.fixture
def client(server):
    def run(*args, env_extra=None):
        env = {**os.environ, "TASKAPI_URL": server}
        if env_extra:
            env.update(env_extra)
        return subprocess.run([sys.executable, "-m", "taskapi.client", *args],
                              env=env, capture_output=True, text=True)
    return run
```

- [ ] **Step 2: Write `test_acceptance.py`** — all 12 tests (node names match `requirements.json`):

```python
def _add(client, title, status=None):
    args = ["add", title] + (["--status", status] if status else [])
    r = client(*args)
    assert r.returncode == 0, r.stderr
    return int(r.stdout.strip())

def test_add_prints_id(client):
    tid = _add(client, "buy milk")
    assert isinstance(tid, int) and tid >= 1

def test_add_then_get_roundtrip(client):
    tid = _add(client, "write report")
    r = client("get", str(tid))
    assert r.returncode == 0
    fields = r.stdout.strip().split("\t")
    assert fields[0] == str(tid) and fields[1] == "open" and fields[2] == "write report"

def test_list_shows_tasks(client):
    tid = _add(client, "unique-list-marker")
    r = client("list")
    assert r.returncode == 0
    assert "unique-list-marker" in r.stdout

def test_list_filters_open(client):
    open_id = _add(client, "stays-open")
    done_id = _add(client, "to-be-done")
    client("done", str(done_id))
    r = client("list", "--status", "open")
    assert r.returncode == 0
    ids = [ln.split("\t")[0] for ln in r.stdout.splitlines() if ln.strip()]
    assert str(open_id) in ids and str(done_id) not in ids

def test_list_filters_done(client):
    done_id = _add(client, "done-filter-marker")
    client("done", str(done_id))
    r = client("list", "--status", "done")
    statuses = [ln.split("\t")[1] for ln in r.stdout.splitlines() if ln.strip()]
    assert statuses and all(s == "done" for s in statuses)

def test_done_marks_task(client):
    tid = _add(client, "mark-me-done")
    assert client("done", str(tid)).returncode == 0
    r = client("get", str(tid))
    assert r.stdout.strip().split("\t")[1] == "done"

def test_rm_deletes_task(client):
    tid = _add(client, "delete-me")
    assert client("rm", str(tid)).returncode == 0
    assert client("get", str(tid)).returncode != 0

def test_get_missing_fails(client):
    r = client("get", "999999")
    assert r.returncode != 0 and r.stderr.strip() != ""

def test_done_missing_fails(client):
    assert client("done", "999999").returncode != 0

def test_rm_missing_fails(client):
    assert client("rm", "999999").returncode != 0

def test_add_status_done(client):
    tid = _add(client, "born-done", status="done")
    r = client("get", str(tid))
    assert r.stdout.strip().split("\t")[1] == "done"

def test_list_id_order(client):
    a = _add(client, "order-a")
    b = _add(client, "order-b")
    r = client("list")
    ids = [int(ln.split("\t")[0]) for ln in r.stdout.splitlines() if ln.strip()]
    assert ids == sorted(ids)
```

- [ ] **Step 3: Commit** (the suite is validated in Task 10 against the reference impl)

```bash
git add scripts/ab/fixtures/acceptance/
git commit -m "test(ab): hidden black-box acceptance suite (12 tests)"
```

---

### Task 10: Reference implementation — validate the grader

A correct, minimal `taskapi` proves the acceptance suite + grader actually pass on a good app. Lives in `fixtures/reference/` and is **never** shown to agents.

**Files:**
- Create: `scripts/ab/fixtures/reference/taskapi/__init__.py` (empty)
- Create: `scripts/ab/fixtures/reference/taskapi/server.py`
- Create: `scripts/ab/fixtures/reference/taskapi/client.py`

- [ ] **Step 1: Write the server** — `scripts/ab/fixtures/reference/taskapi/server.py`:

```python
import json, os
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import urlparse, parse_qs

_TASKS = {}
_NEXT = [1]


class H(BaseHTTPRequestHandler):
    def log_message(self, *a):  # silence
        pass

    def _send(self, code, obj=None):
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        if obj is not None:
            self.wfile.write(json.dumps(obj).encode())

    def do_POST(self):
        n = int(self.headers.get("Content-Length", 0))
        body = json.loads(self.rfile.read(n) or b"{}")
        tid = _NEXT[0]; _NEXT[0] += 1
        _TASKS[tid] = {"id": tid, "title": body.get("title", ""),
                       "status": body.get("status", "open")}
        self._send(201, _TASKS[tid])

    def do_GET(self):
        u = urlparse(self.path)
        if u.path == "/tasks":
            q = parse_qs(u.query)
            items = list(_TASKS.values())
            if "status" in q:
                items = [t for t in items if t["status"] == q["status"][0]]
            self._send(200, sorted(items, key=lambda t: t["id"]))
        else:
            tid = int(u.path.rsplit("/", 1)[-1])
            if tid in _TASKS:
                self._send(200, _TASKS[tid])
            else:
                self._send(404, {"error": "not found"})

    def do_PATCH(self):
        tid = int(self.path.rsplit("/", 1)[-1])
        if tid not in _TASKS:
            return self._send(404, {"error": "not found"})
        n = int(self.headers.get("Content-Length", 0))
        body = json.loads(self.rfile.read(n) or b"{}")
        _TASKS[tid].update({k: v for k, v in body.items() if k == "status"})
        self._send(200, _TASKS[tid])

    def do_DELETE(self):
        tid = int(self.path.rsplit("/", 1)[-1])
        if _TASKS.pop(tid, None) is None:
            return self._send(404, {"error": "not found"})
        self._send(204)


def main():
    port = int(os.environ.get("TASKAPI_PORT", "8080"))
    srv = HTTPServer(("127.0.0.1", port), H)
    print(f"listening on {port}", flush=True)
    srv.serve_forever()


if __name__ == "__main__":
    main()
```

- [ ] **Step 2: Write the client** — `scripts/ab/fixtures/reference/taskapi/client.py`:

```python
import argparse, json, os, sys
from urllib import request, error


def _url(path):
    return os.environ.get("TASKAPI_URL", "http://127.0.0.1:8080") + path


def _req(method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    r = request.Request(_url(path), data=data, method=method,
                        headers={"Content-Type": "application/json"})
    try:
        with request.urlopen(r) as resp:
            raw = resp.read()
            return resp.status, (json.loads(raw) if raw else None)
    except error.HTTPError as e:
        return e.code, None


def _print_task(t):
    print(f"{t['id']}\t{t['status']}\t{t['title']}")


def main(argv):
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    a = sub.add_parser("add"); a.add_argument("title"); a.add_argument("--status", default="open")
    for name in ("get", "done", "rm"):
        p = sub.add_parser(name); p.add_argument("id", type=int)
    ls = sub.add_parser("list"); ls.add_argument("--status")
    args = ap.parse_args(argv)

    if args.cmd == "add":
        _, t = _req("POST", "/tasks", {"title": args.title, "status": args.status})
        print(t["id"]); return 0
    if args.cmd == "list":
        path = "/tasks" + (f"?status={args.status}" if args.status else "")
        _, items = _req("GET", path)
        for t in items:
            _print_task(t)
        return 0
    if args.cmd == "get":
        code, t = _req("GET", f"/tasks/{args.id}")
        if code != 200:
            print("not found", file=sys.stderr); return 1
        _print_task(t); return 0
    if args.cmd == "done":
        code, _ = _req("PATCH", f"/tasks/{args.id}", {"status": "done"})
        if code != 200:
            print("not found", file=sys.stderr); return 1
        return 0
    if args.cmd == "rm":
        code, _ = _req("DELETE", f"/tasks/{args.id}")
        if code not in (200, 204):
            print("not found", file=sys.stderr); return 1
        return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
```

- [ ] **Step 3: Run the acceptance suite against the reference impl** (this validates the grader)

Run: `python -m grade --repo scripts/ab/fixtures/reference --acceptance scripts/ab/fixtures/acceptance --requirements scripts/ab/fixtures/requirements.json` *(run from `scripts/ab/`, or with `PYTHONPATH=scripts/ab`)*
Expected JSON: `"requirements_pct": 100.0`, `"bugs": 0`, all R1–R12 in `passed`.

If anything fails, fix the acceptance suite or reference impl until green — **a grader that can't pass a correct app is worthless.**

- [ ] **Step 4: Commit**

```bash
git add scripts/ab/fixtures/reference/
git commit -m "test(ab): reference taskapi impl; acceptance suite verified 100%"
```

---

### Task 11: Arm prompts + reviewer rubric

Seven prompt files. The **task body is identical** across all (the spec); only the **coordination harness** differs. Each `foreman open`/`foreman chat` invocation uses `$env:FOREMAN_EXE` per the dispatch epic.

**Files:**
- Create: `scripts/ab/fixtures/prompts/arm_a_solo.md`
- Create: `scripts/ab/fixtures/prompts/arm_b_orchestrator.md`
- Create: `scripts/ab/fixtures/prompts/arm_b_backend.md`
- Create: `scripts/ab/fixtures/prompts/arm_b_client.md`
- Create: `scripts/ab/fixtures/prompts/arm_c_orchestrator.md`
- Create: `scripts/ab/fixtures/prompts/arm_c_backend.md`
- Create: `scripts/ab/fixtures/prompts/arm_c_client.md`
- Create: `scripts/ab/fixtures/reviewer_rubric.md`

- [ ] **Step 1: Arm A (solo)** — `arm_a_solo.md`:

```markdown
You are building an app alone. Read `spec.md` in your current directory and
build the complete `taskapi` app (server + client) to satisfy it. Work until
all five client commands work end-to-end against your running server. Do not
dispatch other agents. When finished, say "DONE" on its own line.
```

- [ ] **Step 2: Arm B orchestrator** — `arm_b_orchestrator.md`:

```markdown
You lead a 2-worker build with NO live communication between workers. Read
`spec.md`. Because the wire contract is unspecified and the workers cannot talk
to each other, YOU must design the full HTTP contract upfront (paths, JSON
shapes, status codes, error format) and write it into each worker's task.

Then dispatch exactly two workers into THIS project, each in the same working
directory, using:

  & $env:FOREMAN_EXE open --title "agent · backend" -- claude "<backend task incl. your full contract + spec>"
  & $env:FOREMAN_EXE open --title "agent · client"  -- claude "<client task incl. your full contract + spec>"

Do NOT use `foreman chat`. After dispatching, wait; when both report done,
verify the app runs end-to-end and fix integration issues yourself if needed.
Say "DONE" when the app works.
```

- [ ] **Step 3: Arm B backend** — `arm_b_backend.md`:

```markdown
Build ONLY the server half of `taskapi` (the `taskapi.server` module) per the
contract and spec your lead gave you. Honor the fixed server invocation
interface exactly. You cannot talk to the client author — build precisely to
the contract you were given. Say "DONE" when the server runs.
```

- [ ] **Step 4: Arm B client** — `arm_b_client.md`:

```markdown
Build ONLY the client half of `taskapi` (the `taskapi.client` module) per the
contract and spec your lead gave you. Honor the fixed client invocation
interface exactly. You cannot talk to the server author — build precisely to
the contract you were given. Say "DONE" when the client is complete.
```

- [ ] **Step 5: Arm C orchestrator** — `arm_c_orchestrator.md`:

```markdown
You lead a 2-worker build that coordinates LIVE in the foreman chat room. Read
`spec.md`. Do NOT design the wire contract yourself — the two workers will
negotiate it with each other in chat. Dispatch exactly two workers into THIS
project, same working directory:

  & $env:FOREMAN_EXE open --title "agent · backend" -- claude "<arm_c_backend.md contents + spec>"
  & $env:FOREMAN_EXE open --title "agent · client"  -- claude "<arm_c_client.md contents + spec>"

Then watch the chat room (read with `& $env:FOREMAN_EXE chat --history 50`) and
steer if they go wrong. Say "DONE" when the app works end-to-end.
```

- [ ] **Step 6: Arm C backend** — `arm_c_backend.md`:

```markdown
Build the SERVER half of `taskapi`. The HTTP wire contract is NOT specified —
you and the client author must agree on it. Negotiate it in the foreman chat
room BEFORE building: propose your paths and JSON shapes, e.g.

  & $env:FOREMAN_EXE chat "proposing POST /tasks {title,status} -> 201 {id,title,status}; GET /tasks?status=open"

Wait for the client author to agree or counter, converge, THEN build to the
agreed contract. Honor the fixed server invocation interface. Say "DONE" when
the server runs against the agreed contract.
```

- [ ] **Step 7: Arm C client** — `arm_c_client.md`:

```markdown
Build the CLIENT half of `taskapi`. The HTTP wire contract is NOT specified —
you and the server author must agree on it in the foreman chat room BEFORE
building. Read chat with `& $env:FOREMAN_EXE chat --history 50`, respond to the
server author's proposal (agree or counter), converge, THEN build to the agreed
contract. Honor the fixed client invocation interface. Say "DONE" when done.
```

- [ ] **Step 8: Reviewer rubric** — `reviewer_rubric.md`:

```markdown
# Blind defect review

You are given ONE produced `taskapi` repo, labeled with an opaque id (you do
NOT know which experiment arm produced it). Review for defects the acceptance
suite may miss:

1. Run the app end-to-end yourself (start server, exercise each client command).
2. Count DISTINCT defects: crashes, wrong output format, silent failures,
   ignored flags, resource leaks, contract mismatches between client/server.
3. Report as JSON: {"label": "<id>", "review_defects": <int>, "notes": ["..."]}.

Do not read the acceptance suite. Do not guess the arm. Judge only this repo.
```

- [ ] **Step 9: Commit**

```bash
git add scripts/ab/fixtures/prompts/ scripts/ab/fixtures/reviewer_rubric.md
git commit -m "test(ab): arm prompts (solo/B/C) and blind reviewer rubric"
```

---

## Phase 4 — De-risk (Phase 0 from the spec)

### Task 12: Validate the measurement chain before spending 9 builds

This is a **go/no-go gate**. Do not proceed to Phase 5 until every check is green.

- [ ] **Step 1: Confirm Claude Code writes a discoverable transcript with usage.**
  Dispatch one trivial interactive worker into a foreman project whose terminal cwd is a known temp dir, let it answer one prompt, then:

Run: `python scripts/ab/harvest_tokens.py --cwd "<that temp dir>" --start <epoch_before> --stop <epoch_now>`
Expected: JSON with `sessions >= 1` and `total_tokens > 0`.
**If `sessions == 0` or `total_tokens == 0`:** the token half is broken. STOP. Fall back per spec §9 — use `claude -p --output-format json` headless workers for arms A/B (their stdout carries a `usage` block) and require arm-C workers to self-report tokens via a final chat post. Revise Tasks 3–4 accordingly before continuing.

- [ ] **Step 2: Confirm chat persistence works.**
  In a foreman project, post one chat message, then check `%APPDATA%\foreman\chat-<project>.jsonl` exists and the last line parses as JSON with the expected `text`.
  Expected: file present, line valid.

- [ ] **Step 3: Confirm coordination/work split fires.**
  In the arm-C dry run (next step), confirm the harvester reports `coordination_tokens > 0` for a worker that received at least one `[chat …]` injection.

- [ ] **Step 4: Trivial dry-run of each arm** on a throwaway 1-requirement spec (e.g. "client `ping` prints `pong`") to shake out: prompt wiring, `foreman open`/`foreman chat` invocation from the orchestrator, "DONE" detection, the grader running against a produced repo, and `metrics.json` assembly (next task's format).
  Expected: each arm produces a repo, `grade.py` runs, `harvest_tokens.py` returns tokens, one `metrics.json` is assembled by hand.

- [ ] **Step 5: Record the go/no-go decision** in a scratch note. Only proceed if Steps 1–4 are green (or the fallback from Step 1 is in place and re-validated).

---

## Phase 5 — Run the experiment & analyze

### Task 13: Run protocol + execute 3×3

**Files:**
- Create: `scripts/ab/run_arm.md` (the documented procedure)
- Create (per run, gitignored): `runs/<arm>-<n>/metrics.json`

- [ ] **Step 1: Add `runs/` to `.gitignore`**

```bash
echo "runs/" >> .gitignore
git add .gitignore && git commit -m "chore(ab): ignore experiment run outputs"
```

- [ ] **Step 2: Write `scripts/ab/run_arm.md`** — the reproducible per-run procedure:

````markdown
# Running one arm

Constants (record with every run): model id, K=2 workers (B/C), 45-min cap,
spec hash (`git hash-object scripts/ab/fixtures/spec.md`), foreman build commit.

Interleave arms (A,B,C,A,B,C,…) so cache warm-up doesn't favor one arm.

## Per run
1. Fresh empty working dir: `mkdir runs/work-<arm>-<n>`; `cd` there; `git init`;
   copy in ONLY the spec: `copy ..\..\scripts\ab\fixtures\spec.md .`
2. Record `start_epoch` (`[int][double]::Parse((Get-Date -UFormat %s))`).
3. Open a foreman terminal whose cwd is this dir; paste the arm's orchestrator
   (or, for arm A, solo) prompt from `scripts/ab/fixtures/prompts/`.
4. Supervise until the lead/solo prints `DONE`, or the 45-min cap — then stop.
5. Record `stop_epoch`. `wall_secs = stop - start`.
6. Snapshot: `git add -A && git commit -m "arm <arm> run <n> result"` (attribution).
7. Grade:
   `python scripts/ab/grade.py --repo runs/work-<arm>-<n> > runs/<arm>-<n>/_grade.json`
8. Harvest tokens:
   `python scripts/ab/harvest_tokens.py --cwd runs/work-<arm>-<n> --start <start> --stop <stop> > runs/<arm>-<n>/_tok.json`
9. Blind review: hand the repo (relabeled) to a reviewer agent with
   `reviewer_rubric.md`; capture `review_defects`.
10. Assemble `runs/<arm>-<n>/metrics.json`:
    `{"arm":"<A|B|C>", "run":<n>, "total_tokens":…, "coordination_tokens":…,
      "work_tokens":…, "requirements_pct":…, "bugs": <grade.bugs + review_defects>,
      "wall_secs":…}`

## Collisions (arm B vs C, best-effort, qualitative)
From `git log --name-only` + the chat transcript, note whether both workers
substantively edited the same file. Record in the run's notes (feeds §7 mapping).
````

- [ ] **Step 3: Execute 3 runs per arm (9 runs total)**, following `run_arm.md`, interleaved. Produce `runs/<arm>-<n>/metrics.json` for each.
  Expected: 9 `metrics.json` files; spot-check each has all keys and plausible values (team arms should show higher `total_tokens` than solo; arm C should show non-zero `coordination_tokens`).

- [ ] **Step 4: Commit the procedure** (run outputs stay gitignored)

```bash
git add scripts/ab/run_arm.md
git commit -m "docs(ab): reproducible per-arm run procedure"
```

---

### Task 14: Aggregate & write the results doc

**Files:**
- Create: `docs/chat-ab-results.md`

- [ ] **Step 1: Aggregate**

Run: `python scripts/ab/aggregate.py runs > runs/_aggregate.json`
Expected: JSON with `arms.A/B/C` medians and `comparisons.A_vs_C / B_vs_C / A_vs_B`.

- [ ] **Step 2: Write `docs/chat-ab-results.md`** with:
  - the held-constant config (model, K, cap, spec hash, foreman commit);
  - a per-arm table (median + min/max) for total/coordination/work tokens, requirements_pct, bugs, wall_secs, tokens_per_requirement, bugs_per_1k_tokens;
  - the three comparisons (A↔C headline, **B↔C = chat's marginal value**, A↔B parallelism), each with a one-line plain reading;
  - the small-N caveat (spec §5): note any comparison whose min/max spreads overlap as "directional only — add runs before concluding";
  - collision notes from the run logs.

- [ ] **Step 3: Data-ranked backlog** — append a section that walks the spec §7 mapping table against the observed data and produces a **re-ordered** version of `docs/chat-missing-features.md`'s 9 items (most-justified-by-data first), plus any new QOL items the transcripts revealed (e.g., recurring steering failures, idle members). This is the experiment's deliverable.

- [ ] **Step 4: Commit**

```bash
git add docs/chat-ab-results.md
git commit -m "docs(ab): A/B results + data-ranked chat improvement backlog"
```

---

## Self-review (completed)

- **Spec coverage:** arms (Task 11/13), task app (7–10), all 5 metrics — tokens (3–4), bugs (6+9+reviewer 11), goal % (6+8), time (13) — replication (13), instrumentation (1–6), data→improvement mapping (14), Phase-0 de-risk (12), threats (interleaving in 13, best-effort notes in 3/13). Covered.
- **Placeholders:** none — every coded step ships complete code; fixture/procedure steps ship complete content. The only intentional "fill-in" is the agents' own app output, which is the experiment's subject, not plan content.
- **Type/name consistency:** `summarize_transcript`, `find_sessions`, `parse_jsonl`, `default_projects_root` (harvest); `aggregate`, `_dist` (aggregate); `score_from_junit`, `run_acceptance` (grade); `persist_chat_msg`, `ChatMsg::to_jsonl` (Rust); acceptance test node names match `requirements.json` R1–R12. Consistent.

# Fix flaky test: `human_post_appends_with_reserved_id_and_broadcasts_to_all_members`

**Date:** 2026-06-11
**Consensus:** root-cause fix (re-inject until delivered), no serialization, no new dependencies.

## Diagnosis (verified in code)

The test (`src/wm.rs:4097`) spawns two `cmd /c pause` ConPTY sessions, posts one chat
message via `drain_chat_posts()`, then loops on a 10s deadline waiting for both
children to exit. The injection path is lossy at exactly that moment:

- `drain_chat_posts` → `chat_broadcast` (`src/wm.rs:1452`) → `Session::inject_input`
  writes the paste-wrapped bytes **once**, immediately after spawn, before the test
  has ever called `keepalive()`/`pump()` — i.e. before the startup DSR exchange has
  resolved. Bytes injected pre-ready are eaten by the device-status scan. This trap
  is documented twice in the codebase: `Session::ready()` (`src/terminal.rs:347-352`)
  and the comment in `inject_input_reaches_child_stdin` (`src/terminal.rs:1101-1103`).
- The deferred submit `\r` (`pending_submit`, fired in `pump()` at
  `src/terminal.rs:472-477`) goes out on a fixed 150 ms timer **regardless of
  readiness**. In isolation the DSR resolves in well under 150 ms, so the `\r` lands
  post-ready and rescues the test. Under full-suite load (131 tests, dozens of
  concurrent conhost spawns) the DSR resolves late, **both** writes land pre-ready,
  both are eaten, and nothing is ever sent again — `pause` blocks forever and the
  test fails at any deadline length. That is why it fails in nearly every full run
  but passes in isolation, and why serializing the suite would only hide the race
  (it merely keeps DSR latency under the 150 ms timer).
- The sibling test `chat_broadcast_hits_members_only_excluding_sender`
  (`src/wm.rs:4248`) already ships the correct pattern: pump every session and
  **re-send the broadcast every 50 ms iteration** until the member's stdin has seen
  it, with a comment (`src/wm.rs:4269-4277`) naming this exact trap. The flaky test
  is the only chat test that injects once and waits.

## Change

**File:** `src/wm.rs` — only the test
`human_post_appends_with_reserved_id_and_broadcasts_to_all_members`
(lines ~4097–4157). No product code changes. No new dependencies.

1. **Keep unchanged:** setup (two `pause_argv()` terminals, `open_chat_window`,
   `pending_post = Some("go")`), the single `drain_chat_posts()` call, and all log
   assertions (`from == "you"`, `name == "you"`, frame prefix). These exercise the
   reserved-id/append logic and involve no PTY timing.

2. **Capture the framed line out of the log-assertion block** (the borrow of
   `wm.chat` must end before calling `chat_broadcast(&mut wm)`):

   ```rust
   let framed = {
       let log = wm.chat.borrow();
       let m = log
           .msgs()
           .iter()
           .rfind(|m| m.kind == crate::chat::ChatKind::Post)
           .expect("post missing");
       assert_eq!(m.from, "you");
       assert_eq!(m.name, "you");
       let framed = m.frame("p1");
       assert!(framed.starts_with(&format!("[chat p1 #{}] you: go", m.seq)));
       framed
   };
   ```

3. **Rewrite the exit-wait loop** to mirror `src/wm.rs:4278-4299`: pump every
   session, re-send the broadcast, then check exits:

   ```rust
   // BOTH members exit — the human excludes nobody. Bytes injected before a
   // child's startup DSR scan resolves get eaten (the documented trap; see
   // chat_broadcast_hits_members_only_excluding_sender), so pump every session
   // and RE-SEND the broadcast each iteration until both stdins have seen it —
   // deterministic instead of racing spawn latency.
   let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
   loop {
       for w in wm.windows.iter_mut() {
           if let Content::Terminal(s) = &mut w.tabs[w.active].content {
               s.keepalive();
           }
       }
       wm.chat_broadcast(None, &framed, None);
       let mut done = 0;
       for id in [a, b] {
           let w = wm.windows.iter_mut().find(|w| w.id == id).unwrap();
           let Content::Terminal(s) = &mut w.tabs[w.active].content else {
               panic!()
           };
           if s.exited().is_some() {
               done += 1;
           }
       }
       if done == 2 {
           break;
       }
       assert!(
           std::time::Instant::now() < deadline,
           "a member never got the post"
       );
       std::thread::sleep(std::time::Duration::from_millis(50));
   }
   ```

   Safety of the re-send: `chat_broadcast` only injects into live (`exited()` is
   `None`) member terminal tabs, so an already-exited member is never written to;
   duplicate bytes into a still-pending `pause` are harmless (any byte exits it);
   the chat viewer tab is `Content::Chat` and is skipped by the `if let`. The
   re-send injects without appending to the log, so the log assertions above are
   unaffected. `from: None` matches the original semantics — a human post excludes
   nobody.

## Affected tests

- Modified: `wm::tests::human_post_appends_with_reserved_id_and_broadcasts_to_all_members` only.
- All other tests untouched; no shared state introduced, suite stays fully parallel.

## Verification

App is running from `target\release` — debug `cargo test` only, never `--release`,
never kill `foreman`.

1. `cargo test human_post_appends -- --nocapture` — passes in isolation.
2. `cargo test chat` — the 41 chat-named tests stay green in parallel (~0.6 s).
3. **Flake-gone evidence:** the failure reproduced in nearly every full parallel
   run, so consecutive green full runs are strong evidence. Run the full suite
   3 times: `1..3 | ForEach-Object { cargo test 2>&1 | Select-String 'test result' }`
   — expect `131 passed; 0 failed` all three times. (Pre-fix baseline already
   established by the orchestrator: fails nearly every full run, incl. on b9ced17.)
4. `cargo fmt` afterward (repo was just formatted crate-wide in b9ced17).

## Future work (out of scope)

The same pre-ready swallow exists in production: a chat post broadcast to a
just-dispatched member can be eaten if it arrives before that session's DSR scan
resolves. Candidate product fix: have `inject_input` queue bytes until `ready`
latches in `pump()` (the flag already exists at `src/terminal.rs:350`), then flush.
File separately; not needed to deflake this test.

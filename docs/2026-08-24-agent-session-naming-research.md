# Open-source agent GUI session-naming research

Date: 2026-08-24

> **Implementation decision (2026-08-25):** The findings below remain useful,
> but the accepted v1 design does not use a deterministic first-prompt title or
> a Codex-only backend. Foreman keeps the generic agent label until one
> asynchronous result from the user's selected Codex, Claude, or Grok CLI. See
> [the subsystem contract](agent-session-naming.md) for the implemented design.

## Question

How do open-source GUI or orchestration layers for coding and tool-using agents name chat, session, workspace, or task history entries, and what should Foreman copy for Claude, Codex, and Grok terminal titles?

## Method and evidence standard

This review inspected first-party source at pinned commits. Repository documentation and maintainer issues are used only where they clarify the inspected implementation. Unless a statement is explicitly marked **Inference** or **Unknown**, it is a verified source-code fact.

| Project | Revision inspected | Commit date |
|---|---|---|
| OpenCode | `3ef72fe8f6c54a31e9709e6dff82dc609df8e453` | 2026-08-24 |
| OpenHands UI | `150e76046db026dd944df0506642dc9b7b99391e` | 2026-08-24 |
| OpenHands SDK/server | `041078f26698ccba4b78af6c3069e37bb1556b32` | 2026-08-24 |
| Cline | `f91af30401fe09f9cb2217b2d3645ff6f2aa662e` | 2026-08-24 |
| Roo Code | `b867ec9145750d0ae1ff7f02d35406e9bf2a0b16` | 2026-05-15 |
| Vibe Kanban | `4deb7eca8f381f7cbc1f9d15515a9ab8f8009053` | 2026-04-24 |
| AiderDesk | `2a8bc7f1244473688a02842e3089e8775a7b2116` | 2026-08-25 UTC |
| Agent Cockpit | `2b45ced031eff9f628c98dd93872b5e4b649235c` | 2026-07-25 |
| Open WebUI | `01f4282f1ffe0d6212f58d3afbeae21fffd0c4be` | 2026-07-27 |
| LibreChat | `ac2aef00f6ebed74cde89b51d28e77da5db6c97b` | 2026-08-24 |

## Executive summary

The projects split into two families:

1. **Free deterministic labels:** Cline and Roo Code persist the initial user prompt; Vibe Kanban derives the workspace label from its first line. They make no title-specific model call. This is instant, reliable, and sometimes verbose.
2. **Asynchronous model refinement:** OpenCode, OpenHands, AiderDesk, Agent Cockpit, Open WebUI, and LibreChat make one additional model request. None deliberately blocks the main agent response on naming. The better implementations bound input, provide a deterministic fallback, and re-check whether the user manually renamed before persisting.

The closest Foreman precedent is Agent Cockpit. It immediately seeds the title from the first user message, then makes a subscription-backed one-shot CLI call after assistant output appears. Its slow call runs outside the persistence lock, and it checks manual-title ownership both before the call and again under the lock before writing. OpenHands independently uses the same ownership-recheck pattern and a configurable cheap title model.

The clearest counterexample is OpenCode: it checks title ownership before starting the asynchronous call but not before writing. Its maintainers have an open race report where a late generated title can overwrite a manual rename. AiderDesk has the same structural risk.

## Comparison matrix

| Project | Naming and exact trigger | Input and model/provider | Async, fallback, and bounds | Rename, persistence, safeguards, and cost |
|---|---|---|---|---|
| **OpenCode** | Automatic only for a root session whose title is still the timestamped default and which has exactly one real user turn. It forks the title job during step 1 of the agent loop, concurrent with the main response. | History through the first user turn; subtask-only starts use joined subtask prompts. A hidden, tool-denied `title` agent uses an explicitly configured model, otherwise the provider's `small_model`, otherwise the active model. | Background provider call with two retries. Prompt requests one line, same language, at most 50 characters. Runtime removes `<think>`, selects the first non-empty line, and caps at 100 characters. Failure leaves the default title. | Manual rename uses the same persisted session title. One extra provider call. **Safeguard gap:** ownership is checked only before the slow call, so a concurrent rename may be overwritten. |
| **OpenHands** | Automatic by default. While no stored title exists, a subscriber reacts to the first incoming user `MessageEvent`, launches `asyncio.create_task`, and moves synchronous model work to an executor. | Text parts from that first user event, capped at 1,000 characters. Model precedence: `title_llm_profile`, agent LLM, then no model. A separate cheap/fast profile is supported. | Non-streaming call; generated result capped at 50 characters. Model error, empty response, or unavailable ACP LLM falls back to the first prompt truncated to 50 characters. | Manual edit persists metadata. After generation it re-checks the authoritative stored title and writes only if the title is still absent, so manual rename wins. One extra configured-provider call; deterministic fallback costs nothing when no title LLM is available. |
| **Cline** | No title-specific LLM call in the inspected VS Code path. New-session history is synchronously seeded from the initial prompt before the prompt is sent fire-and-forget to the agent. | The raw initial prompt; no naming provider or model. | No background naming work and no stored character truncation in this path. The UI applies a one-line visual clamp. | `HistoryItem` has only `task`, not separate generated/manual title ownership. It persists into session metadata and is read back as title/prompt. No rename control was found in the inspected history UI. Zero title cost. |
| **Roo Code** | No title-specific LLM call. On each message save, task metadata is re-derived from the trimmed text of `messages[0]`, documented as the initial task message. | First message text; no model/provider. | Empty or image-only cases use localized `incomplete`/`no_messages` fallbacks. UI stores the full value and displays a three-line clamp plus full tooltip. | No separate title field or rename UI in the inspected schema. Each task's `history_item.json` is authoritative; a debounced `_index.json` is a cache, with locked/serialized writes. Zero title cost. |
| **Vibe Kanban** | Workspace creation derives its name synchronously from the submitted message's first line. A newly created session itself is sent without a name. | First line of the user's initial workspace prompt; no title model. | Workspace title is capped at 100 characters, preferring a word boundary after character 50. Unnamed sessions display `Latest` or a date. | Sessions have a nullable persisted `name` and current UI supports manual rename through the session update route. No title-specific model call. Current docs saying sessions cannot be renamed are stale relative to source. |
| **AiderDesk** | Automatic by default when an unnamed task begins its first prompt. It immediately stores `<<generating>>` and starts name generation without awaiting it, while the primary agent proceeds. | First 1,000 prompt characters. A configurable task-name auxiliary model is used; otherwise it inherits the task agent's provider/model. | Background provider call. Prompt asks for a concise, preferably under-50-character verb phrase. Failure/empty result falls back to the first five prompt words. | Generated/fallback name is written to the task's JSON settings; manual inline rename uses the same persistence. One extra provider call. **Inference from code:** no generation token or ownership re-check exists, so a manual rename during generation can be overwritten. |
| **Agent Cockpit** | Session 1 immediately receives an up-to-80-character first-message placeholder. Once the first assistant content is persisted, a per-stream guard fires one asynchronous refinement; new reset sessions are also eligible. | First user message, capped at 2,000 characters by Codex/Claude adapters. It invokes the conversation's existing CLI profile (`codex exec`, `claude --print`, OpenCode one-shot, etc.), reusing that profile's authentication. | Fire-and-forget from the stream. CLI adapters use a 30-second title timeout and deterministic prompt-text fallback. Adapter output is capped at 80 characters and the shared layer hard-cuts every result to eight words. | `titleManuallySet` is checked before the CLI call and again under the workspace lock before canonical index persistence. Manual rename sets that flag and survives later sessions. One extra CLI invocation consumes the selected CLI account's allowance; title code does not select a cheaper model override. |
| **Open WebUI** | Automatic by default for a new saved chat. The browser requests the title background task only on initial chat creation; the server schedules it separately after assistant-message IDs exist. Users may also click Generate Title later. | Default template uses the final two chat messages. The selected chat model is the base; configured local/external task models can replace it. | Server `asyncio.create_task` keeps it off the response path. Prompt asks for raw JSON and a 2–4-word title. Parse/generation failure falls back to the first user message; `New Chat` is the storage/display default. | Title is updated in the SQL row and embedded chat JSON, then emitted live. Manual inline rename and manual regeneration are supported. Admin and user toggles can disable automatic generation; temporary chats skip it. One extra request to the configured model/provider. No hard title-length clamp was found at persistence; the browser-tab display caps at 30 characters. |
| **LibreChat** | Automatic by default. `immediate` timing, now the default, generates in parallel with the main response from the first user message. Optional legacy `final` timing waits for assistant output. New temporary chats are excluded. | Endpoint-specific `titleEndpoint`, `titleModel`, method, and prompt can be configured; otherwise it uses the conversation endpoint/model. Immediate mode passes user input only; final mode may include response content. | Background generation has a 45-second timeout and abort/discard signals. Titles are sanitized and capped at 200 Unicode code points. Agent-path failure leaves the existing default; the legacy Assistants path falls back to user text/attachments/response capped at 40 characters. | Results are cached for live fetch and persisted through `saveConvo`. Manual rename trims and stores up to 100 characters. Stale replacement streams can discard completed titles; stop can cancel in-flight cost. Optional PII filtering can reject generated titles. Usage is recorded as title usage. One extra configured-provider call. |

## Primary-source details

### OpenCode

- Eligibility and bounded first-turn input: [`prompt.ts` lines 193–224](https://github.com/anomalyco/opencode/blob/3ef72fe8f6c54a31e9709e6dff82dc609df8e453/packages/opencode/src/session/prompt.ts#L193-L224).
- Background fork during step 1: [`prompt.ts` lines 1132–1140](https://github.com/anomalyco/opencode/blob/3ef72fe8f6c54a31e9709e6dff82dc609df8e453/packages/opencode/src/session/prompt.ts#L1132-L1140).
- Hidden agent and small-model choice: [`agent.ts` lines 234–249](https://github.com/anomalyco/opencode/blob/3ef72fe8f6c54a31e9709e6dff82dc609df8e453/packages/opencode/src/agent/agent.ts#L234-L249); provider call and sanitization: [`prompt.ts` lines 216–252](https://github.com/anomalyco/opencode/blob/3ef72fe8f6c54a31e9709e6dff82dc609df8e453/packages/opencode/src/session/prompt.ts#L216-L252).
- Prompt contract: [`title.txt` lines 1–30](https://github.com/anomalyco/opencode/blob/3ef72fe8f6c54a31e9709e6dff82dc609df8e453/packages/opencode/src/agent/prompt/title.txt#L1-L30).
- Default and persisted title: [`session.ts` lines 48–54](https://github.com/anomalyco/opencode/blob/3ef72fe8f6c54a31e9709e6dff82dc609df8e453/packages/opencode/src/session/session.ts#L48-L54), [`session.ts` lines 753–755](https://github.com/anomalyco/opencode/blob/3ef72fe8f6c54a31e9709e6dff82dc609df8e453/packages/opencode/src/session/session.ts#L753-L755).
- Maintainer race report: [OpenCode issue #32710](https://github.com/anomalyco/opencode/issues/32710).

### OpenHands

- Configuration and model precedence: [`request.py` lines 261–275](https://github.com/OpenHands/software-agent-sdk/blob/041078f26698ccba4b78af6c3069e37bb1556b32/openhands-sdk/openhands/sdk/conversation/request.py#L261-L275).
- Subscription and nonblocking trigger: [`conversation_service.py` lines 2283–2286](https://github.com/OpenHands/software-agent-sdk/blob/041078f26698ccba4b78af6c3069e37bb1556b32/openhands-agent-server/openhands/agent_server/conversation_service.py#L2283-L2286), [`conversation_service.py` lines 2466–2521](https://github.com/OpenHands/software-agent-sdk/blob/041078f26698ccba4b78af6c3069e37bb1556b32/openhands-agent-server/openhands/agent_server/conversation_service.py#L2466-L2521).
- Prompt cap, 50-character output, and fallback: [`title_utils.py` lines 35–195](https://github.com/OpenHands/software-agent-sdk/blob/041078f26698ccba4b78af6c3069e37bb1556b32/openhands-sdk/openhands/sdk/conversation/title_utils.py#L35-L195).
- Manual rename persistence: [`conversation_service.py` lines 1799–1832](https://github.com/OpenHands/software-agent-sdk/blob/041078f26698ccba4b78af6c3069e37bb1556b32/openhands-agent-server/openhands/agent_server/conversation_service.py#L1799-L1832); [GUI edit](https://github.com/OpenHands/OpenHands/blob/150e76046db026dd944df0506642dc9b7b99391e/src/components/features/conversation/conversation-name.tsx#L69-L102).

### Cline and Roo Code

- Cline creates history from the initial prompt before starting the agent: [`cline-session-factory.ts` lines 1133–1146](https://github.com/cline/cline/blob/f91af30401fe09f9cb2217b2d3645ff6f2aa662e/apps/vscode/src/sdk/cline-session-factory.ts#L1133-L1146), [`sdk-task-start-coordinator.ts` lines 123–155](https://github.com/cline/cline/blob/f91af30401fe09f9cb2217b2d3645ff6f2aa662e/apps/vscode/src/sdk/sdk-task-start-coordinator.ts#L123-L155). Its schema has only `task`: [`HistoryItem.ts` lines 1–19](https://github.com/cline/cline/blob/f91af30401fe09f9cb2217b2d3645ff6f2aa662e/apps/vscode/src/shared/HistoryItem.ts#L1-L19).
- Roo derives task metadata from the initial message: [`taskMetadata.ts` lines 30–117](https://github.com/RooCodeInc/Roo-Code/blob/b867ec9145750d0ae1ff7f02d35406e9bf2a0b16/src/core/task-persistence/taskMetadata.ts#L30-L117). Persistence and cached indexing: [`TaskHistoryStore.ts` lines 20–30](https://github.com/RooCodeInc/Roo-Code/blob/b867ec9145750d0ae1ff7f02d35406e9bf2a0b16/src/core/task-persistence/TaskHistoryStore.ts#L20-L30), [`TaskHistoryStore.ts` lines 154–184](https://github.com/RooCodeInc/Roo-Code/blob/b867ec9145750d0ae1ff7f02d35406e9bf2a0b16/src/core/task-persistence/TaskHistoryStore.ts#L154-L184), [`TaskHistoryStore.ts` lines 437–456](https://github.com/RooCodeInc/Roo-Code/blob/b867ec9145750d0ae1ff7f02d35406e9bf2a0b16/src/core/task-persistence/TaskHistoryStore.ts#L437-L456).

### Vibe Kanban

- Deterministic first-line title and 100-character split: [`string.ts` lines 40–80](https://github.com/BloopAI/vibe-kanban/blob/4deb7eca8f381f7cbc1f9d15515a9ab8f8009053/packages/web-core/src/shared/lib/string.ts#L40-L80); workspace submission uses it as `name`: [`CreateChatBoxContainer.tsx` lines 223–233](https://github.com/BloopAI/vibe-kanban/blob/4deb7eca8f381f7cbc1f9d15515a9ab8f8009053/packages/web-core/src/shared/components/CreateChatBoxContainer.tsx#L223-L233).
- New session creation omits `name` and then sends the prompt: [`useCreateSession.ts` lines 21–44](https://github.com/BloopAI/vibe-kanban/blob/4deb7eca8f381f7cbc1f9d15515a9ab8f8009053/packages/web-core/src/features/workspace-chat/model/hooks/useCreateSession.ts#L21-L44).
- Session fallback labels and manual rename entry: [`SessionChatBox.tsx` lines 363–372](https://github.com/BloopAI/vibe-kanban/blob/4deb7eca8f381f7cbc1f9d15515a9ab8f8009053/packages/ui/src/components/SessionChatBox.tsx#L363-L372), [`SessionChatBox.tsx` lines 834–878](https://github.com/BloopAI/vibe-kanban/blob/4deb7eca8f381f7cbc1f9d15515a9ab8f8009053/packages/ui/src/components/SessionChatBox.tsx#L834-L878).

### AiderDesk

- Immediate placeholder, first-five-word fallback, background request, 1,000-character cap, and model selection: [`task.ts` lines 1189–1232](https://github.com/hotovo/aider-desk/blob/2a8bc7f1244473688a02842e3089e8775a7b2116/src/main/task/task.ts#L1189-L1232).
- Auxiliary model falls back to the task provider/model: [`task.ts` lines 284–289](https://github.com/hotovo/aider-desk/blob/2a8bc7f1244473688a02842e3089e8775a7b2116/src/main/task/task.ts#L284-L289).
- Auto-generation is on by default: [`store.ts` lines 101–110](https://github.com/hotovo/aider-desk/blob/2a8bc7f1244473688a02842e3089e8775a7b2116/src/main/store/store.ts#L101-L110). UI offers a model selector: [`TaskSettings.tsx` lines 230–253](https://github.com/hotovo/aider-desk/blob/2a8bc7f1244473688a02842e3089e8775a7b2116/src/renderer/src/components/settings/TaskSettings.tsx#L230-L253).
- Manual rename: [`TaskSidebar.tsx` lines 322–337](https://github.com/hotovo/aider-desk/blob/2a8bc7f1244473688a02842e3089e8775a7b2116/src/renderer/src/components/project/TaskSidebar/TaskSidebar.tsx#L322-L337). JSON persistence: [`task.ts` lines 447–485](https://github.com/hotovo/aider-desk/blob/2a8bc7f1244473688a02842e3089e8775a7b2116/src/main/task/task.ts#L447-L485).

### Agent Cockpit

- Immediate deterministic seed: [`conversationMessageStore.ts` lines 180–182](https://github.com/daronyondem/agent-cockpit/blob/2b45ced031eff9f628c98dd93872b5e4b649235c/src/services/chat/conversationMessageStore.ts#L180-L182).
- Per-stream fire-and-forget refinement after assistant content: [`chat.ts` lines 371–384](https://github.com/daronyondem/agent-cockpit/blob/2b45ced031eff9f628c98dd93872b5e4b649235c/src/routes/chat.ts#L371-L384).
- Slow call outside the lock, deterministic fallback, eight-word clamp, and double ownership check: [`chatService.ts` lines 1065–1102](https://github.com/daronyondem/agent-cockpit/blob/2b45ced031eff9f628c98dd93872b5e4b649235c/src/services/chatService.ts#L1065-L1102).
- Subscription/auth-backed CLI adapters: [`codex.ts` lines 379–393](https://github.com/daronyondem/agent-cockpit/blob/2b45ced031eff9f628c98dd93872b5e4b649235c/src/services/backends/codex.ts#L379-L393), [`claudeCode.ts` lines 454–468](https://github.com/daronyondem/agent-cockpit/blob/2b45ced031eff9f628c98dd93872b5e4b649235c/src/services/backends/claudeCode.ts#L454-L468), [`opencode.ts` lines 202–209](https://github.com/daronyondem/agent-cockpit/blob/2b45ced031eff9f628c98dd93872b5e4b649235c/src/services/backends/opencode.ts#L202-L209).

### Open WebUI

- Default 2–4-word, two-message JSON prompt and default-enabled setting: [`config.py` lines 2175–2198](https://github.com/open-webui/open-webui/blob/01f4282f1ffe0d6212f58d3afbeae21fffd0c4be/backend/open_webui/config.py#L2175-L2198), [`config.py` line 2265](https://github.com/open-webui/open-webui/blob/01f4282f1ffe0d6212f58d3afbeae21fffd0c4be/backend/open_webui/config.py#L2265).
- Initial-chat-only browser request: [`Chat.svelte` lines 3171–3183](https://github.com/open-webui/open-webui/blob/01f4282f1ffe0d6212f58d3afbeae21fffd0c4be/src/lib/components/chat/Chat.svelte#L3171-L3183). Server background scheduling: [`main.py` lines 1371–1392](https://github.com/open-webui/open-webui/blob/01f4282f1ffe0d6212f58d3afbeae21fffd0c4be/backend/open_webui/main.py#L1371-L1392).
- Task-model selection and provider call: [`tasks.py` lines 110–185](https://github.com/open-webui/open-webui/blob/01f4282f1ffe0d6212f58d3afbeae21fffd0c4be/backend/open_webui/routers/tasks.py#L110-L185). Parse and deterministic fallback: [`middleware.py` lines 3301–3362](https://github.com/open-webui/open-webui/blob/01f4282f1ffe0d6212f58d3afbeae21fffd0c4be/backend/open_webui/utils/middleware.py#L3301-L3362).
- Manual rename and manual regenerate: [`ChatItem.svelte` lines 321–384](https://github.com/open-webui/open-webui/blob/01f4282f1ffe0d6212f58d3afbeae21fffd0c4be/src/lib/components/layout/Sidebar/ChatItem.svelte#L321-L384), [`ChatItem.svelte` lines 387–459](https://github.com/open-webui/open-webui/blob/01f4282f1ffe0d6212f58d3afbeae21fffd0c4be/src/lib/components/layout/Sidebar/ChatItem.svelte#L387-L459).

### LibreChat

- Immediate-vs-final configuration: [`config.ts` lines 667–682](https://github.com/danny-avila/LibreChat/blob/ac2aef00f6ebed74cde89b51d28e77da5db6c97b/packages/data-provider/src/config.ts#L667-L682).
- New-conversation trigger: [`useResumableSSE.ts` lines 4241–4249](https://github.com/danny-avila/LibreChat/blob/ac2aef00f6ebed74cde89b51d28e77da5db6c97b/client/src/hooks/SSE/useResumableSSE.ts#L4241-L4249). Background fetch, bounded retries, and cache update: [`queries.ts` lines 58–211](https://github.com/danny-avila/LibreChat/blob/ac2aef00f6ebed74cde89b51d28e77da5db6c97b/client/src/data-provider/SSE/queries.ts#L58-L211).
- Server timeout, cancellation, stale-result handling, cache, and persistence: [`agents/title.js` lines 36–175](https://github.com/danny-avila/LibreChat/blob/ac2aef00f6ebed74cde89b51d28e77da5db6c97b/api/server/services/Endpoints/agents/title.js#L36-L175).
- Endpoint/model selection: [`agents/client.js` lines 4332–4469](https://github.com/danny-avila/LibreChat/blob/ac2aef00f6ebed74cde89b51d28e77da5db6c97b/api/server/controllers/agents/client.js#L4332-L4469). Output sanitizer: [`sanitizeTitle.ts` lines 1–34](https://github.com/danny-avila/LibreChat/blob/ac2aef00f6ebed74cde89b51d28e77da5db6c97b/packages/api/src/utils/sanitizeTitle.ts#L1-L34).
- Manual rename: [`Convo.tsx` lines 77–104](https://github.com/danny-avila/LibreChat/blob/ac2aef00f6ebed74cde89b51d28e77da5db6c97b/client/src/components/Conversations/Convo.tsx#L77-L104), with a 100-character input cap in [`RenameForm.tsx` lines 44–61](https://github.com/danny-avila/LibreChat/blob/ac2aef00f6ebed74cde89b51d28e77da5db6c97b/client/src/components/Conversations/RenameForm.tsx#L44-L61).

## Patterns that survived comparison

### 1. Naming is never on the primary response path

Every model-backed implementation examined starts naming in a background task, executor, fork, or parallel promise. LibreChat changed its default from post-response generation to immediate parallel generation. Open WebUI explicitly detached the initial-title task. AiderDesk displays a generating placeholder. This is strong evidence that Foreman's agent hook and GUI frame must return immediately and never wait for the naming subprocess.

### 2. First-prompt-only input is the common fast path

OpenCode, OpenHands, AiderDesk, Agent Cockpit, and LibreChat immediate mode name from the first user request. Open WebUI includes the first response, but LibreChat's current default moved away from waiting for it. For a terminal tab title rather than a rich searchable chat record, waiting for assistant output adds lifecycle integration without clear evidence of proportional value.

### 3. A deterministic seed is valuable even with AI refinement

Agent Cockpit is the cleanest hybrid: the title becomes useful immediately, and a failed or slow CLI request cannot leave a generic label. OpenHands also has a deterministic prompt truncation fallback. Cline, Roo, and Vibe Kanban demonstrate that deterministic labels alone are operationally acceptable.

### 4. Manual ownership must be checked after the slow call

OpenHands and Agent Cockpit re-read ownership immediately before persistence. OpenCode does not and has a first-party race report. AiderDesk's generated-name callback similarly writes without checking whether a user renamed in the meantime. Foreman needs both a generation/session identity check and an authoritative managed-vs-fixed check at apply time.

### 5. Cheap-model selection is normal, but not universal

OpenCode prefers a provider `small_model`; OpenHands exposes a title-specific LLM profile; AiderDesk, Open WebUI, and LibreChat allow a title model. Agent Cockpit instead reuses the selected backend CLI profile and therefore the user's subscription/auth context, but does not override it to a cheaper model. A Foreman-specific `gpt-5.6-luna` invocation combines Agent Cockpit's subscription-backed approach with the cheap-model pattern used elsewhere.

### 6. AI naming is an extra consumption event

Every model-backed implementation makes a separate inference request or CLI invocation. With API-backed providers this can be a separately billed request; with a subscription-authenticated CLI it consumes that subscription's allowance. Cline, Roo, and Vibe Kanban show the zero-consumption alternative.

## Implications for Foreman

### Recommended simple design

1. On the first meaningful `UserPromptSubmit` for an auto-managed Claude, Codex, or Grok Session, synchronously derive and display a deterministic title from the first 6–8 useful words, capped around 80 characters.
2. Enqueue exactly one best-effort refinement for that agent Session. Do not wait for assistant output; the open-source evidence supports prompt-only naming and it avoids another vendor lifecycle dependency.
3. Run `codex exec` with subscription authentication and `gpt-5.6-luna` in Foreman's bounded background worker. Set its working directory to Foreman's stable, dedicated title-namer directory outside user repositories so it cannot inherit project `AGENTS.md` files or repository context. This consumes Codex allowance but avoids per-title API-key billing.
4. Send only a capped prompt excerpt, not the transcript or project files, and expose no tools to the naming run. A 1,000–2,000-character cap matches the inspected agent GUIs and is ample for a terminal label.
5. Apply a plain-text result only after stripping controls/whitespace and hard-capping it to eight words and the terminal-title width budget.
6. Immediately before applying, re-resolve the exact Project + Member, agent session ID, generation, enabled epoch, and managed-title ownership. A manual rename or newer Session wins.
7. On failure, timeout, queue saturation, cancellation, or missing Codex authentication, retain the deterministic seed. Do not retry in v1.
8. Offer two explicit modes in settings: **Prompt title only** (zero AI usage) and **Luna refinement** (one subscription allowance event per new agent Session). This makes cost behavior honest without adding an API-key path.

### What not to copy

- Do not use a blocking hook or wait for the main agent response.
- Do not persist `<<generating>>`, `New Chat`, or another generic label when a useful prompt-derived seed is available.
- Do not check manual ownership only when work is queued; check again at application time.
- Do not feed the project transcript or repository context to a three-to-eight-word naming task.
- Do not launch the naming CLI from the user's repository; project instructions are unrelated input and may change the title run's behavior.
- Do not add retries, multiple candidate models, or a general task-generation framework in v1.
- Do not promise that subscription-backed naming is free; it avoids an API bill but still consumes allowance.

## Inferences and remaining unknowns

- **Inference:** AiderDesk can overwrite a manual rename that lands while name generation is pending because its completion callback unconditionally saves the generated/fallback name. No runtime reproduction was performed.
- **Inference:** Prompt-only naming is the best simplicity/performance trade for Foreman. This is supported by the majority of model-backed implementations and LibreChat's current immediate default, but title-quality comparison was not benchmarked.
- **Unknown:** Cline's newer non-VS-Code surfaces were not exhaustively audited; the deterministic finding is verified for its shared VS Code history/session path at the pinned revision.
- **Unknown:** Open WebUI's effective output-token cap depends on the selected model configuration; its persisted title path has no independent character clamp in the inspected code.
- **Unknown:** Subscription allowance accounting for a `gpt-5.6-luna` Codex invocation remains external product behavior and must be verified against the installed Codex CLI during Foreman's implementation spike.

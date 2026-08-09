# P428 mailbox translation review — current RED / repaired candidate GREEN

`npx @informalsystems/quint@0.32.0` runs these models.
`p428_mailbox_delivery.qnt` is the historical pre-fix Rust model, so its SPEC
properties are deliberately RED candidates; `p428_mailbox_repaired.qnt` is a
candidate protocol model for a proposed runtime gate, not current production.

| SPEC G9 / concern | Current Rust and current-model action | Status | Repaired candidate |
| --- | --- | --- | --- |
| Idle starts delivery; busy queues. | Historical pre-fix model first reaches an existing recipient Working observation, queues `sendFirst` and `sendSecond`, then uses `recipientBecomesIdle → observedWorkingToIdle → beginFromIdleTransition` to snapshot the two rows. Idle-origin sends are separate synchronous one-row API actions and cannot be coalesced across API calls. | The pre-read batch then does `acceptFirstWholePrompt → markFirstDeliveredRead → recipientStartsFirstTurn → acceptSecondWholePrompt`, accepting prompt two while App remains Idle and recipient is Working. `noBusySecondPrompt` / `onePromptPerTurn` are RED on that real queue-to-Idle path. | A named event creates one attempt and `accepted` permits one prompt only. This is a candidate-spec finding only; no one-turn runtime gate is in production without user approval. |
| Channel acceptance is not turn start. | `accept*WholePrompt` is external channel acceptance; SQLite `mark*DeliveredRead` is a distinct synchronous action; `recipientStarts*Turn` is a later external action. | Current breaks `marksRequireBusyStartEvidence`: mark may precede process Working. This is expected RED, not an assertion about real terminal completion. | This remains a guarantee-impossible observation gap / contract decision, not a required GREEN invariant. |
| Durable queued row can retry after side effect. | `acceptFirstWholePrompt → failFirstMark/crash → restart → beginFromStartup → acceptFirstWholePrompt` increments `firstAccepts` twice for one queued row. | Current breaks `noDuplicateTurn` RED. SQLite cannot atomically commit an external runtime write; exactly-once turn delivery is therefore not guaranteed. | Repair can constrain duplicate enqueue attempts, not prove exactly-once external consumption. |
| Created order is preserved across normal-mail rooms. | Historical current immediate send reads `pending_messages_for_agent(&nudge.room, ...)`; `queueOlderRoomA → sendNewerRoomB → acceptSecondFromNudgedRoom` selects B without reading older A. | Current breaks `creationOrder` RED. `herdr-jobs` remains a deliberate room-grouped path. | Quint does not prove production ordering because First-before-Second is encoded in the candidate guard. The production evidence is the pN-wide ordered SQLite query and Rust cross-room regressions. |
| Startup scans queued work once per lifetime. | `beginFromStartup` requires a queued row and increments `startupCount`; `restart` resets the per-lifetime count. | Current model satisfies `startupAtMostOncePerLifetime`; mutation keeps `startupPending` after consumption to make a second begin RED. | Keep generation/lifetime count `<= 1`. |
| A real non-Idle→Idle transition can auto-flush; presentation-only Idle→Idle cannot. | Quint models Working/Blocked→Idle and the presentation-only Idle→Idle path. It does not model Unknown/Done. | Current breaks `noPresentationOnlyIdleFlush` RED. Unknown→Idle is covered only by the Rust regression. | Admission consumes Send, StartupOnce, or a modeled Working/Blocked→Idle event. Unknown/Done→Idle remain an explicit model boundary. |
| Unrelated API requests never flush. | `unrelatedApiRequest` is an explicit stutter/event action in both models and cannot set a delivery origin. | Both models satisfy `noUnrelatedApiFlush`; a temporary `beginFromUnrelatedApi` mutation makes the same property RED. | AgentList/PaneList keep queued normal mail untouched; this is distinct from the presentation-only finding. |
| A live, quiescent recipient does not strand normal mail. | Repaired state pairs every queued row with a named Send/Idle event, a delivery attempt, or the active recipient turn. | `noStrandedQueuedWhenQuiescent` is GREEN as a bounded liveness-as-safety invariant. Removing either the Send event or the observed Idle event makes it RED. | This guards event generation/consumption; it is not a proof of infinite scheduler fairness. |
| Stable recipient must not fall back to a reused name. | Regular mail stores canonical global pN via `resolve_msg_recipient`; current actions require `stableP1Live`. `stableP1DisappearsAndNameReused` leaves queued rows undeliverable. | Current satisfies `noAliasFallback`; mutation that accepts `AliasFallback` is RED. No new persisted generation is proposed. | Retain canonical pN/no-name-fallback. |
| Body and Enter are atomic. | `p428_mailbox_input_current.qnt`: `acceptBody → closeRuntimeAfterBody → rejectEnter` yields `BodyOnly`, so `noPartialPrompt` RED. | Historical pre-fix RED only. | `p428_mailbox_input_repaired.qnt` has the same `PromptWrite` domain but only `BodyAndEnter` transition. |

## Action boundary inventory

| Model action | Boundary |
| --- | --- |
| `send*`, `beginFrom*` / `beginNamedEvent`, normal-mail row selection, `mark*`, `appObserves*`, and event consumption | Synchronous Rust stack; normal mail uses one pN/status-indexed all-room query, submits every returned row in created/id order with one atomic body+Enter write per row, and calls `mark*` after each successful channel write. The one-prompt named-event behavior is a candidate model only. |
| `accept*WholePrompt` / `accept*` | Crosses the terminal runtime channel (`try_send_bytes`); success is not process-start evidence. |
| `recipientStarts*Turn`, `recipientBecomesIdle`, `recipientBlocks` | External recipient process/detector boundary. |
| `crash`, `restart`, `stableP1DisappearsAndNameReused` | External lifecycle/identity boundary. |
| `presentationChangedWhileIdle` | `App::emit_pane_state_update` receives a presentation update; the current model then represents its synchronous flush attempt separately. |
| `unrelatedApiRequest` | Synchronous AgentList/PaneList API boundary. It records that a request occurred but does not create a mail eligibility event. |

## G9 content-ID evidence inventory

`scripts/formal_mailbox_pilot_drift_check.py` consumes
`spec_contract_inventory` and checks only freshness: the active G9 mailbox
content IDs, named Quint properties, and named Rust tests must still exist.
It does **not** judge that their meanings agree; that mapping remains p428's
human review responsibility.

| G9 content ID | Quint property | Rust regression test |
| --- | --- | --- |
| `G9-a78d994a68` | `startupAtMostOncePerLifetime` | `reopened_app_with_same_stable_pane_id_delivers_regular_mail_once` |
| `G9-97e894730a` | `noPartialPrompt` | `regular_message_delivery_submits_body_and_enter_atomically`; `idle_direct_message_submits_its_exact_body_without_an_inbox_command` |
| `G9-98418f84fb` | `noBusySecondPrompt` (current RED; contract conflict) | `working_then_idle_submits_regular_message_once`; `regular_message_batch_submits_every_body_in_creation_order` |
| `G9-186325da28` | `bothDeliveredReachable` | `idle_transition_submits_regular_messages_in_global_creation_order`; `idle_direct_message_submits_its_exact_body_without_an_inbox_command` |
| `G9-edb5ccfcdc` | `noAliasFallback` | `restarted_same_name_does_not_nudge_or_read_old_regular_mail`; `inbox_reads_pane_id_and_agent_name_recipients_for_the_same_pane` |
| `G9-62309951e0` | `noUnrelatedApiFlush` | `agent_and_pane_list_do_not_flush_queued_regular_messages` |
| `G9-eba86d3f7b` | `startupAtMostOncePerLifetime`; `noPresentationOnlyIdleFlush` | `startup_flush_submits_current_regular_mail_once`; `idle_direct_message_submits_its_exact_body_without_an_inbox_command`; `idle_transition_submits_regular_messages_in_global_creation_order`; `unknown_then_idle_submits_queued_regular_message` |

## Non-vacuity and limits

The repaired model ties each acceptance to `attemptId` and `activeMessage`, so
`noBusySecondPrompt` / `onePromptPerTurn` reject only two accepts in one named
event; historical `firstTurn` and `secondTurn` do not make a later normal turn RED.
Its turn-start actions require the matching active message and an unstarted flag.
The repaired model reaches `bothDeliveredReachable` through first accept/mark,
recipient Working→Idle, observed Idle event, then second accept/mark. Current
RED witnesses and repaired temporary mutations are listed in
[p428-mailbox-nonvacuity-plan.md](p428-mailbox-nonvacuity-plan.md). The finite
model omits broadcasts, `herdr-jobs`, exact terminal parser behavior, SQLite
error taxonomy, arbitrary rooms/rows, and detector timing. It cannot prove
external exactly-once turn consumption.

`G9-98418f84fb` is intentionally not reported GREEN: the acceptance condition
forbids writing into a busy recipient, while the current behavior paragraph
requires every queued body to be submitted in one Idle opportunity. The current
model reaches the recipient-Working/App-Idle gap between two accepted bodies.
Resolving that contradiction requires the user's explicit product decision.

Created/id ordering is traced separately against the G9 normal-mail behavior,
not against an acceptance-condition content ID. The candidate model's
`creationOrder` is only a consistency check because First-before-Second is
encoded in its acceptance guard; the production evidence is the ordered SQLite
query plus the cross-room Rust regressions.
Likewise `noPartialPrompt` models write atomicity only: room quote/space and
multiline body preservation are intentionally unmodeled in Quint and covered by
the exact-string Rust test above.

## Query-path boundary

For a normal `handle_msg_send`, the code resolves the recipient, checks its
Idle state, and calls `pending_messages_for_agent_in_creation_order(pN)` once
before submitting every returned normal row in created/id order. It does not
call `pending_nudge_for` or a room-scoped pending-message query on that path.
`herdr-jobs` alone uses
`flush_job_msg_nudge_for` and the existing room-grouped `PendingNudge` query.
This structural review is the query-count evidence; exact SQLite query counting
is not exposed by the store test harness.

The old split model, ITF trace, and rejected pilot/evidence documents remain
deleted and must not be used as evidence.

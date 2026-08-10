# P428 mailbox Quint execution results

All commands use the fixed executable `npx @informalsystems/quint@0.32.0` with
the Rust backend.  Exit 1 is expected only for a deliberately negated witness
or a current/temporary-mutation RED property.

## Approved immediate-steering contract

| Check | Result | Evidence |
| --- | --- | --- |
| Typecheck | GREEN | `job-1786253401191-76555-351` |
| Six safety properties | GREEN | `job-1786253401191-76555-351`: available queues retain a named delivery or same-status-report retry event; Working does not wait for Idle; unavailable states never accept; acceptance is at most once in the model; presentation and unrelated APIs do not change the acceptance count. |
| Working send reachability | expected RED witness | `job-1786253401212-76555-352`; [immediate-working-steering.itf.json](immediate-working-steering.itf.json) is exactly `sendAvailable → acceptAvailable` while Working. |
| Unavailable queue reachability | expected RED witness | `job-1786253401240-76555-353`; [immediate-unavailable-queue.itf.json](immediate-unavailable-queue.itf.json) is exactly `becomeBlocked → sendUnavailable`. |
| Same-Working retry reachability | expected RED witness | `job-1786253401259-76555-354`; [immediate-same-status-retry.itf.json](immediate-same-status-retry.itf.json) is exactly `sendAvailable → submitFails → sameAvailableStatusReported`. |
| Rust Working delivery | RED → GREEN | `job-1786251855817-76555-328` stored only (`nudged=[]`); `job-1786251933355-76555-329` submits the full body immediately. |
| Mailbox focused regressions | GREEN | `job-1786253701043-76555-358`: 26/26 passed after Working broadcast, Blocked→Working/Done with an asserted Done state, runtime-ready re-report, immediate Done delivery, startup Working, identity, jobs, presentation-only updates, and no-duplicate expectations were aligned. |

The sections below preserve the historical wait-for-Idle counterexample and
the rejected one-prompt-per-Idle-turn candidate. They explain how the contract
decision was discovered; they are not the current production model.

## Type checking

| Model | Herdr job | Result |
| --- | --- | --- |
| Current mailbox with pre-read membership, busy-send origin, and focused real-batch step set | `job-1786231683555-76555-259` | GREEN |
| Repaired mailbox with reachable Blocked path, unrelated-API event, and quiescent-queue liveness-as-safety check | `job-1786231731773-76555-270` | GREEN |
| Current input atomicity | `job-1786231732047-76555-271` | GREEN |
| Repaired input atomicity | `job-1786231732341-76555-272` | GREEN |

## Current-model RED and GREEN

| Property | Result | Evidence |
| --- | --- | --- |
| `noBusySecondPrompt` | RED | `job-1786231691313-76555-260`; [current-busy.itf.json](current-busy.itf.json) uses `busyStep`, which only selects existing current actions along existing Working → created-order two busy sends → observed Idle → one pre-read batch. The second accept records that first turn was already Working while App remained Idle. |
| `onePromptPerTurn` | RED | `job-1786231691579-76555-261`; [current-turn.itf.json](current-turn.itf.json) uses the same action set and records both accepts in that one batch. |
| `noDuplicateTurn` | RED residual | `job-1786231722311-76555-262`; [current-duplicate.itf.json](current-duplicate.itf.json) shows accept → failed mark/crash → restart → reaccept. |
| `marksRequireBusyStartEvidence` | RED observational gap | `job-1786231722646-76555-263`; [current-mark-gap.itf.json](current-mark-gap.itf.json) records mark before a recipient turn start. |
| `noPresentationOnlyIdleFlush` | RED | `job-1786231722989-76555-264`; [current-presentation.itf.json](current-presentation.itf.json). |
| `creationOrder` | RED | `job-1786231723280-76555-265`; [current-cross-room-order.itf.json](current-cross-room-order.itf.json) selects room B before older queued room A through the historical room-scoped current query. |
| `noUnrelatedApiFlush` | GREEN | `job-1786231731519-76555-269`; `unrelatedApiRequest` cannot start a batch. |
| single-row pre-read batch closes | expected RED witness | `job-1786231723607-76555-266` reaches a one-row snapshot after mark with `batchOpen == false`. |
| `startupAtMostOncePerLifetime` | GREEN | `job-1786231724084-76555-267`. |
| `noAliasFallback` | GREEN | `job-1786231731262-76555-268`. |

## Repaired-model GREEN and non-vacuity

| Property / witness | Result | Evidence |
| --- | --- | --- |
| `noBusySecondPrompt` | GREEN | `job-1786231740968-76555-275` |
| `onePromptPerTurn` | GREEN | `job-1786231741339-76555-276` |
| `creationOrder` | model consistency only | `job-1786231741748-76555-277`: First-before-Second is encoded in the candidate acceptance guard, so this is not proof of created/id ordering. Production ordering evidence comes from the SQLite query and Rust cross-room regressions. |
| `noPresentationOnlyIdleFlush` | GREEN | `job-1786231742098-76555-278` |
| `noUnrelatedApiFlush` | GREEN | `job-1786231742523-76555-279`; `unrelatedApiRequest` remains non-admitting. |
| `activeTurnBlocksNewAttempt` | GREEN | `job-1786231750571-76555-280`: active message must retain its own acceptance attempt until `appObservesIdle` releases it. |
| `noStrandedQueuedWhenQuiescent` | GREEN liveness-as-safety | `job-1786231750888-76555-281`: a queued row may not coexist with a live, Idle, inactive recipient that has neither an in-progress attempt nor a named event. This is a finite safety encoding, not an infinite-fairness proof. |
| both messages delivered | expected RED witness | `job-1786231751146-76555-282`; [repaired-both-delivered.itf.json](repaired-both-delivered.itf.json). The regenerated trace reaches first accept/mark/start, observed Idle, then second accept/mark/start. |
| Blocked→Idle event | expected RED witness | `job-1786231751437-76555-283`; [repaired-blocked-idle.itf.json](repaired-blocked-idle.itf.json). |
| active-message guard, pre-turn mutation | expected RED | `job-1786230546142-76555-166`: removing only `activeMessage == NoActive` permits the post-mark/pre-turn pending event to advance `attemptId` from 1 to 2 while `activeMessage == First`. |
| active-message guard, post-turn mutation | expected RED | `job-1786230546463-76555-167`: the same removed guard permits a recipient-started, recipient-Idle but App-unobserved active first turn to begin attempt 2 while `activeMessage == First`. Temporary mutation models are deleted after the checks. |
| unrelated-API mutation | expected RED | `job-1786230791857-76555-178`: adding only `beginFromUnrelatedApi` makes `noUnrelatedApiFlush` RED. |
| named send-event liveness mutation | expected RED | `job-1786231132081-76555-210`: removing the Send event leaves a queued, quiescent first row and violates `noStrandedQueuedWhenQuiescent`. Temporary mutation model deleted after the check. |
| named Idle-event liveness mutation | expected RED | `job-1786231132099-76555-211`: after first delivery and recipient Idle, suppressing the observed Idle event strands queued second mail in the quiescent state. Temporary mutation model deleted after the check. |

## Input atomicity

| Model property | Result | Evidence |
| --- | --- | --- |
| Current-before-fix `noPartialPrompt` | RED | `job-1786231732683-76555-273`; [atomic-current.itf.json](atomic-current.itf.json) reaches `BodyOnly`. |
| Repaired `noPartialPrompt` | GREEN | `job-1786231740622-76555-274`. |

## PTY write acknowledgement

| Model property | Result | Evidence |
| --- | --- | --- |
| Model typecheck | GREEN | `job-1786336580794-84957-154`; Quint 0.32.0 accepted [p428_mailbox_write_ack.qnt](p428_mailbox_write_ack.qnt), including partial write, dedicated completion event, shared-event saturation, manual inbox, retry, and fatal actor states. |
| Queue acceptance permits a false delivery mark | RED | `job-1786334525164-84957-64`; [current-write-ack.itf.json](current-write-ack.itf.json) reaches `Delivered` while the PTY write is still only `ActorQueued`. |
| Synchronous completion wait blocks the App | RED | `job-1786334525156-84957-63`; [current-sync-block.itf.json](current-sync-block.itf.json) reaches `appBlocked=true` immediately after actor queue acceptance. |
| Repaired delivery and failure state | GREEN | `job-1786336737925-84957-160` and `job-1786336737914-84957-159`; 200,000 traces up to 40 steps found no false-delivery or failed-write queue violation. |
| Repaired event-driven safety | GREEN | `job-1786336737932-84957-161`, `job-1786336737943-84957-162`, and `job-1786336737960-84957-163`; App non-blocking, completion/in-flight correspondence, and at-most-one mark each held for 200,000 traces up to 40 steps. |
| Mailbox completion does not reserve shared AppEvent capacity | GREEN | `job-1786336580802-84957-155`; the dedicated completion path held zero shared-event reservations for 200,000 traces up to 40 steps, including states where the shared channel was full. |

## Production regression evidence

| Regression | Evidence |
| --- | --- |
| Immediate normal send with an older queued row in another room | `job-1786230496604-76555-163` RED injected room B first; the retained pN-wide query is covered by `idle_send_selects_oldest_regular_message_across_rooms`, which submits room A then B. |
| Available recipient receives all queued normal bodies in global creation order | `regular_message_batch_submits_every_body_in_creation_order` and `idle_transition_submits_regular_messages_in_global_creation_order` preserve atomic body+Enter and global ordering; Working sends are covered by the immediate-steering tests. |
| New App and reopened SQLite store for the same stable pN | `job-1786231132003-76555-207`: the reopened App resolves the original global pN, delivers the persisted row once, and a second reflush does not duplicate it. |
| Ordinary list APIs and presentation-only updates | `agent_and_pane_list_do_not_flush_queued_regular_messages` and `presentation_only_idle_update_does_not_flush_regular_messages` cover the no-flush contracts without a per-request all-agent scan. |
| PTY completion, shared-event saturation, and failed-write retry state | `job-1786336988980-84957-167`: 31/31 mailbox tests passed, including a full shared AppEvent channel and a real socket write that fails only after successful enqueue; the failed row remains queued and unread. |
| Unix fatal write, stop race, and handoff lifecycle | `job-1786336988976-84957-166`: 18/18 actor tests passed after serializing the accepting check with enqueue, including deterministic peer-close completion and a fatal handoff write that stays `Released` and rejects rollback. |
| Full native regression | `job-1786337119279-84957-173`: native clippy passed with warnings denied, 2,968/2,968 Rust tests passed, integration assets 2/2 passed, and plugin marketplace 12/12 passed. Maintenance contracts passed 87/87 in `job-1786337044799-84957-169`; fork docs passed in `job-1786337044803-84957-170`. |
| Windows cross-lint environment | `job-1786336839766-84957-164` stopped in the bundled `libsqlite3-sys` C build before compiling Herdr's Windows source because the macOS cross toolchain cannot find `stdlib.h`; Windows-native compilation remains unverified in this environment. |

## Guarantee limits

PTY write completion and the recipient's actual turn start are separate
boundaries. `marksRequireBusyStartEvidence` is therefore a useful observational
RED, but not a repaired GREEN contract. Likewise a completed external runtime
write and the subsequent SQLite mark cannot be one atomic transaction:
`noDuplicateTurn` remains a documented at-least-once limit, not a falsely
claimed exactly-once proof.

The user resolved the historical contract conflict by requiring delivery at
message arrival. Working delivery is now intentional steering: the recipient
agent, not Herdr, decides whether the new information changes current work. The
one-prompt-per-observed-turn runtime gate remains rejected historical evidence.

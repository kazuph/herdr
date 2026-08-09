# P428 non-vacuity plan — immediate steering and historical candidates

## Approved immediate-steering model

| Property | Positive path | Non-vacuity evidence |
| --- | --- | --- |
| `workingSteeringIsImmediate` | Initial Working recipient performs `sendAvailable → acceptAvailable`. | `job-1786253401212-76555-352` violates `notWorkingSteeringReachable`; the compact ITF contains only the two production actions. |
| `unavailableNeverAccepts` | Blocked recipient performs `sendUnavailable` and retains `Queued` with no event. | `job-1786253401240-76555-353` violates `notUnavailableQueueReachable`; the compact ITF is `becomeBlocked → sendUnavailable`. |
| `availableQueueHasDeliveryEvent` | `sendAvailable`, `becomeWorkingWithQueued`, `becomeIdleWithQueued`, `becomeDoneWithQueued`, and `restartAvailableWithQueued` create delivery events; `submitFails` creates the named same-status-report retry boundary. | The six-property run `job-1786253401191-76555-351` explores these paths. This is bounded liveness-as-safety, not infinite scheduler fairness. |
| Same-status retry after failed submit | Working recipient performs `sendAvailable → submitFails → sameAvailableStatusReported`. | `job-1786253401259-76555-354` violates `notSameStatusRetryReachable`; the compact ITF proves the queued/no-event recovery state and its report-driven retry are both reachable. |

## Historical wait-for-Idle investigation

Do not run these checks before p428 approves. Use only
`npx @informalsystems/quint@0.32.0` after approval.

| Property | Current-model witness / result | Repaired-model mutation |
| --- | --- | --- |
| `noBusySecondPrompt`, `onePromptPerTurn` | The current model's `busyStep` selects only `recipientStartsExistingTurn, appObservesWorking, sendFirst, sendSecond, recipientBecomesIdle, observedWorkingToIdle, beginFromIdleTransition, acceptFirstWholePrompt, markFirstDeliveredRead, recipientStartsFirstTurn, acceptSecondWholePrompt`. `twoRowsInBatch` additionally requires both rows to originate from the two busy sends. | Permit a second acceptance after `accepted`; property must fail. |
| Active turn blocks a pending Send event. | Repaired witness: `sendFirst, beginNamedEvent, acceptFirst, markFirst, sendSecond`; `beginNamedEvent` remains disabled because `activeMessage == First`. It remains disabled after `recipientStartsFirstTurn, recipientBecomesIdle` too, until `appObservesWorking, appObservesIdle` clears that active message. | `job-1786230546142-76555-166` removes only `activeMessage == NoActive` and makes the post-mark/pre-turn state RED. `job-1786230546463-76555-167` uses the same removal after recipient start and recipient Idle while App remains unobserved; the same active-message/accepted-attempt property is RED. Temporary mutation models are deleted. |
| `marksRequireBusyStartEvidence` | `acceptFirstWholePrompt, markFirstDeliveredRead` violates it before `recipientStartsFirstTurn`. | Guarantee-impossible observation gap; not a required GREEN invariant. |
| `noDuplicateTurn` | `acceptFirstWholePrompt, failFirstMark/crash, restart, beginFromStartup, acceptFirstWholePrompt` increments first acceptance twice. | RED residual: external runtime write plus SQLite mark cannot be atomic, so repaired model must not claim exactly-once proof. |
| `creationOrder` | `job-1786230576431-76555-169`: the historical room-scoped current model selects room B despite older queued room A, so the current property is RED. | The repaired model encodes First-before-Second and is only a consistency check, not a non-vacuous proof of created/id ordering. The ordered SQLite query and Rust cross-room regressions are the production evidence. |
| `startupAtMostOncePerLifetime` | Current passes; mutate startup consumption to retain `startupPending` and begin twice. | Same count mutation must fail. |
| `noPresentationOnlyIdleFlush` | `beginFromPresentationOnlyIdle` directly makes RED. | Add that action temporarily to make RED. |
| `noUnrelatedApiFlush` | `unrelatedApiRequest` is reachable in the current model but cannot begin a batch, so the property stays GREEN. | `job-1786230791857-76555-178` adds only `beginFromUnrelatedApi`; the same property is RED. |
| `noStrandedQueuedWhenQuiescent` (liveness as safety) | Repaired candidate has no such state: a queued row is paired with a named Send/Idle event or an active attempt/turn. This is a bounded safety approximation, not a proof of scheduler fairness. | `job-1786231132081-76555-210` drops the Send event; `job-1786231132099-76555-211` drops the observed Idle event after first delivery. Both create a queued, live, Idle, inactive state and make the property RED. Temporary mutation models are deleted. |
| Pre-read batch membership and close | Current `beginFromIdleTransition` snapshots `firstInBatch`/`secondInBatch`; `markFirstDeliveredRead` closes a one-row snapshot and keeps only a two-row snapshot open. | `job-1786230825775-76555-186` runs `notSingleRowBatchClosed` and reaches the one-row closed state, proving the close branch is not vacuous. |
| `noAliasFallback` | Current passes; mutate pN loss to accept the alias new pane. | Same mutation must fail. |
| `noPartialPrompt` | Current-before-fix input model RED after `acceptBody, closeRuntimeAfterBody, rejectEnter`. | Set repaired write to `BodyOnly` to make RED. |

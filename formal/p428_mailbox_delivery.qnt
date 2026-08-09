// Historical pre-fix Rust model: pre-read Vec batch, intentionally RED against SPEC G9.
module p428_mailbox_delivery {
  type App = Idle | Working | Blocked
  type Process = ProcessIdle | ProcessWorking | ProcessBlocked | ProcessDown
  type Row = Absent | Queued | DeliveredRead
  type Target = None | StableP1 | AliasFallback
  type Write = NoWrite | BodyOnly | EnterOnly | BodyAndEnter
  type Origin = NoneOrigin | Send | Startup | IdleTransition | PresentationOnlyIdle | UnrelatedApi
  type State = { app: App, process: Process, runtimeOpen: bool, first: Row, second: Row,
    batchOpen: bool, batchOrigin: Origin, firstAccepted: bool, secondAccepted: bool,
    firstAccepts: int, secondAccepts: int, firstMarked: bool, secondMarked: bool,
    firstTurn: bool, secondTurn: bool, firstWrite: Write, secondWrite: Write,
    startupPending: bool, startupCount: int, idleEvent: bool, presentationIdleEvent: bool,
    stableP1Live: bool, aliasNewPane: bool, firstTarget: Target, secondTarget: Target,
    batchRoomScoped: bool, firstInBatch: bool, secondInBatch: bool, twoRowsInBatch: bool,
    firstQueuedWhileBusy: bool, secondQueuedWhileBusy: bool, existingTurnStarted: bool,
    secondAcceptedWhileFirstWorking: bool,
    unrelatedApiEvent: bool }
  var st: State
  action init = { st' = { app: Idle, process: ProcessIdle, runtimeOpen: true, first: Absent, second: Absent,
    batchOpen: false, batchOrigin: NoneOrigin, firstAccepted: false, secondAccepted: false,
    firstAccepts: 0, secondAccepts: 0, firstMarked: false, secondMarked: false,
    firstTurn: false, secondTurn: false, firstWrite: NoWrite, secondWrite: NoWrite,
    startupPending: true, startupCount: 0, idleEvent: false, presentationIdleEvent: false,
    stableP1Live: true, aliasNewPane: false, firstTarget: None, secondTarget: None,
    batchRoomScoped: false, firstInBatch: false, secondInBatch: false, twoRowsInBatch: false,
    firstQueuedWhileBusy: false, secondQueuedWhileBusy: false, existingTurnStarted: false,
    secondAcceptedWhileFirstWorking: false,
    unrelatedApiEvent: false } }
  // An existing recipient turn is observed before the two busy-time sends queue.
  action recipientStartsExistingTurn = all { not(st.existingTurnStarted), st.first == Absent, st.second == Absent,
    st.app == Idle, st.process == ProcessIdle, st' = { ...st, process: ProcessWorking, existingTurnStarted: true } }
  action sendFirst = all { st.first == Absent, st.app == Working or st.app == Blocked,
    st.process == ProcessWorking or st.process == ProcessBlocked,
    st' = { ...st, first: Queued, firstQueuedWhileBusy: true } }
  action sendSecond = all { st.first == Queued, st.second == Absent, st.app == Working or st.app == Blocked,
    st.process == ProcessWorking or st.process == ProcessBlocked,
    st' = { ...st, second: Queued, secondQueuedWhileBusy: true } }
  // A normal idle send runs its select and snapshot in one synchronous API stack.
  action sendFirstImmediately = all { st.first == Absent, not(st.batchOpen), st.app == Idle, st.process == ProcessIdle,
    st' = { ...st, first: Queued, batchOpen: true, batchOrigin: Send, batchRoomScoped: false,
      firstInBatch: true, secondInBatch: false, twoRowsInBatch: false, firstAccepted: false, secondAccepted: false } }
  action sendSecondImmediately = all { st.second == Absent, st.first != Queued, not(st.batchOpen), st.app == Idle, st.process == ProcessIdle,
    st' = { ...st, second: Queued, batchOpen: true, batchOrigin: Send, batchRoomScoped: false,
      firstInBatch: false, secondInBatch: true, twoRowsInBatch: false, firstAccepted: false, secondAccepted: false } }
  // Old room A was queued while busy; a new room B send wakes an Idle pane.
  action queueOlderRoomA = all { st.first == Absent, st' = { ...st, first: Queued } }
  action sendNewerRoomB = all { st.second == Absent, not(st.batchOpen), st.app == Idle, st.process == ProcessIdle,
    st' = { ...st, second: Queued, batchOpen: true, batchOrigin: Send, batchRoomScoped: true,
      firstInBatch: false, secondInBatch: true, twoRowsInBatch: false, firstAccepted: false, secondAccepted: false } }
  // Exact current immediate-send bug: query the nudge's room B rather than all rooms for pN.
  action beginFromStartup = all { not(st.batchOpen), st.startupPending, st.app == Idle, st.first == Queued or st.second == Queued,
    st' = { ...st, batchOpen: true, batchOrigin: Startup, batchRoomScoped: false, startupPending: false, startupCount: st.startupCount + 1,
      firstInBatch: st.first == Queued, secondInBatch: st.second == Queued, twoRowsInBatch: false, firstAccepted: false, secondAccepted: false } }
  action beginFromIdleTransition = all { not(st.batchOpen), st.idleEvent, st.app == Idle, st.first == Queued or st.second == Queued,
    st' = { ...st, batchOpen: true, batchOrigin: IdleTransition, batchRoomScoped: false, idleEvent: false,
      firstInBatch: st.first == Queued, secondInBatch: st.second == Queued,
      twoRowsInBatch: st.first == Queued and st.second == Queued and st.firstQueuedWhileBusy and st.secondQueuedWhileBusy,
      firstAccepted: false, secondAccepted: false } }
  action presentationChangedWhileIdle = all { st.app == Idle, st.first == Queued or st.second == Queued,
    st' = { ...st, presentationIdleEvent: true } }
  action beginFromPresentationOnlyIdle = all { not(st.batchOpen), st.presentationIdleEvent, st.app == Idle,
    st' = { ...st, batchOpen: true, batchOrigin: PresentationOnlyIdle, batchRoomScoped: false, presentationIdleEvent: false,
      firstInBatch: st.first == Queued, secondInBatch: st.second == Queued, twoRowsInBatch: false, firstAccepted: false, secondAccepted: false } }
  // Ordinary AgentList/PaneList requests do not create a delivery attempt.
  action unrelatedApiRequest = all { st' = { ...st, unrelatedApiEvent: true } }
  // External channel acceptance is distinct from recipient process turn start.
  action acceptFirstWholePrompt = all { st.batchOpen, st.firstInBatch, st.first == Queued, st.runtimeOpen, not(st.firstAccepted), st.stableP1Live,
    st' = { ...st, firstAccepted: true, firstAccepts: st.firstAccepts + 1, firstWrite: BodyAndEnter, firstTarget: StableP1 } }
  action markFirstDeliveredRead = all { st.batchOpen, st.first == Queued, st.firstAccepted,
    st' = { ...st, first: DeliveredRead, firstMarked: true, batchOpen: st.secondInBatch } }
  action recipientStartsFirstTurn = all { st.firstAccepted, not(st.firstTurn), st' = { ...st, process: ProcessWorking, firstTurn: true } }
  action appObservesWorking = all { st.app == Idle, st.process == ProcessWorking, not(st.firstAccepted), not(st.secondAccepted),
    st' = { ...st, app: Working } }
  action recipientBlocks = all { st.app == Working, st.process == ProcessWorking, st' = { ...st, process: ProcessBlocked } }
  action appObservesBlocked = all { st.app == Working, st.process == ProcessBlocked, st' = { ...st, app: Blocked } }
  action recipientBecomesIdle = all { not(st.firstAccepted), not(st.secondAccepted),
    st.process == ProcessWorking or st.process == ProcessBlocked, st' = { ...st, process: ProcessIdle } }
  // Exact current pre-read loop: creation order gates second, live status does not.
  action acceptSecondWholePrompt = all { st.batchOpen, st.secondInBatch, (not(st.firstInBatch) or st.first == DeliveredRead), st.second == Queued, st.runtimeOpen, not(st.secondAccepted), st.stableP1Live,
    st' = { ...st, secondAccepted: true, secondAccepts: st.secondAccepts + 1,
      secondAcceptedWhileFirstWorking: st.firstTurn and st.process == ProcessWorking,
      secondWrite: BodyAndEnter, secondTarget: StableP1 } }
  action acceptSecondFromNudgedRoom = all { st.batchOpen, st.batchOrigin == Send, st.batchRoomScoped, st.secondInBatch, st.second == Queued, st.runtimeOpen,
    not(st.secondAccepted), st.stableP1Live,
    st' = { ...st, secondAccepted: true, secondAccepts: st.secondAccepts + 1, secondWrite: BodyAndEnter, secondTarget: StableP1 } }
  action markSecondDeliveredRead = all { st.batchOpen, st.second == Queued, st.secondAccepted,
    st' = { ...st, second: DeliveredRead, secondMarked: true, batchOpen: false } }
  action recipientStartsSecondTurn = all { st.secondAccepted, not(st.secondTurn), st' = { ...st, process: ProcessWorking, secondTurn: true } }
  action failFirstMark = all { st.batchOpen, st.firstAccepted, st.first == Queued, st' = { ...st, batchOpen: false } }
  action crash = all { st.runtimeOpen, st' = { ...st, runtimeOpen: false, process: ProcessDown, batchOpen: false } }
  action restart = all { not(st.runtimeOpen), st' = { ...st, runtimeOpen: true, process: ProcessIdle, app: Idle, startupPending: true, startupCount: 0 } }
  action observedWorkingToIdle = all { st.app == Working, st.process == ProcessIdle, st' = { ...st, app: Idle, idleEvent: true } }
  action observedBlockedToIdle = all { st.app == Blocked, st.process == ProcessIdle, st' = { ...st, app: Idle, idleEvent: true } }
  action stableP1DisappearsAndNameReused = all { st.stableP1Live, st' = { ...st, stableP1Live: false, aliasNewPane: true, batchOpen: false } }
  action step = any { recipientStartsExistingTurn, sendFirst, sendSecond, sendFirstImmediately, sendSecondImmediately,
    queueOlderRoomA, sendNewerRoomB,
    beginFromStartup, beginFromIdleTransition, beginFromPresentationOnlyIdle,
    acceptFirstWholePrompt, markFirstDeliveredRead, recipientStartsFirstTurn, acceptSecondWholePrompt, markSecondDeliveredRead,
    acceptSecondFromNudgedRoom, recipientStartsSecondTurn, failFirstMark, crash, restart, appObservesWorking, recipientBlocks, appObservesBlocked,
    recipientBecomesIdle, observedWorkingToIdle, observedBlockedToIdle, presentationChangedWhileIdle, unrelatedApiRequest,
    stableP1DisappearsAndNameReused }
  // Test-only action set: the same current actions, restricted to the real
  // busy-queue/pre-read path so MBT can find its finite counterexample.
  action busyStep = any { recipientStartsExistingTurn, appObservesWorking, sendFirst, sendSecond,
    recipientBecomesIdle, observedWorkingToIdle, beginFromIdleTransition, acceptFirstWholePrompt,
    markFirstDeliveredRead, recipientStartsFirstTurn, acceptSecondWholePrompt }
  // These current RED properties target the real pre-read batch opened by a
  // recorded busy-to-Idle transition, not two impossible coalesced send APIs.
  val noBusySecondPrompt = not(st.batchOrigin == IdleTransition and st.twoRowsInBatch and st.secondAcceptedWhileFirstWorking)
  val onePromptPerTurn = not(st.batchOrigin == IdleTransition and st.twoRowsInBatch and st.firstAccepted and st.secondAccepted)
  val noDuplicateTurn = st.firstAccepts <= 1 and st.secondAccepts <= 1
  val marksRequireBusyStartEvidence = (not(st.firstMarked) or st.firstTurn) and (not(st.secondMarked) or st.secondTurn)
  val creationOrder = st.secondAccepted implies st.first == DeliveredRead
  val startupAtMostOncePerLifetime = st.startupCount <= 1
  val noAliasFallback = st.firstTarget != AliasFallback and st.secondTarget != AliasFallback
  val noPresentationOnlyIdleFlush = st.batchOrigin != PresentationOnlyIdle
  val noUnrelatedApiFlush = st.batchOrigin != UnrelatedApi
  val singleRowBatchCloses = not(st.firstMarked and st.firstInBatch and not(st.secondInBatch) and st.batchOpen)
  val notSingleRowBatchClosed = not(st.firstMarked and st.firstInBatch and not(st.secondInBatch) and not(st.batchOpen))
}

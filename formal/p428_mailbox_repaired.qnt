module p428_mailbox_repaired {
  type Status = Idle | Working | Blocked
  type Row = Absent | Queued | DeliveredRead
  type Write = NoWrite | BodyAndEnter
  type Origin = NoOrigin | NamedEvent | PresentationOnly | UnrelatedApi
  type ActiveMessage = NoActive | First | Second
  type State = { app: Status, process: Status, first: Row, second: Row, runtimeOpen: bool, event: bool,
    attempt: bool, accepted: bool, firstWrite: Write, secondWrite: Write, firstAccepts: int, secondAccepts: int,
    attemptId: int, firstAcceptedAttempt: int, secondAcceptedAttempt: int, activeMessage: ActiveMessage,
    firstTurn: bool, secondTurn: bool, blockedToIdleObserved: bool, origin: Origin, unrelatedApiEvent: bool }
  var st: State
  action init = { st' = { app: Idle, process: Idle, first: Absent, second: Absent, runtimeOpen: true, event: false,
    attempt: false, accepted: false, firstWrite: NoWrite, secondWrite: NoWrite, firstAccepts: 0, secondAccepts: 0,
    attemptId: 0, firstAcceptedAttempt: 0, secondAcceptedAttempt: 0, activeMessage: NoActive,
    firstTurn: false, secondTurn: false, blockedToIdleObserved: false, origin: NoOrigin, unrelatedApiEvent: false } }
  action sendFirst = all { st.first == Absent, st' = { ...st, first: Queued, event: true } }
  action sendSecond = all { st.second == Absent, st' = { ...st, second: Queued, event: true } }
  action beginNamedEvent = all { not(st.attempt), st.event, st.app == Idle, st.process == Idle, st.activeMessage == NoActive,
    st' = { ...st, event: false, attempt: true, accepted: false, attemptId: st.attemptId + 1,
      origin: NamedEvent } }
  action acceptFirst = all { st.attempt, st.first == Queued, not(st.accepted), st.runtimeOpen,
    st' = { ...st, accepted: true, firstAccepts: st.firstAccepts + 1, firstWrite: BodyAndEnter,
      firstAcceptedAttempt: st.attemptId, activeMessage: First } }
  action markFirst = all { st.attempt, st.first == Queued, st.accepted, st.activeMessage == First,
    st' = { ...st, first: DeliveredRead, attempt: false } }
  action recipientStartsFirstTurn = all { st.activeMessage == First, not(st.firstTurn),
    st' = { ...st, process: Working, firstTurn: true } }
  action appObservesWorking = all { st.app == Idle, st.process == Working, st' = { ...st, app: Working } }
  action recipientBlocks = all { st.process == Working, st' = { ...st, process: Blocked } }
  action appObservesBlocked = all { st.app == Working, st.process == Blocked, st' = { ...st, app: Blocked } }
  action recipientBecomesIdle = all { st.process == Working or st.process == Blocked, st' = { ...st, process: Idle } }
  action appObservesIdle = all { st.app == Working or st.app == Blocked, st.process == Idle,
    st' = { ...st, app: Idle, event: true, activeMessage: NoActive,
      blockedToIdleObserved: st.blockedToIdleObserved or st.app == Blocked } }
  // AgentList/PaneList can occur while an idle row is queued but create no named delivery event.
  action unrelatedApiRequest = all { st' = { ...st, unrelatedApiEvent: true } }
  action acceptSecond = all { st.attempt, st.first == DeliveredRead, st.second == Queued, not(st.accepted), st.runtimeOpen,
    st' = { ...st, accepted: true, secondAccepts: st.secondAccepts + 1, secondWrite: BodyAndEnter,
      secondAcceptedAttempt: st.attemptId, activeMessage: Second } }
  action markSecond = all { st.attempt, st.second == Queued, st.accepted, st.activeMessage == Second,
    st' = { ...st, second: DeliveredRead, attempt: false } }
  action recipientStartsSecondTurn = all { st.activeMessage == Second, not(st.secondTurn),
    st' = { ...st, process: Working, secondTurn: true } }
  action step = any { sendFirst, sendSecond, beginNamedEvent, acceptFirst, markFirst, recipientStartsFirstTurn,
    appObservesWorking, recipientBlocks, appObservesBlocked, recipientBecomesIdle, appObservesIdle, unrelatedApiRequest,
    acceptSecond, markSecond, recipientStartsSecondTurn }
  val noBusySecondPrompt = not(st.firstAcceptedAttempt > 0 and st.firstAcceptedAttempt == st.secondAcceptedAttempt)
  val onePromptPerTurn = not(st.firstAcceptedAttempt > 0 and st.firstAcceptedAttempt == st.secondAcceptedAttempt)
  val creationOrder = st.secondWrite == NoWrite or st.first == DeliveredRead
  val noPresentationOnlyIdleFlush = st.origin != PresentationOnly
  val noUnrelatedApiFlush = st.origin != UnrelatedApi
  // Liveness-as-safety: a quiescent queued row must retain a named delivery event.
  val noStrandedQueuedWhenQuiescent = not(
    (st.first == Queued or st.second == Queued) and st.app == Idle and st.process == Idle and st.runtimeOpen
      and st.activeMessage == NoActive and not(st.attempt) and not(st.event))
  val bothDeliveredReachable = st.first == DeliveredRead and st.second == DeliveredRead
  val blockedThenIdleReachable = st.blockedToIdleObserved
  // activeMessage remains until App observes the recipient's return to Idle.
  val activeTurnBlocksNewAttempt = all {
    st.activeMessage == First implies st.attemptId == st.firstAcceptedAttempt,
    st.activeMessage == Second implies st.attemptId == st.secondAcceptedAttempt,
  }
  // Negated only to ask Quint for the bounded all-delivered witness.
  val notBothDelivered = not(st.first == DeliveredRead and st.second == DeliveredRead)
  val notBlockedThenIdle = not(st.blockedToIdleObserved)
}

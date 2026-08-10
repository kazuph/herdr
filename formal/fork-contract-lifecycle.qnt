module fork_contract_lifecycle {
  // Current fork mailbox lifecycle: only availability events start retries.
  type Row = Absent | Queued | InFlight | Delivered | Cancelled
  type Runtime = Open | Closed
  type State = { row: Row, runtime: Runtime, availability: bool, writeStarted: bool,
    writeSucceeded: bool, restartObserved: bool, cancelObserved: bool, retryEvent: bool,
    panePresent: bool, restorableOwner: bool, ledgerPresent: bool, paneGeneration: int,
    ledgerGeneration: int }
  var st: State
  action init = { st' = { row: Absent, runtime: Open, availability: true, writeStarted: false,
    writeSucceeded: false, restartObserved: false, cancelObserved: false, retryEvent: false,
    panePresent: true, restorableOwner: true, ledgerPresent: true, paneGeneration: 1,
    ledgerGeneration: 1 } }
  action enqueue = all { st.row == Absent, st' = { ...st, row: Queued, retryEvent: true } }
  action beginWrite = all { st.row == Queued, st.runtime == Open, st.availability, st.retryEvent,
    st' = { ...st, row: InFlight, writeStarted: true, retryEvent: false } }
  action writeOk = all { st.row == InFlight, st.runtime == Open, st' = { ...st, row: Delivered, writeSucceeded: true } }
  action writeFails = all { st.row == InFlight, st' = { ...st, row: Queued, retryEvent: false } }
  action runtimeCloses = all { st.panePresent, st.runtime == Open, st' = { ...st, runtime: Closed } }
  action runtimeRestarts = all { st.panePresent, st.runtime == Closed,
    st' = { ...st, runtime: Open, restartObserved: true, retryEvent: st.row == Queued } }
  action availabilityRetry = all { st.row == Queued, st.runtime == Open,
    st' = { ...st, availability: true, retryEvent: true } }
  action cancel = all { st.row == Queued or st.row == InFlight,
    st' = { ...st, row: Cancelled, cancelObserved: true, retryEvent: false } }
  action paneDiesRestorable = all { st.panePresent, st.restorableOwner,
    st' = { ...st, runtime: Closed, panePresent: true, ledgerPresent: true } }
  action paneDiesShell = all { st.panePresent, not(st.restorableOwner),
    st' = { ...st, runtime: Closed, panePresent: false, ledgerPresent: false } }
  action explicitClose = all { st.panePresent,
    st' = { ...st, runtime: Closed, panePresent: false, ledgerPresent: false } }
  action markShell = all { st.panePresent, st' = { ...st, restorableOwner: false } }
  action reusePane = all { not(st.panePresent), not(st.ledgerPresent),
    st' = { ...st, runtime: Open, panePresent: true, restorableOwner: false,
      paneGeneration: st.paneGeneration + 1 } }
  action attachLedger = all { st.panePresent, not(st.ledgerPresent),
    st' = { ...st, ledgerPresent: true, ledgerGeneration: st.paneGeneration } }
  action step = any { enqueue, beginWrite, writeOk, writeFails, runtimeCloses, runtimeRestarts,
    availabilityRetry, cancel, paneDiesRestorable, paneDiesShell, explicitClose, markShell, reusePane,
    attachLedger }
  val deliveredRequiresWrite = st.row == Delivered implies st.writeStarted and st.writeSucceeded
  val cancelledCannotRetry = st.row == Cancelled implies not(st.retryEvent)
  val retryOnlyForQueued = st.retryEvent implies st.row == Queued
  val ledgerHasLiveOwner = st.ledgerPresent implies st.panePresent
  val ledgerMatchesGeneration = st.ledgerPresent implies st.ledgerGeneration == st.paneGeneration
  val removedPaneHasNoLedger = not(st.panePresent) implies not(st.ledgerPresent)
  val deliveredIsReachable = st.row != Delivered
  val retainedRestorablePaneIsReachable = not(st.panePresent and st.ledgerPresent and st.runtime == Closed)
  val cancelledIsReachable = st.row != Cancelled
  val failedWriteQueueIsReachable = not(st.row == Queued and st.writeStarted and not(st.retryEvent))
  val permanentDeletionIsReachable = st.panePresent or st.ledgerPresent
  val reusedGenerationLedgerIsReachable =
    not(st.panePresent and st.ledgerPresent and st.paneGeneration > 1 and st.ledgerGeneration == st.paneGeneration)
}

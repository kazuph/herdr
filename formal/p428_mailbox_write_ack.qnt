module p428_mailbox_write_ack {
  type Row = Queued | Delivered
  type Write = NotQueued | ActorQueued | PtyPartial | PtyWritten | PtyFailed
  type State = {
    row: Row,
    write: Write,
    inFlight: bool,
    completionPending: bool,
    actorAccepting: bool,
    appBlocked: bool,
    sharedEventChannelFull: bool,
    sharedEventReservations: int,
    apiResponses: int,
    markCount: int
  }

  var st: State

  action init = {
    st' = {
      row: Queued,
      write: NotQueued,
      inFlight: false,
      completionPending: false,
      actorAccepting: true,
      appBlocked: false,
      sharedEventChannelFull: false,
      sharedEventReservations: 0,
      apiResponses: 0,
      markCount: 0
    }
  }

  action queueActorWrite = all {
    st.row == Queued,
    st.write == NotQueued,
    not(st.inFlight),
    st.actorAccepting,
    st' = { ...st, write: ActorQueued, inFlight: true }
  }

  action writePartial = all {
    st.write == ActorQueued,
    st.inFlight,
    st' = { ...st, write: PtyPartial }
  }

  action ptyWriteSucceeds = all {
    st.write == ActorQueued or st.write == PtyPartial,
    st.inFlight,
    st.actorAccepting,
    st' = { ...st, write: PtyWritten, completionPending: true }
  }

  action ptyWriteFails = all {
    st.write == ActorQueued or st.write == PtyPartial,
    st.inFlight,
    st' = {
      ...st,
      write: PtyFailed,
      completionPending: true,
      actorAccepting: false
    }
  }

  action processSuccessfulCompletion = all {
    st.row == Queued,
    st.write == PtyWritten,
    st.inFlight,
    st.completionPending,
    st' = {
      ...st,
      row: Delivered,
      inFlight: false,
      completionPending: false,
      markCount: st.markCount + 1
    }
  }

  action processFailedCompletion = all {
    st.row == Queued,
    st.write == PtyFailed,
    st.inFlight,
    st.completionPending,
    st' = { ...st, inFlight: false, completionPending: false }
  }

  action restartActor = all {
    st.row == Queued,
    st.write == PtyFailed,
    not(st.inFlight),
    not(st.actorAccepting),
    st' = { ...st, write: NotQueued, actorAccepting: true }
  }

  action unrelatedApiResponds = all {
    not(st.appBlocked),
    st' = { ...st, apiResponses: st.apiResponses + 1 }
  }

  action fillSharedEventChannel = all {
    not(st.sharedEventChannelFull),
    st' = { ...st, sharedEventChannelFull: true }
  }

  action drainSharedEventChannel = all {
    st.sharedEventChannelFull,
    st' = { ...st, sharedEventChannelFull: false }
  }

  action manualInbox = all {
    st.row == Queued,
    not(st.inFlight),
    st' = {
      ...st,
      row: Delivered,
      write: NotQueued,
      markCount: st.markCount + 1
    }
  }

  // Historical behavior: the App waited synchronously after actor acceptance.
  action waitSynchronously = all {
    st.write == ActorQueued or st.write == PtyPartial,
    st.inFlight,
    not(st.completionPending),
    st' = { ...st, appBlocked: true }
  }

  // Historical behavior: actor queue acceptance was treated as delivery.
  action markAfterActorQueue = all {
    st.row == Queued,
    st.write == ActorQueued,
    st' = { ...st, row: Delivered, markCount: st.markCount + 1 }
  }

  action currentStep = any {
    queueActorWrite,
    writePartial,
    ptyWriteSucceeds,
    ptyWriteFails,
    waitSynchronously,
    markAfterActorQueue,
    unrelatedApiResponds
  }

  action repairedStep = any {
    queueActorWrite,
    writePartial,
    ptyWriteSucceeds,
    ptyWriteFails,
    processSuccessfulCompletion,
    processFailedCompletion,
    restartActor,
    unrelatedApiResponds,
    fillSharedEventChannel,
    drainSharedEventChannel,
    manualInbox
  }

  val deliveredRequiresPtyWrite =
    st.row == Delivered implies st.write == PtyWritten or st.write == NotQueued
  val failedWriteRemainsQueued = st.write == PtyFailed implies st.row == Queued
  val appNeverBlocks = not(st.appBlocked)
  val completionMatchesInFlight = st.completionPending implies st.inFlight
  val markedAtMostOnce = st.markCount <= 1
  val mailboxNeverReservesSharedEventCapacity = st.sharedEventReservations == 0
}

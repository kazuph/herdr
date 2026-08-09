module p428_mailbox_immediate_steering {
  type Status = Idle | Working | Blocked | Unknown | Done
  type Row = Absent | Queued | DeliveredRead
  type Origin = NoOrigin | Send | StatusChange | Startup | Accept | SubmitFailed | Presentation | Unrelated
  type State = {
    status: Status,
    row: Row,
    runtimeOpen: bool,
    deliveryEvent: bool,
    retryOnReport: bool,
    sameStatusRetrySeen: bool,
    accepts: int,
    acceptsBeforeAction: int,
    acceptedWhileWorking: bool,
    acceptedWhileBlocked: bool,
    acceptedWhileUnavailable: bool,
    presentationSeen: bool,
    unrelatedApiSeen: bool,
    origin: Origin,
  }
  var st: State

  action init = { st' = {
    status: Working,
    row: Absent,
    runtimeOpen: true,
    deliveryEvent: false,
    retryOnReport: false,
    sameStatusRetrySeen: false,
    accepts: 0,
    acceptsBeforeAction: 0,
    acceptedWhileWorking: false,
    acceptedWhileBlocked: false,
    acceptedWhileUnavailable: false,
    presentationSeen: false,
    unrelatedApiSeen: false,
    origin: NoOrigin,
  } }

  // Send persists first. Idle/Working/Done recipients receive a named event now;
  // unavailable recipients retain only the durable queued row.
  action sendAvailable = all {
    st.row == Absent,
    st.runtimeOpen,
    st.status == Idle or st.status == Working or st.status == Done,
    st' = { ...st, row: Queued, deliveryEvent: true, origin: Send, acceptsBeforeAction: st.accepts },
  }
  action sendUnavailable = all {
    st.row == Absent,
    not(st.runtimeOpen) or st.status == Blocked or st.status == Unknown,
    st' = { ...st, row: Queued, deliveryEvent: false, origin: Send, acceptsBeforeAction: st.accepts },
  }

  // Runtime acceptance is a safe processing boundary chosen by the recipient
  // UI. It is not delayed until an observed Idle state.
  action acceptAvailable = all {
    st.row == Queued,
    st.deliveryEvent,
    st.runtimeOpen,
    st.status == Idle or st.status == Working or st.status == Done,
    st' = {
      ...st,
      row: DeliveredRead,
      deliveryEvent: false,
      retryOnReport: false,
      accepts: st.accepts + 1,
      acceptsBeforeAction: st.accepts,
      acceptedWhileWorking: st.acceptedWhileWorking or st.status == Working,
      acceptedWhileBlocked: st.acceptedWhileBlocked or st.status == Blocked,
      acceptedWhileUnavailable: st.acceptedWhileUnavailable or st.status == Unknown,
      origin: Accept,
    },
  }

  // A full/closed runtime input channel does not make msg.send successful.
  // The durable row remains queued and the next explicit same-status agent
  // report is the target-specific, event-driven retry boundary.
  action submitFails = all {
    st.row == Queued,
    st.deliveryEvent,
    st.runtimeOpen,
    st.status == Idle or st.status == Working or st.status == Done,
    st' = {
      ...st,
      deliveryEvent: false,
      retryOnReport: true,
      origin: SubmitFailed,
      acceptsBeforeAction: st.accepts,
    },
  }
  action sameAvailableStatusReported = all {
    st.row == Queued,
    not(st.deliveryEvent),
    st.retryOnReport,
    st.runtimeOpen,
    st.status == Idle or st.status == Working or st.status == Done,
    st' = {
      ...st,
      deliveryEvent: true,
      retryOnReport: false,
      sameStatusRetrySeen: true,
      origin: StatusChange,
      acceptsBeforeAction: st.accepts,
    },
  }

  action becomeWorkingWithQueued = all {
    st.status != Working,
    st.row == Queued,
    st.runtimeOpen,
    st' = { ...st, status: Working, deliveryEvent: true, origin: StatusChange, acceptsBeforeAction: st.accepts },
  }
  action becomeWorkingWithoutQueued = all {
    st.status != Working,
    st.row != Queued,
    st' = { ...st, status: Working, origin: StatusChange, acceptsBeforeAction: st.accepts },
  }
  action becomeIdleWithQueued = all {
    st.status != Idle,
    st.row == Queued,
    st.runtimeOpen,
    st' = { ...st, status: Idle, deliveryEvent: true, origin: StatusChange, acceptsBeforeAction: st.accepts },
  }
  action becomeIdleWithoutQueued = all {
    st.status != Idle,
    st.row != Queued,
    st' = { ...st, status: Idle, origin: StatusChange, acceptsBeforeAction: st.accepts },
  }
  action becomeBlocked = all { st.status != Blocked, st' = { ...st, status: Blocked, deliveryEvent: false, origin: StatusChange, acceptsBeforeAction: st.accepts } }
  action becomeUnknown = all { st.status != Unknown, st' = { ...st, status: Unknown, deliveryEvent: false, origin: StatusChange, acceptsBeforeAction: st.accepts } }
  action becomeDoneWithQueued = all {
    st.status != Done,
    st.row == Queued,
    st.runtimeOpen,
    st' = { ...st, status: Done, deliveryEvent: true, origin: StatusChange, acceptsBeforeAction: st.accepts },
  }
  action becomeDoneWithoutAvailableQueue = all {
    st.status != Done,
    not(st.row == Queued and st.runtimeOpen),
    st' = { ...st, status: Done, deliveryEvent: false, origin: StatusChange, acceptsBeforeAction: st.accepts },
  }

  action crash = all { st.runtimeOpen, st' = { ...st, runtimeOpen: false, deliveryEvent: false, origin: Startup, acceptsBeforeAction: st.accepts } }
  action restartAvailableWithQueued = all {
    not(st.runtimeOpen),
    st.row == Queued,
    st.status == Idle or st.status == Working or st.status == Done,
    st' = { ...st, runtimeOpen: true, deliveryEvent: true, origin: Startup, acceptsBeforeAction: st.accepts },
  }
  action restartWithoutAvailableQueue = all {
    not(st.runtimeOpen),
    not(st.row == Queued and (st.status == Idle or st.status == Working or st.status == Done)),
    st' = { ...st, runtimeOpen: true, origin: Startup, acceptsBeforeAction: st.accepts },
  }

  action presentationUpdate = all {
    st' = { ...st, presentationSeen: true, origin: Presentation, acceptsBeforeAction: st.accepts }
  }
  action unrelatedApiRequest = all {
    st' = { ...st, unrelatedApiSeen: true, origin: Unrelated, acceptsBeforeAction: st.accepts }
  }

  action step = any {
    sendAvailable,
    sendUnavailable,
    acceptAvailable,
    submitFails,
    sameAvailableStatusReported,
    becomeWorkingWithQueued,
    becomeWorkingWithoutQueued,
    becomeIdleWithQueued,
    becomeIdleWithoutQueued,
    becomeBlocked,
    becomeUnknown,
    becomeDoneWithQueued,
    becomeDoneWithoutAvailableQueue,
    crash,
    restartAvailableWithQueued,
    restartWithoutAvailableQueue,
    presentationUpdate,
    unrelatedApiRequest,
  }

  // Focused witness sets keep the same production actions while removing
  // unrelated stutter from the concrete evidence trace.
  action workingSendStep = any { sendAvailable, acceptAvailable }
  action unavailableSendStep = any { becomeBlocked, sendUnavailable }
  action failedSubmitRetryStep = any {
    sendAvailable,
    submitFails,
    sameAvailableStatusReported,
    acceptAvailable,
  }

  // Liveness-as-safety: every durable row at an available recipient retains a
  // named event instead of waiting for a future Idle transition.
  val availableQueueHasDeliveryEvent = not(
    st.row == Queued and st.runtimeOpen and (st.status == Idle or st.status == Working or st.status == Done)
      and not(st.deliveryEvent) and not(st.retryOnReport))
  val workingSteeringIsImmediate = not(
    st.row == Queued and st.runtimeOpen and st.status == Working
      and not(st.deliveryEvent) and not(st.retryOnReport))
  val unavailableNeverAccepts = not(st.acceptedWhileBlocked or st.acceptedWhileUnavailable)
  val noDuplicateAcceptance = st.accepts <= 1
  val presentationDoesNotDeliver = not(
    st.origin == Presentation and st.accepts != st.acceptsBeforeAction)
  val unrelatedApiDoesNotDeliver = not(
    st.origin == Unrelated and st.accepts != st.acceptsBeforeAction)

  // Negated only to ask Quint for concrete reachability witnesses.
  val notWorkingSteeringReachable = not(st.acceptedWhileWorking)
  val notUnavailableQueueReachable = not(
    st.row == Queued and (st.status == Blocked or st.status == Unknown))
  val notSameStatusRetryReachable = not(st.sameStatusRetrySeen)
}

module p428_mailbox_input_current {
  type PromptWrite = NoWrite | BodyOnly | EnterOnly | BodyAndEnter
  type State = { runtimeOpen: bool, bodyAccepted: bool, enterRejected: bool, write: PromptWrite }
  var st: State
  action init = { st' = { runtimeOpen: true, bodyAccepted: false, enterRejected: false, write: NoWrite } }
  action acceptBody = all { st.runtimeOpen, not(st.bodyAccepted), st' = { ...st, bodyAccepted: true, write: BodyOnly } }
  action closeRuntimeAfterBody = all { st.bodyAccepted, st.runtimeOpen, st' = { ...st, runtimeOpen: false } }
  action rejectEnter = all { st.bodyAccepted, not(st.runtimeOpen), st' = { ...st, enterRejected: true } }
  action step = any { acceptBody, closeRuntimeAfterBody, rejectEnter }
  val noPartialPrompt = not(st.enterRejected and st.write == BodyOnly)
}

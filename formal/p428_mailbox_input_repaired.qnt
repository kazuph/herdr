module p428_mailbox_input_repaired {
  type PromptWrite = NoWrite | BodyOnly | EnterOnly | BodyAndEnter
  type State = { runtimeOpen: bool, accepted: bool, write: PromptWrite }
  var st: State
  action init = { st' = { runtimeOpen: true, accepted: false, write: NoWrite } }
  action acceptBodyAndEnterTogether = all { st.runtimeOpen, not(st.accepted), st' = { ...st, accepted: true, write: BodyAndEnter } }
  action closeBeforeAcceptance = all { st.runtimeOpen, not(st.accepted), st' = { ...st, runtimeOpen: false } }
  action step = any { acceptBodyAndEnterTogether, closeBeforeAcceptance }
  val noPartialPrompt = st.write != BodyOnly and st.write != EnterOnly
}

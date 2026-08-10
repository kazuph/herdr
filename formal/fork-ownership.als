module fork_ownership

sig Workspace {}
sig PaneId {}
sig Generation {}
sig Pane { owner: one Workspace, publicId: one PaneId, generation: one Generation }
sig LedgerEntry { ledgerPane: one Pane, ledgerGeneration: one Generation }
sig Popup { popupOwner: one Workspace, popupPane: one Pane }

fact PublicPaneIdsAreUnique { all disj left, right: Pane | left.publicId != right.publicId }
fact LedgerUsesCurrentGeneration { all entry: LedgerEntry | entry.ledgerGeneration = entry.ledgerPane.generation }
fact PopupPaneHasTheRecordedOwner { all popup: Popup | popup.popupPane.owner = popup.popupOwner }
assert PaneIdIdentifiesAtMostOnePane { all id: PaneId | lone publicId.id }
assert LedgerCannotCrossPaneGeneration {
  all entry: LedgerEntry | entry.ledgerGeneration = entry.ledgerPane.generation
}
assert LedgerCannotOutliveItsPane { all entry: LedgerEntry | one entry.ledgerPane }
assert PopupOwnershipCannotDrift { all popup: Popup | popup.popupPane.owner = popup.popupOwner }
pred OwnershipWitness {
  some Workspace and some Pane and some LedgerEntry and some Popup
  some disj left, right: Pane | left.owner != right.owner
}
check PaneIdIdentifiesAtMostOnePane for 4
check LedgerCannotCrossPaneGeneration for 4
check LedgerCannotOutliveItsPane for 4
check PopupOwnershipCannotDrift for 4
run OwnershipWitness for 4 but exactly 2 Workspace, exactly 2 Pane

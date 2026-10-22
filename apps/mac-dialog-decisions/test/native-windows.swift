// Concatenate after DialogApplication declarations, before the app entrypoint.
// Opens real AppKit windows; no substitute controls or dialog processes.
let testApp = NSApplication.shared
let owner = DialogApplication()
testApp.delegate = owner
testApp.setActivationPolicy(.accessory)
let directory = CommandLine.arguments[1]
for index in 1...3 {
  owner.open(key: "\(index == 3 ? "mini" : "local") same-\(index)", path: "\(directory)/native/\(index).json")
}
precondition(owner.windows.count == 3)
precondition(NSApp.dockTile.badgeLabel == "3")
precondition(NSApp.activationPolicy() == .regular)
precondition(owner.windows.values.allSatisfy { $0.panel.level == .normal && !$0.panel.collectionBehavior.contains(.canJoinAllSpaces) && $0.panel.collectionBehavior.contains(.moveToActiveSpace) })
let draft = owner.windows["local same-1"]!
draft.textView?.string = "書きかけの日本語"
draft.cardClicked(draft.cards[0])
let menu = owner.applicationDockMenu(testApp)!
precondition(menu.items.count == 3)
precondition(menu.items.allSatisfy { $0.title.contains("herdr 隔離検証 › impl ›") && $0.title.contains("隔離テスト") })
owner.windows["local same-2"]!.close()
precondition(NSApp.dockTile.badgeLabel == "2")
owner.open(key: "mini new", path: "\(directory)/native/3.json")
owner.open(key: "mini new", path: "\(directory)/native/3.json")
precondition(owner.windows.count == 3)
precondition(draft.currentText() == "書きかけの日本語")
precondition(draft.checkedIds() == ["a"])
owner.applicationDidBecomeActive(Notification(name: NSApplication.didBecomeActiveNotification))
precondition(draft.currentText() == "書きかけの日本語")
var evidence: [String: Any] = ["windows": owner.windows.count, "window_level": draft.panel.level.rawValue, "dock_badge": NSApp.dockTile.badgeLabel ?? "", "menu": menu.items.map { $0.title }, "draft": draft.currentText(), "selected": draft.checkedIds()]
for appearance in [NSAppearance.Name.aqua, .darkAqua] {
  testApp.appearance = NSAppearance(named: appearance)
  let view = draft.panel.contentView!
  view.layoutSubtreeIfNeeded()
  if let bitmap = view.bitmapImageRepForCachingDisplay(in: view.bounds) {
    view.cacheDisplay(in: view.bounds, to: bitmap)
    try bitmap.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: "\(directory)/dialog-\(appearance.rawValue).png"))
  }
}
for controller in Array(owner.windows.values) { controller.close() }
precondition(owner.windows.isEmpty)
precondition(NSApp.dockTile.badgeLabel == nil)
precondition(NSApp.activationPolicy() == .accessory)
evidence["empty_activation_policy"] = NSApp.activationPolicy().rawValue
try JSONSerialization.data(withJSONObject: evidence, options: [.prettyPrinted, .sortedKeys]).write(to: URL(fileURLWithPath: "\(directory)/native-window-contract.json"))
print("PASS: real keyed AppKit windows, normal level, Dock badge/menu, draft retention, zero-window accessory")

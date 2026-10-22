// herdr decision dialog — a native AppKit panel built from the daemon's spec.
//
//   herdr-decision-dialog <spec.json>
//
// The daemon (mac-dialog-decisions.mjs) compiles this file with swiftc and
// runs the binary directly. The panel never activates herdr's process and
// never pulls focus back: it is a normal-level, non-activating panel that appears
// on top of the current Space once, and the user clicks into it when ready.
// Every control accepts the first click, so a click on an option answers even
// while another app is frontmost.
//
// Output: JSON lines on stdout (and appended to spec.result_path):
//   {"event":"shown","pid":N}
//   {"result":"answer","option_ids":[...],"text":"..."}
//   {"result":"later"}          あとで / Esc / close button
// Each panel closes when its spec disappears; the app exits when the daemon dies,
// so no window outlives the decision.

import AppKit
import Foundation

// MARK: - spec

struct DialogOption: Decodable {
  let id: String
  let label: String
  let description: String?
  let preview: String?
  let preview_unavailable: Bool?
}

struct DialogDetail: Decodable {
  let label: String
  let value: String
  let monospace: Bool?
}

struct DialogSpec: Decodable {
  let window_title: String
  let source: String?        // 🤖 claude
  let location: String?      // プロジェクト › タブ › ペイン
  let machine: String?       // shown only for a saved machine
  let question_label: String?
  let title: String
  let body: String?
  let details: [DialogDetail]?
  let options: [DialogOption]
  let multi_select: Bool
  let allow_text: Bool
  let deadline: String?
  let queue_remaining: Int?
  let later_label: String?
  let result_path: String?
  let daemon_pid: Int32?
}

// MARK: - output

func emit(_ object: [String: Any], resultPath: String? = nil) {
  guard let data = try? JSONSerialization.data(withJSONObject: object),
    var line = String(data: data, encoding: .utf8) else { return }
  line += "\n"
  let bytes = Data(line.utf8)
  FileHandle.standardOutput.write(bytes)
  if let path = resultPath {
    if let handle = FileHandle(forWritingAtPath: path) {
      handle.seekToEndOfFile()
      handle.write(bytes)
      handle.closeFile()
    } else {
      FileManager.default.createFile(atPath: path, contents: bytes)
    }
  }
}

// MARK: - look

enum Look {
  static let base: CGFloat = 15
  static let small: CGFloat = 13
  static let title: CGFloat = 19
  static let width: CGFloat = 720
  static let pad: CGFloat = 22
  static let accent = NSColor.controlAccentColor
}

func label(_ text: String, size: CGFloat, weight: NSFont.Weight = .regular,
           color: NSColor = .labelColor, mono: Bool = false, selectable: Bool = true) -> NSTextField {
  let field = NSTextField(wrappingLabelWithString: text)
  field.font = mono
    ? NSFont.monospacedSystemFont(ofSize: size, weight: weight)
    : NSFont.systemFont(ofSize: size, weight: weight)
  field.textColor = color
  field.isSelectable = selectable
  field.lineBreakMode = .byWordWrapping
  field.translatesAutoresizingMaskIntoConstraints = false
  field.setContentCompressionResistancePriority(.required, for: .vertical)
  return field
}

// Accept the first click: the panel is usually not key when the user reaches
// for it, and AppKit otherwise swallows that click to activate the window.
final class FirstClickButton: NSButton {
  override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
  override var needsPanelToBecomeKey: Bool { true }
}

final class FirstClickTextView: NSTextView {
  var onCommandReturn: (() -> Void)?
  override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
  override func keyDown(with event: NSEvent) {
    if event.modifierFlags.contains(.command),
      event.keyCode == 36 || event.keyCode == 76
    {
      onCommandReturn?()
      return
    }
    super.keyDown(with: event)
  }
}

// A non-activating panel becomes key (without activating the process) when
// the clicked view asks for it, so every view in the panel does: after any
// click inside, Esc / ⌘↩ / ⌘1… reach the panel.
//
// The process only becomes active when the user clicks into the panel (never
// on its own): text input — including Japanese input methods — needs an
// active app, and a click is the user's own choice to start answering.
final class KeyPanel: NSPanel {
  override var canBecomeKey: Bool { true }
  override func sendEvent(_ event: NSEvent) {
    if event.type == .leftMouseDown,
      NSWorkspace.shared.frontmostApplication?.processIdentifier
        != ProcessInfo.processInfo.processIdentifier
    {
      NSApp.activate(ignoringOtherApps: true)
      makeKey()
    }
    super.sendEvent(event)
  }
  override func cancelOperation(_ sender: Any?) {
    (delegate as? DialogController)?.later()
  }
}

final class KeyRootView: NSView {
  override var needsPanelToBecomeKey: Bool { true }
  override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
}

final class FlippedView: NSView {
  override var needsPanelToBecomeKey: Bool { true }
  override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
  override var isFlipped: Bool { true }
}

// Width ranges are generated from the locked unicode-width 0.2.2 scalar
// contract used by src/screen_prompt.rs, not Foundation's font metrics.
enum TerminalWidth {
  static let ranges: [(UInt32, UInt32, Int)] = [
    (0x0, 0x1F, 0),
    (0x7F, 0x9F, 0),
    (0xAD, 0xAD, 0),
    (0x300, 0x36F, 0),
    (0x483, 0x489, 0),
    (0x591, 0x5BD, 0),
    (0x5BF, 0x5BF, 0),
    (0x5C1, 0x5C2, 0),
    (0x5C4, 0x5C5, 0),
    (0x5C7, 0x5C7, 0),
    (0x605, 0x605, 0),
    (0x610, 0x61A, 0),
    (0x61C, 0x61C, 0),
    (0x64B, 0x65F, 0),
    (0x670, 0x670, 0),
    (0x6D6, 0x6DC, 0),
    (0x6DF, 0x6E4, 0),
    (0x6E7, 0x6E8, 0),
    (0x6EA, 0x6ED, 0),
    (0x70F, 0x70F, 0),
    (0x711, 0x711, 0),
    (0x730, 0x74A, 0),
    (0x7A6, 0x7B0, 0),
    (0x7EB, 0x7F3, 0),
    (0x7FD, 0x7FD, 0),
    (0x816, 0x819, 0),
    (0x81B, 0x823, 0),
    (0x825, 0x827, 0),
    (0x829, 0x82D, 0),
    (0x859, 0x85B, 0),
    (0x890, 0x891, 0),
    (0x897, 0x89F, 0),
    (0x8CA, 0x902, 0),
    (0x93A, 0x93A, 0),
    (0x93C, 0x93C, 0),
    (0x941, 0x948, 0),
    (0x94D, 0x94D, 0),
    (0x951, 0x957, 0),
    (0x962, 0x963, 0),
    (0x981, 0x981, 0),
    (0x9BC, 0x9BC, 0),
    (0x9BE, 0x9BE, 0),
    (0x9C1, 0x9C4, 0),
    (0x9CD, 0x9CD, 0),
    (0x9D7, 0x9D7, 0),
    (0x9E2, 0x9E3, 0),
    (0x9FE, 0x9FE, 0),
    (0xA01, 0xA02, 0),
    (0xA3C, 0xA3C, 0),
    (0xA41, 0xA42, 0),
    (0xA47, 0xA48, 0),
    (0xA4B, 0xA4D, 0),
    (0xA51, 0xA51, 0),
    (0xA70, 0xA71, 0),
    (0xA75, 0xA75, 0),
    (0xA81, 0xA82, 0),
    (0xABC, 0xABC, 0),
    (0xAC1, 0xAC5, 0),
    (0xAC7, 0xAC8, 0),
    (0xACD, 0xACD, 0),
    (0xAE2, 0xAE3, 0),
    (0xAFA, 0xAFF, 0),
    (0xB01, 0xB01, 0),
    (0xB3C, 0xB3C, 0),
    (0xB3E, 0xB3F, 0),
    (0xB41, 0xB44, 0),
    (0xB4D, 0xB4D, 0),
    (0xB55, 0xB57, 0),
    (0xB62, 0xB63, 0),
    (0xB82, 0xB82, 0),
    (0xBBE, 0xBBE, 0),
    (0xBC0, 0xBC0, 0),
    (0xBCD, 0xBCD, 0),
    (0xBD7, 0xBD7, 0),
    (0xC00, 0xC00, 0),
    (0xC04, 0xC04, 0),
    (0xC3C, 0xC3C, 0),
    (0xC3E, 0xC40, 0),
    (0xC46, 0xC48, 0),
    (0xC4A, 0xC4D, 0),
    (0xC55, 0xC56, 0),
    (0xC62, 0xC63, 0),
    (0xC81, 0xC81, 0),
    (0xCBC, 0xCBC, 0),
    (0xCBF, 0xCC0, 0),
    (0xCC2, 0xCC2, 0),
    (0xCC6, 0xCC8, 0),
    (0xCCA, 0xCCD, 0),
    (0xCD5, 0xCD6, 0),
    (0xCE2, 0xCE3, 0),
    (0xD00, 0xD01, 0),
    (0xD3B, 0xD3C, 0),
    (0xD3E, 0xD3E, 0),
    (0xD41, 0xD44, 0),
    (0xD4D, 0xD4E, 0),
    (0xD57, 0xD57, 0),
    (0xD62, 0xD63, 0),
    (0xD81, 0xD81, 0),
    (0xDCA, 0xDCA, 0),
    (0xDCF, 0xDCF, 0),
    (0xDD2, 0xDD4, 0),
    (0xDD6, 0xDD6, 0),
    (0xDDF, 0xDDF, 0),
    (0xE31, 0xE31, 0),
    (0xE34, 0xE3A, 0),
    (0xE47, 0xE4E, 0),
    (0xEB1, 0xEB1, 0),
    (0xEB4, 0xEBC, 0),
    (0xEC8, 0xECE, 0),
    (0xF18, 0xF19, 0),
    (0xF35, 0xF35, 0),
    (0xF37, 0xF37, 0),
    (0xF39, 0xF39, 0),
    (0xF71, 0xF7E, 0),
    (0xF80, 0xF84, 0),
    (0xF86, 0xF87, 0),
    (0xF8D, 0xF97, 0),
    (0xF99, 0xFBC, 0),
    (0xFC6, 0xFC6, 0),
    (0x102D, 0x1030, 0),
    (0x1032, 0x1037, 0),
    (0x1039, 0x103A, 0),
    (0x103D, 0x103E, 0),
    (0x1058, 0x1059, 0),
    (0x105E, 0x1060, 0),
    (0x1071, 0x1074, 0),
    (0x1082, 0x1082, 0),
    (0x1085, 0x1086, 0),
    (0x108D, 0x108D, 0),
    (0x109D, 0x109D, 0),
    (0x1100, 0x115F, 2),
    (0x1160, 0x11FF, 0),
    (0x135D, 0x135F, 0),
    (0x1712, 0x1715, 0),
    (0x1732, 0x1734, 0),
    (0x1752, 0x1753, 0),
    (0x1772, 0x1773, 0),
    (0x17A4, 0x17A4, 2),
    (0x17B4, 0x17B5, 0),
    (0x17B7, 0x17BD, 0),
    (0x17C6, 0x17C6, 0),
    (0x17C9, 0x17D3, 0),
    (0x17D8, 0x17D8, 3),
    (0x17DD, 0x17DD, 0),
    (0x180B, 0x180F, 0),
    (0x1885, 0x1886, 0),
    (0x18A9, 0x18A9, 0),
    (0x1920, 0x1922, 0),
    (0x1927, 0x1928, 0),
    (0x1932, 0x1932, 0),
    (0x1939, 0x193B, 0),
    (0x1A17, 0x1A18, 0),
    (0x1A1B, 0x1A1B, 0),
    (0x1A56, 0x1A56, 0),
    (0x1A58, 0x1A5E, 0),
    (0x1A60, 0x1A60, 0),
    (0x1A62, 0x1A62, 0),
    (0x1A65, 0x1A6C, 0),
    (0x1A73, 0x1A7C, 0),
    (0x1A7F, 0x1A7F, 0),
    (0x1AB0, 0x1ADD, 0),
    (0x1AE0, 0x1AEB, 0),
    (0x1B00, 0x1B03, 0),
    (0x1B34, 0x1B3D, 0),
    (0x1B42, 0x1B44, 0),
    (0x1B6B, 0x1B73, 0),
    (0x1B80, 0x1B81, 0),
    (0x1BA2, 0x1BA5, 0),
    (0x1BA8, 0x1BAD, 0),
    (0x1BE6, 0x1BE6, 0),
    (0x1BE8, 0x1BE9, 0),
    (0x1BED, 0x1BED, 0),
    (0x1BEF, 0x1BF3, 0),
    (0x1C2C, 0x1C33, 0),
    (0x1C36, 0x1C37, 0),
    (0x1CD0, 0x1CD2, 0),
    (0x1CD4, 0x1CE0, 0),
    (0x1CE2, 0x1CE8, 0),
    (0x1CED, 0x1CED, 0),
    (0x1CF4, 0x1CF4, 0),
    (0x1CF8, 0x1CF9, 0),
    (0x1DC0, 0x1DFF, 0),
    (0x200B, 0x200F, 0),
    (0x202A, 0x202E, 0),
    (0x2060, 0x206F, 0),
    (0x20D0, 0x20F0, 0),
    (0x231A, 0x231B, 2),
    (0x2329, 0x232A, 2),
    (0x23E9, 0x23EC, 2),
    (0x23F0, 0x23F0, 2),
    (0x23F3, 0x23F3, 2),
    (0x25FD, 0x25FE, 2),
    (0x2614, 0x2615, 2),
    (0x2630, 0x2637, 2),
    (0x2648, 0x2653, 2),
    (0x267F, 0x267F, 2),
    (0x268A, 0x268F, 2),
    (0x2693, 0x2693, 2),
    (0x26A1, 0x26A1, 2),
    (0x26AA, 0x26AB, 2),
    (0x26BD, 0x26BE, 2),
    (0x26C4, 0x26C5, 2),
    (0x26CE, 0x26CE, 2),
    (0x26D4, 0x26D4, 2),
    (0x26EA, 0x26EA, 2),
    (0x26F2, 0x26F3, 2),
    (0x26F5, 0x26F5, 2),
    (0x26FA, 0x26FA, 2),
    (0x26FD, 0x26FD, 2),
    (0x2705, 0x2705, 2),
    (0x270A, 0x270B, 2),
    (0x2728, 0x2728, 2),
    (0x274C, 0x274C, 2),
    (0x274E, 0x274E, 2),
    (0x2753, 0x2755, 2),
    (0x2757, 0x2757, 2),
    (0x2795, 0x2797, 2),
    (0x27B0, 0x27B0, 2),
    (0x27BF, 0x27BF, 2),
    (0x2B1B, 0x2B1C, 2),
    (0x2B50, 0x2B50, 2),
    (0x2B55, 0x2B55, 2),
    (0x2CEF, 0x2CF1, 0),
    (0x2DE0, 0x2DFF, 0),
    (0x2E80, 0x2E99, 2),
    (0x2E9B, 0x2EF3, 2),
    (0x2F00, 0x2FD5, 2),
    (0x2FF0, 0x3029, 2),
    (0x302A, 0x302F, 0),
    (0x3030, 0x303E, 2),
    (0x3041, 0x3096, 2),
    (0x3099, 0x309A, 0),
    (0x309B, 0x30FF, 2),
    (0x3105, 0x312F, 2),
    (0x3131, 0x3163, 2),
    (0x3164, 0x3164, 0),
    (0x3165, 0x318E, 2),
    (0x3190, 0x31E5, 2),
    (0x31EF, 0x321E, 2),
    (0x3220, 0x3247, 2),
    (0x3250, 0xA48C, 2),
    (0xA490, 0xA4C6, 2),
    (0xA66F, 0xA672, 0),
    (0xA674, 0xA67D, 0),
    (0xA69E, 0xA69F, 0),
    (0xA6F0, 0xA6F1, 0),
    (0xA802, 0xA802, 0),
    (0xA806, 0xA806, 0),
    (0xA80B, 0xA80B, 0),
    (0xA825, 0xA826, 0),
    (0xA82C, 0xA82C, 0),
    (0xA8C4, 0xA8C5, 0),
    (0xA8E0, 0xA8F1, 0),
    (0xA8FA, 0xA8FA, 0),
    (0xA8FF, 0xA8FF, 0),
    (0xA926, 0xA92D, 0),
    (0xA947, 0xA951, 0),
    (0xA953, 0xA953, 0),
    (0xA960, 0xA97C, 2),
    (0xA980, 0xA982, 0),
    (0xA9B3, 0xA9B3, 0),
    (0xA9B6, 0xA9B9, 0),
    (0xA9BC, 0xA9BD, 0),
    (0xA9C0, 0xA9C0, 0),
    (0xA9E5, 0xA9E5, 0),
    (0xAA29, 0xAA2E, 0),
    (0xAA31, 0xAA32, 0),
    (0xAA35, 0xAA36, 0),
    (0xAA43, 0xAA43, 0),
    (0xAA4C, 0xAA4C, 0),
    (0xAA7C, 0xAA7C, 0),
    (0xAAB0, 0xAAB0, 0),
    (0xAAB2, 0xAAB4, 0),
    (0xAAB7, 0xAAB8, 0),
    (0xAABE, 0xAABF, 0),
    (0xAAC1, 0xAAC1, 0),
    (0xAAEC, 0xAAED, 0),
    (0xAAF6, 0xAAF6, 0),
    (0xABE5, 0xABE5, 0),
    (0xABE8, 0xABE8, 0),
    (0xABED, 0xABED, 0),
    (0xAC00, 0xD7A3, 2),
    (0xD7B0, 0xD7C6, 0),
    (0xD7CB, 0xD7FB, 0),
    (0xF900, 0xFAFF, 2),
    (0xFB1E, 0xFB1E, 0),
    (0xFE00, 0xFE0F, 0),
    (0xFE10, 0xFE19, 2),
    (0xFE20, 0xFE2F, 0),
    (0xFE30, 0xFE52, 2),
    (0xFE54, 0xFE66, 2),
    (0xFE68, 0xFE6B, 2),
    (0xFEFF, 0xFEFF, 0),
    (0xFF01, 0xFF60, 2),
    (0xFF9E, 0xFFA0, 0),
    (0xFFE0, 0xFFE6, 2),
    (0xFFF0, 0xFFF8, 0),
    (0x101FD, 0x101FD, 0),
    (0x102E0, 0x102E0, 0),
    (0x10376, 0x1037A, 0),
    (0x10A01, 0x10A03, 0),
    (0x10A05, 0x10A06, 0),
    (0x10A0C, 0x10A0F, 0),
    (0x10A38, 0x10A3A, 0),
    (0x10A3F, 0x10A3F, 0),
    (0x10AE5, 0x10AE6, 0),
    (0x10D24, 0x10D27, 0),
    (0x10D69, 0x10D6D, 0),
    (0x10EAB, 0x10EAC, 0),
    (0x10EFA, 0x10EFF, 0),
    (0x10F46, 0x10F50, 0),
    (0x10F82, 0x10F85, 0),
    (0x11001, 0x11001, 0),
    (0x11038, 0x11046, 0),
    (0x11070, 0x11070, 0),
    (0x11073, 0x11074, 0),
    (0x1107F, 0x11081, 0),
    (0x110B3, 0x110B6, 0),
    (0x110B9, 0x110BA, 0),
    (0x110C2, 0x110C2, 0),
    (0x11100, 0x11102, 0),
    (0x11127, 0x1112B, 0),
    (0x1112D, 0x11134, 0),
    (0x11173, 0x11173, 0),
    (0x11180, 0x11181, 0),
    (0x111B6, 0x111BE, 0),
    (0x111C0, 0x111C0, 0),
    (0x111C2, 0x111C3, 0),
    (0x111C9, 0x111CC, 0),
    (0x111CF, 0x111CF, 0),
    (0x1122F, 0x11231, 0),
    (0x11234, 0x11237, 0),
    (0x1123E, 0x1123E, 0),
    (0x11241, 0x11241, 0),
    (0x112DF, 0x112DF, 0),
    (0x112E3, 0x112EA, 0),
    (0x11300, 0x11301, 0),
    (0x1133B, 0x1133C, 0),
    (0x1133E, 0x1133E, 0),
    (0x11340, 0x11340, 0),
    (0x1134D, 0x1134D, 0),
    (0x11357, 0x11357, 0),
    (0x11366, 0x1136C, 0),
    (0x11370, 0x11374, 0),
    (0x113B8, 0x113B8, 0),
    (0x113BB, 0x113C0, 0),
    (0x113C2, 0x113C2, 0),
    (0x113C5, 0x113C5, 0),
    (0x113C7, 0x113C9, 0),
    (0x113CE, 0x113D2, 0),
    (0x113E1, 0x113E2, 0),
    (0x11438, 0x1143F, 0),
    (0x11442, 0x11444, 0),
    (0x11446, 0x11446, 0),
    (0x1145E, 0x1145E, 0),
    (0x114B0, 0x114B0, 0),
    (0x114B3, 0x114B8, 0),
    (0x114BA, 0x114BA, 0),
    (0x114BD, 0x114BD, 0),
    (0x114BF, 0x114C0, 0),
    (0x114C2, 0x114C3, 0),
    (0x115AF, 0x115AF, 0),
    (0x115B2, 0x115B5, 0),
    (0x115BC, 0x115BD, 0),
    (0x115BF, 0x115C0, 0),
    (0x115DC, 0x115DD, 0),
    (0x11633, 0x1163A, 0),
    (0x1163D, 0x1163D, 0),
    (0x1163F, 0x11640, 0),
    (0x116AB, 0x116AB, 0),
    (0x116AD, 0x116AD, 0),
    (0x116B0, 0x116B7, 0),
    (0x1171D, 0x1171D, 0),
    (0x1171F, 0x1171F, 0),
    (0x11722, 0x11725, 0),
    (0x11727, 0x1172B, 0),
    (0x1182F, 0x11837, 0),
    (0x11839, 0x1183A, 0),
    (0x11930, 0x11930, 0),
    (0x1193B, 0x1193F, 0),
    (0x11941, 0x11941, 0),
    (0x11943, 0x11943, 0),
    (0x119D4, 0x119D7, 0),
    (0x119DA, 0x119DB, 0),
    (0x119E0, 0x119E0, 0),
    (0x11A01, 0x11A0A, 0),
    (0x11A33, 0x11A38, 0),
    (0x11A3B, 0x11A3E, 0),
    (0x11A47, 0x11A47, 0),
    (0x11A51, 0x11A56, 0),
    (0x11A59, 0x11A5B, 0),
    (0x11A84, 0x11A96, 0),
    (0x11A98, 0x11A99, 0),
    (0x11B60, 0x11B60, 0),
    (0x11B62, 0x11B64, 0),
    (0x11B66, 0x11B66, 0),
    (0x11C30, 0x11C36, 0),
    (0x11C38, 0x11C3D, 0),
    (0x11C3F, 0x11C3F, 0),
    (0x11C92, 0x11CA7, 0),
    (0x11CAA, 0x11CB0, 0),
    (0x11CB2, 0x11CB3, 0),
    (0x11CB5, 0x11CB6, 0),
    (0x11D31, 0x11D36, 0),
    (0x11D3A, 0x11D3A, 0),
    (0x11D3C, 0x11D3D, 0),
    (0x11D3F, 0x11D47, 0),
    (0x11D90, 0x11D91, 0),
    (0x11D95, 0x11D95, 0),
    (0x11D97, 0x11D97, 0),
    (0x11EF3, 0x11EF4, 0),
    (0x11F00, 0x11F02, 0),
    (0x11F36, 0x11F3A, 0),
    (0x11F40, 0x11F42, 0),
    (0x11F5A, 0x11F5A, 0),
    (0x13440, 0x13440, 0),
    (0x13447, 0x13455, 0),
    (0x1611E, 0x16129, 0),
    (0x1612D, 0x1612F, 0),
    (0x16AF0, 0x16AF4, 0),
    (0x16B30, 0x16B36, 0),
    (0x16F4F, 0x16F4F, 0),
    (0x16F8F, 0x16F92, 0),
    (0x16FE0, 0x16FE3, 2),
    (0x16FE4, 0x16FE4, 0),
    (0x16FF0, 0x16FF1, 0),
    (0x16FF2, 0x16FF6, 2),
    (0x17000, 0x18CD5, 2),
    (0x18CFF, 0x18D1E, 2),
    (0x18D80, 0x18DF2, 2),
    (0x1AFF0, 0x1AFF3, 2),
    (0x1AFF5, 0x1AFFB, 2),
    (0x1AFFD, 0x1AFFE, 2),
    (0x1B000, 0x1B122, 2),
    (0x1B132, 0x1B132, 2),
    (0x1B150, 0x1B152, 2),
    (0x1B155, 0x1B155, 2),
    (0x1B164, 0x1B167, 2),
    (0x1B170, 0x1B2FB, 2),
    (0x1BC9D, 0x1BC9E, 0),
    (0x1BCA0, 0x1BCA3, 0),
    (0x1CF00, 0x1CF2D, 0),
    (0x1CF30, 0x1CF46, 0),
    (0x1D165, 0x1D169, 0),
    (0x1D16D, 0x1D182, 0),
    (0x1D185, 0x1D18B, 0),
    (0x1D1AA, 0x1D1AD, 0),
    (0x1D242, 0x1D244, 0),
    (0x1D300, 0x1D356, 2),
    (0x1D360, 0x1D376, 2),
    (0x1DA00, 0x1DA36, 0),
    (0x1DA3B, 0x1DA6C, 0),
    (0x1DA75, 0x1DA75, 0),
    (0x1DA84, 0x1DA84, 0),
    (0x1DA9B, 0x1DA9F, 0),
    (0x1DAA1, 0x1DAAF, 0),
    (0x1E000, 0x1E006, 0),
    (0x1E008, 0x1E018, 0),
    (0x1E01B, 0x1E021, 0),
    (0x1E023, 0x1E024, 0),
    (0x1E026, 0x1E02A, 0),
    (0x1E08F, 0x1E08F, 0),
    (0x1E130, 0x1E136, 0),
    (0x1E2AE, 0x1E2AE, 0),
    (0x1E2EC, 0x1E2EF, 0),
    (0x1E4EC, 0x1E4EF, 0),
    (0x1E5EE, 0x1E5EF, 0),
    (0x1E6E3, 0x1E6E3, 0),
    (0x1E6E6, 0x1E6E6, 0),
    (0x1E6EE, 0x1E6EF, 0),
    (0x1E6F5, 0x1E6F5, 0),
    (0x1E8D0, 0x1E8D6, 0),
    (0x1E944, 0x1E94A, 0),
    (0x1F004, 0x1F004, 2),
    (0x1F0CF, 0x1F0CF, 2),
    (0x1F18E, 0x1F18E, 2),
    (0x1F191, 0x1F19A, 2),
    (0x1F200, 0x1F202, 2),
    (0x1F210, 0x1F23B, 2),
    (0x1F240, 0x1F248, 2),
    (0x1F250, 0x1F251, 2),
    (0x1F260, 0x1F265, 2),
    (0x1F300, 0x1F320, 2),
    (0x1F32D, 0x1F335, 2),
    (0x1F337, 0x1F37C, 2),
    (0x1F37E, 0x1F393, 2),
    (0x1F3A0, 0x1F3CA, 2),
    (0x1F3CF, 0x1F3D3, 2),
    (0x1F3E0, 0x1F3F0, 2),
    (0x1F3F4, 0x1F3F4, 2),
    (0x1F3F8, 0x1F43E, 2),
    (0x1F440, 0x1F440, 2),
    (0x1F442, 0x1F4FC, 2),
    (0x1F4FF, 0x1F53D, 2),
    (0x1F54B, 0x1F54E, 2),
    (0x1F550, 0x1F567, 2),
    (0x1F57A, 0x1F57A, 2),
    (0x1F595, 0x1F596, 2),
    (0x1F5A4, 0x1F5A4, 2),
    (0x1F5FB, 0x1F64F, 2),
    (0x1F680, 0x1F6C5, 2),
    (0x1F6CC, 0x1F6CC, 2),
    (0x1F6D0, 0x1F6D2, 2),
    (0x1F6D5, 0x1F6D8, 2),
    (0x1F6DC, 0x1F6DF, 2),
    (0x1F6EB, 0x1F6EC, 2),
    (0x1F6F4, 0x1F6FC, 2),
    (0x1F7E0, 0x1F7EB, 2),
    (0x1F7F0, 0x1F7F0, 2),
    (0x1F90C, 0x1F93A, 2),
    (0x1F93C, 0x1F945, 2),
    (0x1F947, 0x1F9FF, 2),
    (0x1FA70, 0x1FA7C, 2),
    (0x1FA80, 0x1FA8A, 2),
    (0x1FA8E, 0x1FAC6, 2),
    (0x1FAC8, 0x1FAC8, 2),
    (0x1FACD, 0x1FADC, 2),
    (0x1FADF, 0x1FAEA, 2),
    (0x1FAEF, 0x1FAF8, 2),
    (0x20000, 0x2FFFD, 2),
    (0x30000, 0x3FFFD, 2),
    (0xE0000, 0xE0FFF, 0),
  ]
  static func scalar(_ value: UInt32) -> Int {
    var low = 0
    var high = ranges.count
    while low < high {
      let mid = (low + high) / 2
      let range = ranges[mid]
      if value < range.0 { high = mid }
      else if value > range.1 { low = mid + 1 }
      else { return range.2 }
    }
    return 1
  }
  static func cells(_ text: String) -> Int {
    text.unicodeScalars.reduce(0) { $0 + scalar($1.value) }
  }
}


final class TerminalPreview: NSView {
  let lines: [String]
  let font = NSFont.monospacedSystemFont(ofSize: Look.small, weight: .regular)
  var cell: CGFloat { ("M" as NSString).size(withAttributes: [.font: font]).width }
  var rowHeight: CGFloat { ceil(font.ascender - font.descender + font.leading) }
  override var isFlipped: Bool { true }
  init(text: String) {
    lines = text.components(separatedBy: "\n")
    super.init(frame: .zero)
    frame.size = NSSize(width: CGFloat(lines.map { TerminalWidth.cells($0) }.max() ?? 0) * cell,
                        height: CGFloat(lines.count) * rowHeight)
    setAccessibilityLabel(text)
  }
  required init?(coder: NSCoder) { fatalError() }
  override func draw(_ dirtyRect: NSRect) {
    let attributes: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: NSColor.labelColor]
    for (row, line) in lines.enumerated() {
      var column = 0
      for character in line {
        let text = String(character)
        let width = TerminalWidth.cells(text)
        defer { column += width }
        guard width > 0 else { continue }
        let natural = (text as NSString).size(withAttributes: attributes).width
        guard natural > 0 else { continue }
        NSGraphicsContext.saveGraphicsState()
        let transform = NSAffineTransform()
        transform.translateX(by: CGFloat(column) * cell, yBy: CGFloat(row) * rowHeight)
        transform.scaleX(by: CGFloat(width) * cell / natural, yBy: 1)
        transform.concat()
        (text as NSString).draw(at: .zero, withAttributes: attributes)
        NSGraphicsContext.restoreGraphicsState()
      }
    }
  }
}

// One option: number, label, description and preview in a clickable card.
final class OptionCard: NSView {
  let option: DialogOption
  let index: Int
  let multi: Bool
  var checked = false { didSet { refresh() } }
  var onClick: ((OptionCard) -> Void)?
  private var hovering = false { didSet { refresh() } }
  private let mark = NSTextField(labelWithString: "")
  private let box = NSImageView()

  init(option: DialogOption, index: Int, multi: Bool) {
    self.option = option
    self.index = index
    self.multi = multi
    super.init(frame: .zero)
    translatesAutoresizingMaskIntoConstraints = false
    wantsLayer = true
    layer?.cornerRadius = 8
    layer?.borderWidth = 1

    mark.font = NSFont.systemFont(ofSize: Look.base + 1, weight: .semibold)
    mark.translatesAutoresizingMaskIntoConstraints = false
    mark.alignment = .center

    let stack = NSStackView()
    stack.orientation = .vertical
    stack.alignment = .leading
    stack.spacing = 4
    stack.translatesAutoresizingMaskIntoConstraints = false
    let title = label(option.label, size: Look.base + 1, weight: .semibold, selectable: false)
    stack.addArrangedSubview(title)
    if let d = option.description, !d.isEmpty {
      stack.addArrangedSubview(label(d, size: Look.small + 1, color: .secondaryLabelColor, selectable: false))
    }
    if let p = option.preview, !p.isEmpty {
      let preview = TerminalPreview(text: p)
      let scroll = NSScrollView()
      scroll.translatesAutoresizingMaskIntoConstraints = false
      scroll.hasHorizontalScroller = true
      scroll.autohidesScrollers = false
      scroll.drawsBackground = true
      scroll.backgroundColor = .textBackgroundColor
      scroll.documentView = preview
      stack.addArrangedSubview(scroll)
      scroll.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
      scroll.heightAnchor.constraint(equalToConstant: preview.frame.height + NSScroller.scrollerWidth(for: .regular, scrollerStyle: .legacy)).isActive = true
    } else if option.preview_unavailable == true {
      stack.addArrangedSubview(label("プレビューあり（エージェントの画面で確認）", size: Look.small, color: .tertiaryLabelColor, selectable: false))
    }
    addSubview(mark)
    addSubview(stack)
    box.translatesAutoresizingMaskIntoConstraints = false
    addSubview(box)
    NSLayoutConstraint.activate([
      box.centerXAnchor.constraint(equalTo: mark.centerXAnchor),
      box.topAnchor.constraint(equalTo: topAnchor, constant: 10),
      mark.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12),
      mark.topAnchor.constraint(equalTo: topAnchor, constant: 11),
      mark.widthAnchor.constraint(equalToConstant: 34),
      stack.leadingAnchor.constraint(equalTo: mark.trailingAnchor, constant: 8),
      stack.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -14),
      stack.topAnchor.constraint(equalTo: topAnchor, constant: 11),
      stack.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -11),
    ])
    addTrackingArea(NSTrackingArea(rect: .zero, options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect], owner: self))
    toolTip = multi
      ? "クリックで選ぶ／外す（⌘\(index + 1)）"
      : "クリックで選ぶ（⌘\(index + 1)）。送るのは右下の「回答する」"
    refresh()
  }

  required init?(coder: NSCoder) { fatalError() }

  override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
  override var needsPanelToBecomeKey: Bool { true }
  override func mouseEntered(with event: NSEvent) { hovering = true }
  override func mouseExited(with event: NSEvent) { hovering = false }
  override func mouseDown(with event: NSEvent) { layer?.opacity = 0.75 }
  override func mouseUp(with event: NSEvent) {
    layer?.opacity = 1
    if bounds.contains(convert(event.locationInWindow, from: nil)) { onClick?(self) }
  }
  override func resetCursorRects() { addCursorRect(bounds, cursor: .pointingHand) }

  private func refresh() {
    // Checkbox for multi-choice, radio button for one-choice. A click only
    // selects; nothing is sent until 回答する / ⌘↩.
    mark.stringValue = ""
    let name = multi
      ? (checked ? "checkmark.square.fill" : "square")
      : (checked ? "largecircle.fill.circle" : "circle")
    let config = NSImage.SymbolConfiguration(pointSize: Look.base + 5, weight: .regular)
    box.image = NSImage(systemSymbolName: name, accessibilityDescription: checked ? "選択中" : "未選択")?
      .withSymbolConfiguration(config)
    box.contentTintColor = checked ? Look.accent : .secondaryLabelColor
    let active = checked || hovering
    mark.textColor = active ? Look.accent : .secondaryLabelColor
    // Layer colors are plain CGColors: resolve the dynamic system colors in
    // this view's own appearance (light or dark), and again whenever it
    // changes — otherwise the cards keep whichever look was current at
    // creation and become unreadable in the other mode.
    effectiveAppearance.performAsCurrentDrawingAppearance {
      layer?.borderColor = (checked ? Look.accent : (hovering ? Look.accent.withAlphaComponent(0.6) : NSColor.separatorColor)).cgColor
      layer?.backgroundColor = (active ? Look.accent.withAlphaComponent(checked ? 0.18 : 0.08) : NSColor.controlBackgroundColor).cgColor
    }
  }

  override func viewDidChangeEffectiveAppearance() {
    super.viewDidChangeEffectiveAppearance()
    refresh()
  }
}

// MARK: - controller

final class DialogController: NSObject, NSWindowDelegate {
  let spec: DialogSpec
  let specPath: String
  let key: String
  var finished = false
  var watchdog: Timer?
  var onClose: ((String) -> Void)?
  var panel: KeyPanel!
  var cards: [OptionCard] = []
  var textView: FirstClickTextView?
  var submitButton: FirstClickButton!
  var pendingLabel: NSTextField!

  init(spec: DialogSpec, specPath: String, key: String) {
    self.spec = spec
    self.specPath = specPath
    self.key = key
  }

  func finish(_ object: [String: Any]) {
    guard !finished else { return }
    var result = object
    result["key"] = key
    emit(result, resultPath: spec.result_path)
    close()
  }

  func close() {
    guard !finished else { return }
    finished = true
    watchdog?.invalidate()
    panel.orderOut(nil)
    panel.close()
    onClose?(key)
  }

  func checkedIds() -> [String] { cards.filter { $0.checked }.map { $0.option.id } }
  func currentText() -> String {
    (textView?.string ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
  }

  func answer(ids: [String]) {
    finish(["result": "answer", "option_ids": ids, "text": currentText()])
  }

  @objc func submit() {
    let ids = checkedIds()
    let text = currentText()
    if ids.isEmpty && text.isEmpty {
      NSSound.beep()
      return
    }
    answer(ids: ids)
  }

  @objc func later() { finish(["result": "later"]) }

  func windowShouldClose(_ sender: NSWindow) -> Bool {
    later()
    return false
  }

  func cardClicked(_ card: OptionCard) {
    // Selecting never sends: a panel that appears under the pointer while the
    // user is busy elsewhere must not turn a stray click into an answer.
    if spec.multi_select {
      card.checked.toggle()
    } else {
      // Clicking the chosen option again clears it, so a text-only answer
      // stays possible after a choice.
      let clear = card.checked
      for other in cards { other.checked = !clear && other === card }
    }
    refreshSubmit()
  }

  func refreshSubmit() {
    let ready = !checkedIds().isEmpty || !currentText().isEmpty
    submitButton.isEnabled = ready
  }

  func handleKey(_ event: NSEvent) -> NSEvent? {
    guard event.window === panel else { return event }
    let cmd = event.modifierFlags.contains(.command)
    if event.keyCode == 53 { later(); return nil }                 // Esc
    if cmd && (event.keyCode == 36 || event.keyCode == 76) {        // ⌘↩
      submit(); return nil
    }
    if cmd, let ch = event.charactersIgnoringModifiers, let n = Int(ch), n >= 1, n <= cards.count {
      cardClicked(cards[n - 1]); return nil
    }
    // The dialog has no menu bar, and ⌘V / ⌘C / ⌘X / ⌘A / ⌘Z reach a text
    // field only through the Edit menu's key equivalents. Send the same
    // standard edit actions to the focused field directly.
    if cmd, let action = editAction(event) {
      if NSApp.sendAction(action, to: nil, from: panel) { return nil }
    }
    return event
  }

  func editAction(_ event: NSEvent) -> Selector? {
    let mods = event.modifierFlags.intersection([.command, .shift, .option, .control])
    switch (event.charactersIgnoringModifiers?.lowercased(), mods) {
    case ("v", [.command]): return #selector(NSText.paste(_:))
    case ("c", [.command]): return #selector(NSText.copy(_:))
    case ("x", [.command]): return #selector(NSText.cut(_:))
    case ("a", [.command]): return #selector(NSText.selectAll(_:))
    case ("z", [.command]): return Selector(("undo:"))
    case ("z", [.command, .shift]): return Selector(("redo:"))
    default: return nil
    }
  }

  func build() {
    let content = FlippedView()
    content.translatesAutoresizingMaskIntoConstraints = false

    let stack = NSStackView()
    stack.orientation = .vertical
    stack.alignment = .leading
    stack.spacing = 12
    stack.translatesAutoresizingMaskIntoConstraints = false
    content.addSubview(stack)
    NSLayoutConstraint.activate([
      stack.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: Look.pad),
      stack.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -Look.pad),
      stack.topAnchor.constraint(equalTo: content.topAnchor, constant: 18),
      stack.bottomAnchor.constraint(equalTo: content.bottomAnchor, constant: -14),
    ])
    func full(_ v: NSView) {
      stack.addArrangedSubview(v)
      v.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
    }

    // Where it comes from — the first thing the eye lands on.
    var whereParts: [String] = []
    if let s = spec.source, !s.isEmpty { whereParts.append(s) }
    if let m = spec.machine, !m.isEmpty { whereParts.append("🖥 \(m)") }
    if !whereParts.isEmpty {
      full(label(whereParts.joined(separator: "   "), size: Look.small + 1, weight: .medium, color: .secondaryLabelColor))
    }
    if let loc = spec.location, !loc.isEmpty {
      full(label(loc, size: Look.base, weight: .semibold, color: Look.accent))
    }

    var titleText = spec.title
    if let q = spec.question_label, !q.isEmpty { titleText = "［\(q)］ " + titleText }
    full(label(titleText, size: Look.title, weight: .bold))

    if let body = spec.body, !body.isEmpty {
      full(label(body, size: Look.base))
    }

    if let details = spec.details, !details.isEmpty {
      let grid = NSStackView()
      grid.orientation = .vertical
      grid.alignment = .leading
      grid.spacing = 3
      for d in details {
        let row = label("\(d.label)：\(d.value)", size: Look.small, color: .secondaryLabelColor, mono: d.monospace == true)
        grid.addArrangedSubview(row)
        row.widthAnchor.constraint(equalTo: grid.widthAnchor).isActive = true
      }
      full(grid)
    }

    if !spec.options.isEmpty {
      let hint = spec.multi_select
        ? "当てはまるものを選んで（複数可・⌘1〜⌘\(min(spec.options.count, 9)) でも可）、右下の「回答する」か ⌘↩ で送ります。"
        : "1つ選んで（⌘1〜⌘\(min(spec.options.count, 9)) でも可）、右下の「回答する」か ⌘↩ で送ります。選んだだけでは送りません。もう一度押すと選択を外せます。"
      full(label(hint, size: Look.small, color: .secondaryLabelColor, selectable: false))
      for (i, option) in spec.options.enumerated() {
        let card = OptionCard(option: option, index: i, multi: spec.multi_select)
        card.onClick = { [weak self] c in self?.cardClicked(c) }
        cards.append(card)
        full(card)
      }
    }

    // Scrollable body so a long premise never pushes the buttons off screen.
    let scroll = NSScrollView()
    scroll.translatesAutoresizingMaskIntoConstraints = false
    scroll.hasVerticalScroller = true
    scroll.drawsBackground = false
    scroll.documentView = content
    content.widthAnchor.constraint(equalTo: scroll.contentView.widthAnchor).isActive = true

    // Bottom bar: free text, deadline and the two buttons stay put.
    let bottom = NSStackView()
    bottom.orientation = .vertical
    bottom.alignment = .leading
    bottom.spacing = 8
    bottom.translatesAutoresizingMaskIntoConstraints = false
    bottom.edgeInsets = NSEdgeInsets(top: 12, left: Look.pad, bottom: 16, right: Look.pad)

    if spec.allow_text {
      let caption = label(spec.options.isEmpty
        ? "回答（⌘↩ で送信）"
        : "自由入力（任意。選んだ選択肢と一緒に送ります。文だけでも送れます）",
        size: Look.small, color: .secondaryLabelColor, selectable: false)
      bottom.addArrangedSubview(caption)
      let tv = FirstClickTextView()
      tv.font = NSFont.systemFont(ofSize: Look.base)
      tv.isRichText = false
      tv.allowsUndo = true
      tv.isAutomaticQuoteSubstitutionEnabled = false
      tv.textContainerInset = NSSize(width: 6, height: 6)
      tv.onCommandReturn = { [weak self] in self?.submit() }
      tv.delegate = self
      let tscroll = NSScrollView()
      tscroll.translatesAutoresizingMaskIntoConstraints = false
      tscroll.hasVerticalScroller = true
      tscroll.borderType = .bezelBorder
      tscroll.documentView = tv
      tv.minSize = NSSize(width: 0, height: 0)
      tv.maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
      tv.isVerticallyResizable = true
      tv.isHorizontallyResizable = false
      tv.autoresizingMask = [.width]
      tv.textContainer?.widthTracksTextView = true
      bottom.addArrangedSubview(tscroll)
      tscroll.heightAnchor.constraint(equalToConstant: 76).isActive = true
      tscroll.widthAnchor.constraint(equalTo: bottom.widthAnchor, constant: -2 * Look.pad).isActive = true
      textView = tv
    }

    let laterLabel = spec.later_label ?? "10分"
    let laterButton = FirstClickButton(title: "あとで（\(laterLabel)後にもう一度）", target: self, action: #selector(later))
    laterButton.bezelStyle = .rounded
    laterButton.controlSize = .large
    laterButton.font = NSFont.systemFont(ofSize: Look.base)
    laterButton.toolTip = "答えずに閉じます。\(laterLabel)後にまだ答えていなければもう一度出します。Slack・アプリ・グラスではいつでも答えられます。（Esc）"

    submitButton = FirstClickButton(title: "回答する  ⌘↩", target: self, action: #selector(submit))
    submitButton.bezelStyle = .rounded
    submitButton.controlSize = .large
    submitButton.font = NSFont.systemFont(ofSize: Look.base, weight: .semibold)
    submitButton.keyEquivalent = "\r"
    submitButton.keyEquivalentModifierMask = [.command]

    var info: [String] = []
    if let d = spec.deadline, !d.isEmpty { info.append(d) }
    if let q = spec.queue_remaining, q > 0 { info.append("ほかに \(q) 件待っています") }
    let infoLabel = label(info.joined(separator: "  ·  "), size: Look.small, color: .secondaryLabelColor, selectable: false)
    pendingLabel = infoLabel

    let row = NSStackView(views: [infoLabel, NSView(), laterButton, submitButton])
    row.orientation = .horizontal
    row.spacing = 10
    row.translatesAutoresizingMaskIntoConstraints = false
    bottom.addArrangedSubview(row)
    row.widthAnchor.constraint(equalTo: bottom.widthAnchor, constant: -2 * Look.pad).isActive = true

    let separator = NSBox()
    separator.boxType = .separator
    separator.translatesAutoresizingMaskIntoConstraints = false

    let root = KeyRootView()
    root.addSubview(scroll)
    root.addSubview(separator)
    root.addSubview(bottom)
    NSLayoutConstraint.activate([
      scroll.leadingAnchor.constraint(equalTo: root.leadingAnchor),
      scroll.trailingAnchor.constraint(equalTo: root.trailingAnchor),
      scroll.topAnchor.constraint(equalTo: root.topAnchor),
      separator.leadingAnchor.constraint(equalTo: root.leadingAnchor),
      separator.trailingAnchor.constraint(equalTo: root.trailingAnchor),
      separator.topAnchor.constraint(equalTo: scroll.bottomAnchor),
      bottom.leadingAnchor.constraint(equalTo: root.leadingAnchor),
      bottom.trailingAnchor.constraint(equalTo: root.trailingAnchor),
      bottom.topAnchor.constraint(equalTo: separator.bottomAnchor),
      bottom.bottomAnchor.constraint(equalTo: root.bottomAnchor),
    ])

    let screen = NSScreen.main?.visibleFrame ?? NSRect(x: 0, y: 0, width: 1440, height: 900)
    panel = KeyPanel(
      contentRect: NSRect(x: 0, y: 0, width: Look.width, height: 400),
      styleMask: [.titled, .closable, .resizable, .nonactivatingPanel, .utilityWindow],
      backing: .buffered, defer: false)
    panel.title = spec.window_title
    panel.contentView = root
    panel.delegate = self
    panel.level = .normal
    panel.hidesOnDeactivate = false
    panel.isFloatingPanel = false
    panel.becomesKeyOnlyIfNeeded = false
    panel.collectionBehavior = [.moveToActiveSpace, .fullScreenAuxiliary]
    panel.minSize = NSSize(width: 520, height: 260)
    // Size: as tall as the content needs at the real width, capped to 85% of
    // the screen (the premise scrolls; the buttons never leave the window).
    root.layoutSubtreeIfNeeded()
    let contentHeight = stack.fittingSize.height + 32
    let bottomHeight = bottom.fittingSize.height
    let height = min(contentHeight + bottomHeight + 2, screen.height * 0.85)
    panel.setContentSize(NSSize(width: Look.width, height: height))
    panel.center()
    panel.setFrameTopLeftPoint(NSPoint(x: panel.frame.minX, y: screen.maxY - 60))
    refreshSubmit()
  }

  func show() {
    // Shown once, on top, without activating herdr's process or taking the
    // keyboard from whatever the user is doing.
    panel.orderFrontRegardless()
    emit(["event": "shown", "key": key, "pid": Int(ProcessInfo.processInfo.processIdentifier)])
  }

  // Close with the decision: the daemon deletes the spec when it is resolved
  // elsewhere or when the daemon stops; a dead daemon also ends the panel.
  func startWatchdog() {
    watchdog = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in
      guard let self else { return }
      if !FileManager.default.fileExists(atPath: self.specPath) {
        self.close()
      }
      if let pid = self.spec.daemon_pid, pid > 0, kill(pid, 0) != 0, errno == ESRCH {
        NSApp.terminate(nil)
      }
    }
  }
}

extension DialogController: NSTextViewDelegate {
  func textDidChange(_ notification: Notification) { refreshSubmit() }
}

// MARK: - app / keyed window transport

struct DialogCommand: Decodable {
  let command: String
  let key: String
  let spec_path: String?
}

final class DialogApplication: NSObject, NSApplicationDelegate {
  var windows: [String: DialogController] = [:]
  var order: [String] = []
  var cascadePoint = NSPoint.zero

  func open(key: String, path: String) {
    guard windows[key] == nil,
      let data = FileManager.default.contents(atPath: path),
      let spec = try? JSONDecoder().decode(DialogSpec.self, from: data) else { return }
    let controller = DialogController(spec: spec, specPath: path, key: key)
    controller.onClose = { [weak self] key in
      self?.windows.removeValue(forKey: key)
      self?.order.removeAll { $0 == key }
      self?.updateDock()
    }
    controller.build()
    // AppKit's standard cascade keeps successive windows independently reachable.
    if windows.isEmpty { cascadePoint = NSPoint.zero }
    cascadePoint = controller.panel.cascadeTopLeft(from: cascadePoint)
    windows[key] = controller
    order.append(key)
    updateDock()
    controller.show()
    controller.startWatchdog()
  }

  func updateDock() {
    NSApp.dockTile.badgeLabel = windows.isEmpty ? nil : String(windows.count)
    let policy: NSApplication.ActivationPolicy = windows.isEmpty ? .accessory : .regular
    if NSApp.activationPolicy() != policy { NSApp.setActivationPolicy(policy) }
    for controller in windows.values {
      var info = [controller.spec.deadline ?? ""].filter { !$0.isEmpty }
      if windows.count > 1 { info.append("ほかに \(windows.count - 1) 件待っています") }
      controller.pendingLabel.stringValue = info.joined(separator: "  ·  ")
    }
  }

  func applicationDockMenu(_ sender: NSApplication) -> NSMenu? {
    let menu = NSMenu()
    for key in order {
      guard let controller = windows[key] else { continue }
      let title = [controller.spec.location ?? "", controller.spec.title.components(separatedBy: .newlines).first ?? ""].filter { !$0.isEmpty }.joined(separator: " — ")
      let item = NSMenuItem(title: title, action: #selector(reveal(_:)), keyEquivalent: "")
      item.target = self
      item.representedObject = key
      menu.addItem(item)
    }
    return menu
  }

  @objc func reveal(_ sender: NSMenuItem) {
    guard let key = sender.representedObject as? String else { return }
    NSApp.activate(ignoringOtherApps: true)
    windows[key]?.panel.makeKeyAndOrderFront(nil)
  }

  func applicationDidBecomeActive(_ notification: Notification) {
    // An explicit application switch (⌘Tab) must reveal ordinary panels,
    // while preserving the clicked window's editor and first responder.
    let key = NSApp.keyWindow
    for id in order { windows[id]?.panel.orderFront(nil) }
    key?.makeKeyAndOrderFront(nil)
  }

  func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
    for key in order { windows[key]?.panel.orderFront(nil) }
    order.last.flatMap { windows[$0] }?.panel.makeKey()
    return true
  }

  func receive(_ command: DialogCommand) {
    switch command.command {
    case "open": if let path = command.spec_path { open(key: command.key, path: path) }
    case "close": windows[command.key]?.close()
    default: break
    }
  }
}

let app = NSApplication.shared
let owner = DialogApplication()
app.delegate = owner
app.setActivationPolicy(.accessory)
signal(SIGTERM) { _ in exit(0) }
NSEvent.addLocalMonitorForEvents(matching: .keyDown) { event in
  for controller in owner.windows.values where event.window === controller.panel {
    return controller.handleKey(event)
  }
  return event
}
let args = CommandLine.arguments
if args.count == 2, args[1] == "--stream" {
  DispatchQueue.global(qos: .userInitiated).async {
    while let line = readLine() {
      guard let command = try? JSONDecoder().decode(DialogCommand.self, from: Data(line.utf8)) else { continue }
      DispatchQueue.main.async { owner.receive(command) }
    }
    DispatchQueue.main.async { NSApp.terminate(nil) }
  }
} else if args.count == 2 {
  owner.open(key: args[1], path: args[1])
} else {
  FileHandle.standardError.write(Data("usage: herdr-decision-dialog <spec.json> | --stream\n".utf8))
  exit(2)
}
app.run()

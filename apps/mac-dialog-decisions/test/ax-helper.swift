// AX helper for live-verify: inspects and drives the dialog process through
// the Accessibility API without touching the user's mouse.
//
//   ax-helper probe <pid>              -> JSON {frontmost, windows:[{title,pos,size}], controls:[{role,title}]}
//   ax-helper press <pid> <title>      -> AXPress the control whose title contains <title>; prints {"pressed":bool}
//   ax-helper settext <pid> <text>     -> set the first AXTextField's value; prints {"set":bool}
//   ax-helper bounds <pid>             -> JSON {x,y,w,h} of the first window (for screencapture -R)

import Cocoa

let args = CommandLine.arguments
let cmd = args.count > 1 ? args[1] : ""
let pid = pid_t(args.count > 2 ? args[2] : "0") ?? 0
let app = AXUIElementCreateApplication(pid)

func attr(_ el: AXUIElement, _ name: String) -> CFTypeRef? {
  var v: CFTypeRef?
  guard AXUIElementCopyAttributeValue(el, name as CFString, &v) == .success else { return nil }
  return v
}
func str(_ el: AXUIElement, _ name: String) -> String { attr(el, name) as? String ?? "" }
func children(_ el: AXUIElement) -> [AXUIElement] { attr(el, kAXChildrenAttribute) as? [AXUIElement] ?? [] }
func pt(_ el: AXUIElement, _ name: String) -> CGPoint {
  var p = CGPoint.zero
  if let v = attr(el, name) { AXValueGetValue(v as! AXValue, .cgPoint, &p) }
  return p
}
func sz(_ el: AXUIElement, _ name: String) -> CGSize {
  var s = CGSize.zero
  if let v = attr(el, name) { AXValueGetValue(v as! AXValue, .cgSize, &s) }
  return s
}

func walk(_ el: AXUIElement, _ depth: Int, _ visit: (AXUIElement, String, String) -> Void) {
  if depth > 15 { return }
  let role = str(el, kAXRoleAttribute)
  let title = str(el, kAXTitleAttribute)
  visit(el, role, title)
  for c in children(el) { walk(c, depth + 1, visit) }
}

func windows() -> [AXUIElement] { attr(app, kAXWindowsAttribute) as? [AXUIElement] ?? [] }

func jsonEscape(_ s: String) -> String {
  var out = ""
  for u in s.unicodeScalars {
    switch u {
    case "\"": out += "\\\""
    case "\\": out += "\\\\"
    case "\n": out += "\\n"
    case "\t": out += "\\t"
    case "\r": out += "\\r"
    default:
      if u.value < 0x20 { out += String(format: "\\u%04x", u.value) } else { out.unicodeScalars.append(u) }
    }
  }
  return out
}

switch cmd {
case "probe":
  var frontmost = false
  if let v = attr(app, "AXFrontmost") as? Bool { frontmost = v }
  var wjson: [String] = []
  var cjson: [String] = []
  for w in windows() {
    let p = pt(w, kAXPositionAttribute)
    let s = sz(w, kAXSizeAttribute)
    wjson.append("{\"title\":\"\(jsonEscape(str(w, kAXTitleAttribute)))\",\"pos\":[\(p.x),\(p.y)],\"size\":[\(s.width),\(s.height)]}")
    walk(w, 0) { el, role, title in
      if role == "AXButton" || role == "AXCheckBox" || role == "AXTextField" || role == "AXTextArea" {
        cjson.append("{\"role\":\"\(role)\",\"title\":\"\(jsonEscape(title))\"}")
      }
    }
  }
  print("{\"frontmost\":\(frontmost),\"windows\":[\(wjson.joined(separator: ","))],\"controls\":[\(cjson.joined(separator: ","))]}")

case "press":
  let needle = args.count > 3 ? args[3] : ""
  var target: AXUIElement?
  outer: for w in windows() {
    walk(w, 0) { el, role, title in
      guard target == nil else { return }
      guard role == "AXButton" || role == "AXCheckBox" else { return }
      if title.contains(needle) { target = el }
    }
    if target != nil { break outer }
  }
  var pressed = false
  if let el = target {
    let err = AXUIElementPerformAction(el, kAXPressAction as CFString)
    if err == .success {
      pressed = true
    } else {
      // a press that submits makes the app terminate mid-action; the action
      // then reports failure even though it took effect. if the element (or
      // its window) is gone now, treat the press as having landed.
      usleep(800_000)
      var stillThere = false
      for w in windows() {
        var found = false
        walk(w, 0) { el2, _, _ in if CFEqual(el, el2) { found = true } }
        if found { stillThere = true; break }
      }
      pressed = !stillThere
    }
  }
  print("{\"pressed\":\(pressed)}")

case "settext":
  let text = args.count > 3 ? args[3] : ""
  var done = false
  for w in windows() {
    walk(w, 0) { el, role, _ in
      guard !done else { return }
      let ident = str(el, "AXIdentifier")
      if role == "AXTextField" || ident == "mac-dialog-free-text" {
        done = AXUIElementSetAttributeValue(el, kAXValueAttribute as CFString, text as CFTypeRef) == .success
      }
    }
    if done { break }
  }
  print("{\"set\":\(done)}")

case "bounds":
  if let w = windows().first {
    let p = pt(w, kAXPositionAttribute)
    let s = sz(w, kAXSizeAttribute)
    print("{\"x\":\(p.x),\"y\":\(p.y),\"w\":\(s.width),\"h\":\(s.height)}")
  } else {
    print("{}")
  }

default:
  FileHandle.standardError.write("usage: ax-helper probe|press|settext|bounds <pid> [arg]\n".data(using: .utf8)!)
  exit(2)
}

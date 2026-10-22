"""Build real AppKit declarations and assert the count of other open windows."""
import json
from pathlib import Path
import subprocess
import sys

root = Path(__file__).resolve().parents[1]
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=True)
source = (root / 'dialog.swift').read_text().split('let app = NSApplication.shared')[0]
for index in range(3):
    (out / f'{index}.json').write_text(json.dumps(dict(
        window_title='判断待ち件数の隔離テスト', title='件数確認', options=[dict(id='yes', label='はい')],
        multi_select=False, allow_text=False, queue_remaining=100, daemon_pid=None)))
source += r'''
let app = NSApplication.shared
let owner = DialogApplication()
app.delegate = owner
app.setActivationPolicy(.accessory)
let directory = CommandLine.arguments[1]
var records: [[String: Any]] = []
func check(_ count: Int) {
    precondition(owner.windows.count == count)
    let expected = count > 1 ? "ほかに \(count - 1) 件開いています" : ""
    for controller in owner.windows.values {
        precondition(controller.pendingLabel.stringValue == expected)
    }
    records.append(["open": count, "other_open_label": expected, "badge": NSApp.dockTile.badgeLabel ?? ""])
}
for index in 0..<3 { owner.open(key: String(index), path: "\(directory)/\(index).json"); check(index + 1) }
owner.windows["1"]!.close(); check(2)
owner.windows["2"]!.close(); check(1)
owner.windows["0"]!.close(); check(0)
precondition(NSApp.dockTile.badgeLabel == nil)
try JSONSerialization.data(withJSONObject: records, options: [.prettyPrinted, .sortedKeys]).write(to: URL(fileURLWithPath: "\(directory)/window-count.json"))
print("PASS: real window counts 1→2→3→2→1→0; queued count 100 never appears")
'''
(out / 'main.swift').write_text(source)
subprocess.run(['swiftc', '-O', str(out / 'main.swift'), '-o', str(out / 'window-count')], check=True)
subprocess.run([str(out / 'window-count'), str(out)], check=True)

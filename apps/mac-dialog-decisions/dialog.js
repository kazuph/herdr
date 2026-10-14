// mac-dialog-decisions dialog helper (JXA — osascript -l JavaScript).
//
//   osascript -l JavaScript dialog.js <spec.json>
//
// The spec is produced by mac-dialog-decisions.mjs:
//   { window_title, text, options: [{id, title}], multi_select, allow_text }
//
// The window shows the full decision text in a scrollable view. Answering
// writes exactly one JSON line to stdout and exits:
//   {"result":"answer","option_ids":["<id>",...],"text":"<free text>"}
//   {"result":"dismiss"}  (あとで button / Esc / window close)
// A {"event":"shown"} line is emitted first once the window is on screen.
// When the decision resolves elsewhere the daemon kills this process, so no
// stale answer can be sent.

ObjC.import("Cocoa");

var theApp = $.NSApplication.sharedApplication;
var theDelegate = null;
var theWin = null;
var spec = null;
var checkboxes = [];
var inputText = null;
var hintLabel = null;
var finished = false;

// In applet mode (launched via `open`) stdout goes nowhere, so events are
// also appended to spec.result_path as JSON lines for the daemon to poll.
var resultPath = null;

function emit(obj) {
  var s = JSON.stringify(obj) + "\n";
  var data = $.NSString.alloc
    .initWithUTF8String(s)
    .dataUsingEncoding($.NSUTF8StringEncoding);
  $.NSFileHandle.fileHandleWithStandardOutput.writeData(data);
  if (resultPath) {
    try {
      if (
        $.NSFileManager.defaultManager.fileExistsAtPath(resultPath)
      ) {
        var fh = $.NSFileHandle.fileHandleForWritingToURLError(
          $.NSURL.fileURLWithPath(resultPath),
          $.nil,
        );
        fh.seekToEndOfFile;
        fh.writeData(data);
        fh.closeFile;
      } else {
        $.NSFileManager.defaultManager.createFileAtPathContentsAttributes(
          resultPath,
          data,
          $.nil,
        );
      }
    } catch (e) {}
  }
}

function finish(result) {
  if (finished) return;
  finished = true;
  emit(result);
  $.NSApp.terminate(null);
}

function currentText() {
  if (!inputText) return "";
  return String(inputText.string.js || "").trim();
}

function checkedIds() {
  var ids = [];
  for (var i = 0; i < checkboxes.length; i++) {
    if (checkboxes[i].state === $.NSControlStateValueOn) {
      ids.push(checkboxes[i].representedObject.js);
    }
  }
  return ids;
}

var presented = false;

// Order the window front and take focus. Called both from runMain (applet
// mode, where the app is already launched) and applicationDidFinishLaunching
// (plain osascript mode); the flag keeps it single-shot.
function present() {
  if (presented || !theWin) return;
  presented = true;
  theWin.center;
  theWin.level = $.NSFloatingWindowLevel;
  theWin.collectionBehavior =
    $.NSWindowCollectionBehaviorMoveToActiveSpace;
  theWin.makeKeyAndOrderFront(null);
  theWin.orderFrontRegardless;
  scheduleActivateRetries();
  emit({ event: "shown", pid: $.NSProcessInfo.processInfo.processIdentifier });
}

function activateApp() {
  // bring the app itself forward; try the modern APIs first, then the
  // deprecated one as a fallback (any of these can silently no-op)
  try {
    $.NSRunningApplication.currentApplication.activateWithOptions(
      $.NSApplicationActivateAllWindows |
        $.NSApplicationActivateIgnoringOtherApps,
    );
  } catch (e) {}
  try {
    theApp.activate();
  } catch (e) {}
  try {
    theApp.activateIgnoringOtherApps(true);
  } catch (e) {}
}

function scheduleActivateRetries() {
  // Activation can be refused while the app is still launching, so try once
  // now and again a few times once the run loop is actually turning. Uses
  // NSObject's delayed-perform rather than NSTimer, which does not bridge
  // correctly under JXA.
  activateApp();
  for (var i = 1; i <= 12; i++) {
    try {
      theDelegate.performSelectorWithObjectAfterDelay(
        $.NSSelectorFromString("activateRetry:"),
        $.nil,
        i * 0.4,
      );
    } catch (e) {}
  }
}

ObjC.registerSubclass({
  name: "MDDialogDelegate",
  superclass: "NSObject",
  protocols: ["NSApplicationDelegate", "NSWindowDelegate"],
  properties: [],
  methods: {
    "applicationDidFinishLaunching:": {
      types: ["void", ["id"]],
      implementation: function (note) {
        // applet mode presents directly in runMain; this covers osascript
        // mode where the launch callback actually fires
        present();
      },
    },
    // delayed-perform target for activation retries once the run loop is up
    "activateRetry:": {
      types: ["void", ["id"]],
      implementation: function (ignored) {
        if (!finished && !theApp.isActive) activateApp();
      },
    },
    // single-select: one button per option, click = that answer (+free text)
    "optionClicked:": {
      types: ["void", ["id"]],
      implementation: function (sender) {
        var id = sender.representedObject.js;
        finish({ result: "answer", option_ids: [id], text: currentText() });
      },
    },
    // multi-select confirm / text-only 送信: checked ids + free text
    "confirmClicked:": {
      types: ["void", ["id"]],
      implementation: function (sender) {
        var ids = checkedIds();
        var text = currentText();
        if (ids.length === 0 && !text) {
          if (hintLabel) {
            hintLabel.textColor = $.NSColor.systemRedColor;
          }
          return;
        }
        finish({ result: "answer", option_ids: ids, text: text });
      },
    },
    "dismissClicked:": {
      types: ["void", ["id"]],
      implementation: function (sender) {
        finish({ result: "dismiss" });
      },
    },
    "windowWillClose:": {
      types: ["void", ["id"]],
      implementation: function (note) {
        finish({ result: "dismiss" });
      },
    },
  },
});

function makeScrollText(frame, text, editable, fontSize) {
  var scroll = $.NSScrollView.alloc.initWithFrame(frame);
  scroll.hasVerticalScroller = true;
  scroll.autohidesScrollers = false;
  scroll.borderType = $.NSBezelBorder;
  var tv = $.NSTextView.alloc.initWithFrame(
    $.NSMakeRect(0, 0, frame.size.width - 2, frame.size.height - 2),
  );
  tv.editable = editable;
  tv.selectable = true;
  tv.font = $.NSFont.systemFontOfSize(fontSize);
  tv.textContainerInset = $.NSMakeSize(6, 8);
  tv.verticallyResizable = true;
  tv.horizontallyResizable = false;
  tv.autoresizingMask = $.NSViewWidthSizable;
  tv.textContainer.widthTracksTextView = true;
  if (text) tv.string = text;
  scroll.documentView = tv;
  tv.scrollRangeToVisible($.NSMakeRange(0, 0));
  return { scroll: scroll, textView: tv };
}

function buildWindow(delegate) {
  var W = 760;
  var M = 16;
  var GAP = 8;
  var BAR_H = 40;
  var OPT_ROW = 28;
  var OPT_MAX_ROWS = 8;
  var BTN_H = 24;

  var nOpt = spec.options.length;
  var optH =
    nOpt > 0 ? Math.min(nOpt, OPT_MAX_ROWS) * OPT_ROW + (nOpt > OPT_MAX_ROWS ? 4 : 0) : 0;
  var inputH = spec.allow_text ? 78 : 0; // label 16 + gap + input scroll 54
  var textTarget = 320;

  var fixed =
    M * 2 +
    BAR_H +
    (optH > 0 ? optH + GAP : 0) +
    (inputH > 0 ? inputH + GAP : 0);
  var H = fixed + textTarget;
  var screenH = $.NSScreen.mainScreen.visibleFrame.size.height;
  var maxH = Math.floor(screenH * 0.85);
  if (H > maxH) H = maxH;
  var textH = H - fixed;

  var win = $.NSWindow.alloc.initWithContentRectStyleMaskBackingDefer(
    $.NSMakeRect(0, 0, W, H),
    $.NSWindowStyleMaskTitled |
      $.NSWindowStyleMaskClosable |
      $.NSWindowStyleMaskResizable,
    $.NSBackingStoreBuffered,
    false,
  );
  win.title = spec.window_title;
  win.delegate = delegate;
  win.minSize = $.NSMakeSize(520, 320);
  var view = win.contentView;

  // --- bottom bar: hint + confirm + dismiss ---
  var barY = M;
  var rightX = W - M;
  var dismissW = 92;
  var dismiss = $.NSButton.alloc.initWithFrame(
    $.NSMakeRect(rightX - dismissW, barY, dismissW, 30),
  );
  dismiss.title = "あとで";
  dismiss.bezelStyle = $.NSBezelStyleRounded;
  dismiss.target = delegate;
  dismiss.action = "dismissClicked:";
  dismiss.keyEquivalent = "\u001b"; // Esc
  dismiss.autoresizingMask = $.NSViewMinXMargin | $.NSViewMaxYMargin;
  view.addSubview(dismiss);
  rightX -= dismissW + GAP;

  var needsConfirm =
    (spec.options.length > 0 && spec.multi_select) ||
    (spec.options.length === 0 && spec.allow_text);
  if (needsConfirm) {
    var confirmW = spec.multi_select ? 150 : 80;
    var confirm = $.NSButton.alloc.initWithFrame(
      $.NSMakeRect(rightX - confirmW, barY, confirmW, 30),
    );
    confirm.title = spec.multi_select ? "この選択で回答" : "送信";
    confirm.bezelStyle = $.NSBezelStyleRounded;
    confirm.keyEquivalent = "\r";
    confirm.target = delegate;
    confirm.action = "confirmClicked:";
    confirm.autoresizingMask = $.NSViewMinXMargin | $.NSViewMaxYMargin;
    view.addSubview(confirm);
    rightX -= confirmW + GAP;
  }

  hintLabel = $.NSTextField.alloc.initWithFrame(
    $.NSMakeRect(M, barY + 6, rightX - M, 20),
  );
  hintLabel.bezeled = false;
  hintLabel.drawsBackground = false;
  hintLabel.editable = false;
  hintLabel.selectable = false;
  hintLabel.font = $.NSFont.systemFontOfSize(11);
  hintLabel.textColor = $.NSColor.secondaryLabelColor;
  if (spec.options.length > 0 && spec.multi_select) {
    hintLabel.stringValue =
      "選択肢にチェックして「この選択で回答」。自由入力も添えられます。";
  } else if (spec.options.length > 0) {
    hintLabel.stringValue =
      spec.allow_text
        ? "選択肢のボタンで回答。自由入力に書くと一緒に送ります。"
        : "選択肢のボタンで回答します。";
  } else {
    hintLabel.stringValue = "自由入力に書いて「送信」してください。";
  }
  hintLabel.autoresizingMask = $.NSViewMaxYMargin | $.NSViewWidthSizable;
  view.addSubview(hintLabel);

  var cursorY = barY + BAR_H; // top of the bottom bar

  // --- free-text input ---
  if (spec.allow_text) {
    var iy = cursorY + GAP;
    var label = $.NSTextField.alloc.initWithFrame(
      $.NSMakeRect(M, iy + inputH - 18, W - 2 * M, 16),
    );
    label.bezeled = false;
    label.drawsBackground = false;
    label.editable = false;
    label.selectable = false;
    label.font = $.NSFont.systemFontOfSize(11);
    label.textColor = $.NSColor.secondaryLabelColor;
    label.stringValue = "自由入力:";
    label.autoresizingMask = $.NSViewMaxYMargin;
    view.addSubview(label);
    var input = makeScrollText(
      $.NSMakeRect(M, iy, W - 2 * M, inputH - 22),
      "",
      true,
      13,
    );
    input.scroll.autoresizingMask =
      $.NSViewWidthSizable | $.NSViewMaxYMargin;
    view.addSubview(input.scroll);
    inputText = input.textView;
    try {
      // distinguish this editable text view from the read-only body for AX
      inputText.setAccessibilityIdentifier("mac-dialog-free-text");
    } catch (e) {}
    cursorY += inputH + GAP;
  }

  // --- option controls ---
  if (nOpt > 0) {
    var oy = cursorY + GAP;
    var containerH = nOpt * OPT_ROW;
    var innerW = W - 2 * M;
    var optionViews = [];
    for (var i = 0; i < nOpt; i++) {
      var rowY = containerH - (i + 1) * OPT_ROW + (OPT_ROW - BTN_H) / 2;
      var ctl;
      if (spec.multi_select) {
        ctl = $.NSButton.alloc.initWithFrame(
          $.NSMakeRect(0, rowY, innerW - 16, BTN_H),
        );
        ctl.setButtonType($.NSButtonTypeSwitch);
        ctl.title = spec.options[i].title;
      } else {
        ctl = $.NSButton.alloc.initWithFrame(
          $.NSMakeRect(0, rowY, innerW - 16, BTN_H),
        );
        ctl.title = spec.options[i].title;
        ctl.bezelStyle = $.NSBezelStyleRounded;
        ctl.target = delegate;
        ctl.action = "optionClicked:";
      }
      ctl.representedObject = $.NSString.alloc.initWithUTF8String(
        spec.options[i].id,
      );
      optionViews.push(ctl);
      if (spec.multi_select) checkboxes.push(ctl);
    }
    if (nOpt <= OPT_MAX_ROWS) {
      var box = $.NSView.alloc.initWithFrame(
        $.NSMakeRect(M, oy, innerW, optH),
      );
      box.autoresizingMask = $.NSViewWidthSizable | $.NSViewMaxYMargin;
      for (var j = 0; j < optionViews.length; j++) {
        box.addSubview(optionViews[j]);
      }
      view.addSubview(box);
    } else {
      var oscroll = $.NSScrollView.alloc.initWithFrame(
        $.NSMakeRect(M, oy, innerW, optH),
      );
      oscroll.hasVerticalScroller = true;
      oscroll.autohidesScrollers = false;
      oscroll.borderType = $.NSBezelBorder;
      oscroll.autoresizingMask =
        $.NSViewWidthSizable | $.NSViewMaxYMargin;
      var doc = $.NSView.alloc.initWithFrame(
        $.NSMakeRect(0, 0, innerW - 16, containerH),
      );
      for (var j2 = 0; j2 < optionViews.length; j2++) {
        doc.addSubview(optionViews[j2]);
      }
      oscroll.documentView = doc;
      view.addSubview(oscroll);
    }
    cursorY += optH + GAP;
  }

  // --- scrollable full text ---
  var textScroll = makeScrollText(
    $.NSMakeRect(M, cursorY, W - 2 * M, textH),
    spec.text,
    false,
    13,
  );
  textScroll.scroll.autoresizingMask =
    $.NSViewWidthSizable | $.NSViewHeightSizable;
  view.addSubview(textScroll.scroll);

  return win;
}

function run(argv) {
  try {
    runMain(argv);
  } catch (e) {
    emit({ result: "error", message: String(e) });
  }
}

function runMain(argv) {
  var procArgs = $.NSProcessInfo.processInfo.arguments;
  // Spec path sources, in order: env var (direct spawn), script argv
  // (osascript -l JavaScript dialog.js <spec>), then the process arguments
  // (applet launched via `open --args <spec>`).
  var specPath = ObjC.unwrap(
    $.NSProcessInfo.processInfo.environment.objectForKey("MAC_DIALOG_SPEC"),
  );
  if (!specPath && argv && argv.length > 0) {
    specPath = argv[0] && argv[0].js ? argv[0].js : String(argv[0]);
  }
  if (!specPath && procArgs.count > 1) {
    specPath = procArgs.objectAtIndex(1).js;
  }
  if (!specPath) {
    emit({ result: "error", message: "no spec path (argv/MAC_DIALOG_SPEC)" });
    return;
  }
  var raw = $.NSString.stringWithContentsOfFileEncodingError(
    specPath,
    $.NSUTF8StringEncoding,
    null,
  );
  if (!raw || raw.js === undefined) {
    emit({ result: "error", message: "spec file unreadable: " + specPath });
    return;
  }
  try {
    spec = JSON.parse(raw.js);
  } catch (e) {
    emit({ result: "error", message: "spec JSON parse failed" });
    return;
  }
  if (!spec || typeof spec.text !== "string") {
    emit({ result: "error", message: "spec missing text" });
    return;
  }
  if (typeof spec.result_path === "string" && spec.result_path) {
    resultPath = spec.result_path;
  }

  theApp.setActivationPolicy($.NSApplicationActivationPolicyRegular);
  var delegate = $.MDDialogDelegate.alloc.init;
  theDelegate = delegate;
  theApp.delegate = delegate;
  theWin = buildWindow(delegate);

  // Minimal Edit menu so Cmd+C / Cmd+A work inside the text views.
  var menubar = $.NSMenu.alloc.init;
  var editItem = $.NSMenuItem.alloc.init;
  menubar.addItem(editItem);
  var editMenu = $.NSMenu.alloc.initWithTitle("Edit");
  var items = [
    ["Cut", "cut:", "x"],
    ["Copy", "copy:", "c"],
    ["Paste", "paste:", "v"],
    ["Select All", "selectAll:", "a"],
  ];
  for (var i = 0; i < items.length; i++) {
    var item = $.NSMenuItem.alloc.initWithTitleActionKeyEquivalent(
      items[i][0],
      items[i][1],
      items[i][2],
    );
    editMenu.addItem(item);
  }
  editItem.submenu = editMenu;
  theApp.mainMenu = menubar;

  // In applet mode the app is already launched, so applicationDidFinish-
  // Launching never fires; present right away. In osascript mode the
  // delegate fires too — the flag inside present() deduplicates.
  present();

  // Blocks in both modes until finish() calls $.NSApp.terminate.
  $.NSApp.run;
}

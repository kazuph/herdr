// Run with the Cargo.lock unicode-width 0.2.2 crate, on the build machine.
// rustc --edition 2021 --crate-name unicode_width --crate-type lib <crate>/src/lib.rs -o /tmp/libunicode_width.rlib
// rustc --edition 2021 terminal-width-generator.rs --extern unicode_width=/tmp/libunicode_width.rlib -o /tmp/terminal-width-generator
use unicode_width::UnicodeWidthChar;
fn main() {
    println!("enum TerminalWidth {{\n  static let ranges: [(UInt32, UInt32, Int)] = [");
    let mut run = None;
    for value in 0..=0x110000 {
        let width = char::from_u32(value).map(|ch| ch.width().unwrap_or(0)).unwrap_or(1);
        match run {
            Some((start, previous)) if width != previous => {
                println!("    (0x{start:X}, 0x{:X}, {previous}),", value - 1);
                run = if width != 1 { Some((value, width)) } else { None };
            }
            None if width != 1 => run = Some((value, width)),
            _ => {}
        }
    }
    println!("  ]\n  static func scalar(_ value: UInt32) -> Int {{\n    var low = 0\n    var high = ranges.count\n    while low < high {{\n      let mid = (low + high) / 2\n      let range = ranges[mid]\n      if value < range.0 {{ high = mid }}\n      else if value > range.1 {{ low = mid + 1 }}\n      else {{ return range.2 }}\n    }}\n    return 1\n  }}\n  static func cells(_ text: String) -> Int {{\n    text.unicodeScalars.reduce(0) {{ $0 + scalar($1.value) }}\n  }}\n}}");
}

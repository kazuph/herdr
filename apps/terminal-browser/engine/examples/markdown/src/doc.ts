import { writeDemoImage } from "./image";

const imagePath = writeDemoImage();

export const DOC = `# Markdown in a terminal

Everything below is parsed in Rust with **pulldown-cmark** and painted by the
pixel engine — real *italics*, real ~~strikethrough~~, real \`inline code\`,
and [links](https://github.com/pulldown-cmark/pulldown-cmark) with underlines.
Because we render pixels instead of cells, headings can be ***actually bigger***.

## Streaming repair

This document streams in chunk by chunk, the way an LLM would emit it. Watch
the tail of the text: half-typed markers like an unclosed bold never flash as
literal asterisks — a repair pass completes them before parsing, so **styling
appears the moment the opening marker arrives** and stays stable while the
rest streams in.

## Code

Fences use the same tree-sitter highlighting as the rest of the engine. An
unclosed fence renders as code from the first line, so streamed code never
flashes as prose:

\`\`\`rust
fn fib(n: u64) -> u64 {
    match n {
        0 | 1 => n, // comments render muted
        _ => fib(n - 1) + fib(n - 2),
    }
}
\`\`\`

\`\`\`typescript
const spans = parseMarkdown(text, streaming).flatMap((block) => block.spans);
console.log(\`parsed \${spans.length} styled spans\`);
\`\`\`

## Tables

| Feature | Status | Notes |
|:--------|:------:|------:|
| **Bold cells** | ✓ | styled spans work in cells |
| \`inline code\` | ✓ | pills too |
| Alignment | ✓ | left, center, right |
| Streaming | ✓ | rows appear as they arrive |

Jump straight to [Images](#images) — heading links scroll the document.

## Structure

1. Ordered lists keep their numbers
2. Nesting works
   - bullets indent under their parent
   - markers hang in a gutter so wrapped lines stay aligned
3. Task lists show their state:
   - [x] parse markdown in Rust
   - [x] repair half-streamed syntax
   - [ ] tables (soon)

> Blockquotes get a bar and group consecutive blocks together.
> A second paragraph in the same quote shares the bar.

---

## Images

Kitty graphics means real raster images inline — this one is generated on the
fly at startup:

![gradient](${imagePath})

*That's the whole tour — it restarts in a moment.*
`;

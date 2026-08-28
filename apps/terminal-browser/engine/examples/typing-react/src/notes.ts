export interface Note {
  id: number;
  title: string;
  text: string;
}

export const demoNotes: Note[] = [
  {
    id: 1,
    title: "welcome",
    text: [
      "This editor is React all the way down: the sidebar, this text,",
      "and the input you are typing into are host components rendered",
      "by a custom reconciler into a retained scene tree in Rust.",
      "",
      "Typing never round-trips through JavaScript. The engine owns the",
      "text state and echoes keystrokes locally; React hears about it",
      "afterwards through onChange.",
      "",
      "Try: click around, drag a selection, scroll with the wheel,",
      "cmd-z to undo, and switch notes on the left.",
    ].join("\n"),
  },
  {
    id: 2,
    title: "how it renders",
    text: [
      "React commit -> batched mutation ops -> napi bridge -> retained",
      "tree -> taffy layout -> CPU canvas -> kitty graphics frame.",
      "",
      "Scrolling and hover run entirely engine-side, so they stay smooth",
      "no matter what React is doing.",
    ].join("\n"),
  },
  {
    id: 3,
    title: "scratch",
    text: "",
  },
];

import type { Detect } from "../terminal";
import { cmux } from "./cmux";
import { ghostty } from "./ghostty";
import { gtx } from "./gtx";
import { herdr } from "./herdr";
import { kitty } from "./kitty";
import { supacode } from "./supacode";
import { tmux } from "./tmux";
import { tty7 } from "./tty7";
import { vscode } from "./vscode";
import { wezterm } from "./wezterm";

/**
 * not fantastic, but ordering does matter
 */
export const TERMINALS: Detect[] = [
  gtx,
  herdr,
  tmux,
  tty7,
  wezterm,
  kitty,
  cmux,
  supacode,
  ghostty,
  vscode,
];
